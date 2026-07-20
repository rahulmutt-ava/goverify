//! Plumbing shared by every path-sensitive checker (nil, bounds, …):
//! the params-only groundness test, own-preconditions extraction, and
//! the two call-site loops (requires propagation; call-site
//! obligations) that every checker built on `encode_func` needs
//! identically. Extracted in Task 8 to stop a third copy from forming
//! (nil.rs carried the Task 6 originals; bounds.rs needed the same
//! logic verbatim).

use goverify_analysis::{
    Clause, EncodedFunc, Formula, Obligation, Summary, instantiate_requires, strictly_dominates,
};
use goverify_ir::{Callee, FuncId, Function, Op, Pos, Program, ValueId};
use goverify_solver::{Query, SatResult, Term, ptr_is_nil};

/// True iff every free var of `t` is a p<i> param name: the only vars a
/// requires-clause (evaluated at the callee's own entry, or bound at a
/// call site) may depend on.
pub(crate) fn params_only(t: &Term) -> bool {
    t.free_vars().keys().all(|n| {
        n.strip_prefix('p')
            .is_some_and(|rest| rest.parse::<u32>().is_ok())
    })
}

/// The function's own requires clauses as query conjuncts: their free
/// p<i> vars are exactly the encoder's param const names, so they can
/// be conjoined directly.
pub(crate) fn own_preconditions(own: &Summary) -> Vec<Term> {
    own.requires
        .iter()
        .map(|c| c.formula.term.clone())
        .collect()
}

/// Push a clause unless an equal one is present (fixpoint-friendly dedup;
/// `infer_requires` reruns on every round of a recursive SCC's fixpoint).
pub(crate) fn push_clause(out: &mut Vec<Clause>, c: Clause) {
    if !out.contains(&c) {
        out.push(c);
    }
}

/// Checked-deref assumptions (fix-wave fix 2b): for an obligation at
/// (block, instr), every nil-deref site that strictly precedes it on
/// EVERY execution reaching it — same block earlier, or a strictly
/// dominating block — already executed without panicking, so its
/// subject was non-nil on that execution. Emitted guard-conditioned
/// (¬g_site ∨ ¬is_nil(subj)) so a dominance bug can only lose
/// precision, never invent a fact. The obligation's own site is
/// excluded by the strict ordering: a genuine first-failure site still
/// fires, and a finding masked by an earlier one reappears once the
/// earlier is fixed.
pub(crate) fn checked_deref_assumptions(
    sites: &[(usize, usize, ValueId, Option<Pos>)],
    enc: &EncodedFunc,
    idom: &[Option<usize>],
    block: usize,
    instr: usize,
) -> Vec<Term> {
    let mut out = Vec::new();
    for (bj, ij, subj, _) in sites {
        let before = (*bj == block && *ij < instr) || strictly_dominates(idom, *bj, block);
        if !before {
            continue;
        }
        let Some(s) = enc.value(*subj).cloned() else {
            continue;
        };
        let Ok(is_nil) = ptr_is_nil(s) else { continue };
        let Ok(nonnil) = Term::not(is_nil) else {
            continue;
        };
        let Some(g) = enc.guards.get(*bj).cloned() else {
            continue;
        };
        let Ok(ng) = Term::not(g) else { continue };
        if let Ok(implied) = Term::or(vec![ng, nonnil]) {
            out.push(implied);
        }
    }
    out
}

