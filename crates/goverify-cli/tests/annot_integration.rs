//! CLI end-to-end (phase-6 spec §5, Task 11): `check`'s human/json/sarif
//! renderings over the `annot` corpus fixture — the same annotation
//! pipeline `goverify-spec`'s `annot_corpus.rs` exercises at the engine
//! layer, now through the real `goverify` binary: contract violations,
//! bad-annotation config errors, the pragma-ignore filter
//! (`suppressed_pragma` / SARIF `inSource` suppressions), and `--deny
//! warnings` promoting a real warning-severity finding to a gate
//! failure.
//!
//! One shared `XDG_CACHE_HOME` (`cache_home`, below) across every test in
//! this file: each fresh cache root forces the sidecar to rebuild the Go
//! extractor binary from scratch (`Sidecar::build`'s content-hash cache
//! lives under it), and spawning a brand-new binary is an EDR
//! new-file-exec stall hazard (goverify-sandbox-environment memory).
//! `Sidecar::build`'s own doc comment anticipates concurrent callers
//! racing benignly onto the identical cache path, so sharing it across
//! this file's tests (which `cargo test` may run on separate threads) is
//! safe.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

/// One extraction-cache root for the whole test binary's lifetime.
fn cache_home() -> &'static Path {
    static CACHE: OnceLock<tempfile::TempDir> = OnceLock::new();
    CACHE
        .get_or_init(|| tempfile::tempdir().expect("tempdir"))
        .path()
}

/// `goverify check <extra_args> ./...` over `dir`, sharing `cache_home()`.
/// Mirrors `formats_corpus.rs`'s `check()` helper, generalized to an
/// arbitrary working directory (the `--deny warnings` test runs a
/// separate tempdir mini-module rather than the `annot` corpus).
fn check_in(dir: &Path, extra_args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_goverify"))
        .arg("check")
        .args(extra_args)
        .arg("./...")
        .current_dir(dir)
        .env("GOVERIFY_EXTRACTOR_DIR", repo_root().join("extractor"))
        .env("XDG_CACHE_HOME", cache_home())
        .output()
        .expect("spawn goverify check")
}

fn check_annot(extra_args: &[&str]) -> Output {
    check_in(&repo_root().join("testdata/corpus/annot"), extra_args)
}

#[test]
fn human_report_exits_one_flags_zero_warning_and_hides_only_the_ignored_finding() {
    let out = check_annot(&[]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "contract + bad-annotation errors must gate exit 1: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("goverify: pragma: 1 finding(s) suppressed"),
        "exactly one pragma-suppressed finding (Suppressed's nil-deref): {stdout}"
    );
    assert!(
        stdout
            .lines()
            .any(|l| l.contains("warning: unverified-annotation") && l.contains("annot.Zero")),
        "Zero's unverified-annotation warning must render: {stdout}"
    );
    assert!(
        !stdout
            .lines()
            .any(|l| l.contains("nil-deref") && l.contains("annot.Suppressed")),
        "Suppressed's nil-deref finding is pragma-ignored and must not render: {stdout}"
    );
    // The ignore-conjunction addendum: `//goverify:ignore nil` on
    // Suppressed must NOT also swallow its (different-checker) bounds
    // finding — ignore matches (func, checker), not func alone.
    assert!(
        stdout
            .lines()
            .any(|l| l.contains("bounds") && l.contains("annot.Suppressed")),
        "Suppressed's bounds finding (a different checker) must survive the nil-only ignore: {stdout}"
    );
}

#[test]
fn json_report_is_schema_v2_with_one_suppressed_pragma_and_zero_at_warning_severity() {
    let out = check_annot(&["--format", "json"]);
    assert_eq!(out.status.code(), Some(1));
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("check --format json emits valid JSON");
    assert_eq!(v["schema_version"], 2);
    assert_eq!(v["summary"]["suppressed_pragma"], 1);
    let findings = v["findings"].as_array().expect("findings array");
    let zero = findings
        .iter()
        .find(|f| f["func"] == "example.com/annot.Zero" && f["checker"] == "unverified-annotation")
        .expect("Zero's unverified-annotation finding is present in JSON output");
    assert_eq!(zero["severity"], "warning");
    // Suppressed's nil-deref must be entirely absent from the report
    // (pragma suppressions are dropped, not just re-flagged, in JSON).
    assert!(
        !findings
            .iter()
            .any(|f| f["func"] == "example.com/annot.Suppressed" && f["checker"] == "nil"),
        "Suppressed's nil-deref must not appear in JSON findings: {findings:?}"
    );
    assert!(
        findings
            .iter()
            .any(|f| f["func"] == "example.com/annot.Suppressed" && f["checker"] == "bounds"),
        "Suppressed's bounds finding must still appear in JSON findings: {findings:?}"
    );
}

#[test]
fn sarif_report_has_exactly_one_in_source_suppression() {
    let out = check_annot(&["--format", "sarif"]);
    assert_eq!(out.status.code(), Some(1));
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("check --format sarif emits valid JSON");
    let results = v["runs"][0]["results"]
        .as_array()
        .expect("runs[0].results array");
    let in_source: Vec<&serde_json::Value> = results
        .iter()
        .filter(|r| {
            r["suppressions"]
                .as_array()
                .is_some_and(|s| s.iter().any(|x| x["kind"] == "inSource"))
        })
        .collect();
    assert_eq!(
        in_source.len(),
        1,
        "exactly one inSource-suppressed SARIF result (Suppressed's nil-deref): {results:#?}"
    );
}

#[test]
fn deny_warnings_promotes_a_warning_only_fixture_from_exit_zero_to_exit_one() {
    // A mini-module carrying ONLY verify.go's One/Zero (no bad.go /
    // contract.go): zero error-severity findings, exactly one warning
    // (Zero's unverified-annotation). Proves `--deny warnings` actually
    // changes the exit code on a real warning finding — the CLI's own
    // `deny_warnings_flag_is_accepted_and_does_not_error` test (cli.rs)
    // only checks the flag parses; hello has no warnings to promote.
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("go.mod"),
        "module example.com/annotdeny\n\ngo 1.25\n",
    )
    .expect("write go.mod");
    std::fs::write(
        dir.path().join("verify.go"),
        concat!(
            "package annotdeny\n\n",
            "//goverify:ensures ret >= 1\n",
            "func One() int { return 1 }\n\n",
            "// Zero's ensures is false: the one warning-severity finding\n",
            "// this fixture carries.\n",
            "//goverify:ensures ret >= 1\n",
            "func Zero() int { return 0 }\n",
        ),
    )
    .expect("write verify.go");

    let without_deny = check_in(dir.path(), &[]);
    assert_eq!(
        without_deny.status.code(),
        Some(0),
        "a warning-only fixture must exit 0 without --deny: stdout={} stderr={}",
        String::from_utf8_lossy(&without_deny.stdout),
        String::from_utf8_lossy(&without_deny.stderr)
    );

    let with_deny = check_in(dir.path(), &["--deny", "warnings"]);
    assert_eq!(
        with_deny.status.code(),
        Some(1),
        "--deny warnings must promote the warning to a gate failure: stdout={} stderr={}",
        String::from_utf8_lossy(&with_deny.stdout),
        String::from_utf8_lossy(&with_deny.stderr)
    );
}
