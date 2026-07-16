//! Full-pipeline corpus tests: build the vendored extractor, run it
//! over testdata/corpus modules, decode the .gvir output.
//! `mise run corpus` runs exactly this file (blocking CI tier).

use std::path::{Path, PathBuf};

use goverify_extract::{Sidecar, load_package};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn sidecar() -> Sidecar {
    let root = repo_root();
    Sidecar::build(&root.join("extractor"), &root.join("target/extractor-bin"))
        .expect("Sidecar::build")
}

#[test]
fn extracts_and_loads_hello_module() {
    let out = tempfile::tempdir().unwrap();
    let files = sidecar()
        .extract(
            &repo_root().join("testdata/corpus/hello"),
            &["./..."],
            out.path(),
        )
        .expect("Sidecar::extract");
    assert_eq!(files.len(), 1, "hello has no deps: want exactly one .gvir");

    let pkg = load_package(&files[0]).expect("load_package");
    assert_eq!(pkg.import_path, "example.com/hello");
    assert!(
        pkg.functions
            .iter()
            .any(|f| f.id == "example.com/hello.Add"),
        "missing example.com/hello.Add in {:?}",
        pkg.functions.iter().map(|f| &f.id).collect::<Vec<_>>()
    );
}

#[test]
fn extract_fails_cleanly_outside_a_module() {
    let not_a_module = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let err = sidecar()
        .extract(not_a_module.path(), &["./..."], out.path())
        .expect_err("extract in an empty dir must fail");
    let msg = err.to_string();
    assert!(!msg.is_empty(), "error must carry the extractor's stderr");
}

#[test]
fn extraction_is_byte_identical_across_runs() {
    let sc = sidecar();
    let hello = repo_root().join("testdata/corpus/hello");
    let (out1, out2) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());

    let files1 = sc.extract(&hello, &["./..."], out1.path()).unwrap();
    let files2 = sc.extract(&hello, &["./..."], out2.path()).unwrap();
    assert_eq!(files1.len(), files2.len());

    for (a, b) in files1.iter().zip(&files2) {
        assert_eq!(a.file_name(), b.file_name(), "file sets must match");
        assert_eq!(
            std::fs::read(a).unwrap(),
            std::fs::read(b).unwrap(),
            "nondeterministic .gvir bytes: {}",
            a.display()
        );
    }
}

#[test]
fn dependency_traversal_extracts_the_import_closure() {
    let out = tempfile::tempdir().unwrap();
    let files = sidecar()
        .extract(
            &repo_root().join("testdata/corpus/withdeps"),
            &["./..."],
            out.path(),
        )
        .expect("Sidecar::extract");

    let names: Vec<String> = files
        .iter()
        .map(|f| f.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert!(
        names.contains(&"example.com%2Fwithdeps.gvir".to_string()),
        "{names:?}"
    );
    assert!(
        names.contains(&"strings.gvir".to_string()),
        "stdlib dep missing: {names:?}"
    );
    assert!(
        files.len() > 2,
        "expected the transitive closure, got {names:?}"
    );
}

#[test]
fn gvir_contains_no_absolute_paths() {
    let out = tempfile::tempdir().unwrap();
    let files = sidecar()
        .extract(
            &repo_root().join("testdata/corpus/withdeps"),
            &["./..."],
            out.path(),
        )
        .unwrap();
    for f in &files {
        let pkg = load_package(f).unwrap();
        for file in &pkg.files {
            assert!(
                !file.path.starts_with('/') && !file.path.contains(":\\"),
                "{}: absolute path leaked: {}",
                pkg.import_path,
                file.path
            );
        }
    }
}
