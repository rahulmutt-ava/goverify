//! Differential solver tests (parent spec §12.8): Z3Native and
//! SmtLib2Process(z3) must agree on verdicts. Blocking tier runs the
//! fixed set below; the nightly sweep adds proptest-generated queries
//! (`--ignored`).

use goverify_solver::{
    AstTerm as Term, BvCmpOp, Logic, Query, SatResult, SmtLib2Process, SolverLimits, Sort,
    TextSolver, Z3Native, ptr_is_nil, ptr_nil, ptr_sort,
};

fn fixed_queries() -> Vec<(&'static str, Query)> {
    let x = || Term::var("x", Sort::BitVec(8));
    let b = || Term::var("b", Sort::Bool);
    let p = || Term::var("p0", ptr_sort());
    vec![
        ("bool-sat", Query::for_asserts(Logic::QfBv, vec![b()])),
        (
            "bool-unsat",
            Query::for_asserts(
                Logic::QfBv,
                vec![Term::and(vec![b(), Term::not(b()).unwrap()]).unwrap()],
            ),
        ),
        (
            "bv-sat",
            Query::for_asserts(
                Logic::QfBv,
                vec![Term::bv_cmp(BvCmpOp::Ult, x(), Term::bv_lit(8, 5)).unwrap()],
            ),
        ),
        (
            "bv-unsat",
            Query::for_asserts(
                Logic::QfBv,
                vec![Term::bv_cmp(BvCmpOp::Ult, x(), Term::bv_lit(8, 0)).unwrap()],
            ),
        ),
        (
            "ptr-nil-sat",
            Query::for_asserts(Logic::All, vec![ptr_is_nil(p()).unwrap()]),
        ),
        (
            "ptr-nil-unsat",
            Query::for_asserts(
                Logic::All,
                vec![
                    Term::eq(p(), ptr_nil()).unwrap(),
                    Term::not(ptr_is_nil(p()).unwrap()).unwrap(),
                ],
            ),
        ),
        (
            "array-sat",
            Query::for_asserts(
                Logic::QfAbv,
                vec![{
                    let m = Term::var(
                        "m",
                        Sort::Array(Box::new(Sort::BitVec(8)), Box::new(Sort::Bool)),
                    );
                    Term::select(m, Term::bv_lit(8, 3)).unwrap()
                }],
            ),
        ),
    ]
}

/// Generous limits: differential queries are trivial; a CI-box hiccup
/// must not turn a verdict into Unknown and mask a real divergence.
fn limits() -> SolverLimits {
    SolverLimits {
        timeout_ms: 5_000,
        mem_mb: 1024,
    }
}

#[test]
fn backends_agree_on_fixed_queries() {
    let mut native = Z3Native::new(limits());
    let mut process = SmtLib2Process::new("z3", limits());
    for (name, q) in fixed_queries() {
        let text = q.canonical_text();
        let a = native.solve_text(&text);
        let b = process.solve_text(&text);
        assert_ne!(
            a.result,
            SatResult::Unknown,
            "{name}: native Unknown on a trivial query"
        );
        assert_eq!(a.result, b.result, "{name}: backends disagree");
    }
}

/// Nightly sweep: random queries from the reader's term generator shape.
/// Ignored in the blocking tier (parent spec §12: full differential
/// sweep is nightly).
#[test]
#[ignore = "nightly differential sweep"]
fn backends_agree_on_random_queries() {
    use proptest::strategy::{Strategy, ValueTree};
    use proptest::test_runner::TestRunner;

    let mut runner = TestRunner::deterministic(); // fixed seed: reproducible
    let mut native = Z3Native::new(limits());
    let mut process = SmtLib2Process::new("z3", limits());
    for i in 0..256 {
        let t = goverify_solver::testgen::arb_bool_term()
            .new_tree(&mut runner)
            .expect("gen")
            .current();
        let q = Query::for_asserts(Logic::All, vec![t]);
        let text = q.canonical_text();
        let a = native.solve_text(&text).result;
        let b = process.solve_text(&text).result;
        assert_eq!(a, b, "case {i} diverged:\n{text}");
    }
}
