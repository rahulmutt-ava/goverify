//! End-to-end: goverify extract + goverify debug over the conc corpus.

use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn goverify(args: &[&str], cwd: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_goverify"))
        .args(args)
        .current_dir(cwd)
        .env("GOVERIFY_EXTRACTOR_DIR", repo_root().join("extractor"))
        .output()
        .expect("spawn goverify")
}

fn extract_conc(out: &Path) {
    let st = goverify(
        &["extract", "--out", out.to_str().unwrap(), "./..."],
        &repo_root().join("testdata/corpus/conc"),
    );
    assert!(
        st.status.success(),
        "extract failed: {}",
        String::from_utf8_lossy(&st.stderr)
    );
}

fn extract_nil(out: &Path) {
    let st = goverify(
        &["extract", "--out", out.to_str().unwrap(), "./..."],
        &repo_root().join("testdata/corpus/nil"),
    );
    assert!(
        st.status.success(),
        "extract failed: {}",
        String::from_utf8_lossy(&st.stderr)
    );
}

fn extract_bounds(out: &Path) {
    let st = goverify(
        &["extract", "--out", out.to_str().unwrap(), "./..."],
        &repo_root().join("testdata/corpus/bounds"),
    );
    assert!(
        st.status.success(),
        "extract failed: {}",
        String::from_utf8_lossy(&st.stderr)
    );
}

fn extract_hello(out: &Path) {
    let st = goverify(
        &["extract", "--out", out.to_str().unwrap(), "./..."],
        &repo_root().join("testdata/corpus/hello"),
    );
    assert!(
        st.status.success(),
        "extract failed: {}",
        String::from_utf8_lossy(&st.stderr)
    );
}

#[test]
fn debug_ir_prints_lowered_function() {
    let dir = tempfile::tempdir().unwrap();
    extract_conc(dir.path());
    let out = goverify(
        &[
            "debug",
            "ir",
            "--gvir-dir",
            dir.path().to_str().unwrap(),
            "--func",
            "example.com/conc.Fan",
        ],
        &repo_root(),
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains("func example.com/conc.Fan"), "{text}");
    assert!(text.contains("select blocking=true"), "{text}");
    assert!(text.contains("go "), "{text}");
}

#[test]
fn debug_dumps_are_byte_identical_across_extract_and_analyze_runs() {
    let (d1, d2) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    extract_conc(d1.path());
    extract_conc(d2.path());
    for what in ["ir", "callgraph", "sccs", "prepass", "summary"] {
        let o1 = goverify(
            &["debug", what, "--gvir-dir", d1.path().to_str().unwrap()],
            &repo_root(),
        );
        let o2 = goverify(
            &["debug", what, "--gvir-dir", d2.path().to_str().unwrap()],
            &repo_root(),
        );
        assert!(o1.status.success() && o2.status.success());
        assert_eq!(o1.stdout, o2.stdout, "debug {what} is nondeterministic");
    }
}

#[test]
fn func_flag_on_callgraph_warns() {
    // Reuse the same extracted gvir dir the other tests use.
    let dir = tempfile::tempdir().unwrap();
    extract_conc(dir.path());
    let out = goverify(
        &[
            "debug",
            "callgraph",
            "--gvir-dir",
            dir.path().to_str().unwrap(),
            "--func",
            "anything",
        ],
        &repo_root(),
    );
    assert!(out.status.success(), "debug callgraph must still succeed");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--func has no effect"),
        "expected ignore-warning on stderr, got: {stderr}"
    );
}

#[test]
fn debug_findings_contains_bad_finding() {
    // Not byte-equality: the CLI prints unfiltered findings, so stdout
    // may legitimately include stdlib-derived ones alongside nil.go's.
    // Generous timeout: avoid a slow-CI Sat->Unknown flake (nil_corpus.rs
    // has the same reasoning for its own Z3 backend).
    let dir = tempfile::tempdir().unwrap();
    extract_nil(dir.path());
    let out = goverify(
        &[
            "debug",
            "findings",
            "--gvir-dir",
            dir.path().to_str().unwrap(),
            "--solver-timeout-ms",
            "5000",
        ],
        &repo_root(),
    );
    assert!(
        out.status.success(),
        "debug findings: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(
        text.lines()
            .any(|l| l.contains("nil.go") && l.contains("Bad")),
        "expected the nil-corpus Bad finding in output:\n{text}"
    );
}

