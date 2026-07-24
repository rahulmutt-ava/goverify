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

/// Same fixture shape as `manifest_returns_closure_with_deps_and_files`
/// (a module importing "strings"), but copied into a scratch tempdir so
/// tests can edit files in place without mutating the checked-in corpus
/// fixture. Two local packages — root `example.com/m` and a subpackage
/// `example.com/m/inner` that root imports — so cache-invalidation
/// tests can exercise upward-closure through a local dep edge (not just
/// the root-has-no-importers case): root also imports "strings"
/// directly, giving both packages a shared stdlib dependency that must
/// stay cached regardless of which local file is edited.
fn sidecar_and_module() -> (Sidecar, tempfile::TempDir) {
    let module = tempfile::tempdir().unwrap();
    std::fs::write(
        module.path().join("go.mod"),
        "module example.com/m\n\ngo 1.25\n",
    )
    .unwrap();
    std::fs::write(
        module.path().join("main.go"),
        "package m\n\nimport (\n\t\"strings\"\n\n\t\"example.com/m/inner\"\n)\n\nfunc Shout(s string) string { return strings.ToUpper(s) + \"!\" }\n\nfunc UseInner(s string) string { return inner.Wrap(s) }\n",
    )
    .unwrap();
    std::fs::create_dir(module.path().join("inner")).unwrap();
    std::fs::write(
        module.path().join("inner/inner.go"),
        "package inner\n\nimport \"strings\"\n\nfunc Wrap(s string) string { return strings.ToLower(s) + \"?\" }\n",
    )
    .unwrap();
    (sidecar(), module)
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

/// Regression guard for the cross-package type-alias determinism bug:
/// the aliasgen fixture instantiates one generic (shared.Reduce) from two
/// packages that spell the same underlying element type differently — one
/// via a cross-package alias (pkgalias.Entry = base.U), one named
/// (base.U). Before the extractor canonicalized alias spellings, the
/// shared ssa instance recorded whichever spelling won the concurrent
/// type-check race, so re-extraction flipped its bytes (~1-in-4 runs) —
/// the same mechanism that flipped bbolt's io/fs.gvir and os.gvir. The
/// closure includes both instantiating packages in one load, so this is
/// exactly the shape the hello-only determinism test could not catch.
#[test]
fn aliased_generic_instance_extraction_is_byte_identical() {
    let sc = sidecar();
    let aliasgen = repo_root().join("testdata/corpus/aliasgen");
    let (out1, out2) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());

    let files1 = sc.extract(&aliasgen, &["./..."], out1.path()).unwrap();
    let files2 = sc.extract(&aliasgen, &["./..."], out2.path()).unwrap();
    assert_eq!(files1.len(), files2.len(), "file sets must match");

    for (a, b) in files1.iter().zip(&files2) {
        assert_eq!(a.file_name(), b.file_name(), "file sets must match");
        assert_eq!(
            std::fs::read(a).unwrap(),
            std::fs::read(b).unwrap(),
            "nondeterministic .gvir bytes (alias-spelling flip): {}",
            a.display()
        );
    }

    // The canonical underlying spelling must reach output; the alias must
    // not. Load pkgalias and assert its emitted callee names never carry
    // the alias spelling (deterministic post-fix, independent of the race).
    let alias_pkg = files1
        .iter()
        .find(|f| {
            f.file_name()
                .unwrap()
                .to_string_lossy()
                .contains("pkgalias")
        })
        .expect("pkgalias.gvir emitted");
    let pkg = load_package(alias_pkg).expect("load_package");
    for f in &pkg.functions {
        assert!(
            !f.id.contains("pkgalias.Entry") && !f.name.contains("pkgalias.Entry"),
            "alias spelling leaked into function id/name: {}",
            f.id
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
fn manifest_returns_closure_with_deps_and_files() {
    // testdata/corpus/withdeps already imports "strings" (see its
    // main.go) so it doubles as this test's fixture — no need for a
    // separate temp module.
    let pkgs = sidecar()
        .manifest(&repo_root().join("testdata/corpus/withdeps"), &["./..."])
        .expect("Sidecar::manifest");
    let root = pkgs
        .iter()
        .find(|p| p.import_path == "example.com/withdeps")
        .expect("root package in manifest");
    assert!(!root.files.is_empty(), "root package lists its files");
    for f in &root.files {
        assert!(f.is_absolute(), "manifest file paths are absolute");
        assert!(f.exists(), "manifest file paths exist");
    }
    // Closure includes deps: the fixture module imports "strings".
    assert!(pkgs.iter().any(|p| p.import_path == "strings"));
    assert!(root.deps.contains(&"strings".to_string()));
}

#[test]
fn cached_load_cold_warm_and_invalidation() {
    let (sc, module) = sidecar_and_module(); // same fixture as the manifest test
    let cache = tempfile::tempdir().unwrap();

    // Cold: everything extracted, store populated.
    let (pkgs1, s1) =
        goverify_extract::load_packages_cached(&sc, module.path(), &["./..."], cache.path())
            .expect("cold cached load");
    assert_eq!(s1.cached, 0, "cold run extracts everything");
    assert!(s1.extracted >= 2, "root + at least one dep in the closure");

    // Warm: zero extraction, identical packages.
    let (pkgs2, s2) =
        goverify_extract::load_packages_cached(&sc, module.path(), &["./..."], cache.path())
            .expect("warm cached load");
    assert_eq!(s2.extracted, 0, "warm run extracts nothing");
    assert_eq!(s2.cached, s1.extracted);
    assert_eq!(
        pkgs1, pkgs2,
        "cached packages decode identically to freshly extracted ones"
    );

    // Edit the module's own root file: only the root package re-extracts
    // (stdlib deps and the local `inner` subpackage stay cached — root
    // has no importers, and nothing root depends on changed).
    let main_go = module.path().join("main.go");
    let src = std::fs::read_to_string(&main_go).unwrap();
    std::fs::write(&main_go, src.replace("ToUpper", "ToLower")).unwrap();
    let (_pkgs3, s3) =
        goverify_extract::load_packages_cached(&sc, module.path(), &["./..."], cache.path())
            .expect("edited cached load");
    assert_eq!(
        s3.extracted, 1,
        "exactly the edited leaf-of-import-DAG package re-extracts"
    );
    assert_eq!(s3.cached, s1.extracted - 1);

    // Edit the local dependency `inner`: upward closure means BOTH inner
    // (content changed) and root (its recursive key folds inner's key,
    // which just changed) re-extract — exactly those two, nothing more.
    // stdlib ("strings", imported by both root and inner) is untouched
    // by either edit and must stay cached throughout.
    let inner_go = module.path().join("inner/inner.go");
    let inner_src = std::fs::read_to_string(&inner_go).unwrap();
    std::fs::write(&inner_go, inner_src.replace("+ \"?\"", "+ \"??\"")).unwrap();
    let (_pkgs4, s4) =
        goverify_extract::load_packages_cached(&sc, module.path(), &["./..."], cache.path())
            .expect("inner-edited cached load");
    assert_eq!(
        s4.extracted, 2,
        "upward closure: editing inner re-extracts inner + root, nothing else"
    );
    assert_eq!(s4.cached, s1.extracted - 2, "stdlib closure stays cached");
}

#[test]
fn editing_go_directive_invalidates_module_packages() {
    // Finding 1 regression: the module's `go` directive (go.mod) changes
    // emitted SSA per-module (since go1.22, loop-variable semantics)
    // without touching any .go file. It's folded into every module
    // package's manifest file set, so editing it must re-extract the
    // module's own packages while stdlib (no module -> no go.mod line)
    // stays cached.
    let (sc, module) = sidecar_and_module();
    let cache = tempfile::tempdir().unwrap();

    let (_p1, s1) =
        goverify_extract::load_packages_cached(&sc, module.path(), &["./..."], cache.path())
            .expect("cold cached load");
    assert_eq!(s1.cached, 0, "cold run extracts everything");

    // Warm: fully saturated.
    let (_p2, s2) =
        goverify_extract::load_packages_cached(&sc, module.path(), &["./..."], cache.path())
            .expect("warm cached load");
    assert_eq!(s2.extracted, 0, "warm run extracts nothing");
    assert_eq!(s2.cached, s1.extracted);

    // Edit ONLY the go.mod `go` directive (byte change, no .go file
    // touched): the two module packages (example.com/m + .../inner)
    // must re-extract; stdlib stays cached.
    let go_mod = module.path().join("go.mod");
    let orig = std::fs::read_to_string(&go_mod).unwrap();
    assert!(orig.contains("go 1.25"), "fixture go.mod shape: {orig}");
    std::fs::write(&go_mod, orig.replace("go 1.25", "go 1.24")).unwrap();

    let (_p3, s3) =
        goverify_extract::load_packages_cached(&sc, module.path(), &["./..."], cache.path())
            .expect("go.mod-edited cached load");
    assert!(
        s3.extracted >= 2,
        "editing the go directive must re-extract the module's own packages (>=2), got {}",
        s3.extracted
    );
    assert!(
        s3.cached < s2.cached,
        "stdlib may stay cached, but fewer packages are cached than the fully-warm run: {} !< {}",
        s3.cached,
        s2.cached
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
