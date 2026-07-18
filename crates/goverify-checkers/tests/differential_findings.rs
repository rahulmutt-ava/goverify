//! Backend agreement at the findings level (spec §6): Z3Native and
//! SmtLib2Process(z3) must produce byte-identical findings dumps over
//! the checker corpus — guards the canonical printer across the new
//! obligation query shapes.
//!
//! Traces may differ between backends (model TEXT differs — Z3Native's
//! `Z3_model_to_string` vs a CLI `(get-model)` s-expr response render
//! differently). `dump_findings` doesn't print traces, so this compares
//! verdicts + positions + messages only: exactly what must agree.

use goverify_analysis::{BackendRole, EngineConfig, Options, analyze_full, dump_findings};
use goverify_checkers::{BoundsChecker, NilChecker};
use goverify_solver::{SmtLib2Process, SolverLimits, Z3Native};

fn limits() -> SolverLimits {
    SolverLimits {
        timeout_ms: 5_000,
        mem_mb: 1024,
    }
}

fn findings(module: &str, process: bool) -> String {
    let p = goverify_ir::testutil::load_corpus(module);
    let cfg = EngineConfig {
        opts: Options::default(),
        cache_dir: None,
        emit_smt: None,
    };
    let checkers: Vec<&dyn goverify_analysis::Checker> = vec![&NilChecker, &BoundsChecker];
    let a = analyze_full(&p, &cfg, &checkers, &|_role: BackendRole| {
        if process {
            Box::new(SmtLib2Process::new("z3", limits()))
        } else {
            Box::new(Z3Native::new(limits()))
        }
    });
    dump_findings(&a, Some(&format!("example.com/{module}")))
}

#[test]
fn backends_agree_on_nil_corpus() {
    assert_eq!(findings("nil", false), findings("nil", true));
}

#[test]
fn backends_agree_on_bounds_corpus() {
    assert_eq!(findings("bounds", false), findings("bounds", true));
}
