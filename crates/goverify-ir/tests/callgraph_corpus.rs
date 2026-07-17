//! Whole-DAG call graph over the real corpus (phase-2 spec §4.1): static
//! calls, invoke-mode dispatch through method sets, and go/closure edges.

use goverify_ir::{CallGraph, testutil};

#[test]
fn invoke_call_resolves_to_concrete_impl() {
    let p = testutil::load_corpus("conc");
    let g = CallGraph::build(&p);
    let call_all = p.lookup_func("example.com/conc.CloseAll").unwrap();
    let close = p.lookup_func("(*example.com/conc.File).Close").unwrap();
    assert!(
        g.callees(call_all).contains(&close),
        "CloseAll must edge to (*File).Close via implements-approximation; got {:?}",
        g.callees(call_all)
            .iter()
            .map(|&f| p.func_name(f))
            .collect::<Vec<_>>()
    );
}

#[test]
fn go_closure_edges_to_the_closure_body() {
    let p = testutil::load_corpus("conc");
    let g = CallGraph::build(&p);
    let fan = p.lookup_func("example.com/conc.Fan").unwrap();
    let anon = g
        .callees(fan)
        .iter()
        .any(|&f| p.func_name(f).starts_with("example.com/conc.Fan$"));
    assert!(anon, "Fan must edge to its goroutine closure Fan$1");
}
