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
