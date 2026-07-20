//! NilChecker (phase-4 spec §4): path-sensitive nil-safety on the gated
//! SSA encoding. Requires-inference covers every deref site whose nil
//! path is reachable (Sat-gated); requires propagate bottom-up through
//! call sites when the instantiated callee clause stays expressible
//! over this function's params. Obligations (call-site + manifest-local)
//! are raised in `obligations` under the function's own preconditions.

use goverify_analysis::{
    Checker, Clause, Formula, IfaceVar, Obligation, Summary, encode_func_with, iface_var_name,
    sort_of,
};
use goverify_ir::{
    ConstVal, FuncId, Function, Op, Pos, Program, TypeId, TypeKind, TypeTable, ValueId, ValueKind,
};
use goverify_solver::{Query, SatResult, Term, ptr_is_nil, ptr_sort};

use crate::shared::{
    call_site_obligations, own_preconditions, params_only, propagate_requires, push_clause,
};

pub struct NilChecker;

/// All (block index, instr index, address ValueId, pos) deref sites:
/// every `Load`/`Store`/`FieldAddr`/`Field` whose subject is
/// pointer-typed. Reused by Task 7/8's checkers.
pub(crate) fn deref_sites(
    p: &Program,
    func: &Function,
) -> Vec<(usize, usize, ValueId, Option<Pos>)> {
    let mut out = Vec::new();
    for (bi, b) in func.blocks.iter().enumerate() {
        for (ii, ins) in b.instrs.iter().enumerate() {
            let subject = match &ins.op {
                Op::Load { addr, .. } | Op::Store { addr, .. } => *addr,
                Op::FieldAddr { base, .. } | Op::Field { base, .. } => *base,
                _ => continue,
            };
            if !matches!(
                p.types().kind(func.value(subject).ty),
                TypeKind::Pointer { .. }
            ) {
                continue;
            }
            out.push((bi, ii, subject, ins.pos.clone()));
        }
    }
    out
}

/// ¬is_nil(r<i>) as a canonical ensures clause. None on term-construction
/// failure (degrade).
fn nonnil_result_clause(i: u32) -> Option<Clause> {
    let r = Term::var(&iface_var_name(&IfaceVar::Result(i)), ptr_sort());
    Some(Clause {
        tag: "nil-deref".into(),
        formula: Formula {
            term: Term::not(ptr_is_nil(r).ok()?).ok()?,
        },
    })
}

/// ¬is_nil(r<e>) ∨ ¬is_nil(r<i>): "never both nil", the (T, error)
/// correlation in disjunctive normal form.
fn correlation_clause(e: u32, i: u32) -> Option<Clause> {
    let rv = |idx: u32| Term::var(&iface_var_name(&IfaceVar::Result(idx)), ptr_sort());
    Some(Clause {
        tag: "nil-deref".into(),
        formula: Formula {
            term: Term::or(vec![
                Term::not(ptr_is_nil(rv(e)).ok()?).ok()?,
                Term::not(ptr_is_nil(rv(i)).ok()?).ok()?,
            ])
            .ok()?,
        },
    })
}

/// The predeclared `error` interface: Named{name: "error"} over an
/// Interface underlying. Deliberately narrow — a custom interface that
/// happens to embed error is not a correlation anchor.
fn is_error_type(types: &TypeTable, t: TypeId) -> bool {
    match types.kind(t) {
        TypeKind::Named { name, underlying } => {
            name == "error" && matches!(types.kind(*underlying), TypeKind::Interface)
        }
        _ => false,
    }
}

