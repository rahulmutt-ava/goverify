//! Encoder property tests (phase-4 spec §9.2 style, mirroring
//! `goverify-ir/tests/props.rs`'s bounded-generator approach — test code
//! isn't importable across crates, so the generator is reimplemented
//! locally rather than shared).
//!
//! `arb_program` builds small, hand-assembled `gvir::Package`s: up to 4
//! blocks whose CFG is generated as a raw edge set first (`succs`), with
//! `preds` DERIVED from it by inversion — so the two are consistent by
//! construction, the same discipline `props.rs`'s
//! `schedule_stable_under_package_reorder` relies on. Each block gets up
//! to 6 instructions drawn from the encoder's modeled surface (BinOp
//! over random int widths, UnOp, Phi sized to the block's own pred
//! count, Call as a deliberately-unresolvable havoc, Load/Store through
//! a guaranteed pointer param) plus a terminator matching its successor
//! count (Jump/If/Return).

use proptest::prelude::*;

use goverify_analysis::encode_func;
use goverify_extract::gvir;
use goverify_ir::Program;
use goverify_solver::{SatResult, SolverLimits, TextSolver, Z3Native};

// ---- fixed type table -----------------------------------------------
//
// Dense ids 1..=10 (TypeTable::import_package caps its local-id map at
// `types.len()`, so every id referenced below must actually be present).
//   1 int (64 signed)   2 int32 (32 signed)  3 int16 (16 signed)
//   4 uint8 (8 unsigned) 5 uint64 (64 unsigned)
//   6 bool  7 string  8 []int (slice)  9 T (struct)  10 *T (pointer)

/// (type id, width, signed) for the int types available to BinOp/UnOp/Phi.
const INT_TYPES: [(u32, u32, bool); 5] = [
    (1, 64, true),
    (2, 32, true),
    (3, 16, true),
    (4, 8, false),
    (5, 64, false),
];
/// Types a param may be assigned (every modelable type except the bare
/// struct, which only appears as the pointer's pointee).
const PARAM_TYPES: [u32; 9] = [1, 2, 3, 4, 5, 6, 7, 8, 10];
const PTR_TYPE: u32 = 10;
const BOOL_TYPE: u32 = 6;

fn type_table() -> Vec<gvir::Type> {
    use gvir::TypeKind;
    let basic = |id: u32, repr: &str| gvir::Type {
        id,
        repr: repr.into(),
        kind: TypeKind::Basic as i32,
        name: repr.into(),
        ..Default::default()
    };
    vec![
        basic(1, "int"),
        basic(2, "int32"),
        basic(3, "int16"),
        basic(4, "uint8"),
        basic(5, "uint64"),
        basic(6, "bool"),
        basic(7, "string"),
        gvir::Type {
            id: 8,
            repr: "[]int".into(),
            kind: TypeKind::Slice as i32,
            elem: 1,
            ..Default::default()
        },
        gvir::Type {
            id: 9,
            repr: "T".into(),
            kind: TypeKind::Struct as i32,
            ..Default::default()
        },
        gvir::Type {
            id: 10,
            repr: "*T".into(),
            kind: TypeKind::Pointer as i32,
            elem: 9,
            ..Default::default()
        },
    ]
}

// ---- register pools & id allocation -----------------------------------

/// Registers available to later instructions, by category — seeded with
/// baseline consts/a guaranteed pointer param, then grown as instructions
/// are built (so later instructions can chain off earlier dsts).
struct Pools {
    ints: [Vec<u32>; 5],
    bools: Vec<u32>,
    ptrs: Vec<u32>,
}

impl Pools {
    fn new() -> Self {
        Pools {
            ints: Default::default(),
            bools: Vec::new(),
            ptrs: Vec::new(),
        }
    }
}

fn next_id(n: &mut u32) -> u32 {
    let v = *n;
    *n += 1;
    v
}

