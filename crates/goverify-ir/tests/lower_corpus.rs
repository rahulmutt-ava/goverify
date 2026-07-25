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