impl Checker for NilChecker {
    fn name(&self) -> &'static str {
        "nil"
    }

    fn infer_requires(
        &self,
        p: &Program,
        f: FuncId,
        summary_of: &dyn Fn(FuncId) -> Summary,
        discharge: &mut dyn FnMut(&Query) -> SatResult,
    ) -> Vec<Clause> {
        let Some(func) = p.func(f) else {
            return Vec::new();
        };
        let Ok(enc) = encode_func_with(p, f, summary_of) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let sites = deref_sites(p, func);
        let idom = goverify_analysis::dominators(&enc.dag_succs);
        let assume = |bi: usize, ii: usize| {
            crate::shared::checked_deref_assumptions(&sites, &enc, &idom, bi, ii)
        };
        // Own deref sites: every subject whose nil path is reachable
        // (Sat-gated) and expressible over this function's own params.
        for (bi, ii, subject, _pos) in &sites {
            let Some(subj) = enc.value(*subject).cloned() else {
                continue;
            };
            let Ok(is_nil) = ptr_is_nil(subj) else {
                continue;
            };
            if !params_only(&is_nil) {
                continue; // not expressible as a precondition over params
            }
            let mut extra = assume(*bi, *ii);
            extra.push(is_nil.clone());
            if discharge(&enc.reach_query(*bi, extra)) != SatResult::Sat {
                continue; // guarded (unsat) or unknown: stay silent
            }
            let Ok(nonnil) = Term::not(is_nil) else {
                continue;
            };
            push_clause(
                &mut out,
                Clause {
                    tag: "nil-deref".into(),
                    formula: Formula { term: nonnil },
                },
            );
        }
        // Propagated: violable callee requires expressible over this
        // function's own params, bottom-up through the SCC fixpoint.
        propagate_requires(
            p,
            func,
            &enc,
            "nil-deref",
            summary_of,
            discharge,
            &assume,
            &mut out,
        );
        out
    }

    /// Postconditions (spec §3.1): two templates per pointer-sorted
    /// result, candidate-and-check against the function's own encoding.
    ///
    /// 1. Unconditional ¬is_nil(r_i): proven iff, at EVERY return site,
    ///    reach(site) ∧ is_nil(returned term) is Unsat.
    /// 2. Correlation ¬is_nil(r_e) ∨ ¬is_nil(r_i) (≡ is_nil(r_e) ⇒
    ///    ¬is_nil(r_i)) for each error-typed result e, tried only when
    ///    template 1 failed (subsumption). Per-site Go-idiom rule
    ///    (spec planning amendment): a site whose error component is
    ///    the LITERAL nil constant needs the SMT proof on the paired
    ///    result; any other error expression is treated as a non-nil
    ///    error — the documented under-approximation (threat model).
    ///
    /// Every failure degrades to "no clause": missing terms, arity
    /// mismatches, Sat, Unknown. No return sites ⇒ no ensures.
    fn infer_ensures(
        &self,
        p: &Program,
        f: FuncId,
        summary_of: &dyn Fn(FuncId) -> Summary,
        discharge: &mut dyn FnMut(&Query) -> SatResult,
    ) -> Vec<Clause> {
        let Some(func) = p.func(f) else {
            return Vec::new();
        };
        let Ok(enc) = encode_func_with(p, f, summary_of) else {
            return Vec::new();
        };
        // Result types from the signature.
        let TypeKind::Signature { results, .. } = p.types().kind(func.sig) else {
            return Vec::new();
        };
        let results = results.clone();
        // Return sites: (block index, returned ValueIds). Arity mismatch
        // anywhere (malformed .gvir) drops ALL candidates.
        let mut sites: Vec<(usize, Vec<ValueId>)> = Vec::new();
        for (bi, b) in func.blocks.iter().enumerate() {
            for ins in &b.instrs {
                if let Op::Return { vals } = &ins.op {
                    if vals.len() != results.len() {
                        return Vec::new();
                    }
                    sites.push((bi, vals.clone()));
                }
            }
        }
        if sites.is_empty() {
            return Vec::new();
        }
        let ptr_results: Vec<usize> = (0..results.len())
            .filter(|&i| sort_of(p.types(), results[i]).is_some_and(|s| s == ptr_sort()))
            .collect();
        let error_results: Vec<usize> = (0..results.len())
            .filter(|&i| is_error_type(p.types(), results[i]))
            .collect();

        // A site's returned component proven non-nil?
        let mut site_nonnil = |bi: usize, v: ValueId| -> bool {
            let Some(t) = enc.value(v).cloned() else {
                return false;
            };
            let Ok(is_nil) = ptr_is_nil(t) else {
                return false;
            };
            discharge(&enc.reach_query(bi, vec![is_nil])) == SatResult::Unsat
        };

        let mut out = Vec::new();
        for &i in &ptr_results {
            // Template 1: unconditional.
            if sites.iter().all(|(bi, vals)| site_nonnil(*bi, vals[i])) {
                if let Some(c) = nonnil_result_clause(i as u32) {
                    push_clause(&mut out, c);
                }
                continue; // correlation is subsumed
            }
            // Template 2: correlate with each error result.
            for &e in &error_results {
                if e == i {
                    continue;
                }
                let proven = sites.iter().all(|(bi, vals)| {
                    let err_is_nil_literal =
                        matches!(func.value(vals[e]).kind, ValueKind::Const(ConstVal::Nil));
                    if err_is_nil_literal {
                        site_nonnil(*bi, vals[i])
                    } else {
                        true // Go-idiom rule: non-literal error ⇒ non-nil
                    }
                });
                if proven && let Some(c) = correlation_clause(e as u32, i as u32) {
                    push_clause(&mut out, c);
                }
            }
        }
        out
    }

    /// Call-site obligations (real symbolic args, instantiated against
    /// the callee's requires) plus manifest-local obligations (deref of
    /// a ground or params-only subject inside `f` itself), both
    /// discharged under `f`'s own inferred preconditions: this
    /// mechanizes the spec's who-reports-what rule (a guarded/inherited
    /// deref is someone else's — or nobody's, if truly guarded — job).
    fn obligations(
        &self,
        p: &Program,
        f: FuncId,
        summary_of: &dyn Fn(FuncId) -> Summary,
    ) -> Vec<Obligation> {
        let Some(func) = p.func(f) else {
            return Vec::new();
        };
        let Ok(enc) = encode_func_with(p, f, summary_of) else {
            return Vec::new();
        };
        let pre = own_preconditions(&summary_of(f));
        let mut out = Vec::new();
        let sites = deref_sites(p, func);
        let idom = goverify_analysis::dominators(&enc.dag_succs);
        let assume = |bi: usize, ii: usize| {
            crate::shared::checked_deref_assumptions(&sites, &enc, &idom, bi, ii)
        };

        // Local manifest sites: subject term ground (const nil reached
        // through modeled ops) or params-only (then preconditions decide).
        // A `Const` value's own encoded term is never free-vars-empty —
        // `encode_func` ties it down with a separate `v<id> = <lit>`
        // assert rather than inlining the literal (`declare_value`) — so
        // groundness is read off the IR's own `ValueKind`, not the term.
        // Matched narrowly to `ConstVal::Nil` (not any `Const(_)`): a
        // pointer-typed value tagged `Const` with a mismatched constant
        // (malformed/fuzzed .gvir) gets no defining assert from
        // `declare_value`, so treating it as ground would manufacture a
        // finding off a genuinely free variable.
        for (bi, ii, subject, pos) in &sites {
            let Some(subj) = enc.value(*subject).cloned() else {
                continue;
            };
            let is_const_nil = matches!(func.value(*subject).kind, ValueKind::Const(ConstVal::Nil));
            let expressible = is_const_nil || subj.free_vars().is_empty() || params_only(&subj);
            // Summary-constrained call results (spec §4): a deref of a
            // static call's result qualifies iff the callee's INFERRED
            // summary carries a nil-deref ensures clause constraining
            // that exact result index — every finding this raises is
            // backed by a proven callee correlation, and results the
            // inference couldn't characterize stay silent (no FP flood
            // where inference failed).
            let call_result = if expressible {
                None
            } else {
                crate::shared::call_result_of(func, *subject).filter(|(callee, idx)| {
                    let s = summary_of(*callee);
                    s.provenance == goverify_analysis::Provenance::Inferred
                        && s.ensures.iter().any(|c| {
                            c.tag == "nil-deref"
                                && c.formula
                                    .term
                                    .free_vars()
                                    .keys()
                                    .any(|n| n == &format!("r{idx}"))
                        })
                })
            };
            if !expressible && call_result.is_none() {
                continue; // havoc'd heap value: silent (spec §4)
            }
            let Ok(is_nil) = ptr_is_nil(subj) else {
                continue;
            };
            let mut extra = pre.clone();
            extra.extend(assume(*bi, *ii));
            extra.push(is_nil);
            let message = match call_result {
                Some((callee, _)) => format!(
                    "possibly-nil result of {} dereferenced in {}",
                    p.func_name(callee),
                    p.func_name(f)
                ),
                None => format!("nil dereference in {}", p.func_name(f)),
            };
            out.push(Obligation {
                tag: "nil-deref".into(),
                message,
                pos: pos.clone(),
                query: enc.reach_query(*bi, extra),
            });
        }

        // Call sites: instantiated callee requires under own preconditions.
        out.extend(call_site_obligations(
            p,
            func,
            &enc,
            "nil-deref",
            &pre,
            summary_of,
            &assume,
        ));
        out
    }
}