/// Picks an entry from `pool` by `idx`, with a `1/(len+1)` chance of "0"
/// (the reserved missing/opaque value id) — folding the known-vs-havoc
/// operand choice into a single random field instead of a separate flag.
fn pick(pool: &[u32], idx: u32) -> u32 {
    let n = pool.len();
    if n == 0 {
        return 0;
    }
    let i = (idx as usize) % (n + 1);
    if i == n { 0 } else { pool[i] }
}

fn const_int_aux(id: u32, ty: u32, val: i64) -> gvir::AuxValue {
    gvir::AuxValue {
        id,
        kind: "Const".into(),
        r#type: ty,
        r#const: Some(gvir::ConstValue {
            value: Some(gvir::const_value::Value::Int(val)),
        }),
        ..Default::default()
    }
}

fn const_bool_aux(id: u32, ty: u32, val: bool) -> gvir::AuxValue {
    gvir::AuxValue {
        id,
        kind: "Const".into(),
        r#type: ty,
        r#const: Some(gvir::ConstValue {
            value: Some(gvir::const_value::Value::Bool(val)),
        }),
        ..Default::default()
    }
}

fn route_to_pool(tid: u32, id: u32, pools: &mut Pools) {
    match tid {
        1 => pools.ints[0].push(id),
        2 => pools.ints[1].push(id),
        3 => pools.ints[2].push(id),
        4 => pools.ints[3].push(id),
        5 => pools.ints[4].push(id),
        6 => pools.bools.push(id),
        10 => pools.ptrs.push(id),
        _ => {} // string/slice (7,8): declared as params, not otherwise fed to a pool
    }
}

fn binop_token(op_idx: usize) -> &'static str {
    match op_idx % 3 {
        0 => "+",
        1 => "-",
        _ => "/",
    }
}

// ---- instruction recipes: random leaves, deterministic assembly -------

#[derive(Clone, Debug)]
enum InstrRecipe {
    BinOp {
        ty_idx: usize,
        op_idx: usize,
        lhs: u32,
        rhs: u32,
    },
    UnOpInt {
        ty_idx: usize,
        neg: bool,
        operand: u32,
    },
    UnOpBool {
        operand: u32,
    },
    /// `edges` is a fixed-size pool of random picks; only the first
    /// `preds_len` (the block's actual pred count, known only once the
    /// CFG is assembled) are used, so the built `Phi`'s operand count is
    /// exactly its block's pred count.
    Phi {
        ty_idx: usize,
        edges: [u32; 4],
    },
    Call {
        has_dst: bool,
        callee_idx: usize,
        args: [u32; 2],
    },
    Load {
        addr: u32,
    },
    Store {
        addr: u32,
        val: u32,
        val_ty_idx: usize,
    },
}

fn arb_instr_recipe() -> impl Strategy<Value = InstrRecipe> {
    prop_oneof![
        (0usize..5, 0usize..3, any::<u32>(), any::<u32>()).prop_map(
            |(ty_idx, op_idx, lhs, rhs)| InstrRecipe::BinOp {
                ty_idx,
                op_idx,
                lhs,
                rhs
            }
        ),
        (0usize..5, any::<bool>(), any::<u32>()).prop_map(|(ty_idx, neg, operand)| {
            InstrRecipe::UnOpInt {
                ty_idx,
                neg,
                operand,
            }
        }),
        any::<u32>().prop_map(|operand| InstrRecipe::UnOpBool { operand }),
        (
            0usize..5,
            any::<u32>(),
            any::<u32>(),
            any::<u32>(),
            any::<u32>()
        )
            .prop_map(|(ty_idx, e0, e1, e2, e3)| InstrRecipe::Phi {
                ty_idx,
                edges: [e0, e1, e2, e3],
            }),
        (any::<bool>(), 0usize..3, any::<u32>(), any::<u32>()).prop_map(
            |(has_dst, callee_idx, a0, a1)| InstrRecipe::Call {
                has_dst,
                callee_idx,
                args: [a0, a1],
            }
        ),
        any::<u32>().prop_map(|addr| InstrRecipe::Load { addr }),
        (any::<u32>(), any::<u32>(), 0usize..5).prop_map(|(addr, val, val_ty_idx)| {
            InstrRecipe::Store {
                addr,
                val,
                val_ty_idx,
            }
        }),
    ]
}

