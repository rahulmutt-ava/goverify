//! NilTracer (phase-3 spec §8): the nil checker's embryo. Deliberately
//! minimal — entry-block unconditional derefs + constant-nil call args —
//! it exists to prove extractor→IR→engine→cache→Z3→finding end to end.
//! Phase 4 replaces the heuristics with real path-sensitive analysis
//! behind the same Checker trait.

use goverify_analysis::{
    Checker, Clause, Formula, IfaceVar, Obligation, Summary, iface_var_name, instantiate_requires,
};
use goverify_ir::{Callee, ConstVal, FuncId, Op, Program, TypeKind, ValueKind};
use goverify_solver::{Logic, Query, SatResult, Term, ptr_nil, ptr_sort};

pub struct NilTracer;

/// Pointer-typed param derefs in the ENTRY block only: unconditional by
/// construction, no path condition needed.
fn entry_block_deref_params(p: &Program, f: FuncId) -> Vec<u32> {
    let Some(func) = p.func(f) else {
        return Vec::new();
    };
    let Some(entry) = func.blocks.first() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for ins in &entry.instrs {
        let subject = match &ins.op {
            Op::Load { addr, .. } | Op::Store { addr, .. } => *addr,
            Op::FieldAddr { base, .. } | Op::Field { base, .. } => *base,
            _ => continue,
        };
        let info = func.value(subject);
        if !matches!(info.kind, ValueKind::Param) {
            continue;
        }
        if !matches!(p.types().kind(info.ty), TypeKind::Pointer { .. }) {
            continue;
        }
        if let Some(idx) = func.params.iter().position(|&pv| pv == subject) {
            let idx = idx as u32;
            if !out.contains(&idx) {
                out.push(idx);
            }
        }
    }
    out.sort_unstable();
    out
}

