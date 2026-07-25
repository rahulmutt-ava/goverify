//! `--format json` (phase-5b spec §3): the native machine schema.
//! Byte-identical across runs — findings arrive pre-sorted, field order
//! is fixed by struct declaration, escaping is owned by serde_json. No
//! timestamps, no absolute paths (Pos.file is extractor-relative).

use goverify_analysis::{Finding, Severity};
use serde::Serialize;

/// Bump on any change to the emitted shape (consumers key on it).
/// v2 (phase-6 spec §5): adds per-finding `severity` and
/// `summary.suppressed_pragma`.
pub const JSON_SCHEMA_VERSION: u32 = 2;

#[derive(Serialize)]
pub struct Summary {
    pub total: usize,
    pub suppressed_by_baseline: usize,
    /// Pragma-suppressed count (phase-6 spec §5): findings dropped by a
    /// `//goverify:ignore` matching (func, checker).
    pub suppressed_pragma: usize,
    pub diff_base_scoped: bool,
}

#[derive(Serialize)]
struct Output<'a> {
    schema_version: u32,
    findings: Vec<JsonFinding<'a>>,
    summary: &'a Summary,
}

#[derive(Serialize)]
struct JsonFinding<'a> {
    fingerprint: &'a str,
    checker: &'a str,
    tag: &'a str,
    severity: &'static str,
    func: &'a str,
    file: Option<&'a str>,
    line: Option<u32>,
    col: Option<u32>,
    message: &'a str,
    trace: Vec<JsonTraceStep<'a>>,
    model: &'a [(String, String)],
}

#[derive(Serialize)]
struct JsonTraceStep<'a> {
    file: &'a str,
    line: u32,
}

/// `fps` is parallel to `findings` (fingerprint::fingerprints).
pub fn render_json(findings: &[Finding], fps: &[String], summary: &Summary) -> String {
    debug_assert_eq!(
        findings.len(),
        fps.len(),
        "render_json: findings/fps length mismatch would silently truncate the report via zip"
    );
    let findings: Vec<JsonFinding> = findings
        .iter()
        .zip(fps)
        .map(|(f, fp)| JsonFinding {
            fingerprint: fp,
            checker: &f.checker,
            tag: &f.tag,
            severity: match f.severity {
                Severity::Error => "error",
                Severity::Warning => "warning",
            },
            func: &f.func,
            file: f.pos.as_ref().map(|p| p.file.as_str()),
            line: f.pos.as_ref().map(|p| p.line),
            col: f.pos.as_ref().map(|p| p.col),
            message: &f.message,
            trace: f
                .trace
                .iter()
                .filter_map(|s| s.pos.as_ref())
                .map(|p| JsonTraceStep {
                    file: &p.file,
                    line: p.line,
                })
                .collect(),
            model: &f.model,
        })
        .collect();
    let out = Output {
        schema_version: JSON_SCHEMA_VERSION,
        findings,
        summary,
    };
    // Serializing these owned/borrowed plain types cannot fail.
    let mut s = serde_json::to_string_pretty(&out).expect("infallible serialize");
    s.push('\n');
    s
}

#[cfg(test)]
mod tests {
    use goverify_analysis::TraceStep;

    use super::*;

    #[test]
    fn render_json_matches_the_schema_exactly() {
        let f = Finding {
            checker: "nil".to_string(),
            tag: "nil-deref".to_string(),
            func: "example.com/m.F".to_string(),
            pos: Some(goverify_ir::Pos {
                file: "m.go".to_string(),
                line: 7,
                col: 9,
            }),
            message: "possibly-nil result of example.com/m.G dereferenced in example.com/m.F"
                .to_string(),
            trace: vec![
                TraceStep {
                    block: 0,
                    pos: Some(goverify_ir::Pos {
                        file: "m.go".to_string(),
                        line: 6,
                        col: 2,
                    }),
                },
                TraceStep {
                    block: 1,
                    pos: None,
                }, // position-less: dropped
            ],
            model: vec![("p0".to_string(), "(ptr-nil)".to_string())],
            severity: goverify_analysis::Severity::Error,
        };
        let fps = vec!["v1:00112233445566778899aabbccddeeff".to_string()];
        let summary = Summary {
            total: 1,
            suppressed_by_baseline: 0,
            suppressed_pragma: 0,
            diff_base_scoped: false,
        };
        let got = render_json(&[f], &fps, &summary);
        let want = r#"{
  "schema_version": 2,
  "findings": [
    {
      "fingerprint": "v1:00112233445566778899aabbccddeeff",
      "checker": "nil",
      "tag": "nil-deref",
      "severity": "error",
      "func": "example.com/m.F",
      "file": "m.go",
      "line": 7,
      "col": 9,
      "message": "possibly-nil result of example.com/m.G dereferenced in example.com/m.F",
      "trace": [
        {
          "file": "m.go",
          "line": 6
        }
      ],
      "model": [
        [
          "p0",
          "(ptr-nil)"
        ]
      ]
    }
  ],
  "summary": {
    "total": 1,
    "suppressed_by_baseline": 0,
    "suppressed_pragma": 0,
    "diff_base_scoped": false
  }
}
"#;
        assert_eq!(got, want, "json::render_json()");
    }

    #[test]
    fn render_json_empty_findings_is_valid_and_terse() {
        let summary = Summary {
            total: 0,
            suppressed_by_baseline: 0,
            suppressed_pragma: 0,
            diff_base_scoped: false,
        };
        let got = render_json(&[], &[], &summary);
        assert!(got.starts_with('{') && got.ends_with("}\n"), "{got}");
        assert!(got.contains("\"findings\": []"), "{got}");
    }

    #[test]
    fn render_json_maps_warning_severity() {
        let f = Finding {
            checker: "annotation".to_string(),
            tag: "unverified-annotation".to_string(),
            func: "example.com/m.F".to_string(),
            pos: None,
            message: "requires clause could not be verified".to_string(),
            trace: Vec::new(),
            model: Vec::new(),
            severity: goverify_analysis::Severity::Warning,
        };
        let fps = vec!["v1:00".to_string()];
        let summary = Summary {
            total: 1,
            suppressed_by_baseline: 0,
            suppressed_pragma: 0,
            diff_base_scoped: false,
        };
        let got = render_json(&[f], &fps, &summary);
        let v: serde_json::Value = serde_json::from_str(&got).expect("valid JSON");
        assert_eq!(v["findings"][0]["severity"], "warning", "{got}");
    }
}
