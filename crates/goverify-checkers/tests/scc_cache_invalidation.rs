//! G3 (phase-5a spec §7): editing one function re-analyzes exactly its
//! SCC and upward callers; everything else replays from cache. The
//! edit is same-line-count, same-length-irrelevant but crucially adds
//! NO newline, so other functions' positions (hence IR hashes) are
//! untouched.

use goverify_analysis::{Checker, EngineConfig, Options, analyze_full};
use goverify_checkers::NilChecker;
use goverify_solver::{SolverLimits, Z3Native};

const FIXTURE_V1: &str = "package inval\n\nfunc Leaf(x int) int { return x + x }\n\nfunc Caller(x int) int { return Leaf(x) }\n\nfunc Other(x int) int { return x - 1 }\n";
// Same byte length, same line count: only Leaf's body changes.
const FIXTURE_V2: &str = "package inval\n\nfunc Leaf(x int) int { return x * x }\n\nfunc Caller(x int) int { return Leaf(x) }\n\nfunc Other(x int) int { return x - 1 }\n";
const GO_MOD: &str = "module example.com/inval\n\ngo 1.25\n";

fn run(module_dir: &std::path::Path, cache_dir: &std::path::Path) -> (String, u64, u64) {
    let p = goverify_ir::testutil::load_module(module_dir);
    let cfg = EngineConfig {
        opts: Options::default(),
        cache_dir: Some(cache_dir.to_path_buf()),
        emit_smt: None,
        annotations: Default::default(),
        annotation_version: 0,
    };
    let checkers: Vec<&dyn Checker> = vec![&NilChecker];
    let a = analyze_full(&p, &cfg, &checkers, &|_role| {
        Box::new(Z3Native::new(SolverLimits::default()))
    });
    (
        goverify_analysis::dump_findings(&a, None),
        a.scc_cache_hits,
        a.scc_cache_misses,
    )
}

#[test]
fn single_function_edit_invalidates_only_its_scc_and_callers() {
    let module = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    std::fs::write(module.path().join("go.mod"), GO_MOD).unwrap();
    std::fs::write(module.path().join("inval.go"), FIXTURE_V1).unwrap();

    let (out1, hits1, misses1) = run(module.path(), cache.path());
    assert_eq!(hits1, 0, "cold run");
    assert!(misses1 >= 3, "at least Leaf/Caller/Other SCCs analyzed");

    let (out2, hits2, misses2) = run(module.path(), cache.path());
    assert_eq!(out2, out1, "unchanged input replays byte-identically");
    assert_eq!(misses2, 0, "unchanged input is a full hit");
    assert_eq!(hits2, misses1);

    std::fs::write(module.path().join("inval.go"), FIXTURE_V2).unwrap();
    let (_out3, hits3, misses3) = run(module.path(), cache.path());
    assert_eq!(
        misses3, 2,
        "exactly Leaf's SCC and Caller's SCC re-analyze (Other + the rest hit)"
    );
    assert_eq!(hits3, misses1 - 2, "everything else replays");
}

// Phase-6 Task 13: pragma edits are package CONTEXT (goverify-ir
// program.rs's `ctx_hash`), not per-function content — so unlike the
// single-function edit above, a pragma-text edit must rotate the
// `func_ir_hash` (and therefore the SCC cache key) of EVERY function in
// the package, not just the annotated one.
const PRAGMA_GO_MOD: &str = "module example.com/praginval\n\ngo 1.25\n";
const PRAGMA_FIXTURE_V1: &str = "package praginval\n\n//goverify:requires p != nil\nfunc Deref(p *int) int { return *p }\n\nfunc Add(a, b int) int { return a + b }\n";
// Pragma TEXT edited (still parses as a `requires` expression: `&&
// true` is a no-op conjunct) — same line, no newline added, so no
// function's position shifts.
const PRAGMA_FIXTURE_V2: &str = "package praginval\n\n//goverify:requires p != nil && true\nfunc Deref(p *int) int { return *p }\n\nfunc Add(a, b int) int { return a + b }\n";