impl Checker for NilTracer {
    fn name(&self) -> &'static str {
        "nil"
    }

    fn infer_requires(
        &self,
        p: &Program,
        f: FuncId,
        discharge: &mut dyn FnMut(&Query) -> SatResult,
    ) -> Vec<Clause> {
        let mut out = Vec::new();
        for idx in entry_block_deref_params(p, f) {
            let name = iface_var_name(&IfaceVar::Param(idx));
            let pvar = Term::var(&name, ptr_sort());
            let Ok(is_nil) = goverify_solver::ptr_is_nil(pvar.clone()) else {
                continue;
            };
            // Sat = the nil path exists => the deref needs a precondition.
            // Anything else (incl. Unknown) => stay silent (parent §8).
            if discharge(&Query::for_asserts(Logic::All, vec![is_nil.clone()])) != SatResult::Sat {
                continue;
            }
            let Ok(nonnil) = Term::not(is_nil) else {
                continue;
            };
            out.push(Clause {
                tag: "nil-deref".into(),
                formula: Formula { term: nonnil },
            });
        }
        out
    }

    fn obligations(
        &self,
        p: &Program,
        f: FuncId,
        summary_of: &dyn Fn(FuncId) -> Summary,
    ) -> Vec<Obligation> {
        let Some(func) = p.func(f) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for b in &func.blocks {
            for ins in &b.instrs {
                let Op::Call {
                    callee: Callee::Static(c),
                    args,
                    ..
                } = &ins.op
                else {
                    continue;
                };
                let arg_terms: Vec<Option<Term>> = args
                    .iter()
                    .map(|&a| {
                        matches!(func.value(a).kind, ValueKind::Const(ConstVal::Nil)).then(ptr_nil)
                    })
                    .collect();
                for bc in instantiate_requires(&summary_of(*c), &arg_terms) {
                    let Some(v) = bc.violation else { continue };
                    out.push(Obligation {
                        tag: bc.tag.clone(),
                        message: format!(
                            "nil passed to {} (violates its {} requirement)",
                            p.func_name(*c),
                            bc.tag
                        ),
                        pos: ins.pos.clone(),
                        query: Query::for_asserts(Logic::All, vec![v]),
                    });
                }
            }
        }
        out
    }
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

    /// Package "t" with the struct-pointer type pair (1 = T, 2 = *T)
    /// and the given functions.
    fn pkg_with_ptr_types(functions: Vec<gvir::Function>) -> Program {
        let package = gvir::Package {
            import_path: "t".into(),
            functions,
            types: vec![
                ty(1, "T", gvir::TypeKind::Struct, "", 0),
                ty(2, "*T", gvir::TypeKind::Pointer, "", 1),
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

    fn field_addr_on_param() -> gvir::Instruction {
        let mut fa = instr("FieldAddr");
        fa.register = 2;
        fa.operands = vec![1];
        fa.sem = Some(Sem::Field(gvir::FieldSem {
            index: 0,
            name: "X".into(),
        }));
        fa
    }

    /// t.F with the deref unconditionally in the entry block.
    fn deref_program() -> Program {
        pkg_with_ptr_types(vec![deref_func(vec![block(
            0,
            vec![field_addr_on_param(), instr("Return")],
            vec![],
        )])])
    }

    fn z3_discharge() -> impl FnMut(&Query) -> SatResult {
        let mut solver = Z3Native::new(SolverLimits {
            timeout_ms: 5_000,
            mem_mb: 1024,
        });
        move |q| discharge_query(q, &mut solver, None, None).result
    }

    #[test]
    fn unguarded_param_deref_infers_nonnil_requires() {
        let p = deref_program();
        let f = p.lookup_func("t.F").unwrap();
        let reqs = NilTracer.infer_requires(&p, f, &mut z3_discharge());
        assert_eq!(reqs.len(), 1, "one deref'd pointer param: {reqs:?}");
        assert_eq!(reqs[0].tag, "nil-deref");
    }

    #[test]
    fn unknown_verdict_infers_nothing() {
        let p = deref_program();
        let f = p.lookup_func("t.F").unwrap();
        let mut always_unknown = |_q: &Query| SatResult::Unknown;
        assert!(
            NilTracer
                .infer_requires(&p, f, &mut always_unknown)
                .is_empty(),
            "Unknown must not manufacture requires (parent spec §8)"
        );
    }

    #[test]
    fn guarded_deref_in_later_block_infers_nothing() {
        // Entry block only branches; the deref lives in block 1 — the
        // entry-block-only tracer must stay silent.
        let p = pkg_with_ptr_types(vec![deref_func(vec![
            block(
                0,
                vec![{
                    let mut b = instr("If");
                    b.operands = vec![1];
                    b
                }],
                vec![1, 2],
            ),
            block(1, vec![field_addr_on_param(), instr("Return")], vec![]),
            block(2, vec![instr("Return")], vec![]),
        ])]);
        let f = p.lookup_func("t.F").unwrap();
        assert!(
            NilTracer
                .infer_requires(&p, f, &mut z3_discharge())
                .is_empty(),
            "non-entry deref must infer nothing in phase 3"
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
        // Give t.F the requires the tracer itself would infer.
        let requires: Vec<Clause> = NilTracer.infer_requires(&p, callee_id, &mut z3_discharge());
        assert!(!requires.is_empty(), "precondition of this test");
        let summary_of = |f: goverify_ir::FuncId| {
            let mut s = Summary::default();
            if f == callee_id {
                s.requires = requires.clone();
            }
            s
        };
        let obs = NilTracer.obligations(&p, caller_id, &summary_of);
        assert_eq!(obs.len(), 1, "only the const-nil call obligates: {obs:?}");
        assert_eq!(obs[0].tag, "nil-deref");
        let verdict = z3_discharge()(&obs[0].query);
        assert_eq!(
            verdict,
            SatResult::Sat,
            "nil-into-nonnil violation is satisfiable"
        );
    }
}
