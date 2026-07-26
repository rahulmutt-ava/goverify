//! Shared gvir test-fixture builders + solver plumbing, `#[cfg(test)]`
//! only: hand-built packages/functions/instructions mirroring the real
//! lowerer's wire conventions (see goverify-ir's `lower.rs`), plus the
//! `z3_discharge`/`no_summaries` pair every checker's unit tests need.
//! Extracted in Task 8 so nil.rs and bounds.rs share one copy instead of
//! nil.rs's growing a second, divergent one.
#![cfg(test)]

use goverify_analysis::Summary;
use goverify_extract::gvir;
use goverify_extract::gvir::instruction::Sem;
use goverify_ir::{FuncId, Program};
use goverify_solver::{Query, SatResult, SolverLimits, Z3Native, discharge_query};

pub(crate) fn instr(kind: &str) -> gvir::Instruction {
    gvir::Instruction {
        kind: kind.into(),
        ..Default::default()
    }
}

pub(crate) fn block(
    index: u32,
    instrs: Vec<gvir::Instruction>,
    succs: Vec<u32>,
) -> gvir::BasicBlock {
    gvir::BasicBlock {
        index,
        instrs,
        succs,
        preds: vec![],
    }
}

pub(crate) fn ty(id: u32, repr: &str, kind: gvir::TypeKind, name: &str, elem: u32) -> gvir::Type {
    gvir::Type {
        id,
        repr: repr.into(),
        kind: kind as i32,
        name: name.into(),
        elem,
        ..Default::default()
    }
}

/// Same shape as `ty`, plus a static `array_len` (TypeKind::Array's
/// static length; `elem` is still the element type id).
pub(crate) fn array_ty(id: u32, repr: &str, elem: u32, len: u64) -> gvir::Type {
    gvir::Type {
        id,
        repr: repr.into(),
        kind: gvir::TypeKind::Array as i32,
        elem,
        array_len: len,
        ..Default::default()
    }
}

/// Package "t" with the struct-pointer type pair (1 = T, 2 = *T), a
/// bool type (3, for Branch conditions built from a comparison), and
/// the given functions.
pub(crate) fn pkg_with_ptr_types(functions: Vec<gvir::Function>) -> Program {
    let package = gvir::Package {
        import_path: "t".into(),
        functions,
        types: vec![
            ty(1, "T", gvir::TypeKind::Struct, "", 0),
            ty(2, "*T", gvir::TypeKind::Pointer, "", 1),
            ty(3, "bool", gvir::TypeKind::Basic, "bool", 0),
        ],
        ..Default::default()
    };
    Program::from_packages(vec![package])
}

/// Package "t" with an int type (1), a bool type (2), a `[]int` slice
/// type (3, elem 1) and a `[3]int` array type (4, elem 1, len 3):
/// everything bounds.rs's fixtures need.
pub(crate) fn pkg_with_seq_types(functions: Vec<gvir::Function>) -> Program {
    let package = gvir::Package {
        import_path: "t".into(),
        functions,
        types: vec![
            ty(1, "int", gvir::TypeKind::Basic, "int", 0),
            ty(2, "bool", gvir::TypeKind::Basic, "bool", 0),
            ty(3, "[]int", gvir::TypeKind::Slice, "", 1),
            array_ty(4, "[3]int", 1, 3),
        ],
        ..Default::default()
    };
    Program::from_packages(vec![package])
}

/// Package "t" with every integer width/signedness `bounds.rs`'s
/// div-zero/overflow fixtures need, plus bool for branch conditions:
/// 1 = int (64 signed), 2 = int8 (8 signed), 3 = int32 (32 signed),
/// 4 = uint16 (16 unsigned), 5 = uint32 (32 unsigned), 6 = uint64 (64
/// unsigned), 7 = bool, 8 = uint8 (8 unsigned).
pub(crate) fn pkg_with_int_types(functions: Vec<gvir::Function>) -> Program {
    let package = gvir::Package {
        import_path: "t".into(),
        functions,
        types: vec![
            ty(1, "int", gvir::TypeKind::Basic, "int", 0),
            ty(2, "int8", gvir::TypeKind::Basic, "int8", 0),
            ty(3, "int32", gvir::TypeKind::Basic, "int32", 0),
            ty(4, "uint16", gvir::TypeKind::Basic, "uint16", 0),
            ty(5, "uint32", gvir::TypeKind::Basic, "uint32", 0),
            ty(6, "uint64", gvir::TypeKind::Basic, "uint64", 0),
            ty(7, "bool", gvir::TypeKind::Basic, "bool", 0),
            ty(8, "uint8", gvir::TypeKind::Basic, "uint8", 0),
        ],
        ..Default::default()
    };
    Program::from_packages(vec![package])
}

