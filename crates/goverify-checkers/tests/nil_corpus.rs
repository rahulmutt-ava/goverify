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
        annotations: Default::default(),
        annotation_version: 0,
    };
    let checkers: Vec<&dyn goverify_analysis::Checker> = vec![&NilChecker];
    let a = analyze_full(&p, &cfg, &checkers, &|_role| {
        Box::new(Z3Native::new(limits()))
    });
    // Filter to the corpus module: whole-DAG extraction includes stdlib,
    // and stdlib-derived findings would churn with Go toolchain bumps.
    dump_findings(&a, Some("example.com/nil"))
}

fn run_with_cache(cache_dir: std::path::PathBuf) -> (String, u64, u64) {
    let p = goverify_ir::testutil::load_corpus("nil");
    let cfg = EngineConfig {
        opts: Options::default(),
        cache_dir: Some(cache_dir),
        emit_smt: None,
        annotations: Default::default(),
        annotation_version: 0,
    };
    let checkers: Vec<&dyn goverify_analysis::Checker> = vec![&NilChecker];
    let a = analyze_full(&p, &cfg, &checkers, &|_role| {
        Box::new(Z3Native::new(limits()))
    });
    (
        dump_findings(&a, Some("example.com/nil")),
        a.scc_cache_hits,
        a.scc_cache_misses,
    )
}

#[test]
fn cold_and_warm_cache_runs_are_byte_identical() {
    let cache = tempfile::tempdir().unwrap();
    let (cold, cold_hits, cold_misses) = run_with_cache(cache.path().to_path_buf());
    let (warm, warm_hits, warm_misses) = run_with_cache(cache.path().to_path_buf());
    assert_eq!(cold, warm, "cold vs warm findings must be byte-identical");
    assert_eq!(cold_hits, 0, "first run must be all misses");
    assert!(cold_misses > 0, "cold run must populate the cache");
    assert_eq!(warm_misses, 0, "warm run must be all hits");
    assert_eq!(warm_hits, cold_misses, "every SCC replays from cache");
    // Not vacuous: the uncached baseline must agree too.
    assert_eq!(cold, run(None), "cached output equals uncached output");
}

#[test]
fn nil_corpus_findings_match_want_comments() {
    let p = goverify_ir::testutil::load_corpus("nil");
    let cfg = EngineConfig {
        opts: Options::default(),
        cache_dir: None,
        emit_smt: None,
        annotations: Default::default(),
        annotation_version: 0,
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

/// Runs with BOTH a cache dir and an --emit-smt dir set, returning the
/// SCC-cache accounting so the test can prove the cache stayed off.
fn run_with_cache_and_emit(cache_dir: std::path::PathBuf, emit: std::path::PathBuf) -> (u64, u64) {
    let p = goverify_ir::testutil::load_corpus("nil");
    let cfg = EngineConfig {
        opts: Options::default(),
        cache_dir: Some(cache_dir),
        emit_smt: Some(emit),
        annotations: Default::default(),
        annotation_version: 0,
    };
    let checkers: Vec<&dyn goverify_analysis::Checker> = vec![&NilChecker];
    let a = analyze_full(&p, &cfg, &checkers, &|_role| {
        Box::new(Z3Native::new(limits()))
    });
    (a.scc_cache_hits, a.scc_cache_misses)
}

#[test]
fn emit_smt_disables_scc_cache() {
    // Finding 2 regression: --emit-smt is a debug/audit mode whose point
    // is a COMPLETE query dump. An SCC cache hit replays without
    // re-entering discharge, so a warm run would emit only the missed
    // SCCs' queries. With emit_smt set, the SCC cache must be fully
    // disabled — zero hits AND zero misses (never even constructed) —
    // across two consecutive runs sharing the same cache dir, so the dump
    // is always complete.
    let cache = tempfile::tempdir().unwrap();
    let e1 = tempfile::tempdir().unwrap();
    let e2 = tempfile::tempdir().unwrap();
    let (h1, m1) = run_with_cache_and_emit(cache.path().to_path_buf(), e1.path().to_path_buf());
    let (h2, m2) = run_with_cache_and_emit(cache.path().to_path_buf(), e2.path().to_path_buf());
    assert_eq!(
        (h1, m1),
        (0, 0),
        "emit_smt must disable the SCC cache (run 1)"
    );
    assert_eq!(
        (h2, m2),
        (0, 0),
        "emit_smt must disable the SCC cache (run 2)"
    );

    // And the two emit dirs hold the same complete file set: a warm run
    // is not shortchanged (this is the observable consequence of the
    // cache staying off — names are content hashes).
    let list = |d: &std::path::Path| -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(d)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    };
    assert_eq!(
        list(e1.path()),
        list(e2.path()),
        "both runs must emit the identical complete query set"
    );
    assert!(!list(e1.path()).is_empty(), "the tracer must emit queries");
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
