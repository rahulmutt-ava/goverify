//! End-to-end tracer corpus (phase-3 spec §8, §12): source → extractor →
//! IR → engine → Z3 → finding, plus artifact determinism.

use goverify_analysis::{EngineConfig, Options, analyze_full, dump_findings};
use goverify_checkers::NilChecker;
use goverify_solver::{SolverLimits, Z3Native};

fn limits() -> SolverLimits {
    // Corpus queries are trivial; generous timeout so slow CI can't turn
    // a Sat into Unknown and flake the golden.
    SolverLimits {
        timeout_ms: 5_000,
        mem_mb: 1024,
    }
}

fn run(emit: Option<std::path::PathBuf>) -> String {
    let p = goverify_ir::testutil::load_corpus("nil");
    let cfg = EngineConfig {
        opts: Options::default(),
        cache_dir: None,
        emit_smt: emit,
    };
    let checkers: Vec<&dyn goverify_analysis::Checker> = vec![&NilChecker];
    let a = analyze_full(&p, &cfg, &checkers, &|_role| {
        Box::new(Z3Native::new(limits()))
    });
    // Filter to the corpus module: whole-DAG extraction includes stdlib,
    // and stdlib-derived findings would churn with Go toolchain bumps.
    dump_findings(&a, Some("example.com/nil"))
}

#[test]
fn nil_corpus_findings_match_want_comments() {
    let p = goverify_ir::testutil::load_corpus("nil");
    let cfg = EngineConfig {
        opts: Options::default(),
        cache_dir: None,
        emit_smt: None,
    };
    let checkers: Vec<&dyn goverify_analysis::Checker> = vec![&NilChecker];
    let a = analyze_full(&p, &cfg, &checkers, &|_role| {
        Box::new(Z3Native::new(limits()))
    });
    let got: std::collections::BTreeSet<(String, u32, String)> = a
        .findings
        .iter()
        .filter(|f| f.func.contains("example.com/nil"))
        .filter_map(|f| {
            let pos = f.pos.as_ref()?;
            Some((pos.file.clone(), pos.line, f.tag.clone()))
        })
        .collect();
    let want: std::collections::BTreeSet<(String, u32, String)> =
        goverify_ir::testutil::wants("nil").into_iter().collect();
    assert_eq!(got, want, "findings vs want comments");
}

#[test]
fn findings_and_smt_artifacts_are_deterministic() {
    let d1 = tempfile::tempdir().unwrap();
    let d2 = tempfile::tempdir().unwrap();
    let t1 = run(Some(d1.path().to_path_buf()));
    let t2 = run(Some(d2.path().to_path_buf()));
    assert_eq!(t1, t2, "findings dump must be byte-identical across runs");
    let list = |d: &std::path::Path| -> Vec<(String, Vec<u8>)> {
        let mut v: Vec<_> = std::fs::read_dir(d)
            .unwrap()
            .map(|e| {
                let p = e.unwrap().path();
                (
                    p.file_name().unwrap().to_string_lossy().into_owned(),
                    std::fs::read(&p).unwrap(),
                )
            })
            .collect();
        v.sort();
        v
    };
    assert_eq!(
        list(d1.path()),
        list(d2.path()),
        "--emit-smt artifacts must be byte-identical (names are content hashes)"
    );
    assert!(
        !list(d1.path()).is_empty(),
        "the tracer must actually emit queries"
    );
}
