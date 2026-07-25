//! `--format sarif` (phase-5b spec §3, phase-6 spec §5): SARIF 2.1.0,
//! minimal static subset for GitHub code scanning. Determinism is the
//! root invariant: no timestamps, no invocation objects, no absolute
//! URIs — SARIF's optional provenance fields all violate it and are
//! deliberately absent. Pragma- and baseline-suppressed findings are
//! EMITTED as results carrying a `suppressions` array (`"inSource"` /
//! `"external"`) rather than omitted — a spec-mandated change from the
//! phase-5b behavior, which omitted baseline-suppressed results
//! entirely; the counts also go in run.properties.

use goverify_analysis::{Finding, Severity};
use serde::Serialize;

const SARIF_VERSION: &str = "2.1.0";
const SARIF_SCHEMA: &str =
    "https://docs.oasis-open.org/sarif/sarif/v2.1.0/os/schemas/sarif-schema-2.1.0.json";

/// One rule per checker tag (spec §3). Extend when a checker gains a
/// tag; an unlisted tag still emits a result (ruleId only), never
/// panics.
const RULES: &[(&str, &str)] = &[
    ("nil-deref", "possible nil pointer dereference"),
    ("bounds", "possible out-of-range index or slice bound"),
    ("div-zero", "possible integer division by zero"),
    ("overflow", "possible integer conversion overflow"),
    ("contract", "annotated requires violated at a call site"),
    ("bad-annotation", "invalid //goverify: annotation"),
    (
        "unverified-annotation",
        "annotated ensures not proven against the body",
    ),
];

