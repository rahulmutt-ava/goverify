//! Whole-DAG call graph over the real corpus (phase-2 spec §4.1): static
//! calls, invoke-mode dispatch through method sets, and go/closure edges.

use goverify_ir::{CallGraph, Sccs, testutil};

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

/// Stress test for Task 10's iterative Tarjan on the real whole-DAG corpus
/// (Task 9's review noted the conc corpus has a dense call graph — ~190
/// edges off one closure — and stdlib call chains run deep). Verifies it
/// doesn't panic or overflow the stack, and checks the callees-first
/// invariant holds over every real edge: for a caller/callee pair in
/// different SCCs, the callee's SCC must appear strictly before the
/// caller's SCC in `schedule()`.
#[test]
fn sccs_schedule_is_callees_first_over_the_real_corpus() {
    let p = testutil::load_corpus("conc");
    let g = CallGraph::build(&p);
    let sccs = Sccs::compute(&p, &g);
    assert!(
        !sccs.schedule().is_empty(),
        "the whole-DAG corpus must lower at least one function"
    );
    for f in p.func_ids() {
        let caller_scc = sccs.scc_of(f);
        for &c in g.callees(f) {
            let callee_scc = sccs.scc_of(c);
            assert!(
                callee_scc <= caller_scc,
                "{} (scc {caller_scc}) calls {} (scc {callee_scc}), which must \
                 not be scheduled after its caller",
                p.func_name(f),
                p.func_name(c)
            );
        }
    }
}
