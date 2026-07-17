//! Property tests (phase-2 spec §9.2). Bounded cases: blocking tier.

use proptest::prelude::*;

use goverify_extract::gvir;
use goverify_extract::gvir::instruction::Sem;
use goverify_ir::{CallGraph, Program, Sccs};

/// Small pool of plausible operator/name tokens, mixed with a fully
/// arbitrary short string, so generated `sem` payloads exercise both the
/// modeled fast paths (e.g. `binop_kind`'s known tokens) and the
/// unmodeled/garbage-token havoc paths in `lower.rs`.
fn arb_op_str() -> impl Strategy<Value = String> {
    prop_oneof![
        prop::sample::select(vec![
            "+", "-", "*", "/", "%", "&", "|", "^", "<<", ">>", "&^", "==", "!=", "<", "<=", ">",
            ">=", "!", "~",
        ])
        .prop_map(String::from),
        "[a-zA-Z0-9_]{0,6}",
    ]
}

/// Same idea for name-shaped strings (callee ids, builtin names, aux
/// reprs): a few names that mean something to `lower.rs`
/// (`lock_kind`/`close`/plausible ssa ids) mixed with arbitrary garbage.
fn arb_name_str() -> impl Strategy<Value = String> {
    prop_oneof![
        prop::sample::select(vec![
            "",
            "close",
            "append",
            "len",
            "a.F0",
            "b.F1",
            "(*sync.Mutex).Lock",
            "(*sync.RWMutex).Unlock",
        ])
        .prop_map(String::from),
        "[a-zA-Z0-9_.*() ]{0,12}",
    ]
}

fn arb_select_state() -> impl Strategy<Value = gvir::SelectState> {
    (any::<u32>(), any::<u32>(), any::<u32>()).prop_map(|(dir, chan_operand, send_operand)| {
        gvir::SelectState {
            dir,
            chan_operand,
            send_operand,
        }
    })
}

/// All nine `Instruction.sem` oneof payloads, each with randomized
/// content — including out-of-range u32 ids (`iface_type`, `method_sig`,
/// `index`, `asserted`, select operand ids): lowering must resolve these
/// through bounds-checked table lookups, never index directly. Roughly a
/// fifth of cases carry no `sem` at all. Because `kind` (picked
/// independently in `arb_instruction`) and this payload are uncorrelated,
/// most cases end up as a mismatched kind/sem pair (e.g. `"Jump"` with a
/// `CallSem`) — deliberately, per the totality invariant: any kind/sem
/// combination must degrade, never panic.
fn arb_sem() -> impl Strategy<Value = Option<Sem>> {
    prop_oneof![
        2 => Just(None),
        3 => arb_op_str().prop_map(|op| Some(Sem::Binop(gvir::BinOpSem { op }))),
        3 => (arb_op_str(), any::<bool>())
            .prop_map(|(op, comma_ok)| Some(Sem::Unop(gvir::UnOpSem { op, comma_ok }))),
        3 => (
            arb_name_str(),
            arb_name_str(),
            any::<u32>(),
            any::<bool>(),
            arb_name_str(),
            any::<u32>(),
        )
            .prop_map(|(static_callee, method, iface_type, invoke, builtin, method_sig)| {
                Some(Sem::Call(gvir::CallSem {
                    static_callee,
                    method,
                    iface_type,
                    invoke,
                    builtin,
                    method_sig,
                }))
            }),
        3 => (any::<u32>(), arb_name_str())
            .prop_map(|(index, name)| Some(Sem::Field(gvir::FieldSem { index, name }))),
        3 => (any::<u32>(), any::<bool>()).prop_map(|(asserted, comma_ok)| {
            Some(Sem::TypeAssert(gvir::TypeAssertSem {
                asserted,
                comma_ok,
            }))
        }),
        3 => any::<u32>().prop_map(|index| Some(Sem::Extract(gvir::ExtractSem { index }))),
        3 => any::<bool>().prop_map(|comma_ok| Some(Sem::Lookup(gvir::LookupSem { comma_ok }))),
        3 => any::<bool>().prop_map(|heap| Some(Sem::Alloc(gvir::AllocSem { heap }))),
        3 => (prop::collection::vec(arb_select_state(), 0..4), any::<bool>())
            .prop_map(|(states, blocking)| Some(Sem::Select(gvir::SelectSem {
                states,
                blocking,
            }))),
    ]
}