#[cfg(test)]
mod tests {
    use goverify_analysis::{Checker, Clause, Summary};
    use goverify_extract::gvir;
    use goverify_extract::gvir::instruction::Sem;

    use super::*;
    use crate::testfix::{
        block, branch_on, call_static, deref_func, deref_program, eq_instr, field_addr_on,
        field_addr_on_param, instr, nil_aux, no_summaries, pkg_with_ptr_types, z3_discharge,
    };

    #[test]
    fn unguarded_param_deref_infers_nonnil_requires() {
        // Deref moved to block 1, unconditionally jumped to from block 0:
        // no branch, so no path condition needed — still infers.
        let p = pkg_with_ptr_types(vec![deref_func(vec![
            block(0, vec![instr("Jump")], vec![1]),
            block(1, vec![field_addr_on_param(), instr("Return")], vec![]),
        ])]);
        let f = p.lookup_func("t.F").unwrap();
        let reqs = NilChecker.infer_requires(&p, f, &no_summaries, &mut z3_discharge());
        assert_eq!(reqs.len(), 1, "one deref'd pointer param: {reqs:?}");
        assert_eq!(reqs[0].tag, "nil-deref");
    }

    #[test]
    fn guarded_deref_infers_nothing() {
        // if p == nil { return }; deref in the else block. reach(deref)
        // implies p != nil, so reach ∧ nil is UNSAT: no clause. This is
        // the entry-block-panic FP class fix — the phase-3 heuristic
        // could not see this; now the guard is a path condition.
        let mut f = deref_func(vec![
            block(0, vec![eq_instr(3, 1, 4), branch_on(3)], vec![1, 2]),
            block(1, vec![instr("Return")], vec![]),
            block(2, vec![field_addr_on_param(), instr("Return")], vec![]),
        ]);
        f.aux = vec![nil_aux(4)];
        let p = pkg_with_ptr_types(vec![f]);
        let f = p.lookup_func("t.F").unwrap();
        let reqs = NilChecker.infer_requires(&p, f, &no_summaries, &mut z3_discharge());
        assert!(
            reqs.is_empty(),
            "guarded deref must infer nothing: {reqs:?}"
        );
    }

