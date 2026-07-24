# Phase 5b: CI Surface Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship the CI reporting surface — `check --format sarif|json`, `goverify baseline write` + automatic baseline suppression, and `check --diff-base <git-ref>` — on stable position-independent finding fingerprints.

**Architecture:** All new code lives in `goverify-cli`, split lib/bin: a new lib target (`src/lib.rs`) exposes the pure, fuzzable pieces (`fingerprint`, `baseline`) so `fuzz/` can reach the baseline parser; `sarif.rs`, `json.rs`, and `diff.rs` stay bin-side modules beside `render.rs`. Two small additions land in the crates that own the data: `CallGraph::callers_closure` (goverify-ir, transitive callers for diff-base) and `Program::func_semantic_hash` (goverify-ir, a position-blind sibling of `func_ir_hash` — required because `func_ir_hash` is position-sensitive by asserted invariant, and gate G4 promises a comment-only edit yields an empty diff-base report).

**Tech Stack:** Rust workspace + Go sidecar (unchanged). **New workspace deps: `serde` (derive) + `serde_json`** — spec §1 justification: three consumers (SARIF emit, native JSON emit, baseline write/read; the read parses a possibly-human-edited file). `--diff-base` shells out to the `git` CLI (no libgit2 crate; git required only when the flag is used).

**Spec:** `docs/superpowers/specs/2026-07-24-phase5b-ci-surface-design.md`. Parent: `2026-07-16-goverify-design.md` §10.

## Global Constraints

- **Determinism is the root invariant**: SARIF/JSON/baseline outputs are byte-identical across runs. No timestamps, no absolute paths, no map-iteration order reaching output (sort before emitting; `HashMap`/`HashSet` may be used for membership/counting only, never iterated into output).
- **Parsers of bytes the analyzer didn't write must reject, never panic** — the baseline parser gets a fuzz target (Task 7).
- **Errors degrade, never die** — with the spec's one documented exception: a malformed baseline file is a **hard error, exit 2** (user-authored gate config; silent unfiltered fallback would flood CI). Diff-base git/ref/extraction failures are also hard errors (exit 2) — a silently-wrong report set is worse than an actionable failure.
- Fingerprint scheme is versioned in-band (`"v1:"` prefix); baseline files carry `schema_version: 1`; SARIF is pinned 2.1.0. **No `.gvir` schema change, no cache-version bumps** — `func_semantic_hash` is never a cache-key input.
- Checker messages must stay free of source positions (fingerprint invariant, stated in code).
- Run toolchain commands through mise: `mise x -- cargo ...` (sandbox RUSTUP relocation; memory `goverify-sandbox-environment`).
- Commits are **unsigned** in this sandbox: `git commit --no-gpg-sign`. Re-sign before pushing. Commit-message prefix: `phase5b:`.
- Blocking gate per task: `mise run lint` + the task's tests green; `mise run corpus` stays green throughout. `Cargo.lock` changes are committed with the task that causes them.
- Tests never write into checked-in corpus dirs — copy fixtures to a tempdir first. CLI integration tests reuse one extracted fixture / one sidecar build where possible (EDR stall hazard on this machine; memory `sentinelone-exec-stall`).
- Report files go to `.superpowers/sdd/task-N-report-phase5b.md` (the ledger dir is gitignored, but `task-1-investigation.md`, `task-3-investigation.md`, `task-3-report.md` are TRACKED wave-2 records — never overwrite them).

---

## Task Dependency Order

1 → 2 → 3 (formats chain). 4 → 5 → 6 (baseline chain; 4 needs 1). 7 after 4. 8 and 9 are independent (any time). 10 needs 4, 5, 8, 9. 11 needs 2, 3, 4. 12 last.

---

### Task 1: serde deps, cli lib target, fingerprint module

**Files:**
- Modify: `Cargo.toml` (workspace deps)
- Modify: `crates/goverify-cli/Cargo.toml` (lib target + deps)
- Create: `crates/goverify-cli/src/lib.rs`
- Create: `crates/goverify-cli/src/fingerprint.rs`

**Interfaces:**
- Consumes: `goverify_analysis::Finding` (fields `checker`, `tag`, `func`, `pos`, `message`, `trace`, `model` — checker.rs:28).
- Produces: `goverify_cli::fingerprint::fingerprints(&[Finding]) -> Vec<String>` (parallel to input order) and `fingerprint(&Finding, ordinal: u32) -> String`; `pub const SCHEME: &str = "v1"`. Tasks 2–6 and 10 consume these.

- [ ] **Step 1: Add workspace deps**

In the root `Cargo.toml` under `[workspace.dependencies]` (after `tempfile = "3"`):

```toml
serde = { version = "1", features = ["derive"] }
serde_json = "1"
```

- [ ] **Step 2: Give goverify-cli a lib target**

In `crates/goverify-cli/Cargo.toml`, after the `[[bin]]` block add:

```toml
[lib]
name = "goverify_cli"
path = "src/lib.rs"
```

and extend `[dependencies]`:

```toml
blake3.workspace = true
serde.workspace = true
serde_json.workspace = true
```

- [ ] **Step 3: Create `src/lib.rs`**

```rust
//! Library surface of the goverify CLI (the binary lives in main.rs).
//! Exists so `fuzz/` can reach the baseline parser — parsers of bytes
//! the analyzer didn't write must reject, never panic (parent spec
//! §12.4) — and holds the pure, reusable pieces of the reporting
//! layer. Orchestration (formats dispatch, git, rendering) stays
//! bin-side.

pub mod fingerprint;
```

(`pub mod baseline;` joins in Task 4.)

- [ ] **Step 4: Write the failing tests**

Create `crates/goverify-cli/src/fingerprint.rs` with the test module first:

```rust
#[cfg(test)]
mod tests {
    use goverify_analysis::Finding;

    use super::*;

    fn finding(func: &str, msg: &str, line: u32) -> Finding {
        Finding {
            checker: "nil".to_string(),
            tag: "nil-deref".to_string(),
            func: func.to_string(),
            pos: Some(goverify_ir::Pos {
                file: "a.go".to_string(),
                line,
                col: 1,
            }),
            message: msg.to_string(),
            trace: Vec::new(),
            model: Vec::new(),
        }
    }

    #[test]
    fn identical_siblings_get_distinct_ordinal_fingerprints() {
        let fs = vec![finding("p.F", "m", 3), finding("p.F", "m", 9)];
        let fps = fingerprints(&fs);
        assert_eq!(fps.len(), 2, "fingerprints() is parallel to input");
        assert_ne!(fps[0], fps[1], "identical siblings must differ by ordinal");
        assert_eq!(fps[0], fingerprint(&fs[0], 0), "first sibling is ordinal 0");
        assert_eq!(fps[1], fingerprint(&fs[1], 1), "second sibling is ordinal 1");
    }

    #[test]
    fn fingerprints_are_position_independent() {
        // Same shape at different positions: the fingerprint must not
        // move when the finding does (spec §2 — line shifts don't churn
        // the baseline).
        let a = fingerprints(&[finding("p.F", "m", 3)]);
        let b = fingerprints(&[finding("p.F", "m", 300)]);
        assert_eq!(a, b, "position must not reach the fingerprint");
    }

    #[test]
    fn shape_fields_all_reach_the_hash() {
        let base = finding("p.F", "m", 1);
        let fp = fingerprint(&base, 0);
        let mut c = base.clone();
        c.checker = "bounds".to_string();
        assert_ne!(fp, fingerprint(&c, 0), "checker in hash");
        let mut c = base.clone();
        c.func = "p.G".to_string();
        assert_ne!(fp, fingerprint(&c, 0), "func in hash");
        let mut c = base.clone();
        c.tag = "bounds".to_string();
        assert_ne!(fp, fingerprint(&c, 0), "tag in hash");
        let mut c = base.clone();
        c.message = "m2".to_string();
        assert_ne!(fp, fingerprint(&c, 0), "message in hash");
    }

    #[test]
    fn scheme_prefix_and_length() {
        let fp = fingerprint(&finding("p.F", "m", 1), 0);
        assert!(fp.starts_with("v1:"), "in-band scheme version: {fp}");
        assert_eq!(fp.len(), 3 + 32, "16 truncated bytes as hex: {fp}");
    }
}
```

(If `goverify_ir::Pos` field names differ, mirror the real struct — it is the type `Finding.pos` already carries; goverify-ir is already a cli dependency.)

- [ ] **Step 5: Run to verify failure**

Run: `mise x -- cargo test -p goverify-cli --lib`
Expected: FAIL to compile — `fingerprints` / `fingerprint` not defined.

- [ ] **Step 6: Implement**

Above the test module in `fingerprint.rs`:

