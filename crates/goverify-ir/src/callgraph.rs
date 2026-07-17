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
                    match &ins.op {
                        Op::MakeClosure { func, dst, .. } => {
                            address_taken
                                .entry(f.value(*dst).ty)
                                .or_default()
                                .insert(*func);
                        }
                        op => {
                            for v in op_value_operands(op) {
                                mark(v);
                            }
                        }
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
/// silently hide function references. `Callee::Dynamic`'s function-value
/// operand is included for `Call`/`Go`/`Defer` (via `callee_operands`)
/// since it is exactly the kind of function-value read this scan exists
/// to catch; `MakeClosure`'s `func` field is handled separately by the
/// caller (it's a `FuncId`, not a `ValueId`), but its `bindings` are real
/// value reads and are included here.
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