#[test]
fn debug_prepass_and_summary_render() {
    let dir = tempfile::tempdir().unwrap();
    extract_conc(dir.path());
    for what in ["prepass", "summary", "callgraph", "sccs"] {
        let out = goverify(
            &["debug", what, "--gvir-dir", dir.path().to_str().unwrap()],
            &repo_root(),
        );
        assert!(
            out.status.success(),
            "debug {what}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!out.stdout.is_empty(), "debug {what} printed nothing");
    }
}

#[test]
fn check_reports_findings_with_exit_1_and_matches_golden() {
    // Run from the corpus dir so `render_findings`'s `source_root` (CWD,
    // "."`) resolves the spans it prints back into nil.go. Generous
    // timeouts on both tiers: avoid a slow-CI Sat->Unknown flake (same
    // reasoning as nil_corpus.rs's own Z3 backend / debug_findings above).
    let dir = tempfile::tempdir().unwrap();
    extract_nil(dir.path());
    let out = goverify(
        &[
            "check",
            "--gvir-dir",
            dir.path().to_str().unwrap(),
            "--solver-timeout-ms",
            "5000",
            "--obligation-timeout-ms",
            "5000",
        ],
        &repo_root().join("testdata/corpus/nil"),
    );
    assert_eq!(
        out.status.code(),
        Some(1),
        "findings must exit 1: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).unwrap();
    goverify_ir::testutil::check_golden("nil.check.txt", &text);
}

#[test]
fn check_scopes_findings_to_the_module() {
    // The nil corpus has known findings under `example.com/nil`. Running
    // `check` with a `--scope` that matches NO package must filter every
    // one of them out: empty stdout and exit 0, even though the whole-
    // closure analysis still produced findings internally. This exercises
    // the scoping filter end-to-end through the CLI (rendering AND exit
    // code key off the scoped set), deterministically and without relying
    // on a stdlib dependency to leak findings.
    let dir = tempfile::tempdir().unwrap();
    extract_nil(dir.path());
    let out = goverify(
        &[
            "check",
            "--gvir-dir",
            dir.path().to_str().unwrap(),
            "--scope",
            "example.com/not-this-module",
            "--solver-timeout-ms",
            "5000",
            "--obligation-timeout-ms",
            "5000",
        ],
        &repo_root().join("testdata/corpus/nil"),
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "an out-of-scope filter leaves no findings, so exit 0: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.stdout.is_empty(),
        "out-of-scope filter must render nothing: {}",
        String::from_utf8_lossy(&out.stdout)
    );

    // Sanity: scoping to the module itself keeps exactly the in-module
    // findings, and every rendered line references an `example.com/nil`
    // function — no dependency/stdlib finding leaks in.
    let scoped = goverify(
        &[
            "check",
            "--gvir-dir",
            dir.path().to_str().unwrap(),
            "--scope",
            "example.com/nil",
            "--solver-timeout-ms",
            "5000",
            "--obligation-timeout-ms",
            "5000",
        ],
        &repo_root().join("testdata/corpus/nil"),
    );
    assert_eq!(scoped.status.code(), Some(1), "in-module findings exit 1");
    let text = String::from_utf8(scoped.stdout).unwrap();
    // Header lines end with the `[<func>]` bracket; snippet/path/with
    // lines don't. Peel a go/ssa receiver wrapper (`(` / `*`) so a method
    // id like `(*example.com/nil.T).BadMethod` still reads as in-module.
    let mut saw_method = false;
    for line in text.lines().filter(|l| l.ends_with(']')) {
        let func = line.rsplit_once('[').unwrap().1.trim_end_matches(']');
        let bare = func.trim_start_matches(['(', '*']);
        assert!(
            bare.starts_with("example.com/nil"),
            "every finding must be in-module, got: {line}"
        );
        if func.starts_with('(') {
            saw_method = true;
        }
    }
    assert!(
        saw_method,
        "the corpus must exercise a method finding (BadMethod) end-to-end:\n{text}"
    );
}

#[test]
fn check_clean_module_exits_0() {
    // hello corpus (no findings): exit 0, empty stdout.
    let dir = tempfile::tempdir().unwrap();
    extract_hello(dir.path());
    let out = goverify(
        &[
            "check",
            "--gvir-dir",
            dir.path().to_str().unwrap(),
            "--solver-timeout-ms",
            "5000",
            "--obligation-timeout-ms",
            "5000",
        ],
        &repo_root().join("testdata/corpus/hello"),
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "clean module must exit 0: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.stdout.is_empty(),
        "clean module must print nothing: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn check_bounds_corpus_matches_golden() {
    let dir = tempfile::tempdir().unwrap();
    extract_bounds(dir.path());
    let out = goverify(
        &[
            "check",
            "--gvir-dir",
            dir.path().to_str().unwrap(),
            "--solver-timeout-ms",
            "5000",
            "--obligation-timeout-ms",
            "5000",
        ],
        &repo_root().join("testdata/corpus/bounds"),
    );
    assert_eq!(
        out.status.code(),
        Some(1),
        "bounds corpus has findings, must exit 1: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).unwrap();
    goverify_ir::testutil::check_golden("bounds.check.txt", &text);
}