```rust
//! Position-independent finding fingerprints (phase-5b spec §2):
//!
//!   fp = "v1:" + hex(blake3(checker ⊕ func ⊕ tag ⊕ message ⊕ ordinal)[..16])
//!
//! Fields are length-prefixed (no separator injection — same discipline
//! as the cache keys); `ordinal` is this finding's index among identical
//! (checker, func, tag, message) siblings in input (position) order.
//! INVARIANT the scheme leans on: checker messages contain no source
//! positions (checker.rs) — that is what makes fingerprints survive
//! unrelated line shifts. The "v1:" prefix versions the scheme in-band;
//! a change is a new prefix, never a silent re-keying.

use std::collections::HashMap;
use std::fmt::Write;

use goverify_analysis::Finding;

/// In-band fingerprint scheme version. The baseline parser (baseline.rs)
/// rejects entries from any other scheme.
pub const SCHEME: &str = "v1";

/// Fingerprints parallel to `findings` (same order). Compute over the
/// scoped, pre-baseline set (spec §2): scope and diff-base filter at
/// function granularity, so sibling groups (which share one `func`)
/// never split and ordinals are stable across filter combinations.
pub fn fingerprints(findings: &[Finding]) -> Vec<String> {
    let mut seen: HashMap<(&str, &str, &str, &str), u32> = HashMap::new();
    findings
        .iter()
        .map(|f| {
            let key = (
                f.checker.as_str(),
                f.func.as_str(),
                f.tag.as_str(),
                f.message.as_str(),
            );
            let ordinal = seen.entry(key).and_modify(|c| *c += 1).or_insert(0);
            fingerprint(f, *ordinal)
        })
        .collect()
}

/// One finding's fingerprint at a caller-assigned ordinal.
pub fn fingerprint(f: &Finding, ordinal: u32) -> String {
    let mut h = blake3::Hasher::new();
    h.update(b"goverify-fingerprint\0");
    for field in [&f.checker, &f.func, &f.tag, &f.message] {
        h.update(&(field.len() as u64).to_le_bytes());
        h.update(field.as_bytes());
    }
    h.update(&ordinal.to_le_bytes());
    let digest = h.finalize();
    let mut out = String::with_capacity(SCHEME.len() + 1 + 32);
    out.push_str(SCHEME);
    out.push(':');
    for b in &digest.as_bytes()[..16] {
        // Writing to a String is infallible.
        let _ = write!(out, "{b:02x}");
    }
    out
}
```

- [ ] **Step 7: Run to verify pass**

Run: `mise x -- cargo test -p goverify-cli --lib`
Expected: PASS (4 tests).

- [ ] **Step 8: Lint, lock, commit**

```bash
mise x -- cargo build -p goverify-cli   # refresh Cargo.lock
mise run lint
mise run audit                          # new deps: serde, serde_json
git add Cargo.toml Cargo.lock crates/goverify-cli/Cargo.toml crates/goverify-cli/src/lib.rs crates/goverify-cli/src/fingerprint.rs
git commit --no-gpg-sign -m "phase5b: finding fingerprints (v1 scheme) + serde workspace deps + cli lib target"
```

---

### Task 2: `--format` flag + native JSON emitter

**Files:**
- Create: `crates/goverify-cli/src/json.rs`
- Modify: `crates/goverify-cli/src/main.rs` (CheckArgs ~line 49, `run_check` render block ~line 459)

**Interfaces:**
- Consumes: `goverify_cli::fingerprint::fingerprints` (Task 1).
- Produces: `OutputFormat` clap enum (`Human | Json`; `Sarif` joins in Task 3); `json::Summary { total: usize, suppressed_by_baseline: usize, diff_base_scoped: bool }`; `json::render_json(findings: &[Finding], fps: &[String], summary: &Summary) -> String`. Tasks 3, 5, 10, 11 consume the dispatch point and `Summary`.

- [ ] **Step 1: Write the failing unit test**

Create `crates/goverify-cli/src/json.rs` starting with tests:

```rust
#[cfg(test)]
mod tests {
    use goverify_analysis::{Finding, TraceStep};

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
                TraceStep { block: 1, pos: None }, // position-less: dropped
            ],
            model: vec![("p0".to_string(), "(ptr-nil)".to_string())],
        };
        let fps = vec!["v1:00112233445566778899aabbccddeeff".to_string()];
        let summary = Summary {
            total: 1,
            suppressed_by_baseline: 0,
            diff_base_scoped: false,
        };
        let got = render_json(&[f], &fps, &summary);
        let want = r#"{
  "schema_version": 1,
  "findings": [
    {
      "fingerprint": "v1:00112233445566778899aabbccddeeff",
      "checker": "nil",
      "tag": "nil-deref",
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
            diff_base_scoped: false,
        };
        let got = render_json(&[], &[], &summary);
        assert!(got.starts_with('{') && got.ends_with("}\n"), "{got}");
        assert!(got.contains("\"findings\": []"), "{got}");
    }
}
```

Register `mod json;` in main.rs (beside `mod render;`).

- [ ] **Step 2: Run to verify failure**

Run: `mise x -- cargo test -p goverify-cli --bin goverify json::`
Expected: FAIL to compile — `Summary` / `render_json` not defined.

- [ ] **Step 3: Implement `json.rs`**

Above the tests:

```rust
//! `--format json` (phase-5b spec §3): the native machine schema.
//! Byte-identical across runs — findings arrive pre-sorted, field order
//! is fixed by struct declaration, escaping is owned by serde_json. No
//! timestamps, no absolute paths (Pos.file is extractor-relative).

use goverify_analysis::Finding;
use serde::Serialize;

/// Bump on any change to the emitted shape (consumers key on it).
pub const JSON_SCHEMA_VERSION: u32 = 1;

#[derive(Serialize)]
pub struct Summary {
    pub total: usize,
    pub suppressed_by_baseline: usize,
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
    let findings: Vec<JsonFinding> = findings
        .iter()
        .zip(fps)
        .map(|(f, fp)| JsonFinding {
            fingerprint: fp,
            checker: &f.checker,
            tag: &f.tag,
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
```

(Adapt `line`/`col` types to `goverify_ir::Pos`'s actual field types if not `u32`; the test in Step 1 pins the output text either way.)

- [ ] **Step 4: Run to verify pass**

Run: `mise x -- cargo test -p goverify-cli --bin goverify json::`
Expected: PASS. If serde_json's pretty indentation differs from the `want` string, fix the `want` string to the actual (deterministic) output — the assertion's job is pinning the shape, then guarding drift.

- [ ] **Step 5: Wire the flag and dispatch**

In `main.rs`: add the enum (above `CheckArgs`):

```rust
#[derive(Clone, Copy, PartialEq, clap::ValueEnum)]
enum OutputFormat {
    /// Labeled source spans with traces (default).
    Human,
    /// Native machine schema (schema_version 1).
    Json,
}
```

Add to `CheckArgs`:

```rust
    /// Output format (spec §10): human terminal report, or machine
    /// formats for CI. Machine formats are byte-identical across runs.
    #[arg(long, value_enum, default_value_t = OutputFormat::Human)]
    format: OutputFormat,
```

In `run_check`, replace the single `print!("{}", render::render_findings(&scoped, Path::new(".")));` line with:

```rust
    let fps = goverify_cli::fingerprint::fingerprints(&scoped);
    let summary = json::Summary {
        total: scoped.len(),
        suppressed_by_baseline: 0,
        diff_base_scoped: false,
    };
    match ca.format {
        OutputFormat::Human => {
            print!("{}", render::render_findings(&scoped, Path::new(".")));
        }
        OutputFormat::Json => print!("{}", json::render_json(&scoped, &fps, &summary)),
    }
```

- [ ] **Step 6: Full check + commit**

Run: `mise x -- cargo test -p goverify-cli`
Expected: PASS — including the pre-existing `check_cold_and_warm_default_cache_stdout_identical` (default format path is byte-unchanged).

```bash
mise run lint
git add crates/goverify-cli/src/json.rs crates/goverify-cli/src/main.rs
git commit --no-gpg-sign -m "phase5b: check --format json (native schema v1)"
```

---

### Task 3: SARIF emitter

**Files:**
- Create: `crates/goverify-cli/src/sarif.rs`
- Modify: `crates/goverify-cli/src/main.rs` (add `Sarif` variant + dispatch arm, `mod sarif;`)

**Interfaces:**
- Consumes: `fingerprint::fingerprints` (Task 1), `OutputFormat` dispatch (Task 2).
- Produces: `sarif::render_sarif(findings: &[Finding], fps: &[String], suppressed_by_baseline: usize) -> String`. Tasks 5, 11 consume.

- [ ] **Step 1: Write the failing unit test**

Create `crates/goverify-cli/src/sarif.rs`, tests first:

```rust
#[cfg(test)]
mod tests {
    use goverify_analysis::{Finding, TraceStep};

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
        };
        let fps = vec!["v1:00112233445566778899aabbccddeeff".to_string()];
        let got = render_sarif(&[f], &fps, 3);
        // Structural pins (a full golden lands in Task 11's corpus suite):
        let v: serde_json::Value = serde_json::from_str(&got).expect("valid JSON");
        assert_eq!(v["version"], "2.1.0");
        assert_eq!(v["runs"][0]["tool"]["driver"]["name"], "goverify");
        let r = &v["runs"][0]["results"][0];
        assert_eq!(r["ruleId"], "nil-deref");
        assert_eq!(r["level"], "warning");
        assert_eq!(
            r["partialFingerprints"]["goverify/v1"],
            "v1:00112233445566778899aabbccddeeff"
        );
        assert_eq!(
            r["locations"][0]["physicalLocation"]["artifactLocation"]["uri"],
            "m.go"
        );
        assert_eq!(
            r["locations"][0]["physicalLocation"]["region"]["startLine"], 7
        );
        assert_eq!(
            r["codeFlows"][0]["threadFlows"][0]["locations"][0]["location"]
                ["physicalLocation"]["region"]["startLine"],
            6
        );
        let msg = r["message"]["text"].as_str().unwrap();
        assert!(msg.contains("possibly-nil") && msg.contains("with: p0 = (ptr-nil)"), "{msg}");
        assert_eq!(v["runs"][0]["properties"]["suppressedByBaseline"], 3);
        // Determinism guards: no timestamps, no absolute paths.
        assert!(!got.contains("startTimeUtc") && !got.contains("invocation"), "no provenance");
        assert!(!got.contains("\"/"), "no absolute paths: {got}");
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
        };
        let got = render_sarif(&[f], &["v1:0".to_string()], 0);
        let v: serde_json::Value = serde_json::from_str(&got).unwrap();
        let r = &v["runs"][0]["results"][0];
        assert!(r.get("locations").is_none(), "no pos -> no locations: {r}");
        assert!(r.get("codeFlows").is_none(), "no trace -> no codeFlows: {r}");
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `mise x -- cargo test -p goverify-cli --bin goverify sarif::`
Expected: FAIL to compile.

- [ ] **Step 3: Implement `sarif.rs`**

```rust
//! `--format sarif` (phase-5b spec §3): SARIF 2.1.0, minimal static
//! subset for GitHub code scanning. Determinism is the root invariant:
//! no timestamps, no invocation objects, no absolute URIs — SARIF's
//! optional provenance fields all violate it and are deliberately
//! absent. Suppressed-by-baseline results are OMITTED (not emitted with
//! `suppressions`); the count goes in run.properties.

use goverify_analysis::Finding;
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
}

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

