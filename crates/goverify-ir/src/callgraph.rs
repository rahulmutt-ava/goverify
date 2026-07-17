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
}
