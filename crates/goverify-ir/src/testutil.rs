//! Integration-test helpers: extract a corpus module through the real
//! sidecar and load it. Not part of the analyzer API.

use std::path::{Path, PathBuf};

use goverify_extract::Sidecar;

use crate::program::Program;

pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

/// Extract testdata/corpus/<module> (whole DAG) into a kept temp dir and
/// load it. Panics on failure — test-only code.
pub fn load_corpus(module: &str) -> Program {
    let root = repo_root();
    let sc = Sidecar::build(&root.join("extractor"), &root.join("target/extractor-bin"))
        .expect("Sidecar::build");
    let dir = tempfile::tempdir().expect("tempdir").keep();
    sc.extract(&root.join("testdata/corpus").join(module), &["./..."], &dir)
        .expect("extract");
    Program::load_dir(&dir).expect("load_dir")
}

/// Byte-exact golden comparison. UPDATE_GOLDENS=1 rewrites the file;
/// always review the diff by hand before committing.
pub fn check_golden(name: &str, got: &str) {
    let path = repo_root().join("testdata/goldens").join(name);
    if std::env::var_os("UPDATE_GOLDENS").is_some() {
        std::fs::write(&path, got).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("missing golden {name} ({e}); run with UPDATE_GOLDENS=1"));
    assert_eq!(
        want, got,
        "golden {name} drifted; review + UPDATE_GOLDENS=1 if intended"
    );
}
