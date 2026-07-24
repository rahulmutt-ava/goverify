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

#[test]
fn baseline_suppresses_then_resurfaces_on_entry_removal() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let module = tmp.path().join("nil");
    copy_dir(&repo_root().join("testdata/corpus/nil"), &module);

    let w = goverify(&["baseline", "write", "./..."], &module, cache.path());
    assert!(w.status.success(), "{}", String::from_utf8_lossy(&w.stderr));

    // Fully-baselined module: exit 0, no finding blocks, footer names
    // the suppressed count.
    let check = goverify(&["check", "./..."], &module, cache.path());
    assert_eq!(
        check.status.code(),
        Some(0),
        "all findings baselined -> clean gate"
    );
    let stdout = String::from_utf8_lossy(&check.stdout);
    assert_eq!(finding_count(&stdout), 0, "no findings rendered: {stdout}");
    assert!(
        stdout.contains("suppressed"),
        "footer reports suppression: {stdout}"
    );

    // --no-baseline restores the full report.
    let full = goverify(&["check", "--no-baseline", "./..."], &module, cache.path());
    assert_eq!(full.status.code(), Some(1));
    let n = finding_count(&String::from_utf8_lossy(&full.stdout));
    assert!(n > 0);

    // Remove one entry -> exactly that finding resurfaces.
    let path = module.join(".goverify/baseline.json");
    let mut b = goverify_cli::baseline::parse(&std::fs::read(&path).unwrap()).unwrap();
    b.entries.remove(0);
    let pruned = serde_json::to_string_pretty(&serde_json::json!({
        "schema_version": 1,
        "entries": b.entries.iter().map(|e| serde_json::json!({
            "fingerprint": e.fingerprint, "checker": e.checker, "tag": e.tag,
            "func": e.func, "message": e.message,
        })).collect::<Vec<_>>(),
    }))
    .unwrap();
    std::fs::write(&path, pruned).unwrap();
    let one = goverify(&["check", "./..."], &module, cache.path());
    assert_eq!(one.status.code(), Some(1), "one unbaselined finding gates");
    assert_eq!(
        finding_count(&String::from_utf8_lossy(&one.stdout)),
        1,
        "exactly the removed entry resurfaces"
    );
}

#[test]
fn malformed_baseline_is_a_hard_exit_2() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let module = tmp.path().join("hello");
    copy_dir(&repo_root().join("testdata/corpus/hello"), &module);
    std::fs::create_dir_all(module.join(".goverify")).unwrap();
    std::fs::write(module.join(".goverify/baseline.json"), "{").unwrap();

    let out = goverify(&["check", "./..."], &module, cache.path());
    assert_eq!(
        out.status.code(),
        Some(2),
        "malformed baseline -> analyzer error"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("baseline"),
        "actionable error names the file: {stderr}"
    );
}

#[test]
fn explicit_baseline_flags() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let module = tmp.path().join("hello");
    copy_dir(&repo_root().join("testdata/corpus/hello"), &module);

    // --baseline pointing at a missing file: hard error.
    let out = goverify(
        &["check", "--baseline", "nope.json", "./..."],
        &module,
        cache.path(),
    );
    assert_eq!(
        out.status.code(),
        Some(2),
        "explicit missing baseline errors"
    );

    // baseline write rejects baseline-consuming flags.
    let out = goverify(
        &["baseline", "write", "--no-baseline", "./..."],
        &module,
        cache.path(),
    );
    assert_eq!(out.status.code(), Some(2), "write+--no-baseline rejected");
}