/// Arbitrary-ish instruction: known + unknown kinds, randomized `sem`
/// payloads (all nine variants, plus absent), random operands — exercises
/// the malformed-input paths of lowering, including the branch-heavy
/// call/select/closure paths that a `sem`-less instruction can never
/// reach.
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
        "Panic",
        "Send",
        "MakeInterface",
        "TotallyUnknownKind",
    ]);
    (
        kinds,
        any::<u32>(),
        prop::collection::vec(any::<u32>(), 0..5),
        arb_sem(),
    )
        .prop_map(|(kind, register, operands, sem)| gvir::Instruction {
            kind: kind.to_string(),
            register: register % 64,
            operands: operands.into_iter().map(|o| o % 64).collect(),
            sem,
            ..Default::default()
        })
}

/// Random aux-value table entries: known `AuxValue.kind` strings (the
/// ones `lower_function` matches on) plus arbitrary/unknown ones (must
/// degrade to `ValueKind::Opaque`, per `lower.rs`'s `_ =>
/// ValueKind::Opaque` arm). `id` is bounded the same way as instruction
/// registers/operands (mod 64) so a `MakeClosure`'s operand-0 lookup can
/// actually land on a `"Function"`-kind aux slot and exercise the
/// `ValueKind::FuncRef` path instead of only ever hitting the havoc
/// fallback.
fn arb_aux() -> impl Strategy<Value = gvir::AuxValue> {
    let kind = prop_oneof![
        prop::sample::select(vec![
            "Const", "Global", "Function", "Builtin", "FreeVar", "Value",
        ])
        .prop_map(String::from),
        "[A-Za-z]{0,8}",
    ];
    (kind, any::<u32>(), arb_name_str(), any::<u32>()).prop_map(|(kind, id, repr, r#type)| {
        gvir::AuxValue {
            id: id % 64,
            kind,
            repr,
            r#type,
            ..Default::default()
        }
    })
}

/// A function body: 0..3 explicit calls to `names` (so the call graph
/// gets real structure to schedule) interleaved with 0..10 arbitrary
/// instructions (for lowering-totality noise), terminated by a `Return`.
fn arb_function_body(
    names: &'static [&'static str],
) -> impl Strategy<Value = Vec<gvir::Instruction>> {
    let call_to_known = prop::sample::select(names.to_vec()).prop_map(|target| gvir::Instruction {
        kind: "Call".into(),
        sem: Some(Sem::Call(gvir::CallSem {
            static_callee: target.into(),
            ..Default::default()
        })),
        ..Default::default()
    });
    (
        prop::collection::vec(call_to_known, 0..3),
        prop::collection::vec(arb_instruction(), 0..10),
    )
        .prop_map(|(mut calls, noise)| {
            calls.extend(noise);
            calls.push(gvir::Instruction {
                kind: "Return".into(),
                ..Default::default()
            });
            calls
        })
}

/// A single function: `id` fixed (the schedule/callgraph property needs
/// stable names to compare across two independent builds), body and aux
/// table randomized.
fn arb_function(
    id: &'static str,
    call_targets: &'static [&'static str],
) -> impl Strategy<Value = gvir::Function> {
    (
        arb_function_body(call_targets),
        prop::collection::vec(arb_aux(), 0..3),
    )
        .prop_map(move |(instrs, aux)| gvir::Function {
            id: id.into(),
            aux,
            blocks: vec![gvir::BasicBlock {
                index: 0,
                instrs,
                succs: vec![],
            }],
            ..Default::default()
        })
}

