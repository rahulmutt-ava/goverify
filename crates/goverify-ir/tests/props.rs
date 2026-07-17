//! Property tests (phase-2 spec §9.2). Bounded cases: blocking tier.

use proptest::prelude::*;

use goverify_extract::gvir;
use goverify_ir::{CallGraph, Program, Sccs};

/// Arbitrary-ish instruction: known + unknown kinds, random operands and
/// sems left absent — exercises the malformed-input paths of lowering.
fn arb_instruction() -> impl Strategy<Value = gvir::Instruction> {
    let kinds = prop::sample::select(vec![
        "BinOp",
        "UnOp",
        "Store",
        "FieldAddr",
        "IndexAddr",
        "Lookup",
        "Slice",
        "Call",
        "Go",
        "Defer",
        "Select",
        "MakeClosure",
        "Phi",
        "Return",
        "Jump",
        "If",
        "Alloc",
        "TotallyUnknownKind",
    ]);
    (
        kinds,
        any::<u32>(),
        prop::collection::vec(any::<u32>(), 0..5),
    )
        .prop_map(|(kind, register, operands)| gvir::Instruction {
            kind: kind.to_string(),
            register: register % 64,
            operands: operands.into_iter().map(|o| o % 64).collect(),
            ..Default::default()
        })
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

    /// Lowering totality: any structurally-valid package lowers without
    /// panicking, whatever the instruction contents.
    #[test]
    fn lowering_never_panics(instrs in prop::collection::vec(arb_instruction(), 0..30)) {
        let pkg = gvir::Package {
            import_path: "p".into(),
            functions: vec![gvir::Function {
                id: "p.F".into(),
                blocks: vec![gvir::BasicBlock { index: 0, instrs, succs: vec![] }],
                ..Default::default()
            }],
            ..Default::default()
        };
        let p = Program::from_packages(vec![pkg]);
        prop_assert!(p.lookup_func("p.F").is_some());
    }

    /// FuncId assignment, call graph, and SCC schedule are invariant under
    /// package input order.
    #[test]
    fn schedule_stable_under_package_reorder(seed in any::<u64>()) {
        // Fixed small program: 3 packages, cross-package static calls.
        let call = |target: &str| gvir::Instruction {
            kind: "Call".into(),
            sem: Some(gvir::instruction::Sem::Call(gvir::CallSem {
                static_callee: target.into(), ..Default::default() })),
            ..Default::default()
        };
        let func = |id: &str, callees: &[&str]| gvir::Function {
            id: id.into(),
            blocks: vec![gvir::BasicBlock {
                index: 0,
                instrs: callees.iter().map(|c| call(c))
                    .chain([gvir::Instruction { kind: "Return".into(), ..Default::default() }])
                    .collect(),
                succs: vec![],
            }],
            ..Default::default()
        };
        let pkg = |path: &str, fs: Vec<gvir::Function>| gvir::Package {
            import_path: path.into(), functions: fs, ..Default::default() };
        let mut pkgs = vec![
            pkg("a", vec![func("a.F", &["b.G", "c.H"])]),
            pkg("b", vec![func("b.G", &["c.H", "a.F"])]), // cross-package cycle a<->b
            pkg("c", vec![func("c.H", &[])]),
        ];
        // Deterministic pseudo-shuffle from the seed.
        pkgs.rotate_left((seed % 3) as usize);
        if seed % 2 == 0 { pkgs.swap(0, 1); }

        let p = Program::from_packages(pkgs);
        let g = CallGraph::build(&p);
        let s = Sccs::compute(&p, &g);
        let schedule_names: Vec<Vec<&str>> = s.schedule().iter()
            .map(|scc| scc.iter().map(|&f| p.func_name(f)).collect())
            .collect();
        prop_assert_eq!(schedule_names, vec![vec!["c.H"], vec!["a.F", "b.G"]]);
    }
}