/// t.F(p *T): FieldAddr p.X in the block layout given.
pub(crate) fn deref_func(blocks: Vec<gvir::BasicBlock>) -> gvir::Function {
    gvir::Function {
        id: "t.F".into(),
        params: vec![gvir::Param {
            id: 1,
            name: "p".into(),
            r#type: 2,
        }],
        blocks,
        ..Default::default()
    }
}

/// A `FieldAddr` dereferencing `operand` (a pointer value), writing
/// its result to `dst_reg`.
pub(crate) fn field_addr_on(dst_reg: u32, operand: u32) -> gvir::Instruction {
    let mut fa = instr("FieldAddr");
    fa.register = dst_reg;
    fa.operands = vec![operand];
    fa.sem = Some(Sem::Field(gvir::FieldSem {
        index: 0,
        name: "X".into(),
    }));
    fa
}

pub(crate) fn field_addr_on_param() -> gvir::Instruction {
    field_addr_on(2, 1)
}

/// t.F with the deref unconditionally in the entry block.
pub(crate) fn deref_program() -> Program {
    pkg_with_ptr_types(vec![deref_func(vec![block(
        0,
        vec![field_addr_on_param(), instr("Return")],
        vec![],
    )])])
}

pub(crate) fn branch_on(operand: u32) -> gvir::Instruction {
    let mut b = instr("If");
    b.operands = vec![operand];
    b
}

/// v<dst> = (lhs <op> rhs), `dst`'s type given by `ty`: mirrors
/// lower.rs's BinOp arm generically (`eq_instr` below is the "==",
/// bool-typed special case nil.rs's fixtures use throughout).
pub(crate) fn binop_instr(dst: u32, ty: u32, op: &str, lhs: u32, rhs: u32) -> gvir::Instruction {
    gvir::Instruction {
        kind: "BinOp".into(),
        register: dst,
        r#type: ty,
        operands: vec![lhs, rhs],
        sem: Some(Sem::Binop(gvir::BinOpSem { op: op.into() })),
        ..Default::default()
    }
}

/// v<dst> = (lhs == rhs) as bool (type id 3, `pkg_with_ptr_types`'s
/// convention): mirrors lower.rs's BinOp arm.
pub(crate) fn eq_instr(dst: u32, lhs: u32, rhs: u32) -> gvir::Instruction {
    binop_instr(dst, 3, "==", lhs, rhs)
}

/// A nil *T aux constant at the given id.
pub(crate) fn nil_aux(id: u32) -> gvir::AuxValue {
    gvir::AuxValue {
        id,
        kind: "Const".into(),
        repr: "nil".into(),
        r#type: 2,
        r#const: Some(gvir::ConstValue {
            value: Some(gvir::const_value::Value::Nil(true)),
        }),
    }
}

