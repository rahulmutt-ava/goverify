// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Human terminal rendering of `Finding`s (phase-4 spec §5, `check`
//! subcommand, Task 11): span + caret snippet, violation path, model
//! bindings, fired-clause message. Source-derived text (the snippet echo
//! and model values) is untrusted-ish display input (threat-model): it
//! passes through `sanitize` before ever reaching the terminal, and is
//! never parsed for anything — verdicts are already final by the time a
//! `Finding` reaches this module.

use std::path::Path;

use goverify_analysis::Finding;

/// Fixed gutter width for the line-number column (the source-echo
/// snippet and its caret line share it so the `|` separators line up).
const GUTTER_WIDTH: usize = 5;

/// Human terminal rendering (spec §5): span, caret, trace, model
/// values, fired-clause message. `source_root` resolves Pos.file
/// (relative paths from the extractor); unreadable/missing source
/// degrades to the header line without a snippet.
pub fn render_findings(findings: &[Finding], source_root: &Path) -> String {
    let blocks: Vec<String> = findings
        .iter()
        .map(|f| render_one(f, source_root))
        .collect();
    let mut out = blocks.join("\n\n");
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

/// One finding's block of lines (no trailing blank line — `render_findings`
/// owns the between-findings separator).
fn render_one(f: &Finding, source_root: &Path) -> String {
    let mut lines: Vec<String> = Vec::new();

    let pos_str = match &f.pos {
        Some(p) => format!("{}:{}:{}", p.file, p.line, p.col),
        None => "-:-:-".to_string(),
    };
    lines.push(format!("{pos_str}: {}: {} [{}]", f.tag, f.message, f.func));

    if let Some(p) = &f.pos
        && let Some(src) = read_source_line(source_root, &p.file, p.line)
    {
        lines.push(format!("{:>GUTTER_WIDTH$} | {}", p.line, sanitize(&src)));
        let col0 = (p.col.max(1) - 1) as usize;
        lines.push(format!("{:>GUTTER_WIDTH$} | {}^", "", " ".repeat(col0)));
    }

    let path_parts: Vec<String> = f
        .trace
        .iter()
        .filter_map(|step| step.pos.as_ref())
        .map(|p| format!("{}:{}", p.file, p.line))
        .collect();
    if !path_parts.is_empty() {
        lines.push(format!("    path: {}", path_parts.join(" -> ")));
    }

    if !f.model.is_empty() {
        let bindings: Vec<String> = f
            .model
            .iter()
            .map(|(k, v)| format!("{k} = {}", sanitize(v)))
            .collect();
        lines.push(format!("    with: {}", bindings.join(", ")));
    }

    lines.join("\n")
}

/// `line` is 1-based (matches `Pos.line`); `None` on any I/O failure or
/// an out-of-range line (degrade to the header-only rendering, never a
/// panic — the source tree a `check` run reads is the same semi-trusted
/// input the extractor already tolerates).
fn read_source_line(root: &Path, file: &str, line: u32) -> Option<String> {
    let idx = line.checked_sub(1)? as usize;
    let text = std::fs::read_to_string(root.join(file)).ok()?;
    text.lines().nth(idx).map(str::to_string)
}

/// Strip ANSI/control chars from solver-derived text before terminal
/// output (threat-model: model text is untrusted-ish display input).
/// Every C0 control (`< 0x20`) plus DEL (`0x7f`) is dropped — no
/// exceptions (traces/bindings are always single-line, so there's no
/// legitimate tab/newline to preserve).
fn sanitize(s: &str) -> String {
    s.chars()
        .filter(|&c| !((c as u32) < 0x20 || c as u32 == 0x7f))
        .collect()
}

#[cfg(test)]
mod tests {
    use goverify_analysis::TraceStep;
    use goverify_ir::Pos;

    use super::*;

    fn pos(file: &str, line: u32, col: u32) -> Pos {
        Pos {
            file: file.to_string(),
            line,
            col,
        }
    }

    fn base_finding() -> Finding {
        Finding {
            checker: "nil".to_string(),
            tag: "nil-deref".to_string(),
            func: "t.Bad".to_string(),
            pos: Some(pos("m.go", 3, 9)),
            message: "nil passed to t.F (violates its nil-deref requirement)".to_string(),
            trace: Vec::new(),
            model: Vec::new(),
        }
    }

    #[test]
    fn renders_span_caret_trace_and_model() {
        // finding at file "m.go" line 3 col 9, source written to a temp
        // root; trace [block0@line2, block1@line3]; model [("p0","(ptr-nil)")].
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("m.go"),
            "package m\nfunc Bad() int {\n    return deref(nil)\n}\n",
        )
        .unwrap();
        let mut f = base_finding();
        f.trace = vec![
            TraceStep {
                block: 0,
                pos: Some(pos("m.go", 2, 1)),
            },
            TraceStep {
                block: 1,
                pos: Some(pos("m.go", 3, 9)),
            },
        ];
        f.model = vec![("p0".to_string(), "(ptr-nil)".to_string())];

        let got = render_findings(&[f], dir.path());
        let want = "\
m.go:3:9: nil-deref: nil passed to t.F (violates its nil-deref requirement) [t.Bad]
    3 |     return deref(nil)
      |         ^
    path: m.go:2 -> m.go:3
    with: p0 = (ptr-nil)
";
        assert_eq!(got, want, "renderer output must match the frozen format");
    }

    #[test]
    fn missing_source_degrades_to_header() {
        // no snippet lines, still the header
        let dir = tempfile::tempdir().unwrap(); // no m.go written
        let f = base_finding();
        let got = render_findings(&[f], dir.path());
        let want =
            "m.go:3:9: nil-deref: nil passed to t.F (violates its nil-deref requirement) [t.Bad]\n";
        assert_eq!(got, want, "missing source must degrade to the header line");
    }

    #[test]
    fn sanitize_strips_control_sequences() {
        assert_eq!(sanitize("a\x1b[31mred\x07b"), "a[31mredb");
        // keep \t? no: replace every C0 control except nothing — traces
        // are single-line; strip chars < 0x20 plus 0x7f.
        assert_eq!(sanitize("a\tb\nc\x7fd"), "abcd");
    }

    #[test]
    fn findings_render_in_order_with_blank_line_between() {
        let dir = tempfile::tempdir().unwrap();
        let mut a = base_finding();
        a.func = "t.A".to_string();
        let mut b = base_finding();
        b.func = "t.B".to_string();
        let got = render_findings(&[a, b], dir.path());
        let want = "\
m.go:3:9: nil-deref: nil passed to t.F (violates its nil-deref requirement) [t.A]

m.go:3:9: nil-deref: nil passed to t.F (violates its nil-deref requirement) [t.B]
";
        assert_eq!(
            got, want,
            "findings render in order with one blank line between"
        );
    }
}
