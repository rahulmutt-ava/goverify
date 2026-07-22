//! End-to-end ensures inference over a real extracted corpus module:
//! unconditional never-nil, the (T, error) correlation under the
//! Go-idiom rule, and the (nil, nil) rejection case.

use goverify_analysis::{EngineConfig, Options, analyze_full};
use goverify_checkers::NilChecker;
use goverify_solver::{SolverLimits, Z3Native};

fn analysis() -> (goverify_ir::Program, goverify_analysis::Analysis) {
    let p = goverify_ir::testutil::load_corpus("ensures");
    let checkers: Vec<&dyn goverify_analysis::Checker> = vec![&NilChecker];
    let a = analyze_full(
        &p,
        &EngineConfig {
            opts: Options::default(),
            cache_dir: None,
            emit_smt: None,
        },
        &checkers,
        &|_role| {
            Box::new(Z3Native::new(SolverLimits {
                timeout_ms: 5_000,
                mem_mb: 1024,
            }))
        },
    );
    (p, a)
}

/// The sorted free-var names of every "nil-deref" ensures clause.
fn ensures_vars(
    p: &goverify_ir::Program,
    a: &goverify_analysis::Analysis,
    func: &str,
) -> Vec<Vec<String>> {
    let f = p.lookup_func(func).expect(func);
    a.summaries[&f]
        .ensures
        .iter()
        .filter(|c| c.tag == "nil-deref")
        .map(|c| {
            let mut v: Vec<String> = c.formula.term.free_vars().keys().cloned().collect();
            v.sort();
            v
        })
        .collect()
}

#[test]
fn ensures_inference_over_corpus() {
    let (p, a) = analysis();
    assert_eq!(
        ensures_vars(&p, &a, "example.com/ensures.MakeT"),
        vec![vec!["r0".to_string()]],
        "MakeT: unconditional never-nil result"
    );
    assert!(
        ensures_vars(&p, &a, "example.com/ensures.NewT")
            .contains(&vec!["r0".to_string(), "r1".to_string()]),
        "NewT: the (T, error) correlation must be emitted: {:?}",
        ensures_vars(&p, &a, "example.com/ensures.NewT")
    );
    assert!(
        ensures_vars(&p, &a, "example.com/ensures.MayNil").is_empty(),
        "MayNil returns (nil, nil): no nil-deref ensures may validate: {:?}",
        ensures_vars(&p, &a, "example.com/ensures.MayNil")
    );
    assert!(
        ensures_vars(&p, &a, "example.com/ensures.NewTVia")
            .contains(&vec!["r0".to_string(), "r1".to_string()]),
        "NewTVia: the Go-idiom site rule accepts extract-shaped error \
         components, so this forwarding wrapper of idiomatic callees \
         keeps the (T, error) correlation: {:?}",
        ensures_vars(&p, &a, "example.com/ensures.NewTVia")
    );
    assert!(
        ensures_vars(&p, &a, "example.com/ensures.NewTNamed")
            .contains(&vec!["r0".to_string(), "r1".to_string()]),
        "NewTNamed: the Go-idiom site rule accepts extract-shaped error \
         components, so this named-results+defer wrapper of idiomatic \
         callees keeps the correlation too: {:?}",
        ensures_vars(&p, &a, "example.com/ensures.NewTNamed")
    );
    assert_eq!(
        ensures_vars(&p, &a, "example.com/ensures.Rec2"),
        vec![vec!["r0".to_string()]],
        "Rec2: recursion-independent unconditional ensures must survive the SCC fixpoint"
    );
    assert!(
        ensures_vars(&p, &a, "example.com/ensures.Rec").is_empty(),
        "Rec: a self-forwarding recursion must not bootstrap its own ensures: {:?}",
        ensures_vars(&p, &a, "example.com/ensures.Rec")
    );
    assert!(
        ensures_vars(&p, &a, "example.com/ensures.AsIface").is_empty(),
        "AsIface: typed-nil-prone interface result must stay clause-free: {:?}",
        ensures_vars(&p, &a, "example.com/ensures.AsIface")
    );
}

/// KNOWN laundering boundary (wave-2 spec §4, threat-model.md): the
/// Go-idiom rule accepts any non-literal error component at a return
/// site WITHOUT consulting the callee's summary, so a bare forwarding
/// wrapper of a non-idiomatic callee (MayNil can return (nil, nil))
/// receives the (T, error) correlation its callee cannot support — a
/// false-discharge (missed-bug) source. This tripwire pins TODAY'S
/// behavior: if it starts failing, the Go-idiom rule changed — update
/// threat-model.md and the ensures.go comments, then flip this pin
/// deliberately.
#[test]
fn maynilvia_laundering_boundary_tripwire() {
    let (p, a) = analysis();
    assert!(
        ensures_vars(&p, &a, "example.com/ensures.MayNilVia")
            .contains(&vec!["r0".to_string(), "r1".to_string()]),
        "MayNilVia: the Go-idiom rule launders the correlation through \
         wrappers of non-idiomatic callees (KNOWN boundary): {:?}",
        ensures_vars(&p, &a, "example.com/ensures.MayNilVia")
    );
}