/// Finding message + model bindings, matching the human renderer's
/// `with:` line so both surfaces read the same.
fn message_text(f: &Finding) -> String {
    if f.model.is_empty() {
        return f.message.clone();
    }
    let bindings: Vec<String> = f.model.iter().map(|(k, v)| format!("{k} = {v}")).collect();
    format!("{}\nwith: {}", f.message, bindings.join(", "))
}

/// `fps` is parallel to `findings` (fingerprint::fingerprints).
pub fn render_sarif(findings: &[Finding], fps: &[String], suppressed_by_baseline: usize) -> String {
    let results: Vec<SarifResult> = findings
        .iter()
        .zip(fps)
        .map(|(f, fp)| {
            let flow: Vec<ThreadFlowLocation> = f
                .trace
                .iter()
                .filter_map(|s| s.pos.as_ref())
                .map(|p| ThreadFlowLocation { location: location(p) })
                .collect();
            SarifResult {
                rule_id: &f.tag,
                level: "warning",
                message: Text {
                    text: message_text(f),
                },
                locations: f.pos.as_ref().map(|p| [location(p)]),
                partial_fingerprints: PartialFingerprints { goverify_v1: fp },
                code_flows: (!flow.is_empty()).then(|| {
                    [CodeFlow {
                        thread_flows: [ThreadFlow { locations: flow }],
                    }]
                }),
            }
        })
        .collect();
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
                suppressed_by_baseline,
            },
        }],
    };
    let mut s = serde_json::to_string_pretty(&out).expect("infallible serialize");
    s.push('\n');
    s
}
```

- [ ] **Step 4: Wire the variant**

In `main.rs`: add `mod sarif;`, extend the enum:

```rust
    /// SARIF 2.1.0 for GitHub code scanning.
    Sarif,
```

and the dispatch:

```rust
        OutputFormat::Sarif => print!("{}", sarif::render_sarif(&scoped, &fps, 0)),
```

- [ ] **Step 5: Run tests, lint, commit**

Run: `mise x -- cargo test -p goverify-cli`
Expected: PASS.

```bash
mise run lint
git add crates/goverify-cli/src/sarif.rs crates/goverify-cli/src/main.rs
git commit --no-gpg-sign -m "phase5b: check --format sarif (2.1.0, deterministic subset)"
```

---

### Task 4: pipeline refactor + `baseline write`

**Files:**
- Create: `crates/goverify-cli/src/baseline.rs`
- Modify: `crates/goverify-cli/src/lib.rs` (add `pub mod baseline;`)
- Modify: `crates/goverify-cli/src/main.rs` (extract `analyze_module` + `acquire_program` from `run_check`; new `Baseline` subcommand)

**Interfaces:**
- Consumes: `fingerprint::fingerprints` (Task 1).
- Produces:
  - `goverify_cli::baseline::{Baseline, BaselineEntry, BASELINE_SCHEMA_VERSION}`; `baseline::render(findings: &[Finding], fps: &[String]) -> String`; `baseline::parse(bytes: &[u8]) -> Result<Baseline, String>`. Tasks 5, 7 consume `parse`; Task 5 consumes the write path's file.
  - main.rs internals later tasks consume: `struct Analyzed { program: goverify_ir::Program, scoped: Vec<Finding>, cache_root: Option<PathBuf>, timings: bool }`; `fn analyze_module(ca: &CheckArgs) -> Result<Analyzed, Box<dyn std::error::Error>>`; `fn acquire_program(dir: &Path, gvir_dir: Option<&PathBuf>, patterns: &[String], cache_root: Option<&PathBuf>, timings: bool) -> Result<goverify_ir::Program, Box<dyn std::error::Error>>` (Task 10 calls it with the base worktree dir).

- [ ] **Step 1: Write the failing baseline-module tests**

Create `crates/goverify-cli/src/baseline.rs`, tests first:

```rust
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
```

Add `pub mod baseline;` to `src/lib.rs`.

- [ ] **Step 2: Run to verify failure**

Run: `mise x -- cargo test -p goverify-cli --lib baseline::`
Expected: FAIL to compile.

- [ ] **Step 3: Implement `baseline.rs`**

```rust
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

#[derive(Serialize, Deserialize)]
pub struct Baseline {
    pub schema_version: u32,
    pub entries: Vec<BaselineEntry>,
}

#[derive(Serialize, Deserialize)]
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
    if let Some(bad) = b.entries.iter().find(|e| !e.fingerprint.starts_with(&expect)) {
        return Err(format!(
            "unsupported fingerprint scheme in entry {:?} (this build writes {}...)",
            bad.fingerprint, expect
        ));
    }
    Ok(b)
}
```

- [ ] **Step 4: Run to verify pass**

Run: `mise x -- cargo test -p goverify-cli --lib baseline::`
Expected: PASS.

- [ ] **Step 5: Refactor `run_check` into `analyze_module` (pure refactor — behavior byte-identical)**

In `main.rs`, extract everything from the top of `run_check` through the scope filter into:

```rust
/// Everything `check` and `baseline write` share (spec §4): cache-root
/// resolution, program acquisition (extraction cache when available),
/// the engine run, and module scoping. Returns the SCOPED findings in
/// render order plus the pieces `--diff-base` (spec §5) needs.
struct Analyzed {
    program: goverify_ir::Program,
    scoped: Vec<goverify_analysis::Finding>,
    cache_root: Option<PathBuf>,
    timings: bool,
}

fn analyze_module(ca: &CheckArgs) -> Result<Analyzed, Box<dyn std::error::Error>> {
    // ... body moved verbatim from run_check: timings/cache_root
    // resolution, program acquisition via acquire_program(Path::new("."),
    // ...), diagnostics, solver limits + retry backends, analyze_full,
    // escalation count, scope resolution + scope_findings ...
}
```

and split program acquisition (the `match (&ca.gvir_dir, &cache_root)` block) into the dir-parameterized helper Task 10 reuses:

```rust
/// Program acquisition for the module rooted at `dir` (spec §9 paths):
/// an explicit gvir_dir loads as-is; with a cache root, extract through
/// the extraction cache and fall back to plain extraction on any cache
/// failure (degrade, never die); otherwise plain extraction.
fn acquire_program(
    dir: &Path,
    gvir_dir: Option<&PathBuf>,
    patterns: &[String],
    cache_root: Option<&PathBuf>,
    timings: bool,
) -> Result<goverify_ir::Program, Box<dyn std::error::Error>>
```

(The existing `load_program(&DebugArgs)` path is cwd-bound through `sidecar.extract(Path::new("."), ...)` — thread `dir` through instead. `run_check` becomes: `let a = analyze_module(&ca)?;` followed by the fingerprint/summary/format-dispatch block from Tasks 2–3 and the exit-code computation, all reading `a.scoped`.)

- [ ] **Step 6: Verify the refactor is invisible**

Run: `mise x -- cargo test -p goverify-cli`
Expected: PASS — in particular `check_cold_and_warm_default_cache_stdout_identical` and the debug_integration suite, unchanged. This is the wave's G1 tripwire; do not proceed while any output differs.

- [ ] **Step 7: Add the `baseline write` subcommand**

In `main.rs`:

```rust
    /// Manage the findings baseline (spec §10).
    Baseline {
        #[command(subcommand)]
        what: BaselineWhat,
    },
```

```rust
#[derive(Subcommand)]
enum BaselineWhat {
    /// Record current findings in .goverify/baseline.json; later
    /// `check` runs report only new findings.
    Write(CheckArgs),
}
```

dispatch in `run()`:

```rust
        Cmd::Baseline { what } => match what {
            BaselineWhat::Write(ca) => run_baseline_write(ca),
        },
