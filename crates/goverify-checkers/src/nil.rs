//! NilChecker (phase-4 spec §4): path-sensitive nil-safety on the gated
//! SSA encoding. Requires-inference covers every deref site whose nil
//! path is reachable (Sat-gated); requires propagate bottom-up through
//! call sites when the instantiated callee clause stays expressible
//! over this function's params. Obligations (call-site + manifest-local)
//! are raised in `obligations` under the function's own preconditions.

use goverify_analysis::{
    Checker, Clause, Formula, Obligation, Summary, encode_func, instantiate_requires,
};
use goverify_ir::{
    Callee, ConstVal, FuncId, Function, Op, Pos, Program, TypeKind, ValueId, ValueKind,
};
use goverify_solver::{Query, SatResult, Term, ptr_is_nil};

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

/// True iff every free var of `t` is a p<i> param name: the only vars a
/// requires-clause (evaluated at the callee's own entry, or bound at a
/// call site) may depend on.
pub(crate) fn params_only(t: &Term) -> bool {
    t.free_vars().keys().all(|n| {
        n.strip_prefix('p')
            .is_some_and(|rest| rest.parse::<u32>().is_ok())
    })
}

/// Push a clause unless an equal one is present (fixpoint-friendly dedup;
/// `infer_requires` reruns on every round of a recursive SCC's fixpoint).
fn push_clause(out: &mut Vec<Clause>, c: Clause) {
    if !out.contains(&c) {
        out.push(c);
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
        let Ok(enc) = encode_func(p, f) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        // Own deref sites: every subject whose nil path is reachable
        // (Sat-gated) and expressible over this function's own params.
        for (bi, _ii, subject, _pos) in deref_sites(p, func) {
            let Some(subj) = enc.value(subject).cloned() else {
                continue;
            };
            let Ok(is_nil) = ptr_is_nil(subj) else {
                continue;
            };
            if !params_only(&is_nil) {
                continue; // not expressible as a precondition over params
            }
            if discharge(&enc.reach_query(bi, vec![is_nil.clone()])) != SatResult::Sat {
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
            let arg_terms: Vec<Option<Term>> =
                args.iter().map(|a| enc.value(*a).cloned()).collect();
            for bc in instantiate_requires(&summary_of(*c), &arg_terms) {
                if bc.tag != "nil-deref" {
                    continue; // this checker only owns its own tag
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
                    &mut out,
                    Clause {
                        tag: "nil-deref".into(),
                        formula: Formula { term: bound },
                    },
                );
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
        let Ok(enc) = encode_func(p, f) else {
            return Vec::new();
        };
        let pre = own_preconditions(&summary_of(f));
        let mut out = Vec::new();

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
        for (bi, _ii, subject, pos) in deref_sites(p, func) {
            let Some(subj) = enc.value(subject).cloned() else {
                continue;
            };
            let is_const_nil = matches!(func.value(subject).kind, ValueKind::Const(ConstVal::Nil));
            let expressible = is_const_nil || subj.free_vars().is_empty() || params_only(&subj);
            if !expressible {
                continue; // havoc'd heap value: silent (spec §4)
            }
            let Ok(is_nil) = ptr_is_nil(subj) else {
                continue;
            };
            let mut extra = pre.clone();
            extra.push(is_nil);
            out.push(Obligation {
                tag: "nil-deref".into(),
                message: format!("nil dereference in {}", p.func_name(f)),
                pos,
                query: enc.reach_query(bi, extra),
            });
        }

        // Call sites: instantiated callee requires under own preconditions.
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
                    if bc.tag != "nil-deref" {
                        continue;
                    }
                    let Some(v) = bc.violation else { continue };
                    let mut extra = pre.clone();
                    extra.push(v);
                    out.push(Obligation {
                        tag: bc.tag.clone(),
                        message: format!(
                            "nil passed to {} (violates its {} requirement)",
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
}

/// The function's own requires clauses as query conjuncts: their free
/// p<i> vars are exactly the encoder's param const names, so they can
/// be conjoined directly.
fn own_preconditions(own: &Summary) -> Vec<Term> {
    own.requires
        .iter()
        .map(|c| c.formula.term.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use goverify_analysis::{Checker, Clause, Summary};
    use goverify_extract::gvir;
    use goverify_extract::gvir::instruction::Sem;
    use goverify_ir::Program;
    use goverify_solver::{Query, SatResult, SolverLimits, Z3Native, discharge_query};

    use super::*;

    // -- local gvir builders (mirrors goverify-analysis's testpkg) --------

    fn instr(kind: &str) -> gvir::Instruction {
        gvir::Instruction {
            kind: kind.into(),
            ..Default::default()
        }
    }

    fn block(index: u32, instrs: Vec<gvir::Instruction>, succs: Vec<u32>) -> gvir::BasicBlock {
        gvir::BasicBlock {
            index,
            instrs,
            succs,
            preds: vec![],
        }
    }

    fn ty(id: u32, repr: &str, kind: gvir::TypeKind, name: &str, elem: u32) -> gvir::Type {
        gvir::Type {
            id,
            repr: repr.into(),
            kind: kind as i32,
            name: name.into(),
            elem,
            ..Default::default()
        }
    }

    /// Package "t" with the struct-pointer type pair (1 = T, 2 = *T), a
    /// bool type (3, for Branch conditions built from a comparison), and
    /// the given functions.
    fn pkg_with_ptr_types(functions: Vec<gvir::Function>) -> Program {
        let package = gvir::Package {
            import_path: "t".into(),
            functions,
            types: vec![
                ty(1, "T", gvir::TypeKind::Struct, "", 0),
                ty(2, "*T", gvir::TypeKind::Pointer, "", 1),
                ty(3, "bool", gvir::TypeKind::Basic, "bool", 0),
            ],
            ..Default::default()
        };
        Program::from_packages(vec![package])
    }

    /// t.F(p *T): FieldAddr p.X in the block layout given.
    fn deref_func(blocks: Vec<gvir::BasicBlock>) -> gvir::Function {
        gvir::Function {
            id: "t.F".into(),
            params: vec![gvir::Param {
                id: 1,
                name: "p".into(),
                r#type: 2,
            }],
            blocks,
            ..Default::default()
        }
    }

    /// A `FieldAddr` dereferencing `operand` (a pointer value), writing
    /// its result to `dst_reg`.
    fn field_addr_on(dst_reg: u32, operand: u32) -> gvir::Instruction {
        let mut fa = instr("FieldAddr");
        fa.register = dst_reg;
        fa.operands = vec![operand];
        fa.sem = Some(Sem::Field(gvir::FieldSem {
            index: 0,
            name: "X".into(),
        }));
        fa
    }

    fn field_addr_on_param() -> gvir::Instruction {
        field_addr_on(2, 1)
    }

    /// t.F with the deref unconditionally in the entry block.
    fn deref_program() -> Program {
        pkg_with_ptr_types(vec![deref_func(vec![block(
            0,
            vec![field_addr_on_param(), instr("Return")],
            vec![],
        )])])
    }

    fn branch_on(operand: u32) -> gvir::Instruction {
        let mut b = instr("If");
        b.operands = vec![operand];
        b
    }

    /// v<dst> = (lhs == rhs) as bool: mirrors lower.rs's BinOp arm.
    fn eq_instr(dst: u32, lhs: u32, rhs: u32) -> gvir::Instruction {
        gvir::Instruction {
            kind: "BinOp".into(),
            register: dst,
            r#type: 3, // bool
            operands: vec![lhs, rhs],
            sem: Some(Sem::Binop(gvir::BinOpSem { op: "==".into() })),
            ..Default::default()
        }
    }

    /// A nil *T aux constant at the given id.
    fn nil_aux(id: u32) -> gvir::AuxValue {
        gvir::AuxValue {
            id,
            kind: "Const".into(),
            repr: "nil".into(),
            r#type: 2,
            r#const: Some(gvir::ConstValue {
                value: Some(gvir::const_value::Value::Nil(true)),
            }),
        }
    }

    /// A static call instruction: operands = [callee-slot(unused), args…]
    /// (mirrors lower.rs's non-invoke call convention). `dst_reg` 0 means
    /// no destination (statement call).
    fn call_static(callee: &str, dst_reg: u32, dst_ty: u32, args: Vec<u32>) -> gvir::Instruction {
        let mut c = instr("Call");
        c.register = dst_reg;
        c.r#type = dst_ty;
        c.operands = std::iter::once(0).chain(args).collect();
        c.sem = Some(Sem::Call(gvir::CallSem {
            static_callee: callee.into(),
            ..Default::default()
        }));
        c
    }

    fn z3_discharge() -> impl FnMut(&Query) -> SatResult {
        let mut solver = Z3Native::new(SolverLimits {
            timeout_ms: 5_000,
            mem_mb: 1024,
        });
        move |q| discharge_query(q, &mut solver, None, None).result
    }

    /// A `summary_of` that never has anything to say (used by tests that
    /// don't exercise requires propagation).
    fn no_summaries(_f: goverify_ir::FuncId) -> Summary {
        Summary::default()
    }

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
}