/// A manifest int constant aux value: `id` at the given `ty` (a type id
/// in the enclosing package), value `val`.
pub(crate) fn int_aux(id: u32, ty: u32, val: i64) -> gvir::AuxValue {
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

/// A static call instruction: operands = [callee-slot(unused), args…]
/// (mirrors lower.rs's non-invoke call convention). `dst_reg` 0 means
/// no destination (statement call).
pub(crate) fn call_static(
    callee: &str,
    dst_reg: u32,
    dst_ty: u32,
    args: Vec<u32>,
) -> gvir::Instruction {
    let mut c = instr("Call");
    c.register = dst_reg;
    c.r#type = dst_ty;
    c.operands = std::iter::once(0).chain(args).collect();
    c.sem = Some(Sem::Call(gvir::CallSem {
        static_callee: callee.into(),
        ..Default::default()
    }));
    c
}

/// A builtin call instruction (e.g. `len`): same operand convention as
/// `call_static` (callee slot unused, args follow), `sem.builtin` set
/// instead of `static_callee`.
pub(crate) fn call_builtin(
    name: &str,
    dst_reg: u32,
    dst_ty: u32,
    args: Vec<u32>,
) -> gvir::Instruction {
    let mut c = instr("Call");
    c.register = dst_reg;
    c.r#type = dst_ty;
    c.operands = std::iter::once(0).chain(args).collect();
    c.sem = Some(Sem::Call(gvir::CallSem {
        builtin: name.into(),
        ..Default::default()
    }));
    c
}

/// `v<dst_reg> = base[index]` (address form): mirrors lower.rs's
/// `IndexAddr` arm (`v(0)` = base, `v(1)` = index, no `Sem` needed).
pub(crate) fn index_addr_instr(
    dst_reg: u32,
    dst_ty: u32,
    base: u32,
    index: u32,
) -> gvir::Instruction {
    gvir::Instruction {
        kind: "IndexAddr".into(),
        register: dst_reg,
        r#type: dst_ty,
        operands: vec![base, index],
        ..Default::default()
    }
}

/// `v<dst_reg> = base[low:high]` (`0` operand slots = "not given",
/// mirroring lower.rs's `vopt` convention: a missing bound lowers to Go's
/// own zero-value default).
pub(crate) fn slice_instr(
    dst_reg: u32,
    dst_ty: u32,
    base: u32,
    low: u32,
    high: u32,
    max: u32,
) -> gvir::Instruction {
    gvir::Instruction {
        kind: "Slice".into(),
        register: dst_reg,
        r#type: dst_ty,
        operands: vec![base, low, high, max],
        ..Default::default()
    }
}

/// `v<dst_reg> = make([]elem, len, cap)`; `cap_operand` 0 means "not
/// given" (Go's 2-arg `make`, cap defaults to len — mirrors lower.rs's
/// `MakeSlice` arm, whose second operand is `.unwrap_or(0)` when absent).
pub(crate) fn make_slice_instr(
    dst_reg: u32,
    dst_ty: u32,
    len: u32,
    cap_operand: u32,
) -> gvir::Instruction {
    gvir::Instruction {
        kind: "MakeSlice".into(),
        register: dst_reg,
        r#type: dst_ty,
        operands: vec![len, cap_operand],
        ..Default::default()
    }
}

/// `v<dst_reg> = <dst_ty>(src)`: mirrors lower.rs's `Convert` arm (a
/// single `v(0)` operand, no `Sem` needed — same shape as
/// `index_addr_instr`/`slice_instr`).
pub(crate) fn convert_instr(dst_reg: u32, dst_ty: u32, src: u32) -> gvir::Instruction {
    gvir::Instruction {
        kind: "Convert".into(),
        register: dst_reg,
        r#type: dst_ty,
        operands: vec![src],
        ..Default::default()
    }
}

/// Package with the given import path and functions — the generic
/// wire-level package builder `leak.rs`'s fixtures need (goverify-analysis's
/// `testpkg::pkg` equivalent; not otherwise needed by nil.rs/bounds.rs,
/// whose fixtures go through the `pkg_with_*_types` helpers instead).
pub(crate) fn pkg(path: &str, functions: Vec<gvir::Function>) -> gvir::Package {
    gvir::Package {
        import_path: path.into(),
        functions,
        ..Default::default()
    }
}

/// Function with declared params and no other aux — ported from
/// goverify-analysis's `testpkg::func_with_params` (needed here for
/// `leak.rs`'s direct-param-passing goroutine fixtures).
pub(crate) fn func_with_params(
    id: &str,
    params: Vec<gvir::Param>,
    blocks: Vec<gvir::BasicBlock>,
) -> gvir::Function {
    gvir::Function {
        id: id.into(),
        params,
        blocks,
        ..Default::default()
    }
}

/// `register = make(chan T, <cap value id>)` — MakeChan wire shape:
/// operands [cap], per lower.rs's `"MakeChan" => Op::Make{args: vec![v(0)]}`.
/// Ported from goverify-analysis's `testpkg::gvir_make_chan`.
pub(crate) fn gvir_make_chan(register: u32, cap_operand: u32) -> gvir::Instruction {
    gvir::Instruction {
        kind: "MakeChan".into(),
        register,
        operands: vec![cap_operand],
        ..Default::default()
    }
}

/// Function with FreeVar aux values (ids in order) and no params.
/// Ported from goverify-analysis's `testpkg::func_with_free_vars`.
pub(crate) fn func_with_free_vars(
    id: &str,
    free_var_ids: Vec<u32>,
    blocks: Vec<gvir::BasicBlock>,
) -> gvir::Function {
    gvir::Function {
        id: id.into(),
        aux: free_var_ids
            .into_iter()
            .map(|fv| gvir::AuxValue {
                id: fv,
                kind: "FreeVar".into(),
                ..Default::default()
            })
            .collect(),
        blocks,
        ..Default::default()
    }
}

/// Function with arbitrary aux values (Function refs, consts, …).
/// Ported from goverify-analysis's `testpkg::func_with_aux`.
pub(crate) fn func_with_aux(
    id: &str,
    aux: Vec<gvir::AuxValue>,
    blocks: Vec<gvir::BasicBlock>,
) -> gvir::Function {
    gvir::Function {
        id: id.into(),
        aux,
        blocks,
        ..Default::default()
    }
}

/// A `Function`-kind aux value naming `target`. Ported from
/// goverify-analysis's `testpkg::fn_aux`.
pub(crate) fn fn_aux(id: u32, target: &str) -> gvir::AuxValue {
    gvir::AuxValue {
        id,
        kind: "Function".into(),
        repr: target.into(),
        ..Default::default()
    }
}

/// A manifest int constant aux value at package-less type 0 (leak.rs's
/// fixtures only need *some* defined value for a MakeChan's cap operand,
/// never its actual int value). Ported from goverify-analysis's
/// `testpkg::const_int_aux`.
pub(crate) fn const_int_aux(id: u32, v: i64) -> gvir::AuxValue {
    gvir::AuxValue {
        id,
        kind: "Const".into(),
        r#const: Some(gvir::ConstValue {
            value: Some(gvir::const_value::Value::Int(v)),
        }),
        ..Default::default()
    }
}