#[derive(Serialize)]
struct Sarif<'a> {
    #[serde(rename = "$schema")]
    schema: &'static str,
    version: &'static str,
    runs: [Run<'a>; 1],
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Run<'a> {
    tool: Tool,
    results: Vec<SarifResult<'a>>,
    properties: RunProperties,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RunProperties {
    suppressed_by_baseline: usize,
    suppressed_by_pragma: usize,
}

#[derive(Serialize)]
struct Tool {
    driver: Driver,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Driver {
    name: &'static str,
    semantic_version: &'static str,
    rules: Vec<Rule>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Rule {
    id: &'static str,
    short_description: Text,
}

#[derive(Serialize)]
struct Text {
    text: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SarifResult<'a> {
    rule_id: &'a str,
    level: &'static str,
    message: Text,
    #[serde(skip_serializing_if = "Option::is_none")]
    locations: Option<[Location<'a>; 1]>,
    partial_fingerprints: PartialFingerprints<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    code_flows: Option<[CodeFlow<'a>; 1]>,
    /// Present only for pragma-/baseline-suppressed results (phase-6
    /// spec §5): absent for kept findings.
    #[serde(skip_serializing_if = "Option::is_none")]
    suppressions: Option<[Suppression; 1]>,
}

/// A SARIF suppression: `"inSource"` for a `//goverify:ignore` pragma,
/// `"external"` for a baseline entry (the two suppression mechanisms
/// the CLI supports, spec §5).
#[derive(Serialize)]
struct Suppression {
    kind: &'static str,
}

// The `goverify/v1` key below is NOT derived from `fingerprint::SCHEME`
// at runtime (renaming a serde field to a computed string would need a
// custom Serialize impl, and this key is part of the pinned SARIF
// byte output). Keep the two in sync by hand; `partial_fingerprints_key_matches_scheme`
// below fails the build the day they drift.
#[derive(Serialize)]
struct PartialFingerprints<'a> {
    #[serde(rename = "goverify/v1")]
    goverify_v1: &'a str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Location<'a> {
    physical_location: PhysicalLocation<'a>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PhysicalLocation<'a> {
    artifact_location: ArtifactLocation<'a>,
    region: Region,
}

#[derive(Serialize)]
struct ArtifactLocation<'a> {
    uri: &'a str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Region {
    start_line: u32,
    start_column: u32,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CodeFlow<'a> {
    thread_flows: [ThreadFlow<'a>; 1],
}

#[derive(Serialize)]
struct ThreadFlow<'a> {
    locations: Vec<ThreadFlowLocation<'a>>,
}

#[derive(Serialize)]
struct ThreadFlowLocation<'a> {
    location: Location<'a>,
}

fn location(p: &goverify_ir::Pos) -> Location<'_> {
    Location {
        physical_location: PhysicalLocation {
            artifact_location: ArtifactLocation { uri: &p.file },
            region: Region {
                start_line: p.line,
                start_column: p.col,
            },
        },
    }
}

/// Finding message + model bindings, in the same `with:`-line shape the
/// human renderer uses. Not a byte-for-byte match, though: per spec §3,
/// machine formats deliberately carry raw values and rely on JSON
/// string escaping, whereas the human renderer sanitizes for a
/// terminal — the two surfaces can render the same model binding
/// differently.
fn message_text(f: &Finding) -> String {
    if f.model.is_empty() {
        return f.message.clone();
    }
    let bindings: Vec<String> = f.model.iter().map(|(k, v)| format!("{k} = {v}")).collect();
    format!("{}\nwith: {}", f.message, bindings.join(", "))
}

/// Build one result for `f`/`fp`; `suppressions` is `None` for a kept
/// finding, `Some([Suppression { kind }])` for a suppressed one — the
/// shared shape between kept and suppressed results (only this field
/// differs).
fn build_result<'a>(
    f: &'a Finding,
    fp: &'a str,
    suppressions: Option<[Suppression; 1]>,
) -> SarifResult<'a> {
    let flow: Vec<ThreadFlowLocation> = f
        .trace
        .iter()
        .filter_map(|s| s.pos.as_ref())
        .map(|p| ThreadFlowLocation {
            location: location(p),
        })
        .collect();
    SarifResult {
        rule_id: &f.tag,
        // SARIF level from severity (phase-6 spec §5): a deliberate,
        // documented non-additive change — existing findings (all Error
        // before annotations) flip from "warning" to "error" (Task 14
        // shakeout addendum).
        level: match f.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        },
        message: Text {
            text: message_text(f),
        },
        locations: f.pos.as_ref().map(|p| [location(p)]),
        partial_fingerprints: PartialFingerprints { goverify_v1: fp },
        code_flows: (!flow.is_empty()).then_some([CodeFlow {
            thread_flows: [ThreadFlow { locations: flow }],
        }]),
        suppressions,
    }
}

/// `fps` is parallel to `findings` (fingerprint::fingerprints);
/// `sup_pragma`/`sup_baseline` are the findings suppressed by the
/// pragma-ignore filter and the baseline filter respectively, each
/// paired with its own fingerprint (phase-6 spec §5). Suppressed
/// findings are emitted as results carrying a `suppressions` array
/// rather than omitted, in this deterministic order: kept findings
/// first (no suppression), then pragma-suppressed ("inSource"), then
/// baseline-suppressed ("external") — each group in its input order.
pub fn render_sarif(
    findings: &[Finding],
    fps: &[String],
    sup_pragma: &[(Finding, String)],
    sup_baseline: &[(Finding, String)],
) -> String {
    debug_assert_eq!(
        findings.len(),
        fps.len(),
        "render_sarif: findings/fps length mismatch would silently truncate the report via zip"
    );
    let mut results: Vec<SarifResult> = findings
        .iter()
        .zip(fps)
        .map(|(f, fp)| build_result(f, fp, None))
        .collect();
    results.extend(
        sup_pragma
            .iter()
            .map(|(f, fp)| build_result(f, fp, Some([Suppression { kind: "inSource" }]))),
    );
    results.extend(
        sup_baseline
            .iter()
            .map(|(f, fp)| build_result(f, fp, Some([Suppression { kind: "external" }]))),
    );
    let out = Sarif {
        schema: SARIF_SCHEMA,
        version: SARIF_VERSION,
        runs: [Run {
            tool: Tool {
                driver: Driver {
                    name: "goverify",
                    semantic_version: env!("CARGO_PKG_VERSION"),
                    rules: RULES
                        .iter()
                        .map(|(id, desc)| Rule {
                            id,
                            short_description: Text {
                                text: (*desc).to_string(),
                            },
                        })
                        .collect(),
                },
            },
            results,
            properties: RunProperties {
                suppressed_by_baseline: sup_baseline.len(),
                suppressed_by_pragma: sup_pragma.len(),
            },
        }],
    };
    let mut s = serde_json::to_string_pretty(&out).expect("infallible serialize");
    s.push('\n');
    s
}

