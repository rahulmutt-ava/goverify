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