    #[test]
    fn conditionally_reachable_deref_still_infers() {
        // if c { deref(p) } — reach ∧ nil is SAT (c free): clause emitted.
        // (Conditional-deref requires are deliberate — spec §4.)
        let mut f = deref_func(vec![
            block(0, vec![branch_on(3)], vec![1, 2]),
            block(1, vec![field_addr_on_param(), instr("Return")], vec![]),
            block(2, vec![instr("Return")], vec![]),
        ]);
        f.params.push(gvir::Param {
            id: 3,
            name: "c".into(),
            r#type: 3, // bool
        });
        let p = pkg_with_ptr_types(vec![f]);
        let func = p.lookup_func("t.F").unwrap();
        let reqs = NilChecker.infer_requires(&p, func, &no_summaries, &mut z3_discharge());
        assert_eq!(
            reqs.len(),
            1,
            "conditional deref still needs a requires: {reqs:?}"
        );
        assert_eq!(reqs[0].tag, "nil-deref");
    }

    #[test]
    fn requires_propagate_through_call_sites() {
        // t.F(p) { deref p }  t.G(q) { t.F(q) } — G must inherit
        // requires ¬nil(q) via instantiate_requires + summary_of.
        let f_func = deref_func(vec![block(
            0,
            vec![field_addr_on_param(), instr("Return")],
            vec![],
        )]);
        let g_func = gvir::Function {
            id: "t.G".into(),
            params: vec![gvir::Param {
                id: 1,
                name: "q".into(),
                r#type: 2,
            }],
            blocks: vec![block(
                0,
                vec![call_static("t.F", 0, 0, vec![1]), instr("Return")],
                vec![],
            )],
            ..Default::default()
        };
        let p = pkg_with_ptr_types(vec![f_func, g_func]);
        let f_id = p.lookup_func("t.F").unwrap();
        let g_id = p.lookup_func("t.G").unwrap();
        let freqs = NilChecker.infer_requires(&p, f_id, &no_summaries, &mut z3_discharge());
        assert!(!freqs.is_empty(), "precondition of this test");
        let summary_of = |fid: goverify_ir::FuncId| {
            let mut s = Summary::default();
            if fid == f_id {
                s.requires = freqs.clone();
            }
            s
        };
        let greqs = NilChecker.infer_requires(&p, g_id, &summary_of, &mut z3_discharge());
        assert_eq!(greqs.len(), 1, "G inherits F's requires: {greqs:?}");
        assert_eq!(greqs[0].tag, "nil-deref");
        let fv = greqs[0].formula.term.free_vars();
        let free: Vec<&String> = fv.keys().collect();
        assert_eq!(free, vec!["p0"], "propagated formula is over G's own p0");
    }