#[cfg(test)]
mod tests {
    use goverify_analysis::TraceStep;

    use super::*;

    #[test]
    fn render_sarif_shape() {
        let f = Finding {
            checker: "nil".to_string(),
            tag: "nil-deref".to_string(),
            func: "example.com/m.F".to_string(),
            pos: Some(goverify_ir::Pos {
                file: "m.go".to_string(),
                line: 7,
                col: 9,
            }),
            message: "possibly-nil result dereferenced".to_string(),
            trace: vec![TraceStep {
                block: 0,
                pos: Some(goverify_ir::Pos {
                    file: "m.go".to_string(),
                    line: 6,
                    col: 2,
                }),
            }],
            model: vec![("p0".to_string(), "(ptr-nil)".to_string())],
            severity: goverify_analysis::Severity::Error,
        };
        let fps = vec!["v1:00112233445566778899aabbccddeeff".to_string()];
        let got = render_sarif(&[f], &fps, &[], &[]);
        // Structural pins (a full golden lands in Task 11's corpus suite):
        let v: serde_json::Value = serde_json::from_str(&got).expect("valid JSON");
        assert_eq!(v["version"], "2.1.0");
        assert_eq!(v["runs"][0]["tool"]["driver"]["name"], "goverify");
        let r = &v["runs"][0]["results"][0];
        assert_eq!(r["ruleId"], "nil-deref");
        assert!(
            r.get("suppressions").is_none(),
            "a kept finding carries no suppressions: {r}"
        );
        // Error severity -> SARIF level "error" (phase-6 spec §5).
        assert_eq!(r["level"], "error");
        assert_eq!(
            r["partialFingerprints"]["goverify/v1"],
            "v1:00112233445566778899aabbccddeeff"
        );
        assert_eq!(
            r["locations"][0]["physicalLocation"]["artifactLocation"]["uri"],
            "m.go"
        );
        assert_eq!(
            r["locations"][0]["physicalLocation"]["region"]["startLine"],
            7
        );
        assert_eq!(
            r["codeFlows"][0]["threadFlows"][0]["locations"][0]["location"]["physicalLocation"]["region"]
                ["startLine"],
            6
        );
        let msg = r["message"]["text"].as_str().unwrap();
        assert!(
            msg.contains("possibly-nil") && msg.contains("with: p0 = (ptr-nil)"),
            "{msg}"
        );
        assert_eq!(v["runs"][0]["properties"]["suppressedByBaseline"], 0);
        assert_eq!(v["runs"][0]["properties"]["suppressedByPragma"], 0);
        // Determinism guards: no timestamps, no absolute paths.
        assert!(
            !got.contains("startTimeUtc") && !got.contains("invocation"),
            "no provenance"
        );
        assert!(!got.contains("\"/"), "no absolute paths: {got}");
    }

    #[test]
    fn partial_fingerprints_key_matches_scheme() {
        // The literal `#[serde(rename = "goverify/v1")]` above must stay
        // in lock-step with `fingerprint::SCHEME` (F4, phase5b review):
        // a future SCHEME bump that forgets this rename would silently
        // decouple the SARIF key from the value's scheme prefix.
        assert_eq!(
            "goverify/v1",
            format!("goverify/{}", goverify_cli::fingerprint::SCHEME),
            "partialFingerprints key must be goverify/<fingerprint::SCHEME>"
        );
    }

