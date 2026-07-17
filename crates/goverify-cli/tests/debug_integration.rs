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