fn build_instr(
    recipe: &InstrRecipe,
    alloc: &mut u32,
    pools: &mut Pools,
    preds_len: usize,
) -> gvir::Instruction {
    match recipe {
        InstrRecipe::BinOp {
            ty_idx,
            op_idx,
            lhs,
            rhs,
        } => {
            let (ty, ..) = INT_TYPES[*ty_idx];
            let l = pick(&pools.ints[*ty_idx], *lhs);
            let r = pick(&pools.ints[*ty_idx], *rhs);
            let dst = next_id(alloc);
            pools.ints[*ty_idx].push(dst);
            gvir::Instruction {
                kind: "BinOp".into(),
                register: dst,
                r#type: ty,
                operands: vec![l, r],
                sem: Some(gvir::instruction::Sem::Binop(gvir::BinOpSem {
                    op: binop_token(*op_idx).into(),
                })),
                ..Default::default()
            }
        }
        InstrRecipe::UnOpInt {
            ty_idx,
            neg,
            operand,
        } => {
            let (ty, ..) = INT_TYPES[*ty_idx];
            let o = pick(&pools.ints[*ty_idx], *operand);
            let dst = next_id(alloc);
            pools.ints[*ty_idx].push(dst);
            let op = if *neg { "-" } else { "^" };
            gvir::Instruction {
                kind: "UnOp".into(),
                register: dst,
                r#type: ty,
                operands: vec![o],
                sem: Some(gvir::instruction::Sem::Unop(gvir::UnOpSem {
                    op: op.into(),
                    comma_ok: false,
                })),
                ..Default::default()
            }
        }
        InstrRecipe::UnOpBool { operand } => {
            let o = pick(&pools.bools, *operand);
            let dst = next_id(alloc);
            pools.bools.push(dst);
            gvir::Instruction {
                kind: "UnOp".into(),
                register: dst,
                r#type: BOOL_TYPE,
                operands: vec![o],
                sem: Some(gvir::instruction::Sem::Unop(gvir::UnOpSem {
                    op: "!".into(),
                    comma_ok: false,
                })),
                ..Default::default()
            }
        }
        InstrRecipe::Phi { ty_idx, edges } => {
            let (ty, ..) = INT_TYPES[*ty_idx];
            let dst = next_id(alloc);
            let picked: Vec<u32> = edges
                .iter()
                .take(preds_len)
                .map(|&e| pick(&pools.ints[*ty_idx], e))
                .collect();
            pools.ints[*ty_idx].push(dst);
            gvir::Instruction {
                kind: "Phi".into(),
                register: dst,
                r#type: ty,
                operands: picked,
                ..Default::default()
            }
        }
        InstrRecipe::Call {
            has_dst,
            callee_idx,
            args,
        } => {
            // Deliberately unresolvable (no body anywhere in the
            // package): `lower_plain_call` always falls through to a
            // plain `Op::Call`, which the encoder never models (`len`
            // aside) — i.e. always a havoc'd dst.
            let names = ["p.G", "p.H", "unknown.Callee"];
            let name = names[*callee_idx % names.len()];
            let a0 = pick(&pools.ints[0], args[0]);
            let a1 = pick(&pools.ints[0], args[1]);
            let dst = if *has_dst {
                let d = next_id(alloc);
                pools.ints[0].push(d);
                Some(d)
            } else {
                None
            };
            gvir::Instruction {
                kind: "Call".into(),
                register: dst.unwrap_or(0),
                r#type: if dst.is_some() { INT_TYPES[0].0 } else { 0 },
                // operands[0] is the (unused, for a static callee) callee
                // slot; real args start at index 1 — mirrors
                // `lower_plain_call`'s `skip(1)`.
                operands: vec![0, a0, a1],
                sem: Some(gvir::instruction::Sem::Call(gvir::CallSem {
                    static_callee: name.into(),
                    ..Default::default()
                })),
                ..Default::default()
            }
        }
        InstrRecipe::Load { addr } => {
            // Lowers via `UnOp "*"` (deref), not a distinct "Load" kind.
            let a = pick(&pools.ptrs, *addr);
            let dst = next_id(alloc);
            pools.ints[0].push(dst);
            gvir::Instruction {
                kind: "UnOp".into(),
                register: dst,
                r#type: INT_TYPES[0].0,
                operands: vec![a],
                sem: Some(gvir::instruction::Sem::Unop(gvir::UnOpSem {
                    op: "*".into(),
                    comma_ok: false,
                })),
                ..Default::default()
            }
        }
        InstrRecipe::Store {
            addr,
            val,
            val_ty_idx,
        } => {
            let a = pick(&pools.ptrs, *addr);
            let v = pick(&pools.ints[*val_ty_idx], *val);
            gvir::Instruction {
                kind: "Store".into(),
                operands: vec![a, v],
                ..Default::default()
            }
        }
    }
}