```

handler:

```rust
/// `baseline write` (spec §4): the identical pipeline as `check`,
/// recording the scoped findings instead of rendering them. Exit 0 on
/// success regardless of finding count — recording findings is the
/// point.
fn run_baseline_write(ca: CheckArgs) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let a = analyze_module(&ca)?;
    let fps = goverify_cli::fingerprint::fingerprints(&a.scoped);
    let dir = Path::new(".goverify");
    std::fs::create_dir_all(dir)?;
    let path = dir.join("baseline.json");
    std::fs::write(&path, goverify_cli::baseline::render(&a.scoped, &fps))?;
    eprintln!(
        "goverify: baseline: {} finding(s) recorded in {}",
        a.scoped.len(),
        path.display()
    );
    Ok(ExitCode::SUCCESS)
}
```

- [ ] **Step 8: Integration test (write path)**

In `crates/goverify-cli/tests/` create `report_integration.rs`:

```rust
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
        .filter(|l| l.contains(": nil-deref: ") || l.contains(": bounds: ")
            || l.contains(": div-zero: ") || l.contains(": overflow: "))
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
    assert!(w1.status.success(), "{}", String::from_utf8_lossy(&w1.stderr));
    let path = module.join(".goverify/baseline.json");
    let text1 = std::fs::read(&path).expect("baseline written");
    let b = goverify_cli::baseline::parse(&text1).expect("own output parses");
    assert_eq!(b.entries.len(), n, "one entry per scoped finding");

    let w2 = goverify(&["baseline", "write", "./..."], &module, cache.path());
    assert!(w2.status.success());
    let text2 = std::fs::read(&path).unwrap();
    assert_eq!(text1, text2, "baseline write is byte-deterministic");
}
```

- [ ] **Step 9: Run, lint, commit**

Run: `mise x -- cargo test -p goverify-cli --test report_integration`
Expected: PASS.

```bash
mise run lint
git add crates/goverify-cli/src/baseline.rs crates/goverify-cli/src/lib.rs crates/goverify-cli/src/main.rs crates/goverify-cli/tests/report_integration.rs
git commit --no-gpg-sign -m "phase5b: analyze_module refactor + goverify baseline write"
```

---

### Task 5: baseline application on `check`

**Files:**
- Modify: `crates/goverify-cli/src/main.rs` (CheckArgs flags, `run_check` filter, `run_baseline_write` flag rejection)
- Modify: `crates/goverify-cli/tests/report_integration.rs`

**Interfaces:**
- Consumes: `baseline::parse` (Task 4), `fingerprint::fingerprints` (Task 1), `Summary` (Task 2).
- Produces: `--baseline <path>` / `--no-baseline` on `check`; filtering after scope, before render; exit code on the post-filter count; `fn apply_baseline(ca, findings, fps) -> Result<(Vec<Finding>, Vec<String>, usize), Box<dyn Error>>` (Task 10 relocates its call after the diff filter).

- [ ] **Step 1: Write the failing integration tests**

Append to `report_integration.rs`:

```rust
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
    assert_eq!(check.status.code(), Some(0), "all findings baselined -> clean gate");
    let stdout = String::from_utf8_lossy(&check.stdout);
    assert_eq!(finding_count(&stdout), 0, "no findings rendered: {stdout}");
    assert!(stdout.contains("suppressed"), "footer reports suppression: {stdout}");

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
    assert_eq!(out.status.code(), Some(2), "malformed baseline -> analyzer error");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("baseline"), "actionable error names the file: {stderr}");
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
    assert_eq!(out.status.code(), Some(2), "explicit missing baseline errors");

    // baseline write rejects baseline-consuming flags.
    let out = goverify(
        &["baseline", "write", "--no-baseline", "./..."],
        &module,
        cache.path(),
    );
    assert_eq!(out.status.code(), Some(2), "write+--no-baseline rejected");
}
```

- [ ] **Step 2: Run to verify failure**

Run: `mise x -- cargo test -p goverify-cli --test report_integration baseline`
Expected: FAIL — unknown `--no-baseline` / `--baseline` flags; the suppression test fails on exit code.

- [ ] **Step 3: Implement**

CheckArgs additions:

```rust
    /// Baseline file to suppress known findings (default:
    /// .goverify/baseline.json when it exists — spec §4). The exit code
    /// gates on the post-suppression count.
    #[arg(long, conflicts_with = "no_baseline")]
    baseline: Option<PathBuf>,
    /// Ignore any baseline file.
    #[arg(long)]
    no_baseline: bool,
```

The filter, in `main.rs`:

```rust
/// Baseline filtering (spec §4). An explicit --baseline must exist; the
/// implicit .goverify/baseline.json applies only when present. A
/// malformed or unreadable file is a hard error (exit 2 via run()) —
/// the documented degrade-never-die exception: this is user-authored
/// gate configuration, and silently reporting unfiltered findings would
/// flood CI and misreport the gate. Returns (kept findings, their
/// fingerprints, suppressed count).
fn apply_baseline(
    ca: &CheckArgs,
    findings: Vec<goverify_analysis::Finding>,
    fps: Vec<String>,
) -> Result<(Vec<goverify_analysis::Finding>, Vec<String>, usize), Box<dyn std::error::Error>> {
    let path: Option<PathBuf> = if ca.no_baseline {
        None
    } else {
        match &ca.baseline {
            Some(p) => {
                if !p.is_file() {
                    return Err(format!("baseline {} not found", p.display()).into());
                }
                Some(p.clone())
            }
            None => {
                let implied = Path::new(".goverify").join("baseline.json");
                implied.is_file().then_some(implied)
            }
        }
    };
    let Some(path) = path else {
        return Ok((findings, fps, 0));
    };
    let bytes =
        std::fs::read(&path).map_err(|e| format!("baseline {}: {e}", path.display()))?;
    let b = goverify_cli::baseline::parse(&bytes)
        .map_err(|e| format!("baseline {}: {e}", path.display()))?;
    let set: std::collections::HashSet<&str> =
        b.entries.iter().map(|e| e.fingerprint.as_str()).collect();
    let mut kept = Vec::new();
    let mut kept_fps = Vec::new();
    let mut suppressed = 0usize;
    for (f, fp) in findings.into_iter().zip(fps) {
        if set.contains(fp.as_str()) {
            suppressed += 1;
        } else {
            kept.push(f);
            kept_fps.push(fp);
        }
    }
    Ok((kept, kept_fps, suppressed))
}
```

In `run_check`, between the fingerprint computation and the format dispatch (fingerprints BEFORE the baseline filter — spec §2):

```rust
    let fps = goverify_cli::fingerprint::fingerprints(&a.scoped);
    let (scoped, fps, suppressed) = apply_baseline(&ca, a.scoped, fps)?;
    let summary = json::Summary {
        total: scoped.len(),
        suppressed_by_baseline: suppressed,
        diff_base_scoped: false,
    };
```

Human footer (after the `Human` arm's `print!`, inside the match arm):

```rust
            if suppressed > 0 {
                println!("goverify: baseline: {suppressed} finding(s) suppressed");
            }
```

Sarif arm passes `suppressed` instead of `0`. Exit code already keys off `scoped` (now post-filter).

In `run_baseline_write`, first line:

```rust
    if ca.baseline.is_some() || ca.no_baseline {
        return Err("baseline write records the full finding set; \
                    --baseline/--no-baseline do not apply"
            .into());
    }
```

- [ ] **Step 4: Run to verify pass**

Run: `mise x -- cargo test -p goverify-cli --test report_integration`
Expected: PASS (all baseline tests). Also run `mise x -- cargo test -p goverify-cli` — the pre-existing cold/warm byte-identity test must still pass (no baseline file exists in that fixture flow, so output is untouched).

- [ ] **Step 5: Lint + commit**

```bash
mise run lint
git add crates/goverify-cli/src/main.rs crates/goverify-cli/tests/report_integration.rs
git commit --no-gpg-sign -m "phase5b: auto-apply baseline on check (--baseline/--no-baseline, exit-2 on malformed)"
```

---

### Task 6: ordinal semantics end-to-end

**Files:**
- Modify: `crates/goverify-cli/tests/report_integration.rs`

**Interfaces:**
- Consumes: everything from Tasks 1–5. Pure test task — a reviewer gate on the fingerprint ordinal promise (spec §2: baselining one of two identical-shape findings leaves the other reported).

- [ ] **Step 1: Write the test (it should pass immediately if Tasks 1–5 are correct — it exists to catch ordinal regressions and to prove the sibling-group behavior on real analyzer output)**

```rust
/// Two identical-shape findings in one function (spec §2): same
/// (checker, func, tag, message), distinct positions -> distinct
/// ordinal fingerprints; baselining one leaves the other reported.
#[test]
fn identical_siblings_baseline_independently() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let module = tmp.path().join("ordpair");
    std::fs::create_dir_all(&module).unwrap();
    std::fs::write(
        module.join("go.mod"),
        "module example.com/ordpair\n\ngo 1.25.10\n",
    )
    .unwrap();
    // Two derefs of the same possibly-nil callee result on separate
    // lines: identical checker/func/tag/message, different positions.
    std::fs::write(
        module.join("ordpair.go"),
        r#"package ordpair

func mk() *int { return nil }

func twice() int {
	a := *mk()
	b := *mk()
	return a + b
}
"#,
    )
    .unwrap();

    let out = goverify(&["check", "--format", "json", "./..."], &module, cache.path());
    assert_eq!(out.status.code(), Some(1), "{}", String::from_utf8_lossy(&out.stderr));
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("valid --format json");
    let findings = v["findings"].as_array().unwrap();
    assert_eq!(findings.len(), 2, "two sibling findings expected: {v}");
    assert_eq!(findings[0]["message"], findings[1]["message"], "identical shape");
    assert_ne!(
        findings[0]["fingerprint"], findings[1]["fingerprint"],
        "ordinals separate identical siblings"
    );

    // Baseline everything, then remove ONE entry: exactly one resurfaces.
    let w = goverify(&["baseline", "write", "./..."], &module, cache.path());
    assert!(w.status.success());
    let path = module.join(".goverify/baseline.json");
    let mut b = goverify_cli::baseline::parse(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(b.entries.len(), 2);
    b.entries.pop();
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&serde_json::json!({
            "schema_version": 1,
            "entries": [{
                "fingerprint": b.entries[0].fingerprint,
                "checker": b.entries[0].checker,
                "tag": b.entries[0].tag,
                "func": b.entries[0].func,
                "message": b.entries[0].message,
            }],
        }))
        .unwrap(),
    )
    .unwrap();
    let one = goverify(&["check", "--format", "json", "./..."], &module, cache.path());
    assert_eq!(one.status.code(), Some(1));
    let v: serde_json::Value = serde_json::from_slice(&one.stdout).unwrap();
    assert_eq!(v["findings"].as_array().unwrap().len(), 1, "one sibling suppressed: {v}");
    assert_eq!(v["summary"]["suppressed_by_baseline"], 1);
}
```

- [ ] **Step 2: Run it**

Run: `mise x -- cargo test -p goverify-cli --test report_integration identical_siblings`
Expected: PASS. **If the fixture doesn't yield exactly 2 findings** (checker shape drift — e.g. the deref pattern gets requires-lifted or merged), adapt the fixture until it produces two findings with identical `(checker, func, tag, message)` — mirror a deref-of-callee-result pattern from `testdata/corpus/nil` (those are pinned by `nil_corpus` and known to report). Do NOT weaken the distinct-fingerprint assertions.

- [ ] **Step 3: Commit**

```bash
mise run lint
git add crates/goverify-cli/tests/report_integration.rs
git commit --no-gpg-sign -m "phase5b: ordinal fingerprint end-to-end test (identical siblings baseline independently)"
```

---

### Task 7: baseline-parser fuzz target

**Files:**
- Create: `fuzz/fuzz_targets/baseline_parse.rs`
- Modify: `fuzz/Cargo.toml`
- Modify: `mise.toml` (`[tasks.fuzz]`)
- Modify: `.github/workflows/nightly.yml`

**Interfaces:**
- Consumes: `goverify_cli::baseline::parse` (Task 4).
- Produces: the `baseline_parse` fuzz target in the smoke + nightly rotations.

- [ ] **Step 1: Add the fuzz dependency and target**

In `fuzz/Cargo.toml` `[dependencies]` (beside the other workspace crates):

```toml
goverify-cli = { path = "../crates/goverify-cli" }
```

and a new `[[bin]]` block (mirroring `scc_entry`'s):

```toml
[[bin]]
name = "baseline_parse"
path = "fuzz_targets/baseline_parse.rs"
test = false
doc = false
```

(Match the exact attribute set the existing `[[bin]]` blocks use.)

Create `fuzz/fuzz_targets/baseline_parse.rs`:

```rust
//! Parse arbitrary bytes as a baseline file. The baseline is
//! user-editable gate configuration — the parser must reject, never
//! panic (phase-5b spec §4; parent spec §12.4).

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = goverify_cli::baseline::parse(data);
});
```

- [ ] **Step 2: Wire the rotations**

`mise.toml` `[tasks.fuzz]`, append to the run list:

```toml
  "cargo +nightly fuzz run baseline_parse -- -max_total_time=60",