/// Three packages ("a", "b", "c"), two functions each, every function
/// body randomized (calls to any of the 6 functions, plus arbitrary
/// noise instructions and aux values). Cross-package call edges — and
/// therefore SCCs — vary from case to case.
fn arb_program() -> impl Strategy<Value = Vec<gvir::Package>> {
    const NAMES: [&str; 6] = ["a.F0", "a.F1", "b.F0", "b.F1", "c.F0", "c.F1"];
    (
        arb_function(NAMES[0], &NAMES),
        arb_function(NAMES[1], &NAMES),
        arb_function(NAMES[2], &NAMES),
        arb_function(NAMES[3], &NAMES),
        arb_function(NAMES[4], &NAMES),
        arb_function(NAMES[5], &NAMES),
    )
        .prop_map(|(a0, a1, b0, b1, c0, c1)| {
            vec![
                gvir::Package {
                    import_path: "a".into(),
                    functions: vec![a0, a1],
                    ..Default::default()
                },
                gvir::Package {
                    import_path: "b".into(),
                    functions: vec![b0, b1],
                    ..Default::default()
                },
                gvir::Package {
                    import_path: "c".into(),
                    functions: vec![c0, c1],
                    ..Default::default()
                },
            ]
        })
}

/// Build `Program` -> `CallGraph` -> `Sccs` from `pkgs`.
fn build(pkgs: Vec<gvir::Package>) -> (Program, CallGraph, Sccs) {
    let p = Program::from_packages(pkgs);
    let g = CallGraph::build(&p);
    let s = Sccs::compute(&p, &g);
    (p, g, s)
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

    /// Lowering totality: any structurally-valid package lowers without
    /// panicking, whatever the instruction contents — including
    /// randomized `sem` payloads (all nine variants) and randomized aux
    /// entries, deliberately mismatched against `kind` most of the time.
    #[test]
    fn lowering_never_panics(
        instrs in prop::collection::vec(arb_instruction(), 0..30),
        aux in prop::collection::vec(arb_aux(), 0..5),
    ) {
        let pkg = gvir::Package {
            import_path: "p".into(),
            functions: vec![gvir::Function {
                id: "p.F".into(),
                aux,
                blocks: vec![gvir::BasicBlock { index: 0, instrs, succs: vec![] }],
                ..Default::default()
            }],
            ..Default::default()
        };
        let p = Program::from_packages(vec![pkg]);
        prop_assert!(p.lookup_func("p.F").is_some());
    }

    /// FuncId assignment, call graph, and SCC schedule are invariant
    /// under package input order (a regression guard for
    /// `Program::from_packages`'s import-path sort) AND under rebuilding
    /// from scratch: two independent `Program`/`CallGraph`/`Sccs` builds
    /// use two different `HashMap` `RandomState` seeds, so comparing them
    /// genuinely catches map-iteration order leaking into
    /// `schedule()`/`callees()` — the failure mode this crate's
    /// determinism comments worry about, which a single build (or a
    /// same-process reuse of one `Program`) can never expose.
    #[test]
    fn schedule_stable_under_package_reorder(
        pkgs in arb_program(),
        seed in any::<u64>(),
    ) {
        let mut reordered = pkgs.clone();
        reordered.rotate_left((seed % 3) as usize);
        if seed % 2 == 0 {
            reordered.swap(0, 1);
        }

        let (p1, g1, s1) = build(pkgs);
        let (p2, g2, s2) = build(reordered);

        let ids1: Vec<_> = p1.func_ids().collect();
        let ids2: Vec<_> = p2.func_ids().collect();
        prop_assert_eq!(&ids1, &ids2, "FuncId assignment must be order-invariant");
        for &fid in &ids1 {
            prop_assert_eq!(
                g1.callees(fid), g2.callees(fid),
                "callees({:?}) must be identical across independent builds", fid
            );
        }
        prop_assert_eq!(
            s1.schedule(), s2.schedule(),
            "SCC schedule must be identical across independent builds"
        );
    }
}