/// `register = MakeClosure <fn_aux> [bindings…]` — wire shape per
/// lower.rs ~line 441: operands [fn, bindings…], fn is a Function aux.
/// Ported from goverify-analysis's `testpkg::make_closure`.
pub(crate) fn make_closure(
    register: u32,
    fn_aux_operand: u32,
    bindings: Vec<u32>,
) -> gvir::Instruction {
    let mut operands = vec![fn_aux_operand];
    operands.extend(bindings);
    gvir::Instruction {
        kind: "MakeClosure".into(),
        register,
        operands,
        ..Default::default()
    }
}

/// `go <target>()` — zero-arg static go call (`Callee::Static`); operand
/// layout `[callee slot, args…]` per lower.rs (the slot itself is
/// dropped by lowering, so an empty-operands `Go` yields zero args, not
/// a havoc). Ported from goverify-analysis's `testpkg::go_call`.
pub(crate) fn go_call(target: &str) -> gvir::Instruction {
    gvir::Instruction {
        kind: "Go".into(),
        sem: Some(Sem::Call(gvir::CallSem {
            static_callee: target.into(),
            ..Default::default()
        })),
        ..Default::default()
    }
}

/// Same as `go_call`, with the given argument value ids appended after
/// the (unused-for-static) callee slot — mirrors lower.rs's `skip(1)`
/// convention: `go_call_args("t.G", vec![2])` yields operands `[0, 2]`.
pub(crate) fn go_call_args(target: &str, args: Vec<u32>) -> gvir::Instruction {
    let mut operands = vec![0];
    operands.extend(args);
    gvir::Instruction {
        operands,
        ..go_call(target)
    }
}

/// `go <target>(<closure>)` where `<target>` is invoked through a
/// `MakeClosure` register: operands are just `[closure_register]` —
/// lower.rs drops slot 0's value unconditionally, and the closure's own
/// free-var bindings carry the captured values (not `Go`'s `args`, which
/// is empty here after `.skip(1)` on a single-element operand list).
pub(crate) fn go_call_via_closure(target: &str, closure_register: u32) -> gvir::Instruction {
    gvir::Instruction {
        operands: vec![closure_register],
        ..go_call(target)
    }
}