```

`.github/workflows/nightly.yml`, after the `scc_entry` run line add:

```yaml
      - run: cargo +nightly fuzz run baseline_parse -- -max_total_time=900
```

and bump the fuzz job's budget for the sixth 900s target (five were 75 min in a 105-min budget; six are 90):

```yaml
    timeout-minutes: 120
```

Update the budget comment above it (currently "Five 900s fuzz runs are 75 min...") to say six/90.

- [ ] **Step 3: Smoke-run**

Run: `mise run fuzz` (or just the new target: `mise x -- cargo +nightly fuzz run baseline_parse -- -max_total_time=60`)
Expected: 60s, 0 crashes. Record execs/s in the task report. (EDR hazard: the first execution of a freshly built fuzz binary may stall ~50 min on this machine — memory `sentinelone-exec-stall`. If it stalls, park and continue with Task 8; come back.)

- [ ] **Step 4: Lint + commit**

```bash
mise run lint
git add fuzz/Cargo.toml fuzz/fuzz_targets/baseline_parse.rs fuzz/Cargo.lock mise.toml .github/workflows/nightly.yml
git commit --no-gpg-sign -m "phase5b: baseline_parse fuzz target (reject-never-panic) + nightly budget"
```

---

### Task 8: `CallGraph::callers_closure` (goverify-ir)

**Files:**
- Modify: `crates/goverify-ir/src/callgraph.rs`

**Interfaces:**
- Consumes: the existing `CallGraph` (callgraph.rs:13, `callees(&self, f: FuncId) -> &[FuncId]`).
- Produces: `pub fn callers_closure(&self, seeds: &[FuncId]) -> std::collections::HashSet<FuncId>` — every function from which some seed is reachable through call edges, **seeds included**. Task 10 consumes. (If `FuncId` doesn't already derive `Hash`, add it — it's a plain index newtype.)

- [ ] **Step 1: Write the failing test**

In `callgraph.rs`'s test module (mirror however existing tests there construct graphs — `compute_from_graph` tests build small `CallGraph` values in-crate):

```rust
    #[test]
    fn callers_closure_walks_call_edges_backward() {
        // 0 -> 1 -> 2,  3 -> 1,  4 isolated.
        let g = graph(5, &[(0, 1), (1, 2), (3, 1)]); // reuse/build the
        // same in-crate constructor the existing Sccs tests use; if none
        // exists, build the CallGraph struct literal directly (in-crate).
        let closure = g.callers_closure(&[FuncId(2)]);
        let mut got: Vec<u32> = closure.iter().map(|f| f.0).collect();
        got.sort_unstable();
        assert_eq!(got, vec![0, 1, 2, 3], "callers_closure(seeds=[2])");

        let closure = g.callers_closure(&[FuncId(4)]);
        let mut got: Vec<u32> = closure.iter().map(|f| f.0).collect();
        got.sort_unstable();
        assert_eq!(got, vec![4], "isolated seed is its own closure");

        assert!(
            g.callers_closure(&[]).is_empty(),
            "empty seeds -> empty closure"
        );
    }
```

(Adapt `FuncId(2)` construction and the `graph(...)` helper to the file's actual test idioms — the assertions are the contract.)

- [ ] **Step 2: Run to verify failure**

Run: `mise x -- cargo test -p goverify-ir callers_closure`
Expected: FAIL to compile.

- [ ] **Step 3: Implement**

```rust
    /// Reverse reachability (phase-5b spec §5): every function from
    /// which some seed is reachable through call edges — the seeds'
    /// transitive callers, seeds included. `--diff-base` scopes its
    /// report to `callers_closure(changed functions)`: a changed
    /// callee's summary feeds every transitive caller's obligations.
    /// Returned as a set for membership tests only — callers must never
    /// iterate it into output (iteration order is not deterministic).
    pub fn callers_closure(&self, seeds: &[FuncId]) -> std::collections::HashSet<FuncId> {
        // Reverse adjacency over the callee lists.
        let n = self.callees.len(); // adapt to the actual field name
        let mut rev: Vec<Vec<FuncId>> = vec![Vec::new(); n];
        for caller in 0..n {
            let caller = FuncId(caller as u32);
            for &callee in self.callees(caller) {
                rev[callee.0 as usize].push(caller);
            }
        }
        let mut closure: std::collections::HashSet<FuncId> = HashSet::with_capacity(seeds.len());
        let mut stack: Vec<FuncId> = Vec::new();
        for &s in seeds {
            if closure.insert(s) {
                stack.push(s);
            }
        }
        while let Some(v) = stack.pop() {
            for &caller in &rev[v.0 as usize] {
                if closure.insert(caller) {
                    stack.push(caller);
                }
            }
        }
        closure
    }
```

(Use the struct's real field/accessor names; `use std::collections::HashSet;` at top per the file's import style.)

- [ ] **Step 4: Run, lint, commit**

Run: `mise x -- cargo test -p goverify-ir`
Expected: PASS.

```bash
mise run lint
git add crates/goverify-ir/src/callgraph.rs
git commit --no-gpg-sign -m "phase5b: CallGraph::callers_closure (reverse reachability for --diff-base)"
```

---

### Task 9: `Program::func_semantic_hash` (goverify-ir)

**Files:**
- Modify: `crates/goverify-ir/src/program.rs` (beside `ctx_hash`/`func_hash`/`external_hash`, program.rs:58-112, and pass 3 of `from_packages`, program.rs:143-173)

**Interfaces:**
- Consumes: `gvir::Package` / `gvir::Function` / `gvir::Pragma` (positions at `Function.pos` field 7, `Instruction.pos` field 5, `Instruction.detail` field 6, `Pragma.pos` field 3 — gvir.proto).
- Produces: `pub fn func_semantic_hash(&self, id: FuncId) -> [u8; 32]` — position-blind sibling of `func_ir_hash`. Task 10 consumes. **Never a cache-key input.**

- [ ] **Step 1: Write the failing tests**

In `program.rs`'s test module, extend the existing `pkg(line)` fixture style (program.rs:371) — add a variant whose function has one block with one instruction, so both a position mutation and a semantic mutation are expressible:

```rust
    #[test]
    fn semantic_hash_ignores_positions_and_detail_but_not_semantics() {
        use goverify_extract::gvir;
        fn pkg(line: u32, op: &str, detail: &str) -> gvir::Package {
            gvir::Package {
                schema_version: goverify_extract::SCHEMA_VERSION.to_string(),
                go_version: "go1.25.10".to_string(),
                extractor_version: "0.1.0".to_string(),
                import_path: "example.com/h".to_string(),
                files: vec![],
                types: vec![],
                functions: vec![gvir::Function {
                    id: "example.com/h.F".to_string(),
                    name: "F".to_string(),
                    r#type: 0,
                    params: vec![],
                    aux: vec![],
                    blocks: vec![gvir::BasicBlock {
                        index: 0,
                        instrs: vec![gvir::Instruction {
                            kind: "BinOp".to_string(),
                            register: 1,
                            r#type: 0,
                            operands: vec![],
                            pos: Some(gvir::Position { file: 0, line, col: 1 }),
                            detail: detail.to_string(),
                            sem: Some(gvir::instruction::Sem::Binop(gvir::BinOpSem {
                                op: op.to_string(),
                            })),
                        }],
                        succs: vec![],
                        preds: vec![],
                    }],
                    pos: Some(gvir::Position { file: 0, line, col: 1 }),
                }],
                method_sets: vec![],
                pragmas: vec![],
            }
        }
        let base = Program::from_packages(vec![pkg(1, "+", "a + b")]);
        let shifted = Program::from_packages(vec![pkg(50, "+", "a + b")]);
        let redetailed = Program::from_packages(vec![pkg(1, "+", "x + y")]);
        let changed = Program::from_packages(vec![pkg(1, "*", "a + b")]);
        let f = base.lookup_func("example.com/h.F").expect("lookup_func");

        // Position shift: ir hash moves (asserted invariant), semantic
        // hash must NOT (G4: comment-only edits are diff-invisible).
        assert_ne!(base.func_ir_hash(f), shifted.func_ir_hash(f));
        assert_eq!(
            base.func_semantic_hash(f),
            shifted.func_semantic_hash(f),
            "positions must not reach the semantic hash"
        );
        // detail is debug-only prose: also excluded.
        assert_eq!(
            base.func_semantic_hash(f),
            redetailed.func_semantic_hash(f),
            "Instruction.detail must not reach the semantic hash"
        );
        // A real semantic change moves both.
        assert_ne!(
            base.func_semantic_hash(f),
            changed.func_semantic_hash(f),
            "operator change is a semantic change"
        );
    }

    #[test]
    fn semantic_hash_of_externals_pins_identity() {
        // A function only referenced, never declared: name-only hash,
        // equal across programs (externals are havoc; they never count
        // as "changed").
        let p = Program::from_packages(vec![]);
        drop(p); // no interned funcs: nothing to assert here beyond compile —
                 // cover via the existing external path: any interned-not-declared
                 // id must give func_semantic_hash == its value in a second
                 // identical Program.
    }
