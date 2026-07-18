//! Plumbing shared by every path-sensitive checker (nil, bounds, …):
//! the params-only groundness test, own-preconditions extraction, and
//! the two call-site loops (requires propagation; call-site
//! obligations) that every checker built on `encode_func` needs
//! identically. Extracted in Task 8 to stop a third copy from forming
//! (nil.rs carried the Task 6 originals; bounds.rs needed the same
//! logic verbatim).

use goverify_analysis::{Clause, EncodedFunc, Formula, Obligation, Summary, instantiate_requires};
use goverify_ir::{Callee, FuncId, Function, Op, Program};
use goverify_solver::{Query, SatResult, Term};

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

/// Propagated requires: every callee requires-clause tagged `tag`,
/// instantiated at each of `func`'s static call sites, kept only when it
/// stays expressible over `func`'s own params (`params_only`) and its
/// violation is confirmed reachable (`discharge` returns `Sat` — never
/// `Unknown`/`Unsat`). Appends to `out` with fixpoint-safe dedup.
pub(crate) fn propagate_requires(
    p: &Program,
    func: &Function,
    enc: &EncodedFunc,
    tag: &str,
    summary_of: &dyn Fn(FuncId) -> Summary,
    discharge: &mut dyn FnMut(&Query) -> SatResult,
    out: &mut Vec<Clause>,
) {
    let _ = p; // kept for signature symmetry with call_site_obligations
    for (bi, ins) in func
        .blocks
        .iter()
        .enumerate()
        .flat_map(|(bi, b)| b.instrs.iter().map(move |i| (bi, i)))
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
            if discharge(&enc.reach_query(bi, vec![violation])) != SatResult::Sat {
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
) -> Vec<Obligation> {
    let mut out = Vec::new();
    for (bi, b) in func.blocks.iter().enumerate() {
        for ins in &b.instrs {
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
