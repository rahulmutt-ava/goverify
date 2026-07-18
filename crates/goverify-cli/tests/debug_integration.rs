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