```

(For the externals property, follow whatever the existing external-hash test does — if none exists, assert via a package whose function has a call-site `aux` reference to an undeclared id, mirroring how existing tests trigger interning; keep the assertion "identical across two identically-built Programs".)

- [ ] **Step 2: Run to verify failure**

Run: `mise x -- cargo test -p goverify-ir semantic_hash`
Expected: FAIL to compile — `func_semantic_hash` not defined.

- [ ] **Step 3: Implement**

Beside `ctx_hash`/`func_hash` in `program.rs`:

```rust
/// Position-blind context hash (phase-5b spec §5): `ctx_hash` with
/// `Pragma.pos` cleared — a comment shift above a pragma must not mark
/// every function in the package as changed for `--diff-base`.
fn semantic_ctx_hash(pkg: &gvir::Package) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(b"goverify-func-semctx\0");
    let mut field = |bytes: &[u8]| {
        h.update(&(bytes.len() as u64).to_le_bytes());
        h.update(bytes);
    };
    field(pkg.schema_version.as_bytes());
    field(pkg.go_version.as_bytes());
    field(pkg.extractor_version.as_bytes());
    field(pkg.import_path.as_bytes());
    for t in &pkg.types {
        field(&t.encode_to_vec());
    }
    for m in &pkg.method_sets {
        field(&m.encode_to_vec());
    }
    for f in &pkg.files {
        field(f.path.as_bytes());
    }
    for pr in &pkg.pragmas {
        let mut pr = pr.clone();
        pr.pos = None;
        field(&pr.encode_to_vec());
    }
    *h.finalize().as_bytes()
}

/// Position-blind sibling of `func_hash` (phase-5b spec §5): the
/// function re-encoded with `Function.pos`, every `Instruction.pos`,
/// and every `Instruction.detail` cleared. `--diff-base` compares these
/// across git refs, so a comment-only edit (positions shift, semantics
/// don't) yields an empty changed set (gate G4). `detail` is debug-only
/// prose (gvir.proto) and is dropped for the same reason.
/// NEVER a cache-key input: cache keys stay position-sensitive
/// (`func_hash`) so warm replays render exact positions.
fn semantic_func_hash(ctx: &[u8; 32], f: &gvir::Function) -> [u8; 32] {
    let mut g = f.clone();
    g.pos = None;
    for b in &mut g.blocks {
        for i in &mut b.instrs {
            i.pos = None;
            i.detail = String::new();
        }
    }
    let mut h = blake3::Hasher::new();
    h.update(b"goverify-func-sem\0");
    h.update(ctx);
    let bytes = g.encode_to_vec();
    h.update(&(bytes.len() as u64).to_le_bytes());
    h.update(&bytes);
    *h.finalize().as_bytes()
}
```

In `from_packages` pass 3, compute the second vector in the same loop with the same bodied-winner rule (program.rs:156-173):

```rust
        p.func_hashes = p.func_names.iter().map(|n| external_hash(n)).collect();
        p.func_sem_hashes = p.func_hashes.clone(); // externals: same name-only hash
        let mut bodied = vec![false; p.func_names.len()];
        for pkg in &pkgs {
            let ctx = ctx_hash(pkg);
            let sem_ctx = semantic_ctx_hash(pkg);
            for f in &pkg.functions {
                if let Some(&id) = p.by_name.get(f.id.as_str()) {
                    let idx = id.0 as usize;
                    if f.blocks.is_empty() {
                        if !bodied[idx] {
                            p.func_hashes[idx] = func_hash(&ctx, f);
                            p.func_sem_hashes[idx] = semantic_func_hash(&sem_ctx, f);
                        }
                    } else {
                        p.func_hashes[idx] = func_hash(&ctx, f);
                        p.func_sem_hashes[idx] = semantic_func_hash(&sem_ctx, f);
                        bodied[idx] = true;
                    }
                }
            }
        }
```

Add the field (`func_sem_hashes: Vec<[u8; 32]>` beside `func_hashes`, program.rs:29) and the accessor beside `func_ir_hash`:

```rust
    /// Position-blind sibling of `func_ir_hash` (phase-5b spec §5) —
    /// `--diff-base`'s changed-function comparator. Not a cache key.
    pub fn func_semantic_hash(&self, id: FuncId) -> [u8; 32] {
        self.func_sem_hashes
            .get(id.0 as usize)
            .copied()
            .unwrap_or([0u8; 32])
    }
```

- [ ] **Step 4: Run, lint, commit**

Run: `mise x -- cargo test -p goverify-ir && mise run corpus`
Expected: PASS — corpus green proves the extra pass-3 work changes no output.

```bash
mise run lint
git add crates/goverify-ir/src/program.rs
git commit --no-gpg-sign -m "phase5b: Program::func_semantic_hash (position-blind, for --diff-base G4)"
```

---

### Task 10: `check --diff-base <git-ref>`

**Files:**
- Create: `crates/goverify-cli/src/diff.rs`
- Modify: `crates/goverify-cli/src/main.rs` (flag, wiring, `run_baseline_write` rejection)
- Modify: `crates/goverify-cli/tests/report_integration.rs`

**Interfaces:**
- Consumes: `analyze_module` / `acquire_program` (Task 4), `CallGraph::callers_closure` (Task 8), `Program::func_semantic_hash` + `lookup_func` + `func_ids` + `func_name` (Task 9 / goverify-ir), `apply_baseline` (Task 5).
- Produces: `--diff-base <ref>` on `check`; `diff::checkout_base(module_dir: &Path, git_ref: &str) -> Result<BaseCheckout, String>` (RAII worktree, `.module_dir` field); `diff::changed_funcs(cur: &Program, base: &Program) -> Vec<FuncId>`.

- [ ] **Step 1: Write the failing integration tests**

Append to `report_integration.rs`:

```rust
fn git_ok(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        // Hermetic identity + no signing, regardless of global config.
        .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
        .args(args)
        .output()
        .expect("spawn git");
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

/// Two independent possibly-nil derefs: `hotCaller`'s finding depends
/// (transitively) on `mk`; `coldCaller`'s on `mk2` only.
fn write_diff_fixture(dir: &Path) {
    std::fs::write(dir.join("go.mod"), "module example.com/diffbase\n\ngo 1.25.10\n").unwrap();
    std::fs::write(
        dir.join("main.go"),
        r#"package diffbase

func mk() *int { return nil }

func mk2() *int { return nil }

func hotCaller() int { return *mk() }

func coldCaller() int { return *mk2() }
"#,
    )
    .unwrap();
}

#[test]
fn diff_base_reports_changed_functions_and_transitive_callers_only() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let module = tmp.path().join("diffbase");
    std::fs::create_dir_all(&module).unwrap();
    write_diff_fixture(&module);
    git_ok(&module, &["init", "-q"]);
    git_ok(&module, &["add", "-A"]);
    git_ok(&module, &["commit", "-q", "-m", "base"]);

    // Sanity: unfiltered check reports findings in BOTH callers.
    let full = goverify(&["check", "--format", "json", "./..."], &module, cache.path());
    assert_eq!(full.status.code(), Some(1), "{}", String::from_utf8_lossy(&full.stderr));
    let v: serde_json::Value = serde_json::from_slice(&full.stdout).unwrap();
    let funcs = |v: &serde_json::Value| -> Vec<String> {
        v["findings"].as_array().unwrap().iter()
            .map(|f| f["func"].as_str().unwrap().to_string()).collect()
    };
    let all = funcs(&v);
    assert!(all.iter().any(|f| f.contains("hotCaller")), "{all:?}");
    assert!(all.iter().any(|f| f.contains("coldCaller")), "{all:?}");

    // Semantic edit to mk only (body changes; mk2 and callers untouched).
    let src = std::fs::read_to_string(module.join("main.go")).unwrap();
    let edited = src.replace(
        "func mk() *int { return nil }",
        "func mk() *int { x := 1; _ = x; return nil }",
    );
    assert_ne!(src, edited, "edit applied");
    std::fs::write(module.join("main.go"), edited).unwrap();

    let out = goverify(
        &["check", "--diff-base", "HEAD", "--format", "json", "./..."],
        &module,
        cache.path(),
    );
    assert_eq!(out.status.code(), Some(1), "{}", String::from_utf8_lossy(&out.stderr));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let got = funcs(&v);
    assert!(got.iter().all(|f| !f.contains("coldCaller")),
        "coldCaller is outside the changed closure: {got:?}");
    assert!(got.iter().any(|f| f.contains("hotCaller")),
        "hotCaller transitively calls changed mk: {got:?}");
    assert_eq!(v["summary"]["diff_base_scoped"], true);
}

