//! Machine-format determinism (phase-5b spec §6): --format sarif|json
//! byte-identical across independent runs; goldens pinned on the hello
//! corpus (no findings -> no solver-witness churn on Z3 bumps; the
//! findings-bearing nil corpus is byte-equality-only for that reason).

use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn check(corpus: &str, format: &str, cache_home: &Path) -> Vec<u8> {
    let out = Command::new(env!("CARGO_BIN_EXE_goverify"))
        .args(["check", "--format", format, "./..."])
        .current_dir(repo_root().join("testdata/corpus").join(corpus))
        .env("GOVERIFY_EXTRACTOR_DIR", repo_root().join("extractor"))
        .env("XDG_CACHE_HOME", cache_home)
        .output()
        .expect("spawn goverify");
    assert!(
        out.status.code() == Some(0) || out.status.code() == Some(1),
        "check --format {format} on {corpus}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

#[test]
fn machine_formats_are_byte_identical_across_cold_and_warm_runs() {
    for corpus in ["hello", "nil"] {
        for format in ["json", "sarif"] {
            let cache = tempfile::tempdir().unwrap();
            let cold = check(corpus, format, cache.path());
            let warm = check(corpus, format, cache.path());
            assert_eq!(
                cold, warm,
                "{corpus} --format {format}: cold/warm stdout must be byte-identical"
            );
            // Independent cache: full recompute must also agree.
            let cache2 = tempfile::tempdir().unwrap();
            let fresh = check(corpus, format, cache2.path());
            assert_eq!(
                cold, fresh,
                "{corpus} --format {format}: independent runs must agree"
            );
        }
    }
}

#[test]
fn hello_goldens_pin_the_empty_report_shape() {
    let cache = tempfile::tempdir().unwrap();
    for (format, golden) in [("json", "hello_check.json"), ("sarif", "hello_check.sarif")] {
        let got = check("hello", format, cache.path());
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/goldens")
            .join(golden);
        let want =
            std::fs::read(&path).unwrap_or_else(|e| panic!("golden {}: {e}", path.display()));
        assert_eq!(
            got, want,
            "--format {format} drifted from {golden}; if the change is \
             intentional, regenerate the golden and bump the schema version \
             it pins"
        );
    }
}