/// Propagated requires: every callee requires-clause tagged `tag`,
/// instantiated at each of `func`'s static call sites, kept only when it
/// stays expressible over `func`'s own params (`params_only`) and its
/// violation is confirmed reachable (`discharge` returns `Sat` — never
/// `Unknown`/`Unsat`). Appends to `out` with fixpoint-safe dedup.
#[allow(clippy::too_many_arguments)]
pub(crate) fn propagate_requires(
    p: &Program,
    func: &Function,
    enc: &EncodedFunc,
    tag: &str,
    summary_of: &dyn Fn(FuncId) -> Summary,
    discharge: &mut dyn FnMut(&Query) -> SatResult,
    assume: &dyn Fn(usize, usize) -> Vec<Term>,
    out: &mut Vec<Clause>,
) {
    let _ = p; // kept for signature symmetry with call_site_obligations
    for (bi, ii, ins) in func
        .blocks
        .iter()
        .enumerate()
        .flat_map(|(bi, b)| b.instrs.iter().enumerate().map(move |(ii, i)| (bi, ii, i)))
    {
        let Op::Call {
            callee: Callee::Static(c),
            args,
            ..
        } = &ins.op
        else {
            continue;
        };
        let arg_terms: Vec<Option<Term>> = args.iter().map(|a| enc.value(*a).cloned()).collect();
        for bc in instantiate_requires(&summary_of(*c), &arg_terms) {
            if bc.tag != tag {
                continue; // each checker only owns its own tag
            }
            let (Some(bound), Some(violation)) = (bc.bound, bc.violation) else {
                continue;
            };
            if !params_only(&bound) {
                continue;
            }
            let mut extra = assume(bi, ii);
            extra.push(violation);
            if discharge(&enc.reach_query(bi, extra)) != SatResult::Sat {
                continue;
            }
            push_clause(
                out,
                Clause {
                    tag: tag.into(),
                    formula: Formula { term: bound },
                },
            );
        }
    }
}

/// Resolve `v` through same-function `Op::Assign` chains (`ChangeType`
/// lowers to Assign — lower.rs) to its root value. Depth-capped at 64,
/// mirroring AddrKey's cap: crafted .gvir may fabricate cycles or
/// arbitrarily long chains, and untrusted bytes must degrade (return
/// the current value), never hang or recurse unboundedly.
pub(crate) fn canonical_value(func: &Function, v: ValueId) -> ValueId {
    let mut cur = v;
    for _ in 0..64 {
        let src = func
            .blocks
            .iter()
            .flat_map(|b| &b.instrs)
            .find_map(|ins| match &ins.op {
                Op::Assign { dst, src } if *dst == cur && *src != cur => Some(*src),
                _ => None,
            });
        match src {
            Some(s) => cur = s,
            None => return cur,
        }
    }
    cur
}

/// `v` resolved (through Assign chains) to "result <index> of a static
/// call": the call's own dst for a single-value call, or an Extract
/// component of a tuple call. None for anything else.
pub(crate) fn call_result_of(func: &Function, v: ValueId) -> Option<(FuncId, u32)> {
    let cur = canonical_value(func, v);
    for b in &func.blocks {
        for ins in &b.instrs {
            match &ins.op {
                Op::Call {
                    dst: Some(d),
                    callee: Callee::Static(c),
                    ..
                } if *d == cur => return Some((*c, 0)),
                Op::Extract { dst, tuple, index } if *dst == cur => {
                    let t = canonical_value(func, *tuple);
                    for b2 in &func.blocks {
                        for i2 in &b2.instrs {
                            if let Op::Call {
                                dst: Some(d),
                                callee: Callee::Static(c),
                                ..
                            } = &i2.op
                                && *d == t
                            {
                                return Some((*c, *index));
                            }
                        }
                    }
                    return None;
                }
                _ => {}
            }
        }
    }
    None
}