#[test]
fn diff_base_comment_only_edit_reports_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let module = tmp.path().join("diffbase");
    std::fs::create_dir_all(&module).unwrap();
    write_diff_fixture(&module);
    git_ok(&module, &["init", "-q"]);
    git_ok(&module, &["add", "-A"]);
    git_ok(&module, &["commit", "-q", "-m", "base"]);

    // Comment at the top: every position shifts, no semantics change (G4).
    let src = std::fs::read_to_string(module.join("main.go")).unwrap();
    std::fs::write(module.join("main.go"), format!("// a comment\n{src}")).unwrap();

    let out = goverify(
        &["check", "--diff-base", "HEAD", "--format", "json", "./..."],
        &module,
        cache.path(),
    );
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["summary"]["total"], 0, "comment-only edit -> empty report: {v}");
}

#[test]
fn diff_base_unknown_ref_is_exit_2_and_leaves_no_worktree() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let module = tmp.path().join("diffbase");
    std::fs::create_dir_all(&module).unwrap();
    write_diff_fixture(&module);
    git_ok(&module, &["init", "-q"]);
    git_ok(&module, &["add", "-A"]);
    git_ok(&module, &["commit", "-q", "-m", "base"]);

    let out = goverify(
        &["check", "--diff-base", "no-such-ref", "./..."],
        &module,
        cache.path(),
    );
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("no-such-ref"), "names the ref: {stderr}");

    // Success path leaves no worktree behind either.
    let ok = goverify(
        &["check", "--diff-base", "HEAD", "./..."],
        &module,
        cache.path(),
    );
    assert!(ok.status.code() == Some(0) || ok.status.code() == Some(1));
    let wt = Command::new("git")
        .arg("-C").arg(&module)
        .args(["worktree", "list", "--porcelain"])
        .output().unwrap();
    let listing = String::from_utf8_lossy(&wt.stdout);
    assert_eq!(listing.matches("worktree ").count(), 1,
        "only the main worktree remains: {listing}");
}
```

- [ ] **Step 2: Run to verify failure**

Run: `mise x -- cargo test -p goverify-cli --test report_integration diff_base`
Expected: FAIL — unknown `--diff-base` flag.

- [ ] **Step 3: Implement `diff.rs`**

```rust
//! `--diff-base` (phase-5b spec §5): git-worktree the base ref, extract
//! it (the extraction cache applies — the base tree's packages are
//! content-keyed like any other), compare position-blind function
//! hashes, and scope the report to changed functions plus their
//! transitive callers. All git access shells out to the `git` CLI
//! (dependency policy, spec §1: git is required only when the flag is
//! used). Failures here are hard errors — a silently-wrong report set
//! contradicts the explicit request (spec §5).

use std::path::{Path, PathBuf};
use std::process::Command;

use goverify_ir::{FuncId, Program};

pub struct BaseCheckout {
    /// The directory inside the worktree corresponding to the checked
    /// module (worktree root + the module's path prefix in the repo).
    pub module_dir: PathBuf,
    repo_dir: PathBuf,
    worktree: PathBuf,
    _tmp: tempfile::TempDir,
}

impl Drop for BaseCheckout {
    fn drop(&mut self) {
        // Best-effort cleanup on every exit path (spec §5): deregister
        // the worktree; the TempDir removes the files themselves.
        let _ = Command::new("git")
            .arg("-C")
            .arg(&self.repo_dir)
            .arg("worktree")
            .arg("remove")
            .arg("--force")
            .arg(&self.worktree)
            .output();
        let _ = Command::new("git")
            .arg("-C")
            .arg(&self.repo_dir)
            .args(["worktree", "prune"])
            .output();
    }
}

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| format!("cannot run git: {e} (--diff-base requires git on PATH)"))?;
    if !out.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Check `git_ref` out into a temp worktree of the repo containing
/// `module_dir`.
pub fn checkout_base(module_dir: &Path, git_ref: &str) -> Result<BaseCheckout, String> {
    git(
        module_dir,
        &["rev-parse", "--verify", "--quiet", &format!("{git_ref}^{{commit}}")],
    )
    .map_err(|_| format!("--diff-base: unknown git ref {git_ref:?}"))?;
    // The module may sit below the repo root; mirror that inside the
    // worktree.
    let prefix = git(module_dir, &["rev-parse", "--show-prefix"])?;
    let repo_dir = PathBuf::from(git(module_dir, &["rev-parse", "--show-toplevel"])?);
    let tmp = tempfile::tempdir().map_err(|e| format!("--diff-base: tempdir: {e}"))?;
    let worktree = tmp.path().join("base");
    let worktree_str = worktree
        .to_str()
        .ok_or_else(|| "--diff-base: non-UTF-8 temp path".to_string())?;
    git(
        &repo_dir,
        &["worktree", "add", "--detach", worktree_str, git_ref],
    )?;
    let module_dir = worktree.join(&prefix);
    Ok(BaseCheckout {
        module_dir,
        repo_dir,
        worktree,
        _tmp: tmp,
    })
}

/// Changed = present now with a different position-blind hash, or
/// absent at the base (new function). Functions deleted at HEAD have no
/// current findings — nothing to report (spec §5). Externals hash by
/// name in both programs and so never count as changed.
pub fn changed_funcs(cur: &Program, base: &Program) -> Vec<FuncId> {
    cur.func_ids()
        .filter(|&f| match base.lookup_func(cur.func_name(f)) {
            None => true,
            Some(b) => base.func_semantic_hash(b) != cur.func_semantic_hash(f),
        })
        .collect()
}
```

- [ ] **Step 4: Wire into `run_check`**

CheckArgs:

```rust
    /// Report only findings in functions changed since this git ref, or
    /// in their transitive callers (spec §10). Analysis still covers
    /// everything; only the report is scoped. Requires git.
    #[arg(long, value_name = "GIT_REF")]
    diff_base: Option<String>,
```

`mod diff;` beside the other modules. In `run_check`, after `let a = analyze_module(&ca)?;` and before the fingerprint computation:

```rust
    // Filter order (spec §5): scope (already applied) -> diff-base ->
    // fingerprints -> baseline. Fingerprint ordinals are stable across
    // this: diff-base filters whole functions, so identical-sibling
    // groups never split (spec §2).
    let (scoped, diff_base_scoped) = match &ca.diff_base {
        None => (a.scoped, false),
        Some(git_ref) => {
            let base = diff::checkout_base(Path::new("."), git_ref)?;
            let base_prog = acquire_program(
                &base.module_dir,
                None,
                &ca.patterns,
                a.cache_root.as_ref(),
                a.timings,
            )
            .map_err(|e| format!("--diff-base: extracting {git_ref:?}: {e}"))?;
            let changed = diff::changed_funcs(&a.program, &base_prog);
            let g = goverify_ir::CallGraph::build(&a.program);
            let keep = g.callers_closure(&changed);
            let kept: Vec<goverify_analysis::Finding> = a
                .scoped
                .into_iter()
                .filter(|f| {
                    // A finding whose function isn't in the current
                    // program is kept (conservative: never hide by
                    // accident).
                    a.program
                        .lookup_func(&f.func)
                        .is_none_or(|id| keep.contains(&id))
                })
                .collect();
            (kept, true)
            // `base` drops here: worktree removed on success and (via
            // Drop) on every error path above.
        }
    };
```

Set `diff_base_scoped` into the `Summary`, and pass the post-diff `scoped` through the existing fingerprint → baseline → dispatch chain. In `run_baseline_write`, extend the rejection:

```rust
    if ca.baseline.is_some() || ca.no_baseline || ca.diff_base.is_some() {
        return Err("baseline write records the full finding set; \
                    --baseline/--no-baseline/--diff-base do not apply"
            .into());
    }
```

(If the borrow of `a.program` vs. moving `a.scoped` fights the borrow checker, destructure `Analyzed` first: `let Analyzed { program, scoped, cache_root, timings } = analyze_module(&ca)?;`.)

- [ ] **Step 5: Run to verify pass**

Run: `mise x -- cargo test -p goverify-cli --test report_integration`
Expected: PASS (all three diff-base tests + all baseline tests). The changed-closure test's sanity half must show findings in BOTH callers before the diff assertions run — if the fixture yields a different finding shape, fix the fixture (nil-corpus deref patterns), not the assertions.

- [ ] **Step 6: Full suite, lint, commit**

Run: `mise x -- cargo test -p goverify-cli && mise run corpus`
Expected: PASS.

```bash
mise run lint
git add crates/goverify-cli/src/diff.rs crates/goverify-cli/src/main.rs crates/goverify-cli/tests/report_integration.rs
git commit --no-gpg-sign -m "phase5b: check --diff-base (worktree extract + semantic-hash changed set + callers closure)"
```

---

### Task 11: machine-format determinism suite + goldens

**Files:**
- Create: `crates/goverify-cli/tests/formats_corpus.rs`
- Create: `crates/goverify-cli/tests/goldens/hello_check.json`
- Create: `crates/goverify-cli/tests/goldens/hello_check.sarif`
- Modify: `mise.toml` (`[tasks.corpus]`)

**Interfaces:**
- Consumes: `--format json|sarif` (Tasks 2–3), `baseline write` (Task 4).
- Produces: the corpus determinism suite covers the new machine formats (spec §6).

- [ ] **Step 1: Write the tests**

`crates/goverify-cli/tests/formats_corpus.rs`:

```rust
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
        let want = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("golden {}: {e}", path.display()));
        assert_eq!(
            got,
            want,
            "--format {format} drifted from {golden}; if the change is \
             intentional, regenerate the golden and bump the schema version \
             it pins"
        );
    }
}
```

- [ ] **Step 2: Generate the goldens (once, then they're pinned)**

```bash
mise x -- cargo build -p goverify-cli
export GOVERIFY_EXTRACTOR_DIR="$(pwd)/extractor"
cd testdata/corpus/hello
XDG_CACHE_HOME=$(mktemp -d) ../../../target/debug/goverify check --format json ./... \
  > ../../../crates/goverify-cli/tests/goldens/hello_check.json
