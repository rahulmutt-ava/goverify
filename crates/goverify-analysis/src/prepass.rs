//! Pre-pass, value domain (phase-2 spec §5): a function is value-clean if
//! nothing in it can raise a value obligation — no deref of a
//! non-locally-allocated pointer, no indexing/lookup/slicing, no
//! division/remainder, no narrowing conversion. Syntactic and
//! intraprocedural by design; sound direction: false (=not clean) is
//! always safe, so unknowns are not clean.

use std::collections::HashSet;

use goverify_ir::{BinOpKind, FuncId, Op, Program, TypeKind, ValueId};

/// Numeric family (final-review I1): `int_width` used to map int64,
/// uint64, and float64 all to width 64 and compare widths alone, so
/// int64->uint64 (sign reinterpretation) and float64->int64 (lossy
/// truncation) both satisfied `dw < sw` being false and were classified
/// clean — unsound for a phase-4+ value obligation. `SignedInt` and
/// `UnsignedInt` are distinct variants (not one `Int` variant plus a
/// separate signedness bool) precisely so a same-width, cross-signedness
/// conversion can never accidentally compare equal to a same-family one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NumFamily {
    SignedInt,
    UnsignedInt,
    Float,
}

fn num_info(name: &str) -> Option<(NumFamily, u32)> {
    use NumFamily::{Float, SignedInt, UnsignedInt};
    Some(match name {
        "int8" => (SignedInt, 8),
        "int16" => (SignedInt, 16),
        "int32" | "rune" => (SignedInt, 32),
        "int" | "int64" => (SignedInt, 64),
        "uint8" | "byte" => (UnsignedInt, 8),
        "uint16" => (UnsignedInt, 16),
        "uint32" => (UnsignedInt, 32),
        "uint" | "uint64" | "uintptr" => (UnsignedInt, 64),
        "float32" => (Float, 32),
        "float64" => (Float, 64),
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
                            match (num_info(d), num_info(s)) {
                                // Clean only same-family (int vs uint vs
                                // float, signedness included) AND
                                // non-narrowing; anything cross-family
                                // (int64->uint64, float64->int64) or
                                // narrowing (int64->int32) is dirty.
                                (Some((df, dw)), Some((sf, sw))) => df != sf || dw < sw,
                                _ => true, // unknown basics: not clean
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

    /// Regression (final-review I1): int64 -> uint64 is a same-width sign
    /// reinterpretation, not a narrowing conversion; `int_width` used to
    /// map both to width 64 and classify this clean.
    #[test]
    fn cross_signedness_same_width_convert_is_not_clean() {
        let mut conv = instr("Convert");
        conv.register = 2;
        conv.r#type = 2; // uint64 result
        conv.operands = vec![1];
        let p = build(
            vec![
                ty(1, "int64", gvir::TypeKind::Basic, "int64"),
                ty(2, "uint64", gvir::TypeKind::Basic, "uint64"),
            ],
            vec![param(1, 1)],
            vec![conv, instr("Return")],
        );
        assert!(!clean(&p));
    }

    /// Regression (final-review I1): float64 -> int64 is a lossy
    /// truncation despite matching width; must not be clean.
    #[test]
    fn float_to_int_same_width_convert_is_not_clean() {
        let mut conv = instr("Convert");
        conv.register = 2;
        conv.r#type = 2; // int64 result
        conv.operands = vec![1];
        let p = build(
            vec![
                ty(1, "float64", gvir::TypeKind::Basic, "float64"),
                ty(2, "int64", gvir::TypeKind::Basic, "int64"),
            ],
            vec![param(1, 1)],
            vec![conv, instr("Return")],
        );
        assert!(!clean(&p));
    }

    /// int32 -> int64: same family (signed int), widening — clean.
    #[test]
    fn widening_same_family_convert_is_clean() {
        let mut conv = instr("Convert");
        conv.register = 2;
        conv.r#type = 2; // int64 result
        conv.operands = vec![1];
        let p = build(
            vec![
                ty(1, "int32", gvir::TypeKind::Basic, "int32"),
                ty(2, "int64", gvir::TypeKind::Basic, "int64"),
            ],
            vec![param(1, 1)],
            vec![conv, instr("Return")],
        );
        assert!(clean(&p));
    }

    /// int64 -> int32: same family (signed int), narrowing — not clean.
    #[test]
    fn narrowing_same_family_convert_is_not_clean() {
        let mut conv = instr("Convert");
        conv.register = 2;
        conv.r#type = 2; // int32 result
        conv.operands = vec![1];
        let p = build(
            vec![
                ty(1, "int64", gvir::TypeKind::Basic, "int64"),
                ty(2, "int32", gvir::TypeKind::Basic, "int32"),
            ],
            vec![param(1, 1)],
            vec![conv, instr("Return")],
        );
        assert!(!clean(&p));
    }
}
