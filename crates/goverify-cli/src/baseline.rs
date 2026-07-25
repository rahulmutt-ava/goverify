//! Baseline file (phase-5b spec §4): schema, deterministic writer,
//! validating parser. The file is user-editable gate configuration —
//! the parser must reject, never panic (fuzz target: baseline_parse).
//! Matching uses the fingerprint ONLY; the readable fields exist for
//! humans reviewing baseline diffs.

use goverify_analysis::Finding;
use serde::{Deserialize, Serialize};

use crate::fingerprint;

/// Bump on any change to the file shape. The parser hard-rejects other
/// versions (spec §4: actionable error naming both versions).
pub const BASELINE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
pub struct Baseline {
    pub schema_version: u32,
    pub entries: Vec<BaselineEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BaselineEntry {
    pub fingerprint: String,
    pub checker: String,
    pub tag: String,
    pub func: String,
    pub message: String,
}

/// Deterministic baseline text: entries sorted by fingerprint, pretty
/// JSON, trailing newline. `fps` is parallel to `findings`.
pub fn render(findings: &[Finding], fps: &[String]) -> String {
    debug_assert_eq!(
        findings.len(),
        fps.len(),
        "baseline::render: findings/fps length mismatch would silently truncate the baseline via zip"
    );
    let mut entries: Vec<BaselineEntry> = findings
        .iter()
        .zip(fps)
        .map(|(f, fp)| BaselineEntry {
            fingerprint: fp.clone(),
            checker: f.checker.clone(),
            tag: f.tag.clone(),
            func: f.func.clone(),
            message: f.message.clone(),
        })
        .collect();
    entries.sort_by(|a, b| a.fingerprint.cmp(&b.fingerprint));
    let b = Baseline {
        schema_version: BASELINE_SCHEMA_VERSION,
        entries,
    };
    let mut s = serde_json::to_string_pretty(&b).expect("infallible serialize");
    s.push('\n');
    s
}

/// Validating parse. Errors carry the reason; the caller turns them
/// into a hard exit-2 error — the documented degrade-never-die
/// exception (spec §4).
pub fn parse(bytes: &[u8]) -> Result<Baseline, String> {
    let b: Baseline =
        serde_json::from_slice(bytes).map_err(|e| format!("not a baseline file: {e}"))?;
    if b.schema_version != BASELINE_SCHEMA_VERSION {
        return Err(format!(
            "unsupported baseline schema_version {} (this build reads {})",
            b.schema_version, BASELINE_SCHEMA_VERSION
        ));
    }
    let expect = format!("{}:", fingerprint::SCHEME);
    if let Some(bad) = b
        .entries
        .iter()
        .find(|e| !e.fingerprint.starts_with(&expect))
    {
        return Err(format!(
            "unsupported fingerprint scheme in entry {:?} (this build writes {}...)",
            bad.fingerprint, expect
        ));
    }
    Ok(b)
}

#[cfg(test)]
mod tests {
    use goverify_analysis::Finding;

    use super::*;

    fn finding(func: &str, msg: &str) -> Finding {
        Finding {
            checker: "nil".to_string(),
            tag: "nil-deref".to_string(),
            func: func.to_string(),
            pos: None,
            message: msg.to_string(),
            trace: Vec::new(),
            model: Vec::new(),
            severity: goverify_analysis::Severity::Error,
        }
    }

    #[test]
    fn render_is_sorted_deterministic_and_round_trips() {
        let fs = vec![finding("p.B", "m2"), finding("p.A", "m1")];
        let fps = crate::fingerprint::fingerprints(&fs);
        let text = render(&fs, &fps);
        assert_eq!(text, render(&fs, &fps), "byte-identical across calls");
        let b = parse(text.as_bytes()).expect("own output parses");
        assert_eq!(b.schema_version, BASELINE_SCHEMA_VERSION);
        assert_eq!(b.entries.len(), 2);
        assert!(
            b.entries[0].fingerprint <= b.entries[1].fingerprint,
            "entries sorted by fingerprint"
        );
        assert_eq!(b.entries.iter().filter(|e| e.func == "p.A").count(), 1);
    }

    #[test]
    fn parse_rejects_garbage_wrong_version_and_foreign_scheme() {
        assert!(parse(b"{").is_err(), "truncated JSON");
        assert!(parse(b"[]").is_err(), "wrong top-level shape");
        let wrong_version = br#"{"schema_version": 99, "entries": []}"#;
        let e = parse(wrong_version).unwrap_err();
        assert!(e.contains("99"), "names the version: {e}");
        let foreign = br#"{"schema_version": 1, "entries": [
            {"fingerprint": "v9:aa", "checker": "nil", "tag": "t", "func": "f", "message": "m"}]}"#;
        let e = parse(foreign).unwrap_err();
        assert!(e.contains("v9:aa"), "names the foreign fingerprint: {e}");
    }
}