    #[test]
    fn propagation_stops_at_unexpressible_args() {
        // t.H() { t.F(load-result) } — arg term is a havoc'd v<id>:
        // free vars ⊄ params ⇒ no clause on H.
        let f_func = deref_func(vec![block(
            0,
            vec![field_addr_on_param(), instr("Return")],
            vec![],
        )]);
        let h_func = gvir::Function {
            id: "t.H".into(),
            blocks: vec![block(
                0,
                vec![
                    call_static("t.K", 2, 2, vec![]),
                    call_static("t.F", 0, 0, vec![2]),
                    instr("Return"),
                ],
                vec![],
            )],
            ..Default::default()
        };
        let p = pkg_with_ptr_types(vec![f_func, h_func]);
        let f_id = p.lookup_func("t.F").unwrap();
        let h_id = p.lookup_func("t.H").unwrap();
        let freqs = NilChecker.infer_requires(&p, f_id, &no_summaries, &mut z3_discharge());
        assert!(!freqs.is_empty(), "precondition of this test");
        let summary_of = |fid: goverify_ir::FuncId| {
            let mut s = Summary::default();
            if fid == f_id {
                s.requires = freqs.clone();
            }
            s
        };
        let hreqs = NilChecker.infer_requires(&p, h_id, &summary_of, &mut z3_discharge());
        assert!(
            hreqs.is_empty(),
            "havoc'd call arg is not expressible over H's params: {hreqs:?}"
        );
    }

    #[test]
    fn unknown_verdict_infers_nothing() {
        let p = deref_program();
        let f = p.lookup_func("t.F").unwrap();
        let mut always_unknown = |_q: &Query| SatResult::Unknown;
        assert!(
            NilChecker
                .infer_requires(&p, f, &no_summaries, &mut always_unknown)
                .is_empty(),
            "Unknown must not manufacture requires (parent spec §8)"
        );
    }