XDG_CACHE_HOME=$(mktemp -d) ../../../target/debug/goverify check --format sarif ./... \
  > ../../../crates/goverify-cli/tests/goldens/hello_check.sarif
cd ../../..
```

Inspect both files: `findings: []`, `summary.total: 0`, SARIF `results: []` — and **no absolute paths anywhere** (grep for `/Users`).

- [ ] **Step 3: Run the suite**

Run: `mise x -- cargo test -p goverify-cli --test formats_corpus`
Expected: PASS (2 tests; the byte-identity one runs 12 checks — it reuses the sidecar build via the shared cache, but is still the slowest new test).

- [ ] **Step 4: Add to the corpus task**

In `mise.toml` `[tasks.corpus]`, append to the run list (after the goverify-checkers line):

```toml
  "cargo test -p goverify-cli --test formats_corpus",
```

Run: `mise run corpus`
Expected: green end-to-end.

- [ ] **Step 5: Lint + commit**

```bash
mise run lint
git add crates/goverify-cli/tests/formats_corpus.rs crates/goverify-cli/tests/goldens/ mise.toml
git commit --no-gpg-sign -m "phase5b: machine-format determinism suite + hello goldens in corpus gate"
```

---

### Task 12: shakeout gates G1–G5, addendum, README

**Files:**
- Create: `docs/shakeout-phase5b-ci-surface.md`
- Modify: `README.md` (lines 12-14 phase note + a usage subsection)
- Report: `.superpowers/sdd/task-12-report-phase5b.md` (gitignored ledger)

**Interfaces:**
- Consumes: everything. This is the wave's acceptance gate (spec §7) against the 457-finding bbolt baseline.

- [ ] **Step 1: Build both binaries (wave tip + wave base)**

```bash
mise x -- cargo build --release -p goverify-cli
BASE=$(git merge-base HEAD main)   # = the commit this wave branched from (66dc5e2)
git worktree add /tmp/goverify-base "$BASE"
(cd /tmp/goverify-base && mise x -- cargo build --release -p goverify-cli)
```

(EDR note: first exec of each fresh binary may stall — run a `--help` warm-up of both, park if stalled.)

- [ ] **Step 2: G1 — human path unchanged (byte-identical 457)**

```bash
export GOVERIFY_EXTRACTOR_DIR="$(pwd)/extractor"
mise run shakeout   # ensure bbolt clone + warm cache exist
cd .goverify/shakeout/bbolt
CACHE="$(pwd)/../cache"
GOVERIFY_EXTRACTOR_DIR="$(git -C ../../.. rev-parse --show-toplevel)/extractor" \
  ../../../target/release/goverify check ./... --cache-dir "$CACHE" > /tmp/g1-new.txt; echo "exit=$?"
GOVERIFY_EXTRACTOR_DIR="/tmp/goverify-base/extractor" \
  /tmp/goverify-base/target/release/goverify check ./... --cache-dir "$CACHE" > /tmp/g1-old.txt; echo "exit=$?"
cmp /tmp/g1-old.txt /tmp/g1-new.txt && echo "G1 PASS"
grep -cE '^[^ ].*: (nil-deref|bounds|div-zero|overflow): ' /tmp/g1-new.txt   # expect 457
```

Expected: `cmp` silent (identical), 457 finding headers, both exits 1. (Count via finding-header grep, NEVER `wc -l` — memory.)

- [ ] **Step 3: G2 — machine-format determinism at bbolt scale**

```bash
FRESH=$(mktemp -d)
../../../target/release/goverify check --format sarif ./... --cache-dir "$FRESH" > /tmp/g2-sarif-cold.json
../../../target/release/goverify check --format sarif ./... --cache-dir "$FRESH" > /tmp/g2-sarif-warm.json
../../../target/release/goverify check --format json ./... --cache-dir "$FRESH" > /tmp/g2-json-1.json
../../../target/release/goverify check --format json ./... --cache-dir "$FRESH" > /tmp/g2-json-2.json
cmp /tmp/g2-sarif-cold.json /tmp/g2-sarif-warm.json && cmp /tmp/g2-json-1.json /tmp/g2-json-2.json && echo "G2 PASS"
grep -c '"ruleId"' /tmp/g2-sarif-cold.json   # expect 457
grep -F '/Users' /tmp/g2-sarif-cold.json && echo "G2 FAIL: absolute path" || true
```

Expected: both `cmp`s silent, 457 results, no absolute paths.

- [ ] **Step 4: G3 — baseline exactness**

```bash
../../../target/release/goverify baseline write ./... --cache-dir "$CACHE"
../../../target/release/goverify check ./... --cache-dir "$CACHE" > /tmp/g3-clean.txt; echo "exit=$?"   # expect 0
grep "457 finding(s) suppressed" /tmp/g3-clean.txt && echo "G3a PASS"
# Remove one entry (jq available via mise/nix; otherwise a 5-line python)
python3 - <<'EOF'
import json
p = ".goverify/baseline.json"
b = json.load(open(p))
removed = b["entries"].pop(0)
json.dump(b, open(p, "w"), indent=2)
print("removed:", removed["fingerprint"], removed["func"])
EOF
../../../target/release/goverify check ./... --cache-dir "$CACHE" > /tmp/g3-one.txt; echo "exit=$?"     # expect 1
grep -cE ': (nil-deref|bounds|div-zero|overflow): ' /tmp/g3-one.txt   # expect exactly 1
```

Expected: clean run exit 0 with "457 finding(s) suppressed"; after removing one entry, exit 1 with exactly 1 finding, matching the removed entry's func. Delete `.goverify/baseline.json` afterwards (bbolt clone stays pristine).

- [ ] **Step 5: G4 — diff-base semantics on bbolt**

The exactness half is already gated by `report_integration.rs` (changed-closure + comment-only tests). At bbolt scale, smoke both directions (bbolt is a git clone):

```bash
# Comment-only edit -> empty report set.
sed -i.bak '1i\
// goverify shakeout probe comment
' tx.go
../../../target/release/goverify check --diff-base HEAD ./... --cache-dir "$CACHE" > /tmp/g4-comment.txt; echo "exit=$?"  # expect 0
grep -cE ': (nil-deref|bounds|div-zero|overflow): ' /tmp/g4-comment.txt   # expect 0
mv tx.go.bak tx.go
# Semantic edit -> nonempty strict subset.
# (append a no-op statement inside a bbolt function body, run again, expect
# exit 1 with a count > 0 and < 457, all in the edited func's caller closure;
# then git checkout -- the file.)
git checkout -- .
```

Expected: comment edit reports 0 (G4 PASS on the spec's hash-comparison rationale); semantic edit reports a nonempty strict subset of 457.

- [ ] **Step 6: G5 — timing (report-only)**

```bash
GOVERIFY_TIMINGS=1 /usr/bin/time ../../../target/release/goverify check --diff-base HEAD ./... --cache-dir "$CACHE" > /dev/null
```

Record wall time and the phase breakdown vs. a plain warm `check` (expected delta ≈ one warm extraction pass of the base tree + hash comparison). Report-only, no threshold.

- [ ] **Step 7: Write the addendum + README, commit**

`docs/shakeout-phase5b-ci-surface.md`: gate verdicts G1–G5 with the exact numbers and commands above, the baseline/diff-base usage story, and any deviations. Update `README.md`: rewrite the "later phases" sentence (lines 12-14) to reflect that SARIF/JSON, baselines, and diff-base shipped; add a short usage block:

```markdown
### CI usage

    goverify check --format sarif ./...      # GitHub code scanning
    goverify baseline write ./...            # adopt on an existing codebase
    goverify check ./...                     # …now reports only new findings
    goverify check --diff-base origin/main ./...   # PR-scoped report
```

```bash
git worktree remove --force /tmp/goverify-base
mise run lint && mise run test && mise run corpus
git add docs/shakeout-phase5b-ci-surface.md README.md
git commit --no-gpg-sign -m "phase5b: shakeout addendum (gates G1-G5) + README CI-usage docs"
```

---

## Self-Review Notes (resolved during planning)

- **Spec coverage:** §2 fingerprints → Task 1; §3 formats → Tasks 2–3; §4 baselines → Tasks 4–6 (+ fuzz Task 7); §5 diff-base → Tasks 8–10; §6 testing → Tasks 6, 7, 10, 11; §7 gates → Task 12; §8 non-goals untouched.
- **`func_ir_hash` is position-sensitive by asserted invariant** (program.rs test: "a position change must change the hash") — the spec's §5 "same function-IR hashes the scc cache computes" would fail gate G4. Task 9's `func_semantic_hash` is the plan-level correction; the spec's G4 intent (comment-only edit ⇒ empty report) wins over its mechanism sentence. Surface as a spec deviation in the final summary.
- **Fuzz reachability** forced the goverify-cli lib target (fuzz targets can only depend on lib crates) — Task 1.
- **Type-consistency check:** `fingerprints`/`fingerprint` (Tasks 1→2,4,5), `Summary` fields (2→5,10), `baseline::{render,parse}` (4→5,6,7), `Analyzed`/`analyze_module`/`acquire_program` (4→5,10), `callers_closure` returning `HashSet<FuncId>` (8→10), `func_semantic_hash` (9→10) — names match across tasks.
