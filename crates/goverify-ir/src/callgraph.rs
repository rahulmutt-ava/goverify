//! Whole-DAG call graph (phase-2 spec §4.1). Static edges are precise;
//! invoke edges use implements-based approximation over method sets;
//! function-value edges use address-taken × signature matching. Extra
//! edges only widen summaries — they never invent findings.

use std::collections::{BTreeSet, HashMap};

use crate::func::{ValueId, ValueKind};
use crate::op::{Callee, Op, SelectArm};
use crate::program::{FuncId, MethodInfo, Program};
use crate::types::TypeId;

pub struct CallGraph {
    callees: Vec<Vec<FuncId>>, // indexed by FuncId
}

/// `(method name, sig) → [(owner type's method set, concrete FuncId)]`,
/// the index `resolve_invoke` filters by method-set inclusion.
type MethodIndex<'a> = HashMap<(&'a str, TypeId), Vec<(&'a Vec<MethodInfo>, FuncId)>>;

impl CallGraph {
    pub fn callees(&self, f: FuncId) -> &[FuncId] {
        self.callees.get(f.0 as usize).map_or(&[], Vec::as_slice)
    }

    pub fn build(p: &Program) -> CallGraph {
        // Index 1: (method name, sig) → [(owner type methods, concrete FuncId)]
        // Index 2: address-taken functions grouped by signature TypeId.
        let mut by_name_sig: MethodIndex = HashMap::new();
        for methods in p.method_sets.values() {
            if methods.iter().any(|m| m.func.is_none()) {
                continue; // interface set: not a concrete implementer
            }
            for m in methods {
                if let Some(f) = m.func {
                    by_name_sig
                        .entry((m.name.as_str(), m.sig))
                        .or_default()
                        .push((methods, f));
                }
            }
        }
        let mut address_taken: HashMap<TypeId, BTreeSet<FuncId>> = HashMap::new();
        for id in p.func_ids() {
            let Some(f) = p.func(id) else { continue };
            for b in &f.blocks {
                for ins in &b.instrs {
                    let mut mark = |v: ValueId| {
                        if let ValueKind::FuncRef(target) = &f.value(v).kind {
                            address_taken
                                .entry(f.value(v).ty)
                                .or_default()
                                .insert(*target);
                        }
                    };
                    // Unconditional, every op: covers every plain value
                    // operand *and* `MakeClosure`'s `bindings` (a captured
                    // value that is itself a bare `FuncRef` — e.g. `g :=
                    // someNamedFunc; return func(){ g() }` — must be
                    // address-taken even when the binding is its only use
                    // anywhere in the function; review fix, was
                    // previously skipped because the match below used to
                    // short-circuit `MakeClosure` before reaching this
                    // scan).
                    for v in op_value_operands(&ins.op) {
                        mark(v);
                    }
                    // `func` is a `FuncId` embedded directly in the op, not
                    // a `ValueId` in the value table, so it can't come
                    // through `op_value_operands` above — recorded here
                    // separately, in addition to (not instead of) the scan.
                    if let Op::MakeClosure { func, dst, .. } = &ins.op {
                        address_taken
                            .entry(f.value(*dst).ty)
                            .or_default()
                            .insert(*func);
                    }
                }
            }
        }
        let n = p.func_ids().count();
        let mut callees: Vec<BTreeSet<FuncId>> = vec![BTreeSet::new(); n];
        for id in p.func_ids() {
            let Some(f) = p.func(id) else { continue };
            let out = &mut callees[id.0 as usize];
            for b in &f.blocks {
                for ins in &b.instrs {
                    let callee = match &ins.op {
                        Op::Call { callee, .. }
                        | Op::Go { callee, .. }
                        | Op::Defer { callee, .. } => callee,
                        _ => continue,
                    };
                    match callee {
                        Callee::Static(t) => {
                            out.insert(*t);
                        }
                        Callee::Builtin(_) => {}
                        Callee::Invoke { iface, method, sig } => {
                            resolve_invoke(p, &by_name_sig, *iface, method, *sig, out);
                        }
                        Callee::Dynamic { value } => {
                            if let Some(set) = address_taken.get(&f.value(*value).ty) {
                                out.extend(set.iter().copied());
                            }
                        }
                    }
                }
            }
        }
        CallGraph {
            callees: callees
                .into_iter()
                .map(|s| s.into_iter().collect())
                .collect(),
        }
    }
}

fn resolve_invoke(
    p: &Program,
    by_name_sig: &MethodIndex,
    iface: TypeId,
    method: &str,
    sig: TypeId,
    out: &mut BTreeSet<FuncId>,
) {
    let Some(candidates) = by_name_sig.get(&(method, sig)) else {
        return;
    };
    // Interface's own method set, when known, filters candidates to true
    // implementers (method-set inclusion).
    let iface_ms: Option<&Vec<MethodInfo>> = p
        .method_sets
        .get(&iface)
        .filter(|ms| ms.iter().all(|m| m.func.is_none()));
    for (impl_ms, f) in candidates {
        let implements = match iface_ms {
            Some(req) => req.iter().all(|rm| {
                impl_ms
                    .iter()
                    .any(|im| im.name == rm.name && im.sig == rm.sig)
            }),
            None => true, // anonymous iface: name+sig fallback
        };
        if implements {
            out.insert(*f);
        }
    }
}

/// Every ValueId an op reads (not defines). Add arms for ALL Op variants;
/// the compiler's exhaustiveness check is the point — a new op can't
/// silently hide function references. `build()`'s address-taken scan
/// calls this unconditionally on *every* instruction, `MakeClosure`
/// included — its `bindings` are real value reads (a bound value can
/// itself be a bare `FuncRef`) and must be scanned, same as any other
/// op's operands; `Callee::Dynamic`'s function-value operand is included
/// for `Call`/`Go`/`Defer` (via `callee_operands`) for the same reason.
/// `MakeClosure`'s `func` field is the one exception: it's a `FuncId`
/// embedded directly in the op, not a `ValueId` in the value table, so it
/// can't be returned from here — `build()` records it separately, in
/// addition to (not instead of) calling this function.
fn op_value_operands(op: &Op) -> Vec<ValueId> {
    match op {
        Op::Assign { src, .. } => vec![*src],
        Op::Alloc { .. } => vec![],
        Op::Load { addr, .. } => vec![*addr],
        Op::Store { addr, val } => vec![*addr, *val],
        Op::FieldAddr { base, .. } => vec![*base],
        Op::Field { base, .. } => vec![*base],
        Op::IndexAddr { base, index, .. } => vec![*base, *index],
        Op::Index { base, index, .. } => vec![*base, *index],
        Op::Lookup { map, key, .. } => vec![*map, *key],
        Op::Slice {
            base,
            low,
            high,
            max,
            ..
        } => {
            let mut v = vec![*base];
            v.extend(low.iter().chain(high.iter()).chain(max.iter()).copied());
            v
        }
        Op::BinOp { lhs, rhs, .. } => vec![*lhs, *rhs],
        Op::UnOp { operand, .. } => vec![*operand],
        Op::Convert { src, .. } => vec![*src],
        Op::Extract { tuple, .. } => vec![*tuple],
        Op::Phi { edges, .. } => edges.clone(),
        Op::Call { callee, args, .. } => callee_operands(callee, args),
        Op::MakeClosure { bindings, .. } => bindings.clone(),
        Op::MakeInterface { src, .. } => vec![*src],
        Op::Make { args, .. } => args.clone(),
        Op::Send { chan, val } => vec![*chan, *val],
        Op::Recv { chan, .. } => vec![*chan],
        Op::CloseChan { chan } => vec![*chan],
        Op::Select { arms, .. } => select_arm_operands(arms),
        Op::Go { callee, args } => callee_operands(callee, args),
        Op::Defer { callee, args } => callee_operands(callee, args),
        Op::Return { vals } => vals.clone(),
        Op::Jump => vec![],
        Op::Branch { cond } => vec![*cond],
        Op::Panic { val } => vec![*val],
        Op::TypeAssert { src, .. } => vec![*src],
        Op::Lock { mu, .. } => vec![*mu],
        Op::Havoc { .. } => vec![],
    }
}

fn callee_operands(callee: &Callee, args: &[ValueId]) -> Vec<ValueId> {
    let mut v = args.to_vec();
    if let Callee::Dynamic { value } = callee {
        v.push(*value);
    }
    v
}

fn select_arm_operands(arms: &[SelectArm]) -> Vec<ValueId> {
    arms.iter()
        .flat_map(|a| std::iter::once(a.chan).chain(a.send))
        .collect()
}

/// Strongly-connected-component condensation of a `CallGraph`
/// (iterative Tarjan; phase-2 spec, Task 10). A mutually-recursive cluster
/// of functions collapses into one SCC; `schedule()` orders SCCs
/// callees-first (reverse topological — every SCC appears after all SCCs
/// it calls into), which is exactly the order an analysis that needs
/// callee summaries before caller summaries must process functions in.
///
/// Determinism: the outer loop seeds roots in ascending `FuncId` order and
/// visits callees in `CallGraph::callees`'s sorted order, so the schedule
/// (and thus any analysis driven by it) is byte-stable across runs and
/// machines — no `HashMap` iteration anywhere in this algorithm. Tarjan is
/// formulated iteratively with an explicit frame stack rather than
/// recursively: the call graph can have long chains (deep stdlib call
/// chains), and a recursive DFS would risk a stack overflow on them.
pub struct Sccs {
    schedule: Vec<Vec<FuncId>>,   // callees-first
    scc_of: Vec<usize>,           // FuncId index -> position in schedule
    callee_sccs: Vec<Vec<usize>>, // per schedule position, deduped, no self
}

impl Sccs {
    pub fn compute(p: &Program, g: &CallGraph) -> Sccs {
        Self::compute_from_graph(p.func_ids().count(), g)
    }

    /// Split out so unit tests can hand-build small graphs (`from_edges`)
    /// without constructing a `Program`.
    pub fn compute_from_graph(n: usize, g: &CallGraph) -> Sccs {
        const UNVISITED: u32 = u32::MAX;
        let mut index = vec![UNVISITED; n];
        let mut lowlink = vec![0u32; n];
        let mut on_stack = vec![false; n];
        let mut stack: Vec<u32> = Vec::new();
        let mut next_index = 0u32;
        let mut schedule: Vec<Vec<FuncId>> = Vec::new();
        let mut scc_of = vec![usize::MAX; n];

        // Iterative Tarjan: frame = (node, next-child-cursor). Seeding the
        // outer loop in ascending FuncId order (0..n) and consuming each
        // node's callees in `g.callees`'s sorted order is what makes the
        // resulting schedule deterministic.
        for root in 0..n as u32 {
            if index[root as usize] != UNVISITED {
                continue;
            }
            let mut frames: Vec<(u32, usize)> = vec![(root, 0)];
            while let Some(&(node, cursor)) = frames.last() {
                let ni = node as usize;
                if cursor == 0 {
                    index[ni] = next_index;
                    lowlink[ni] = next_index;
                    next_index += 1;
                    stack.push(node);
                    on_stack[ni] = true;
                }
                let edges = g.callees(FuncId(node));
                if cursor < edges.len() {
                    let child = edges[cursor].0;
                    frames.last_mut().unwrap().1 += 1;
                    let ci = child as usize;
                    if ci >= n {
                        continue; // defensive: malformed/out-of-range edge
                    }
                    if index[ci] == UNVISITED {
                        frames.push((child, 0));
                    } else if on_stack[ci] {
                        lowlink[ni] = lowlink[ni].min(index[ci]);
                    }
                } else {
                    frames.pop();
                    if let Some(&(parent, _)) = frames.last() {
                        let pi = parent as usize;
                        lowlink[pi] = lowlink[pi].min(lowlink[ni]);
                    }
                    if lowlink[ni] == index[ni] {
                        let mut members = Vec::new();
                        loop {
                            let w = stack.pop().unwrap();
                            on_stack[w as usize] = false;
                            members.push(FuncId(w));
                            if w == node {
                                break;
                            }
                        }
                        members.sort_unstable();
                        for m in &members {
                            scc_of[m.0 as usize] = schedule.len();
                        }
                        schedule.push(members);
                    }
                }
            }
        }
        // Callee-SCC deps per schedule slot: which other SCCs does this
        // one call into (excluding itself — a self-edge from recursion
        // within the SCC isn't an inter-SCC dependency). `scc_of.get(...)`
        // (not direct indexing) mirrors the main traversal loop's `if ci
        // >= n { continue; }` guard: a caller-supplied `g` can have a
        // callee id out of range for this `n` (a future Task 13-15 caller
        // passing a stale/mismatched graph, or a hand-built test graph),
        // and this must degrade — skip the bad edge — rather than index
        // out of bounds and panic.
        let mut callee_sccs: Vec<Vec<usize>> = vec![Vec::new(); schedule.len()];
        for (si, members) in schedule.iter().enumerate() {
            let mut deps: Vec<usize> = members
                .iter()
                .flat_map(|&m| {
                    g.callees(m)
                        .iter()
                        .filter_map(|&c| scc_of.get(c.0 as usize).copied())
                })
                .filter(|&d| d != si && d != usize::MAX)
                .collect();
            deps.sort_unstable();
            deps.dedup();
            callee_sccs[si] = deps;
        }
        Sccs {
            schedule,
            scc_of,
            callee_sccs,
        }
    }

    /// SCCs in callees-first (reverse topological) order; members within
    /// each SCC sorted ascending by `FuncId`.
    pub fn schedule(&self) -> &[Vec<FuncId>] {
        &self.schedule
    }

    /// Index into `schedule()` of the SCC containing `f`.
    pub fn scc_of(&self, f: FuncId) -> usize {
        self.scc_of[f.0 as usize]
    }

    /// SCCs that schedule-position `i` calls into (deduped, excludes `i`
    /// itself).
    pub fn callee_sccs(&self, i: usize) -> &[usize] {
        &self.callee_sccs[i]
    }
}

/// Test-only constructor: builds a `CallGraph` directly from an edge list
/// (`(caller, callee)` pairs, indexed 0..n), skipping `Program`/lowering so
/// SCC unit tests can hand-build small graphs. `pub(crate)` (not private)
/// so it's usable from other in-crate test modules; `#[cfg(test)]` keeps it
/// out of the release build.
#[cfg(test)]
pub(crate) fn from_edges(n: usize, edges: &[(u32, u32)]) -> CallGraph {
    let mut callees = vec![std::collections::BTreeSet::new(); n];
    for &(a, b) in edges {
        callees[a as usize].insert(FuncId(b));
    }
    CallGraph {
        callees: callees
            .into_iter()
            .map(|s| s.into_iter().collect())
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use goverify_extract::gvir;

    /// Regression (review fix): a named function captured *only* as a
    /// `MakeClosure` binding — never appearing as a plain value operand
    /// anywhere else in the function — must still be recorded
    /// address-taken under its signature TypeId, so a later
    /// `Callee::Dynamic` call through that same signature resolves to it.
    ///
    /// Before the fix, `build()`'s address-taken scan special-cased
    /// `MakeClosure` to record only its `func` field and never inspected
    /// `bindings`, so this edge was silently missing — an
    /// under-approximation, the one failure mode this module must never
    /// have (extra edges are fine; missing ones are not).
    ///
    /// Models `g := t.NamedFn; return func(){ ...g... }` where `g` is
    /// never used in `t.Outer` except as the closure binding, plus an
    /// unrelated dynamic call through a same-signature-typed parameter
    /// that must resolve to `t.NamedFn` via the address-taken set.
    #[test]
    fn makeclosure_binding_only_reference_is_address_taken() {
        let pkg = gvir::Package {
            import_path: "t".into(),
            types: vec![gvir::Type {
                id: 1,
                repr: "func()".into(),
                kind: gvir::TypeKind::Signature as i32,
                ..Default::default()
            }],
            functions: vec![gvir::Function {
                id: "t.Outer".into(),
                params: vec![gvir::Param {
                    id: 1,
                    name: "p".into(),
                    r#type: 1, // same signature type as the captured func
                }],
                aux: vec![
                    gvir::AuxValue {
                        id: 2,
                        kind: "Function".into(),
                        repr: "t.NamedFn".into(),
                        r#type: 1, // captured func's own signature type
                        ..Default::default()
                    },
                    gvir::AuxValue {
                        id: 3,
                        kind: "Function".into(),
                        repr: "t.Outer$1".into(),
                        ..Default::default()
                    },
                ],
                blocks: vec![gvir::BasicBlock {
                    index: 0,
                    instrs: vec![
                        // g := t.NamedFn bound into the closure; g is
                        // never referenced anywhere else in t.Outer.
                        gvir::Instruction {
                            kind: "MakeClosure".into(),
                            register: 4,
                            operands: vec![3, 2], // [fn, bindings...]
                            ..Default::default()
                        },
                        // An unrelated dynamic call through a value that
                        // merely happens to share t.NamedFn's signature.
                        gvir::Instruction {
                            kind: "Call".into(),
                            operands: vec![1],
                            sem: Some(gvir::instruction::Sem::Call(gvir::CallSem::default())),
                            ..Default::default()
                        },
                        gvir::Instruction {
                            kind: "Return".into(),
                            ..Default::default()
                        },
                    ],
                    succs: vec![],
                }],
                ..Default::default()
            }],
            ..Default::default()
        };
        let p = Program::from_packages(vec![pkg]);
        let outer = p.lookup_func("t.Outer").unwrap();
        let named_fn = p.lookup_func("t.NamedFn").unwrap();
        let g = CallGraph::build(&p);
        assert!(
            g.callees(outer).contains(&named_fn),
            "t.NamedFn is captured only as a MakeClosure binding and must \
             still be address-taken under its signature type, so the \
             dynamic call resolves to it; got {:?}",
            g.callees(outer)
        );
    }

    #[test]
    fn schedule_is_callees_first() {
        // 0 -> 1 -> 2, 0 -> 2
        let g = from_edges(3, &[(0, 1), (1, 2), (0, 2)]);
        let sccs = Sccs::compute_from_graph(3, &g);
        let order: Vec<u32> = sccs.schedule().iter().map(|s| s[0].0).collect();
        assert_eq!(
            order,
            vec![2, 1, 0],
            "Sccs::schedule() must be callees-first"
        );
    }

    #[test]
    fn mutual_recursion_is_one_scc() {
        // 0 <-> 1, both call 2
        let g = from_edges(3, &[(0, 1), (1, 0), (0, 2), (1, 2)]);
        let sccs = Sccs::compute_from_graph(3, &g);
        assert_eq!(
            sccs.schedule().len(),
            2,
            "Sccs::schedule() must collapse the mutually-recursive pair into one SCC"
        );
        assert_eq!(sccs.schedule()[0], vec![FuncId(2)]);
        assert_eq!(sccs.schedule()[1], vec![FuncId(0), FuncId(1)]); // sorted members
    }

    #[test]
    fn self_recursive_function_is_its_own_scc() {
        let g = from_edges(2, &[(0, 0), (0, 1)]);
        let sccs = Sccs::compute_from_graph(2, &g);
        assert_eq!(sccs.schedule(), &[vec![FuncId(1)], vec![FuncId(0)]]);
        assert_eq!(
            sccs.callee_sccs(1),
            &[0],
            "Sccs::callee_sccs() must exclude the self-edge"
        );
    }

    /// Regression (review fix): `from_edges` doesn't validate that edge
    /// targets are `< n`, so it can hand-build the same malformed-graph
    /// shape a real caller might pass — e.g. a future Task 13-15 caller
    /// with a `g`/`n` mismatch, or any fuzzed/inconsistent input reaching
    /// `Sccs` through `Program`/`CallGraph`. FuncId(5) is out of range for
    /// n=2. Before the fix, the callee-SCC pass indexed `scc_of` directly
    /// with an unchecked callee id and panicked; the main traversal loop
    /// already guarded the same input class (`if ci >= n { continue; }`),
    /// so this must degrade the same way, not panic.
    #[test]
    fn callee_sccs_ignores_out_of_range_callee_ids() {
        let g = from_edges(2, &[(0, 1), (0, 5)]);
        let sccs = Sccs::compute_from_graph(2, &g);
        assert_eq!(sccs.schedule(), &[vec![FuncId(1)], vec![FuncId(0)]]);
        assert_eq!(
            sccs.callee_sccs(1), // schedule slot for {FuncId(0)}
            &[0],
            "the out-of-range callee (FuncId(5)) must be dropped, not panic"
        );
    }
}
