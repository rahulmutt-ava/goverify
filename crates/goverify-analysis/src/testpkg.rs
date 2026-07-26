//! Test-only builders for hand-written gvir packages.

use goverify_extract::gvir;

pub fn call(target: &str) -> gvir::Instruction {
    gvir::Instruction {
        kind: "Call".into(),
        sem: Some(gvir::instruction::Sem::Call(gvir::CallSem {
            static_callee: target.into(),
            ..Default::default()
        })),
        ..Default::default()
    }
}

pub fn go_call(target: &str) -> gvir::Instruction {
    gvir::Instruction {
        kind: "Go".into(),
        ..call(target)
    }
}

pub fn defer_call(target: &str) -> gvir::Instruction {
    gvir::Instruction {
        kind: "Defer".into(),
        ..call(target)
    }
}

/// `<kind>(<chan_operand>)`-shaped `Call`/`Go`/`Defer` on the `close`
/// builtin, matching `lower.rs`'s operand layout: `[<callee slot>,
/// args…]` (the callee slot itself is unused for a builtin, so any
/// placeholder value works).
fn builtin_close(kind: &str, chan_operand: u32) -> gvir::Instruction {
    gvir::Instruction {
        kind: kind.into(),
        operands: vec![0, chan_operand],
        sem: Some(gvir::instruction::Sem::Call(gvir::CallSem {
            builtin: "close".into(),
            ..Default::default()
        })),
        ..Default::default()
    }
}

pub fn defer_close(chan_operand: u32) -> gvir::Instruction {
    builtin_close("Defer", chan_operand)
}

pub fn go_close(chan_operand: u32) -> gvir::Instruction {
    builtin_close("Go", chan_operand)
}

pub fn instr(kind: &str) -> gvir::Instruction {
    gvir::Instruction {
        kind: kind.into(),
        ..Default::default()
    }
}

pub fn block(index: u32, instrs: Vec<gvir::Instruction>, succs: Vec<u32>) -> gvir::BasicBlock {
    gvir::BasicBlock {
        index,
        instrs,
        succs,
        preds: vec![],
    }
}

/// Same shape as `block`, plus explicit `preds` (needed by guard-encoding
/// tests, which read `Block.preds` directly — `block`'s hand-built
/// fixtures otherwise leave `preds` empty since `Program::from_packages`
/// only fills it from the wire).
pub fn block_p(
    index: u32,
    instrs: Vec<gvir::Instruction>,
    preds: Vec<u32>,
    succs: Vec<u32>,
) -> gvir::BasicBlock {
    gvir::BasicBlock {
        index,
        instrs,
        succs,
        preds,
    }
}

pub fn func(id: &str, blocks: Vec<gvir::BasicBlock>) -> gvir::Function {
    gvir::Function {
        id: id.into(),
        blocks,
        ..Default::default()
    }
}

/// Same shape as `func`, plus declared params (needed by value-clean
/// tests that inspect param types).
pub fn func_with_params(
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

pub fn pkg(path: &str, functions: Vec<gvir::Function>) -> gvir::Package {
    gvir::Package {
        import_path: path.into(),
        functions,
        ..Default::default()
    }
}

/// `register = make(chan T, <cap value id>)` — MakeChan wire shape:
/// operands [cap], per lower.rs's `"MakeChan" => Op::Make{args: vec![v(0)]}`.
pub fn gvir_make_chan(register: u32, cap_operand: u32) -> gvir::Instruction {
    gvir::Instruction {
        kind: "MakeChan".into(),
        register,
        operands: vec![cap_operand],
        ..Default::default()
    }
}

/// Function with FreeVar aux values (ids in order) and no params.
pub fn func_with_free_vars(
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

/// `register = MakeClosure <fn_aux> [bindings…]` — wire shape per
/// lower.rs ~line 441: operands [fn, bindings…], fn is a Function aux.
pub fn make_closure(register: u32, fn_aux_operand: u32, bindings: Vec<u32>) -> gvir::Instruction {
    let mut operands = vec![fn_aux_operand];
    operands.extend(bindings);
    gvir::Instruction {
        kind: "MakeClosure".into(),
        register,
        operands,
        ..Default::default()
    }
}

/// Function with arbitrary aux values (Function refs, consts, …).
pub fn func_with_aux(
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

pub fn fn_aux(id: u32, target: &str) -> gvir::AuxValue {
    gvir::AuxValue {
        id,
        kind: "Function".into(),
        repr: target.into(),
        ..Default::default()
    }
}

pub fn const_int_aux(id: u32, v: i64) -> gvir::AuxValue {
    gvir::AuxValue {
        id,
        kind: "Const".into(),
        r#const: Some(gvir::ConstValue {
            value: Some(gvir::const_value::Value::Int(v)),
        }),
        ..Default::default()
    }
}
