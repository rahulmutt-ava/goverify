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
