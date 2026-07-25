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
