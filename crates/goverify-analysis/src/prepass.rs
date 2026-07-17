//! Pre-pass, value domain (phase-2 spec §5): a function is value-clean if
//! nothing in it can raise a value obligation — no deref of a
//! non-locally-allocated pointer, no indexing/lookup/slicing, no
//! division/remainder, no narrowing conversion. Syntactic and
//! intraprocedural by design; sound direction: false (=not clean) is
//! always safe, so unknowns are not clean.

use std::collections::HashSet;

use goverify_ir::{BinOpKind, FuncId, Op, Program, TypeKind, ValueId};

fn int_width(name: &str) -> Option<u32> {
    Some(match name {
        "int8" | "uint8" | "byte" => 8,
        "int16" | "uint16" => 16,
        "int32" | "uint32" | "rune" | "float32" => 32,
        "int" | "uint" | "int64" | "uint64" | "uintptr" | "float64" => 64,
        _ => return None,
    })
}

pub fn value_clean(p: &Program, id: FuncId) -> bool {
    let Some(f) = p.func(id) else {
        return false;
    };
    let allocs: HashSet<ValueId> = f
        .blocks
        .iter()
        .flat_map(|b| &b.instrs)
        .filter_map(|i| match &i.op {
            Op::Alloc { dst, .. } => Some(*dst),
            _ => None,
        })
        .collect();
    let local = |v: ValueId| allocs.contains(&v);
    for b in &f.blocks {
        for ins in &b.instrs {
            let dirty = match &ins.op {
                Op::Load { addr, .. } | Op::Store { addr, .. } => !local(*addr),
                Op::FieldAddr { base, .. } => !local(*base),
                Op::IndexAddr { .. } | Op::Index { .. } | Op::Lookup { .. } | Op::Slice { .. } => {
                    true
                }
                Op::BinOp {
                    kind: BinOpKind::Div | BinOpKind::Rem,
                    ..
                } => true,
                Op::Convert { dst, src } => {
                    let (dt, st) = (
                        p.types().kind(f.value(*dst).ty),
                        p.types().kind(f.value(*src).ty),
                    );
                    match (dt, st) {
                        (TypeKind::Basic { name: d }, TypeKind::Basic { name: s }) => {
                            match (int_width(d), int_width(s)) {
                                (Some(dw), Some(sw)) => dw < sw, // narrowing
                                _ => true,                       // unknown basics: not clean
                            }
                        }
                        _ => true,
                    }
                }
                _ => false,
            };
            if dirty {
                return false;
            }
        }
    }
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Domains {
    pub value_clean: bool,
    pub concurrency_clean: bool,
}

#[cfg(test)]
mod tests {
    use goverify_extract::gvir;
    use goverify_ir::Program;

    use super::*;
    use crate::testpkg::{block, func_with_params, instr, pkg};

    fn ty(id: u32, repr: &str, kind: gvir::TypeKind, name: &str) -> gvir::Type {
        gvir::Type {
            id,
            repr: repr.into(),
            kind: kind as i32,
            name: name.into(),
            ..Default::default()
        }
    }

    fn param(id: u32, t: u32) -> gvir::Param {
        gvir::Param {
            id,
            name: format!("p{id}"),
            r#type: t,
        }
    }

    fn build(
        types: Vec<gvir::Type>,
        params: Vec<gvir::Param>,
        instrs: Vec<gvir::Instruction>,
    ) -> Program {
        let mut package = pkg(
            "t",
            vec![func_with_params(
                "t.F",
                params,
                vec![block(0, instrs, vec![])],
            )],
        );
        package.types = types;
        Program::from_packages(vec![package])
    }

    fn clean(p: &Program) -> bool {
        value_clean(p, p.lookup_func("t.F").unwrap())
    }

    #[test]
    fn param_deref_is_not_clean() {
        let mut load = instr("UnOp");
        load.register = 2;
        load.operands = vec![1];
        load.sem = Some(gvir::instruction::Sem::Unop(gvir::UnOpSem {
            op: "*".into(),
            ..Default::default()
        }));
        let mut pointer = ty(2, "*int", gvir::TypeKind::Pointer, "");
        pointer.elem = 1;
        let p = build(
            vec![ty(1, "int", gvir::TypeKind::Basic, "int"), pointer],
            vec![param(1, 2)],
            vec![load, instr("Return")],
        );
        assert!(!clean(&p));
    }

    #[test]
    fn pure_arithmetic_is_clean() {
        let mut add = instr("BinOp");
        add.register = 3;
        add.operands = vec![1, 2];
        add.sem = Some(gvir::instruction::Sem::Binop(gvir::BinOpSem {
            op: "+".into(),
        }));
        let p = build(
            vec![ty(1, "int", gvir::TypeKind::Basic, "int")],
            vec![param(1, 1), param(2, 1)],
            vec![add, instr("Return")],
        );
        assert!(clean(&p));
    }

    #[test]
    fn division_is_not_clean() {
        let mut div = instr("BinOp");
        div.register = 3;
        div.operands = vec![1, 2];
        div.sem = Some(gvir::instruction::Sem::Binop(gvir::BinOpSem {
            op: "/".into(),
        }));
        let p = build(
            vec![ty(1, "int", gvir::TypeKind::Basic, "int")],
            vec![param(1, 1), param(2, 1)],
            vec![div, instr("Return")],
        );
        assert!(!clean(&p));
    }

    #[test]
    fn narrowing_convert_is_not_clean() {
        let mut conv = instr("Convert");
        conv.register = 2;
        conv.r#type = 2; // int8 result
        conv.operands = vec![1];
        let p = build(
            vec![
                ty(1, "int64", gvir::TypeKind::Basic, "int64"),
                ty(2, "int8", gvir::TypeKind::Basic, "int8"),
            ],
            vec![param(1, 1)],
            vec![conv, instr("Return")],
        );
        assert!(!clean(&p));
    }
}