// ---- CFG + package assembly --------------------------------------------

fn build_package(
    nblocks: usize,
    succs_raw: Vec<Vec<u32>>,
    instr_recipes: Vec<Vec<InstrRecipe>>,
    cond_idxs: Vec<u32>,
    param_type_idxs: Vec<usize>,
) -> gvir::Package {
    // CFG as an edge set first: clip out-of-range targets, dedup — then
    // preds is DERIVED from succs by inversion, so the two never disagree.
    let succs: Vec<Vec<u32>> = succs_raw
        .into_iter()
        .map(|mut s| {
            s.retain(|&x| (x as usize) < nblocks);
            s.sort_unstable();
            s.dedup();
            s
        })
        .collect();
    let mut preds: Vec<Vec<u32>> = vec![Vec::new(); nblocks];
    for (b, ss) in succs.iter().enumerate() {
        for &s in ss {
            preds[s as usize].push(b as u32);
        }
    }

    let mut alloc = 1u32;
    let mut pools = Pools::new();
    let mut aux = Vec::new();

    // Baseline: one const per int width/signedness plus a bool const, so
    // BinOp/UnOp/Phi/If always have at least one concrete operand to draw
    // on regardless of which params the case happens to generate.
    for (i, &(ty, _w, _s)) in INT_TYPES.iter().enumerate() {
        let id = next_id(&mut alloc);
        aux.push(const_int_aux(id, ty, 3 + i as i64 * 2));
        pools.ints[i].push(id);
    }
    let cbool = next_id(&mut alloc);
    aux.push(const_bool_aux(cbool, BOOL_TYPE, true));
    pools.bools.push(cbool);

    // Params: random modelable types, plus one guaranteed pointer param
    // so Load/Store on a pointer param is reliably reachable rather than
    // left to the ~1/9 chance a random param happens to land on `*T`.
    let mut params = Vec::new();
    for ty_idx in param_type_idxs {
        let tid = PARAM_TYPES[ty_idx % PARAM_TYPES.len()];
        let id = next_id(&mut alloc);
        params.push(gvir::Param {
            id,
            name: format!("a{id}"),
            r#type: tid,
        });
        route_to_pool(tid, id, &mut pools);
    }
    let ptr_id = next_id(&mut alloc);
    params.push(gvir::Param {
        id: ptr_id,
        name: format!("a{ptr_id}"),
        r#type: PTR_TYPE,
    });
    pools.ptrs.push(ptr_id);

    let mut blocks = Vec::with_capacity(nblocks);
    for b in 0..nblocks {
        let mut instrs = Vec::new();
        let preds_len = preds[b].len();
        for recipe in &instr_recipes[b] {
            instrs.push(build_instr(recipe, &mut alloc, &mut pools, preds_len));
        }
        match succs[b].len() {
            0 => instrs.push(gvir::Instruction {
                kind: "Return".into(),
                ..Default::default()
            }),
            1 => instrs.push(gvir::Instruction {
                kind: "Jump".into(),
                ..Default::default()
            }),
            _ => {
                let cond = pick(&pools.bools, cond_idxs[b]);
                instrs.push(gvir::Instruction {
                    kind: "If".into(),
                    operands: vec![cond],
                    ..Default::default()
                });
            }
        }
        blocks.push(gvir::BasicBlock {
            index: b as u32,
            instrs,
            succs: succs[b].clone(),
            preds: preds[b].clone(),
        });
    }

    gvir::Package {
        import_path: "p".into(),
        types: type_table(),
        functions: vec![gvir::Function {
            id: "p.F".into(),
            params,
            aux,
            blocks,
            ..Default::default()
        }],
        ..Default::default()
    }
}

