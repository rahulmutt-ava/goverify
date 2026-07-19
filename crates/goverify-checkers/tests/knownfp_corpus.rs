//! Known-FP pins from the phase-4 bbolt shakeout triage
//! (docs/shakeout-phase4-bbolt.md). Every `// want:` in
//! testdata/corpus/knownfp pins CURRENT (wrong) analyzer behavior for a
//! confirmed false-positive class — each carries a KNOWN-FP(phase-5)
//! comment naming its class. Phase 5 (requires-lifting et al.) turns
//! these findings off and must flip the pins to match.

use goverify_analysis::{EngineConfig, Options, analyze_full};
use goverify_checkers::{BoundsChecker, NilChecker};
use goverify_solver::{SolverLimits, Z3Native};

fn limits() -> SolverLimits {
    // Corpus queries are trivial; generous timeout so slow CI can't turn
    // a Sat into Unknown and flake the pins.
    SolverLimits {
        timeout_ms: 5_000,
        mem_mb: 1024,
    }
}

#[test]
fn knownfp_corpus_findings_match_want_comments() {
    let p = goverify_ir::testutil::load_corpus("knownfp");
    let cfg = EngineConfig {
        opts: Options::default(),
        cache_dir: None,
        emit_smt: None,
    };
    let checkers: Vec<&dyn goverify_analysis::Checker> = vec![&NilChecker, &BoundsChecker];
    let a = analyze_full(&p, &cfg, &checkers, &|_role| {
        Box::new(Z3Native::new(limits()))
    });
    let got: std::collections::BTreeSet<(String, u32, String)> = a
        .findings
        .iter()
        .filter(|f| f.func.contains("example.com/knownfp"))
        .filter_map(|f| {
            let pos = f.pos.as_ref()?;
            Some((pos.file.clone(), pos.line, f.tag.clone()))
        })
        .collect();
    let want: std::collections::BTreeSet<(String, u32, String)> =
        goverify_ir::testutil::wants("knownfp")
            .into_iter()
            .collect();
    assert_eq!(got, want, "known-FP pins vs current analyzer behavior");
}
