use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn goverify() -> Command {
    Command::new(env!("CARGO_BIN_EXE_goverify"))
}

#[test]
fn extract_writes_gvir_and_exits_zero() {
    let out = tempfile::tempdir().unwrap();
    let output = goverify()
        .current_dir(repo_root().join("testdata/corpus/hello"))
        .arg("extract")
        .arg("-o")
        .arg(out.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("example.com%2Fhello.gvir"),
        "stdout: {stdout}"
    );
    assert!(out.path().join("example.com%2Fhello.gvir").exists());
}

#[test]
fn extract_outside_a_module_exits_two() {
    let dir = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let output = goverify()
        .current_dir(dir.path())
        .arg("extract")
        .arg("-o")
        .arg(out.path())
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(2),
        "analyzer error must exit 2 (spec §10)"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).starts_with("goverify:"),
        "errors go to stderr with the goverify: prefix"
    );
}