#[test]
fn pragma_text_edit_invalidates_whole_package_sccs() {
    let module = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    std::fs::write(module.path().join("go.mod"), PRAGMA_GO_MOD).unwrap();
    std::fs::write(module.path().join("praginval.go"), PRAGMA_FIXTURE_V1).unwrap();

    let (out1, hits1, misses1) = run(module.path(), cache.path());
    assert_eq!(hits1, 0, "cold run");
    assert!(misses1 >= 2, "at least Deref's and Add's SCCs analyzed");

    let (out2, hits2, misses2) = run(module.path(), cache.path());
    assert_eq!(out2, out1, "unchanged input replays byte-identically");
    assert_eq!(misses2, 0, "unchanged input is a full hit");
    assert_eq!(hits2, misses1);

    // Edit ONLY the pragma line's text — Deref's own body, Add's body,
    // and every function's position are byte-for-byte unchanged.
    std::fs::write(module.path().join("praginval.go"), PRAGMA_FIXTURE_V2).unwrap();
    let (_out3, hits3, misses3) = run(module.path(), cache.path());
    // Whole-package granularity is EXPECTED, not a bug: `ctx_hash` folds
    // every `pkg.pragmas` entry into EVERY function's `func_ir_hash` in
    // the package (so the type/method-set/pragma table changing
    // invalidates the package uniformly). Add's SCC never reads the
    // pragma and its own IR bytes are untouched, but it still misses
    // because its member hash is `func_hash(ctx_hash(pkg), Add)` and
    // `ctx_hash` changed.
    assert_eq!(
        misses3, misses1,
        "a pragma-text edit misses every SCC in the package, including \
         functions the pragma doesn't annotate"
    );
    assert_eq!(hits3, 0, "no SCC survives a pragma edit in its package");
}

// Phase-7 Task 9: two invalidation pins for the goroutine-leak checker's
// findings, colocated here with the other SCC-cache pins rather than in
// `leak_corpus.rs` (whose `cold_and_warm_cache_runs_are_byte_identical`
// already pins byte-identical cold/warm replay for the leak corpus; this
// pin additionally asserts `scc_cache_hits > 0` on the warm run and lives
// alongside the config-rotation pin below).
fn leak_corpus_limits() -> SolverLimits {
    // Corpus queries are trivial; a generous timeout keeps a slow CI
    // runner from turning a Sat into Unknown and flaking the pin (see
    // leak_corpus.rs's `limits()`).
    SolverLimits {
        timeout_ms: 5_000,
        mem_mb: 1024,
    }
}

fn run_leak_corpus(cache_dir: std::path::PathBuf) -> (String, u64, u64) {
    let p = goverify_ir::testutil::load_corpus("leak");
    let cfg = EngineConfig {
        opts: Options::default(),
        cache_dir: Some(cache_dir),
        emit_smt: None,
        annotations: Default::default(),
        annotation_version: 0,
    };
    let checkers: Vec<&dyn Checker> = vec![&goverify_checkers::LeakChecker];
    let a = analyze_full(&p, &cfg, &checkers, &|_role| {
        Box::new(Z3Native::new(leak_corpus_limits()))
    });
    (
        goverify_analysis::dump_findings(&a, Some("example.com/leak")),
        a.scc_cache_hits,
        a.scc_cache_misses,
    )
}

#[test]
fn leak_findings_replay_byte_identical_on_warm_cache() {
    let cache = tempfile::tempdir().unwrap();
    let (cold, cold_hits, cold_misses) = run_leak_corpus(cache.path().to_path_buf());
    assert_eq!(cold_hits, 0, "cold run");
    assert!(cold_misses > 0, "cold run must populate the cache");

    let (warm, warm_hits, warm_misses) = run_leak_corpus(cache.path().to_path_buf());
    assert_eq!(
        warm, cold,
        "warm replay of the leak corpus must be byte-identical to the cold run"
    );
    assert_eq!(warm_misses, 0, "warm run must be a full hit");
    assert!(
        warm_hits > 0,
        "warm run must actually replay leak findings from the SCC cache"
    );
}

#[test]
fn checker_set_rotation_does_not_replay_leak_findings() {
    // vec![&NilChecker] then default_checkers() over the SAME cache dir:
    // the checker-set change must rotate `CacheConfigKey` (checker
    // name+version list, see engine.rs), so the second run must NOT
    // replay the one-checker entries as stale hits.
    let cache = tempfile::tempdir().unwrap();
    let build_cfg = || EngineConfig {
        opts: Options::default(),
        cache_dir: Some(cache.path().to_path_buf()),
        emit_smt: None,
        annotations: Default::default(),
        annotation_version: 0,
    };

    let one_checker: Vec<&dyn Checker> = vec![&NilChecker];
    let _a1 = analyze_full(
        &goverify_ir::testutil::load_corpus("leak"),
        &build_cfg(),
        &one_checker,
        &|_role| Box::new(Z3Native::new(leak_corpus_limits())),
    );

    let all_checkers = goverify_checkers::default_checkers();
    let a2 = analyze_full(
        &goverify_ir::testutil::load_corpus("leak"),
        &build_cfg(),
        &all_checkers,
        &|_role| Box::new(Z3Native::new(leak_corpus_limits())),
    );

    assert_eq!(
        a2.scc_cache_hits, 0,
        "checker-set rotation must not replay the one-checker run's cache entries"
    );
    assert!(
        a2.scc_cache_misses > 0,
        "the second config must actually (re)populate the cache"
    );

    let findings2 = goverify_analysis::dump_findings(&a2, Some("example.com/leak"));
    assert!(
        findings2.contains("goroutine-leak"),
        "default_checkers() run must include goroutine-leak findings, which a stale \
         one-checker replay would omit: {findings2}"
    );
}
