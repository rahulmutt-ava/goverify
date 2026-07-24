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
        assert_eq!(
            fps[1],
            fingerprint(&fs[1], 1),
            "second sibling is ordinal 1"
        );
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
