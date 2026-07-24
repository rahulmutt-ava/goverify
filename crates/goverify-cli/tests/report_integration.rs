//! Baseline + diff-base integration (phase-5b). Fixtures are COPIED to
//! tempdirs — tests never write into checked-in corpus dirs.

use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn goverify(args: &[&str], cwd: &Path, cache_home: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_goverify"))
        .args(args)
        .current_dir(cwd)
        .env("GOVERIFY_EXTRACTOR_DIR", repo_root().join("extractor"))
        .env("XDG_CACHE_HOME", cache_home)
        .output()
        .expect("spawn goverify")
}

/// Recursive copy (corpus fixtures are flat or shallow).
fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let to = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &to);
        } else {
            std::fs::copy(entry.path(), &to).unwrap();
        }
    }
}

/// Finding blocks in human output: header lines look like
/// `pos: tag: message [func]`.
fn finding_count(stdout: &str) -> usize {
    stdout
        .lines()
        .filter(|l| {
            l.contains(": nil-deref: ")
                || l.contains(": bounds: ")
                || l.contains(": div-zero: ")
                || l.contains(": overflow: ")
        })
        .count()
}

#[test]
fn baseline_write_records_scoped_findings_deterministically() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let module = tmp.path().join("nil");
    copy_dir(&repo_root().join("testdata/corpus/nil"), &module);

    let check = goverify(&["check", "./..."], &module, cache.path());
    assert_eq!(check.status.code(), Some(1), "nil corpus has findings");
    let n = finding_count(&String::from_utf8_lossy(&check.stdout));
    assert!(n > 0, "expected findings in the nil corpus");

    let w1 = goverify(&["baseline", "write", "./..."], &module, cache.path());
    assert!(
        w1.status.success(),
        "{}",
        String::from_utf8_lossy(&w1.stderr)
    );
    let path = module.join(".goverify/baseline.json");
    let text1 = std::fs::read(&path).expect("baseline written");
    let b = goverify_cli::baseline::parse(&text1).expect("own output parses");
    assert_eq!(b.entries.len(), n, "one entry per scoped finding");

    let w2 = goverify(&["baseline", "write", "./..."], &module, cache.path());
    assert!(w2.status.success());
    let text2 = std::fs::read(&path).unwrap();
    assert_eq!(text1, text2, "baseline write is byte-deterministic");
}