/// Call-site obligations: every callee requires-clause tagged `tag`,
/// instantiated with the call site's real (possibly symbolic) argument
/// terms, raised as a candidate `Obligation` under `func`'s own
/// preconditions `pre` — discharging is the caller's job (the engine, or
/// a unit test's explicit `discharge` call).
pub(crate) fn call_site_obligations(
    p: &Program,
    func: &Function,
    enc: &EncodedFunc,
    tag: &str,
    pre: &[Term],
    summary_of: &dyn Fn(FuncId) -> Summary,
    assume: &dyn Fn(usize, usize) -> Vec<Term>,
) -> Vec<Obligation> {
    let mut out = Vec::new();
    for (bi, b) in func.blocks.iter().enumerate() {
        for (ii, ins) in b.instrs.iter().enumerate() {
            let Op::Call {
                callee: Callee::Static(c),
                args,
                ..
            } = &ins.op
            else {
                continue;
            };
            let arg_terms: Vec<Option<Term>> =
                args.iter().map(|a| enc.value(*a).cloned()).collect();
            for bc in instantiate_requires(&summary_of(*c), &arg_terms) {
                if bc.tag != tag {
                    continue;
                }
                let Some(v) = bc.violation else { continue };
                let mut extra = pre.to_vec();
                extra.extend(assume(bi, ii));
                extra.push(v);
                out.push(Obligation {
                    tag: bc.tag.clone(),
                    message: format!(
                        "call to {} violates its {} requirement",
                        p.func_name(*c),
                        bc.tag
                    ),
                    pos: ins.pos.clone(),
                    query: enc.reach_query(bi, extra),
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testfix::{block, call_static, instr, pkg_with_ptr_types};

    /// t.F(p *T) { v3 := p (Assign); v4 := v3 (Assign); … }: the chain
    /// canonicalizes to the param.
    #[test]
    fn canonical_value_follows_assign_chains() {
        let mut a1 = instr("ChangeType"); // lowers to Op::Assign
        a1.register = 3;
        a1.r#type = 2;
        a1.operands = vec![1];
        let mut a2 = instr("ChangeType");
        a2.register = 4;
        a2.r#type = 2;
        a2.operands = vec![3];
        let f = goverify_extract::gvir::Function {
            id: "t.F".into(),
            params: vec![goverify_extract::gvir::Param {
                id: 1,
                name: "p".into(),
                r#type: 2,
            }],
            blocks: vec![block(0, vec![a1, a2, instr("Return")], vec![])],
            ..Default::default()
        };
        let p = pkg_with_ptr_types(vec![f]);
        let func = p.func(p.lookup_func("t.F").unwrap()).unwrap();
        assert_eq!(
            canonical_value(func, ValueId(4)),
            ValueId(1),
            "v4 chain resolves to param"
        );
        assert_eq!(
            canonical_value(func, ValueId(3)),
            ValueId(1),
            "v3 resolves to param"
        );
        assert_eq!(
            canonical_value(func, ValueId(1)),
            ValueId(1),
            "param is its own root"
        );
    }

    /// A crafted self-cycle (dst == src) must terminate at the depth cap.
    #[test]
    fn canonical_value_survives_crafted_cycles() {
        let mut a = instr("ChangeType");
        a.register = 3;
        a.r#type = 2;
        a.operands = vec![3]; // v3 := v3
        let f = goverify_extract::gvir::Function {
            id: "t.F".into(),
            blocks: vec![block(0, vec![a, instr("Return")], vec![])],
            ..Default::default()
        };
        let p = pkg_with_ptr_types(vec![f]);
        let func = p.func(p.lookup_func("t.F").unwrap()).unwrap();
        let _ = canonical_value(func, ValueId(3)); // must return, not hang
    }

    /// t.F() { v2 := call t.K() } — v2 resolves to (t.K, 0).
    #[test]
    fn call_result_of_single_value_call() {
        let f = goverify_extract::gvir::Function {
            id: "t.F".into(),
            blocks: vec![block(
                0,
                vec![call_static("t.K", 2, 2, vec![]), instr("Return")],
                vec![],
            )],
            ..Default::default()
        };
        let p = pkg_with_ptr_types(vec![f]);
        let k = p.lookup_func("t.K").unwrap();
        let func = p.func(p.lookup_func("t.F").unwrap()).unwrap();
        assert_eq!(
            call_result_of(func, ValueId(2)),
            Some((k, 0)),
            "call dst resolves to (t.K, 0)"
        );
        assert_eq!(
            call_result_of(func, ValueId(1)),
            None,
            "non-call value has no call result"
        );
    }
}