/// Small arbitrary programs: 1..=4 blocks, CFG as an edge set (`succs`)
/// with `preds` derived from it, up to 6 instructions per block from the
/// encoder's modeled surface, random param types (plus a guaranteed
/// pointer param).
fn arb_program() -> impl Strategy<Value = gvir::Package> {
    (1usize..=4).prop_flat_map(|nblocks| {
        let succs =
            prop::collection::vec(prop::collection::vec(0u32..nblocks as u32, 0..=2), nblocks);
        let recipes =
            prop::collection::vec(prop::collection::vec(arb_instr_recipe(), 0..=6), nblocks);
        let cond_idxs = prop::collection::vec(any::<u32>(), nblocks);
        let param_types = prop::collection::vec(0usize..PARAM_TYPES.len(), 0..=3);
        (succs, recipes, cond_idxs, param_types)
            .prop_map(move |(s, r, c, p)| build_package(nblocks, s, r, c, p))
    })
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

    #[test]
    fn encode_never_panics_and_is_deterministic(prog in arb_program()) {
        let p = Program::from_packages(vec![prog]);
        for f in p.func_ids() {
            if p.func(f).is_none() { continue; }
            let a = encode_func(&p, f);
            let b = encode_func(&p, f);
            match (a, b) {
                (Ok(a), Ok(b)) => {
                    for bi in 0..a.guards.len() {
                        prop_assert_eq!(
                            a.reach_query(bi, vec![]).canonical_text(),
                            b.reach_query(bi, vec![]).canonical_text()
                        );
                    }
                }
                (Err(a), Err(b)) => prop_assert_eq!(a, b),
                _ => prop_assert!(false, "determinism: Ok/Err disagree"),
            }
        }
    }

    #[test]
    fn queries_are_well_formed_for_z3(prog in arb_program()) {
        // Every reach query must be parseable/solvable: drive through
        // Z3Native and require SOME verdict (never a crash). Cap: first
        // 4 blocks per function.
        let p = Program::from_packages(vec![prog]);
        let mut solver = Z3Native::new(SolverLimits { timeout_ms: 2_000, mem_mb: 256 });
        for f in p.func_ids() {
            if p.func(f).is_none() { continue; }
            let Ok(enc) = encode_func(&p, f) else { continue };
            for bi in 0..enc.guards.len().min(4) {
                let text = enc.reach_query(bi, vec![]).canonical_text();
                let res = solver.solve_text(&text);
                prop_assert!(
                    matches!(res.result, SatResult::Sat | SatResult::Unsat | SatResult::Unknown),
                    "block {bi} produced no verdict:\n{text}"
                );
            }
        }
    }
}
