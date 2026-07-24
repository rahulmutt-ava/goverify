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
fn check_cold_and_warm_default_cache_stdout_identical() {
    // Isolated cache root (never the user's real one): point
    // XDG_CACHE_HOME at a tempdir so the default-on path is what's
    // actually under test.
    let cache_home = tempfile::tempdir().unwrap();
    let run = || {
        goverify()
            .current_dir(repo_root().join("testdata/corpus/hello"))
            .arg("check")
            .arg("./...")
            .env("XDG_CACHE_HOME", cache_home.path())
            .output()
            .unwrap()
    };
    let cold = run();
    let warm = run();
    assert_eq!(
        String::from_utf8_lossy(&cold.stdout),
        String::from_utf8_lossy(&warm.stdout),
        "cold vs warm stdout byte-identical"
    );
    assert_eq!(cold.status.code(), warm.status.code(), "same exit code");
    // The default root actually got populated.
    assert!(
        cache_home.path().join("goverify").join("scc").exists(),
        "scc layer created under XDG_CACHE_HOME/goverify"
    );
    assert!(
        cache_home.path().join("goverify").join("extract").exists(),
        "extract layer created under XDG_CACHE_HOME/goverify"
    );
}

#[test]
fn no_cache_flag_runs_uncached() {
    let cache_home = tempfile::tempdir().unwrap();
    let out = goverify()
        .current_dir(repo_root().join("testdata/corpus/hello"))
        .arg("check")
        .arg("--no-cache")
        .arg("./...")
        .env("XDG_CACHE_HOME", cache_home.path())
        .output()
        .unwrap();
    assert!(
        out.status.code() == Some(0) || out.status.code() == Some(1),
        "check must not error: {out:?}"
    );
    assert!(
        !cache_home.path().join("goverify").join("scc").exists(),
        "--no-cache must not touch the cache root"
    );
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