/// `go` through a dynamic (function-value) callee: empty
/// `static_callee`/`builtin`/`invoke` in `CallSem` is exactly the "none
/// of the above" shape lower.rs's `Call`/`Defer`/`Go` arm falls through
/// to `Callee::Dynamic { value: v(0) }` for.
pub(crate) fn go_call_dynamic() -> gvir::Instruction {
    gvir::Instruction {
        kind: "Go".into(),
        sem: Some(Sem::Call(gvir::CallSem::default())),
        ..Default::default()
    }
}

/// `<-ch` — Recv lowers from kind "UnOp" with Sem::Unop{op: "<-"}
/// (lower.rs ~line 197), register = dst, operands [chan].
pub(crate) fn recv(register: u32, chan_operand: u32) -> gvir::Instruction {
    gvir::Instruction {
        kind: "UnOp".into(),
        register,
        operands: vec![chan_operand],
        sem: Some(Sem::Unop(gvir::UnOpSem {
            op: "<-".into(),
            comma_ok: false,
        })),
        ..Default::default()
    }
}

/// `ch <- val` — operands [chan, val] (lower.rs "Send").
pub(crate) fn send(chan_operand: u32, val_operand: u32) -> gvir::Instruction {
    gvir::Instruction {
        kind: "Send".into(),
        operands: vec![chan_operand, val_operand],
        ..Default::default()
    }
}

/// `*addr = val` — operands [addr, val] (lower.rs "Store").
pub(crate) fn store(addr_operand: u32, val_operand: u32) -> gvir::Instruction {
    gvir::Instruction {
        kind: "Store".into(),
        operands: vec![addr_operand, val_operand],
        ..Default::default()
    }
}

/// `return vals…` — operands are the returned value ids.
pub(crate) fn ret(vals: Vec<u32>) -> gvir::Instruction {
    gvir::Instruction {
        kind: "Return".into(),
        operands: vals,
        ..Default::default()
    }
}

/// Select over `states` (dir 1 = send, 2 = recv) — kind "Select" +
/// SelectSem{states, blocking}; needs a register (lower bails without dst).
pub(crate) fn select(
    register: u32,
    states: Vec<(u32, u32, u32)>, // (dir, chan_operand, send_operand-or-0)
    blocking: bool,
) -> gvir::Instruction {
    gvir::Instruction {
        kind: "Select".into(),
        register,
        sem: Some(Sem::Select(gvir::SelectSem {
            blocking,
            states: states
                .into_iter()
                .map(|(dir, chan_operand, send_operand)| gvir::SelectState {
                    dir,
                    chan_operand,
                    send_operand,
                })
                .collect(),
        })),
        ..Default::default()
    }
}

/// A heap `Alloc` at `register` — a "foreign cell" unrelated to any
/// tracked channel, for `leak.rs`'s escape-walk fixtures.
pub(crate) fn alloc_instr(register: u32) -> gvir::Instruction {
    gvir::Instruction {
        kind: "Alloc".into(),
        register,
        sem: Some(Sem::Alloc(gvir::AllocSem { heap: true })),
        ..Default::default()
    }
}

/// `v<register> = iface(src)` — MakeInterface wire shape: a single
/// operand, no `Sem` needed (mirrors `convert_instr`'s shape).
pub(crate) fn make_interface_instr(register: u32, src_operand: u32) -> gvir::Instruction {
    gvir::Instruction {
        kind: "MakeInterface".into(),
        register,
        operands: vec![src_operand],
        ..Default::default()
    }
}

pub(crate) fn z3_discharge() -> impl FnMut(&Query) -> SatResult {
    let mut solver = Z3Native::new(SolverLimits {
        timeout_ms: 5_000,
        mem_mb: 1024,
    });
    move |q| discharge_query(q, &mut solver, None, None).result
}

/// A `summary_of` that never has anything to say (used by tests that
/// don't exercise requires propagation).
pub(crate) fn no_summaries(_f: FuncId) -> Summary {
    Summary::default()
}