    #[test]
    fn positionless_finding_and_empty_trace_omit_optional_blocks() {
        let f = Finding {
            checker: "nil".to_string(),
            tag: "nil-deref".to_string(),
            func: "example.com/m.F".to_string(),
            pos: None,
            message: "m".to_string(),
            trace: Vec::new(),
            model: Vec::new(),
            severity: goverify_analysis::Severity::Error,
        };
        let got = render_sarif(&[f], &["v1:0".to_string()], &[], &[]);
        let v: serde_json::Value = serde_json::from_str(&got).unwrap();
        let r = &v["runs"][0]["results"][0];
        assert!(r.get("locations").is_none(), "no pos -> no locations: {r}");
        assert!(
            r.get("codeFlows").is_none(),
            "no trace -> no codeFlows: {r}"
        );
    }

    #[test]
    fn render_sarif_level_reflects_severity() {
        let mk = |severity| Finding {
            checker: "annotation".to_string(),
            tag: "unverified-annotation".to_string(),
            func: "example.com/m.F".to_string(),
            pos: None,
            message: "m".to_string(),
            trace: Vec::new(),
            model: Vec::new(),
            severity,
        };
        let got = render_sarif(
            &[
                mk(goverify_analysis::Severity::Error),
                mk(goverify_analysis::Severity::Warning),
            ],
            &["v1:0".to_string(), "v1:1".to_string()],
            &[],
            &[],
        );
        let v: serde_json::Value = serde_json::from_str(&got).unwrap();
        assert_eq!(v["runs"][0]["results"][0]["level"], "error");
        assert_eq!(v["runs"][0]["results"][1]["level"], "warning");
    }

    /// One kept, one pragma-suppressed, one baseline-suppressed finding
    /// (phase-6 spec §5): asserts result order (kept, then pragma, then
    /// baseline), each carrying the right `suppressions` entry (or
    /// none), the run-properties counts, and that two independent
    /// renders are byte-identical.
    #[test]
    fn render_sarif_emits_suppressions_and_counts() {
        let mk = |tag: &str, label: &str| Finding {
            checker: "nil".to_string(),
            tag: tag.to_string(),
            func: "example.com/m.F".to_string(),
            pos: None,
            message: format!("m-{label}"),
            trace: Vec::new(),
            model: Vec::new(),
            severity: goverify_analysis::Severity::Error,
        };
        let kept = mk("nil-deref", "kept");
        let pragma = mk("nil-deref", "pragma");
        let baseline = mk("nil-deref", "baseline");
        let kept_fps = vec!["v1:0".to_string()];
        let sup_pragma = vec![(pragma, "v1:1".to_string())];
        let sup_baseline = vec![(baseline, "v1:2".to_string())];

        let render = || {
            render_sarif(
                std::slice::from_ref(&kept),
                &kept_fps,
                &sup_pragma,
                &sup_baseline,
            )
        };
        let got = render();
        let v: serde_json::Value = serde_json::from_str(&got).unwrap();
        let results = v["runs"][0]["results"].as_array().unwrap();
        assert_eq!(results.len(), 3, "kept + pragma + baseline: {v}");

        assert_eq!(results[0]["message"]["text"], "m-kept");
        assert!(
            results[0].get("suppressions").is_none(),
            "kept: {:?}",
            results[0]
        );

        assert_eq!(results[1]["message"]["text"], "m-pragma");
        assert_eq!(results[1]["suppressions"][0]["kind"], "inSource");

        assert_eq!(results[2]["message"]["text"], "m-baseline");
        assert_eq!(results[2]["suppressions"][0]["kind"], "external");

        assert_eq!(v["runs"][0]["properties"]["suppressedByPragma"], 1);
        assert_eq!(v["runs"][0]["properties"]["suppressedByBaseline"], 1);

        // Determinism: two independent renders are byte-identical.
        assert_eq!(render(), got, "render_sarif must be deterministic");
    }
}