    #[test]
    fn const_nil_arg_produces_obligation_other_args_dont() {
        // t.Caller(q *T) { t.F(nil); t.F(q) } — aux value 3 is const nil.
        // Operand layout mirrors lower.rs's non-invoke call convention:
        // [callee-slot (unused for a static callee), args…].
        let mut call_nil = instr("Call");
        call_nil.operands = vec![0, 3];
        call_nil.sem = Some(Sem::Call(gvir::CallSem {
            static_callee: "t.F".into(),
            ..Default::default()
        }));
        let mut call_param = instr("Call");
        call_param.operands = vec![0, 1];
        call_param.sem = Some(Sem::Call(gvir::CallSem {
            static_callee: "t.F".into(),
            ..Default::default()
        }));
        let caller = gvir::Function {
            id: "t.Caller".into(),
            params: vec![gvir::Param {
                id: 1,
                name: "q".into(),
                r#type: 2,
            }],
            aux: vec![gvir::AuxValue {
                id: 3,
                kind: "Const".into(),
                repr: "nil".into(),
                r#type: 2,
                r#const: Some(gvir::ConstValue {
                    value: Some(gvir::const_value::Value::Nil(true)),
                }),
            }],
            blocks: vec![block(
                0,
                vec![call_nil, call_param, instr("Return")],
                vec![],
            )],
            ..Default::default()
        };
        let p = pkg_with_ptr_types(vec![
            deref_func(vec![block(
                0,
                vec![field_addr_on_param(), instr("Return")],
                vec![],
            )]),
            caller,
        ]);
        let callee_id = p.lookup_func("t.F").unwrap();
        let caller_id = p.lookup_func("t.Caller").unwrap();
        // Give t.F the requires the checker itself would infer.
        let requires: Vec<Clause> =
            NilChecker.infer_requires(&p, callee_id, &no_summaries, &mut z3_discharge());
        assert!(!requires.is_empty(), "precondition of this test");
        let summary_with_f_only = |f: goverify_ir::FuncId| {
            let mut s = Summary::default();
            if f == callee_id {
                s.requires = requires.clone();
            }
            s
        };
        // Caller also forwards q into F unconditionally (the second call),
        // so — same as `wrapper` — it inherits F's requires as its own
        // precondition; the real engine always feeds a function's own
        // inferred summary back into `obligations` (engine.rs's
        // `summary_of`), so an accurate unit test must too.
        let caller_requires =
            NilChecker.infer_requires(&p, caller_id, &summary_with_f_only, &mut z3_discharge());
        let summary_of = |f: goverify_ir::FuncId| {
            let mut s = Summary::default();
            if f == callee_id {
                s.requires = requires.clone();
            } else if f == caller_id {
                s.requires = caller_requires.clone();
            }
            s
        };
        let obs = NilChecker.obligations(&p, caller_id, &summary_of);
        assert_eq!(
            obs.len(),
            2,
            "both call sites raise a candidate obligation: {obs:?}"
        );
        assert!(obs.iter().all(|o| o.tag == "nil-deref"));
        let sat: Vec<_> = obs
            .iter()
            .filter(|o| z3_discharge()(&o.query) == SatResult::Sat)
            .collect();
        assert_eq!(
            sat.len(),
            1,
            "only the const-nil call obligates once discharged; the q-arg \
             call is covered by Caller's own inherited precondition: {obs:?}"
        );
    }

    #[test]
    fn wrapper_does_not_self_report() {
        // t.F(p) { deref p }  t.G(q) { t.F(q) } — G's own summary (as
        // `summary_of` would report it once requires-inference has run)
        // already carries the propagated ¬nil(p0). obligations() on G
        // must not raise a Sat finding: own-preconditions ∧ reach ∧
        // is-nil(q) is UNSAT because own-preconditions already assert
        // ¬nil(q).
        let f_func = deref_func(vec![block(
            0,
            vec![field_addr_on_param(), instr("Return")],
            vec![],
        )]);
        let g_func = gvir::Function {
            id: "t.G".into(),
            params: vec![gvir::Param {
                id: 1,
                name: "q".into(),
                r#type: 2,
            }],
            blocks: vec![block(
                0,
                vec![call_static("t.F", 0, 0, vec![1]), instr("Return")],
                vec![],
            )],
            ..Default::default()
        };
        let p = pkg_with_ptr_types(vec![f_func, g_func]);
        let f_id = p.lookup_func("t.F").unwrap();
        let g_id = p.lookup_func("t.G").unwrap();
        let freqs = NilChecker.infer_requires(&p, f_id, &no_summaries, &mut z3_discharge());
        assert!(!freqs.is_empty(), "precondition of this test");
        let summary_with_f_only = |fid: goverify_ir::FuncId| {
            let mut s = Summary::default();
            if fid == f_id {
                s.requires = freqs.clone();
            }
            s
        };
        let greqs = NilChecker.infer_requires(&p, g_id, &summary_with_f_only, &mut z3_discharge());
        assert!(!greqs.is_empty(), "G must inherit F's requires");
        let summary_of = |fid: goverify_ir::FuncId| {
            let mut s = Summary::default();
            if fid == f_id {
                s.requires = freqs.clone();
            } else if fid == g_id {
                s.requires = greqs.clone();
            }
            s
        };
        let obs = NilChecker.obligations(&p, g_id, &summary_of);
        for ob in &obs {
            assert_ne!(
                z3_discharge()(&ob.query),
                SatResult::Sat,
                "wrapper must not self-report under its own inherited precondition: {ob:?}"
            );
        }
    }

    #[test]
    fn local_manifest_nil_deref_obligates() {
        // t.LocalNil() { deref a manifest const-nil aux value } — not
        // via a call, no params at all: one obligation, Sat under Z3.
        let f = gvir::Function {
            id: "t.LocalNil".into(),
            aux: vec![nil_aux(1)],
            blocks: vec![block(0, vec![field_addr_on(2, 1), instr("Return")], vec![])],
            ..Default::default()
        };
        let p = pkg_with_ptr_types(vec![f]);
        let f_id = p.lookup_func("t.LocalNil").unwrap();
        let obs = NilChecker.obligations(&p, f_id, &no_summaries);
        assert_eq!(obs.len(), 1, "one manifest-local nil deref: {obs:?}");
        assert_eq!(obs[0].tag, "nil-deref");
        assert_eq!(
            z3_discharge()(&obs[0].query),
            SatResult::Sat,
            "manifest-local nil deref is satisfiable"
        );
    }

    #[test]
    fn havoc_subject_deref_stays_silent() {
        // t.HV() { v2 := t.K() (unknown callee: v2 havocs); deref v2 } —
        // free vars ⊄ params and not ground ⇒ no local obligation (FP
        // storm guard: we cannot say anything about an arbitrary heap
        // value that came out of an unmodeled call).
        let f = gvir::Function {
            id: "t.HV".into(),
            blocks: vec![block(
                0,
                vec![
                    call_static("t.K", 2, 2, vec![]),
                    field_addr_on(3, 2),
                    instr("Return"),
                ],
                vec![],
            )],
            ..Default::default()
        };
        let p = pkg_with_ptr_types(vec![f]);
        let f_id = p.lookup_func("t.HV").unwrap();
        let obs = NilChecker.obligations(&p, f_id, &no_summaries);
        assert!(obs.is_empty(), "havoc'd subject must not obligate: {obs:?}");
    }

    #[test]
    fn symbolic_arg_instantiation_binds_caller_terms() {
        // t.H(a *T, c bool) { p := phi(a, nil) based on c; t.F(p) }:
        // violation is-nil(ite(g, a, nil)) is Sat (c can pick the nil
        // edge) ⇒ obligation fires even though no literal nil arg
        // appears at the call site — the symbolic upgrade over phase 3,
        // which could only see literal nil args.
        let f_func = deref_func(vec![block(
            0,
            vec![field_addr_on_param(), instr("Return")],
            vec![],
        )]);
        let branch = branch_on(2); // cond = c (param id 2)
        // Value ids must stay within the lowerer's per-function ceiling
        // (params + aux + instr count, NOT the raw ids' max — see
        // `value_id_ceiling` in goverify-ir's lower.rs): 2 params + 1 aux
        // + 6 instrs = 9, so ids run 1..=4 here, densely packed.
        let phi = gvir::Instruction {
            kind: "Phi".into(),
            register: 4,
            r#type: 2,
            operands: vec![1, 3], // preds [1,2] -> edges [a, nil]
            ..Default::default()
        };
        let h_func = gvir::Function {
            id: "t.H".into(),
            params: vec![
                gvir::Param {
                    id: 1,
                    name: "a".into(),
                    r#type: 2,
                },
                gvir::Param {
                    id: 2,
                    name: "c".into(),
                    r#type: 3,
                },
            ],
            aux: vec![nil_aux(3)],
            blocks: vec![
                block(0, vec![branch], vec![1, 2]),
                gvir::BasicBlock {
                    index: 1,
                    instrs: vec![instr("Jump")],
                    succs: vec![3],
                    preds: vec![0],
                },
                gvir::BasicBlock {
                    index: 2,
                    instrs: vec![instr("Jump")],
                    succs: vec![3],
                    preds: vec![0],
                },
                gvir::BasicBlock {
                    index: 3,
                    instrs: vec![phi, call_static("t.F", 0, 0, vec![4]), instr("Return")],
                    succs: vec![],
                    preds: vec![1, 2],
                },
            ],
            ..Default::default()
        };
        let p = pkg_with_ptr_types(vec![f_func, h_func]);
        let f_id = p.lookup_func("t.F").unwrap();
        let h_id = p.lookup_func("t.H").unwrap();
        let freqs = NilChecker.infer_requires(&p, f_id, &no_summaries, &mut z3_discharge());
        assert!(!freqs.is_empty(), "precondition of this test");
        let summary_of = |fid: goverify_ir::FuncId| {
            let mut s = Summary::default();
            if fid == f_id {
                s.requires = freqs.clone();
            }
            s
        };
        let obs = NilChecker.obligations(&p, h_id, &summary_of);
        assert_eq!(obs.len(), 1, "the symbolic call site obligates: {obs:?}");
        assert_eq!(obs[0].tag, "nil-deref");
        assert_eq!(
            z3_discharge()(&obs[0].query),
            SatResult::Sat,
            "the nil edge of the phi is a reachable violation"
        );
    }

    /// Builds `t.Caller(q *T)`: `v3 = FieldAddr q .0 ; v4 = Load v3 ; call
    /// t.F(v4)` with an extra `v5 = FieldAddr v4 .0` deref of the loaded,
    /// havoc'd pointer either BEFORE (`deref_before_call = true`) or
    /// AFTER the call — fix 2b's dominating-vs-not shape. Returns the
    /// candidate obligations from `NilChecker.obligations` on the caller,
    /// with `t.F`'s own inferred `¬nil(p0)` requirement wired through
    /// `summary_of` exactly as the real engine would (mirrors
    /// `const_nil_arg_produces_obligation_other_args_dont`).
    fn dominating_deref_fixture(deref_before_call: bool) -> Vec<Obligation> {
        let mut v3 = field_addr_on(3, 1); // v3 = FieldAddr(q, .0)
        v3.r#type = 2; // *T: usable as Load's address operand
        let mut v4 = instr("UnOp");
        v4.register = 4;
        v4.r#type = 2; // *T: the loaded, havoc'd pointer
        v4.operands = vec![3];
        v4.sem = Some(Sem::Unop(gvir::UnOpSem {
            op: "*".into(),
            comma_ok: false,
        }));
        let v5 = field_addr_on(5, 4); // v5 = FieldAddr(v4, .0): the deref
        let call = call_static("t.F", 0, 0, vec![4]);
        let instrs = if deref_before_call {
            vec![v3, v4, v5, call, instr("Return")]
        } else {
            vec![v3, v4, call, v5, instr("Return")]
        };
        let caller = gvir::Function {
            id: "t.Caller".into(),
            params: vec![gvir::Param {
                id: 1,
                name: "q".into(),
                r#type: 2,
            }],
            blocks: vec![block(0, instrs, vec![])],
            ..Default::default()
        };
        let p = pkg_with_ptr_types(vec![
            deref_func(vec![block(
                0,
                vec![field_addr_on_param(), instr("Return")],
                vec![],
            )]),
            caller,
        ]);
        let callee_id = p.lookup_func("t.F").unwrap();
        let caller_id = p.lookup_func("t.Caller").unwrap();
        let requires: Vec<Clause> =
            NilChecker.infer_requires(&p, callee_id, &no_summaries, &mut z3_discharge());
        assert!(!requires.is_empty(), "precondition of this test");
        let summary_with_f_only = |f: FuncId| {
            let mut s = Summary::default();
            if f == callee_id {
                s.requires = requires.clone();
            }
            s
        };
        let caller_requires =
            NilChecker.infer_requires(&p, caller_id, &summary_with_f_only, &mut z3_discharge());
        let summary_of = |f: FuncId| {
            let mut s = Summary::default();
            if f == callee_id {
                s.requires = requires.clone();
            } else if f == caller_id {
                s.requires = caller_requires.clone();
            }
            s
        };
        NilChecker.obligations(&p, caller_id, &summary_of)
    }

    #[test]
    fn dominating_deref_discharges_call_obligation() {
        // Caller: v3 = FieldAddr p0 .0 ; v4 = Load v3 ; v5 = FieldAddr v4 .0
        // (a dereference of the LOADED, havoc'd pointer v4) ; then
        // Call callee(v4) where callee requires ¬nil(p0).
        // Before fix 2b the call obligation is Sat (v4 is a free heap
        // value). With the dominating deref at v5 assumed to have
        // succeeded, it must discharge Unsat: no finding.
        let obs = dominating_deref_fixture(true);
        let mut d = z3_discharge();
        assert!(
            obs.iter().all(|o| d(&o.query) != SatResult::Sat),
            "call obligation dominated by a prior deref of the same value must be Unsat (fix 2b): {obs:?}"
        );
    }

    #[test]
    fn later_deref_grants_no_assumption() {
        // Same shape but the extra FieldAddr deref of v4 comes AFTER the
        // call instruction: nothing dominates the call, obligation stays Sat.
        let obs = dominating_deref_fixture(false);
        let mut d = z3_discharge();
        assert!(
            obs.iter().any(|o| d(&o.query) == SatResult::Sat),
            "an obligation with no dominating deref must survive (fix 2b red): {obs:?}"
        );
    }
}
