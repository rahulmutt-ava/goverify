//! Lowering totality over the real corpus: extract conc (whole DAG,
//! sync + runtime deps included), lower everything, count havocs.

use goverify_ir::{Program, testutil};

#[test]
fn lowers_conc_corpus_with_full_dag() {
    let p: Program = testutil::load_corpus("conc");
    let close = p
        .lookup_func("(*example.com/conc.File).Close")
        .expect("Close lowered");
    assert!(p.func(close).is_some(), "Close must have a body");
    // Every function lowered; havoc diagnostics are allowed but bounded.
    let havoc_diags = p
        .diagnostics()
        .iter()
        .filter(|d| d.contains("unmodeled"))
        .count();
    assert!(
        havoc_diags < 200,
        "unexpected havoc explosion: {havoc_diags}"
    );
}

#[test]
fn preds_survive_lowering_and_are_in_range() {
    let p = goverify_ir::testutil::load_corpus("ops");
    let mut saw_preds = false;
    for f in p.func_ids() {
        let Some(func) = p.func(f) else { continue };
        for b in &func.blocks {
            if !b.preds.is_empty() {
                saw_preds = true;
            }
            for &pr in &b.preds {
                assert!(
                    (pr as usize) < func.blocks.len(),
                    "{}: pred {pr} out of range",
                    p.func_name(f)
                );
            }
        }
    }
    assert!(saw_preds, "ops corpus branches must yield preds");
}

#[test]
fn hello_pragma_attached_and_names_plumbed() {
    let p = goverify_ir::testutil::load_corpus("hello");
    let f = p.lookup_func("example.com/hello.Deref").expect("Deref");
    let prs = p.pragmas(f);
    assert_eq!(prs.len(), 1, "Deref pragma count");
    assert_eq!(prs[0].text, "//goverify:requires p != nil");
    assert!(prs[0].pos.is_some(), "pragma carries a position");
    let func = p.func(f).expect("body");
    assert_eq!(func.param_names, vec!["p".to_string()]);
    assert_eq!(func.result_names, vec![String::new()]);
    assert!(p.unmatched_pragmas().is_empty());
}

// Phase-6 Task 13: pins `func_semantic_hash` (the `--diff-base`
// changed-function comparator) against real pragma-bearing source
// extracted through the sidecar, alongside `func_ir_hash` (the
// position-sensitive SCC-cache key). Three tempdir variants of the same
// tiny module, all extracted in this ONE test so the already-built
// sidecar binary (see `testutil::load_module`, cached by content hash)
// is reused rather than risking three separate freshly-built-binary
// execs across test functions (EDR: new-file execs can stall).
#[test]
fn pragma_edit_rotates_semantic_hash_position_shift_does_not() {
    const GO_MOD: &str = "module example.com/hello\n\ngo 1.25\n";
    // Variant A: unchanged (mirrors testdata/corpus/hello/hello.go's
    // Deref pragma exactly).
    const SRC_A: &str =
        "package hello\n\n//goverify:requires p != nil\nfunc Deref(p *int) int { return *p }\n";
    // Variant B: a blank line inserted ABOVE the pragma — Deref's (and
    // the pragma's) line number shifts by one; the pragma TEXT is
    // byte-identical to A.
    const SRC_B: &str =
        "package hello\n\n\n//goverify:requires p != nil\nfunc Deref(p *int) int { return *p }\n";
    // Variant C: the pragma's TEXT edited (`&& true` is a no-op conjunct
    // — still parses as a `requires` expression), no line added, so
    // Deref sits at the exact same position as A.
    const SRC_C: &str = "package hello\n\n//goverify:requires p != nil && true\nfunc Deref(p *int) int { return *p }\n";

    fn write_variant(src: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("go.mod"), GO_MOD).expect("write go.mod");
        std::fs::write(dir.path().join("hello.go"), src).expect("write hello.go");
        dir
    }

    let dir_a = write_variant(SRC_A);
    let dir_b = write_variant(SRC_B);
    let dir_c = write_variant(SRC_C);

    let a = goverify_ir::testutil::load_module(dir_a.path());
    let b = goverify_ir::testutil::load_module(dir_b.path());
    let c = goverify_ir::testutil::load_module(dir_c.path());

    let fa = a.lookup_func("example.com/hello.Deref").expect("A: Deref");
    let fb = b.lookup_func("example.com/hello.Deref").expect("B: Deref");
    let fc = c.lookup_func("example.com/hello.Deref").expect("C: Deref");

    // Cache-key sensitivity: the blank-line shift must still rotate the
    // position-sensitive `func_ir_hash` (both Deref's own position and
    // the pragma's `pos` inside `ctx_hash` moved).
    assert_ne!(
        a.func_ir_hash(fa),
        b.func_ir_hash(fb),
        "func_ir_hash must be position-sensitive: a comment/blank-line \
         shift above the pragma has to rotate the SCC-cache key"
    );

    // `--diff-base` invariant: a position-only shift is diff-invisible.
    assert_eq!(
        a.func_semantic_hash(fa),
        b.func_semantic_hash(fb),
        "func_semantic_hash must ignore a blank-line shift above the \
         pragma (G4: comment/position-only edits are diff-invisible)"
    );
    // ...but an actual pragma-text edit is a real annotation change and
    // must mark the function changed for `--diff-base`.
    assert_ne!(
        a.func_semantic_hash(fa),
        c.func_semantic_hash(fc),
        "func_semantic_hash must change when the pragma's TEXT is edited"
    );
}
