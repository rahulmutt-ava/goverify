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

/// Regression (final-review C1): `Renamer.Rename` is declared with named
/// params/results (`Rename(newName string) (ok bool, err error)`); its
/// only implementer, `Thing.Rename`, uses a differently-named param and
/// unnamed results (`Rename(n string) (bool, error)`). Before the
/// structural-signature-key fix, `emit.go`'s per-repr TypeId interning
/// gave these two declarations different sig TypeIds despite being
/// structurally identical, so the exact-TypeId match in
/// `resolve_invoke`/method-set-inclusion silently dropped the edge.
#[test]
fn invoke_call_resolves_despite_differently_named_params() {
    let p = testutil::load_corpus("conc");
    let g = CallGraph::build(&p);
    let rename_all = p.lookup_func("example.com/conc.RenameAll").unwrap();
    let rename = p.lookup_func("(*example.com/conc.Thing).Rename").unwrap();
    assert!(
        g.callees(rename_all).contains(&rename),
        "RenameAll must edge to (*Thing).Rename despite differently-named \
         interface vs implementation params; got {:?}",
        g.callees(rename_all)
            .iter()
            .map(|&f| p.func_name(f))
            .collect::<Vec<_>>()
    );
}

/// Regression (final-review C1): `InvokeCB`'s dynamic call `cb("hi")` is
/// through a parameter declared `func(x string) int`, but the function
/// value passed to it (`NamedParamImpl`) is declared `func(m string)
/// int` — same underlying structure, different parameter name. Before
/// the fix, the address-taken map (keyed by raw sig TypeId) never
/// matched the call site's dynamic-lookup key, so the edge to
/// `NamedParamImpl` was silently dropped.
#[test]
fn dynamic_call_resolves_despite_differently_named_params() {
    let p = testutil::load_corpus("conc");
    let g = CallGraph::build(&p);
    let invoke_cb = p.lookup_func("example.com/conc.InvokeCB").unwrap();
    let named_param_impl = p.lookup_func("example.com/conc.NamedParamImpl").unwrap();
    assert!(
        g.callees(invoke_cb).contains(&named_param_impl),
        "InvokeCB must edge to NamedParamImpl despite differently-named \
         func-type params; got {:?}",
        g.callees(invoke_cb)
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
