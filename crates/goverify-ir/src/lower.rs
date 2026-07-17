//! gvir → IR lowering (spec §3). Lowering is TOTAL: unmodeled instruction
//! kinds and any out-of-range id from the wire degrade to `Op::Havoc` plus
//! a diagnostic, never a panic — this reads fuzzed input. Calls, defers,
//! goroutines, selects, and closures get real ops here; the
//! `sync.Mutex`/`sync.RWMutex` lock methods and the `close` builtin are
//! rewritten to `Op::Lock`/`Op::CloseChan` intrinsics.

use goverify_extract::gvir;
use goverify_extract::gvir::instruction::Sem;

use crate::func::{Block, ConstVal, Function, Instr, Pos, ValueId, ValueInfo, ValueKind};
use crate::op::{BinOpKind, Callee, LockKind, MakeKind, Op, SelectArm, UnOpKind};
use crate::program::Program;
use crate::types::TypeId;

impl Program {
    /// Lower every function body in `pkg`. Bodyless (external) functions
    /// stay `None` in the program's function table.
    pub(crate) fn lower_package(&mut self, pkg: &gvir::Package, tmap: &[TypeId]) {
        for gf in &pkg.functions {
            if gf.blocks.is_empty() {
                continue; // bodyless: stays None (external)
            }
            let f = self.lower_function(pkg, gf, tmap);
            let id = self.intern_func(&gf.id);
            self.set_func_body(id, f);
        }
    }

    fn lower_function(
        &mut self,
        pkg: &gvir::Package,
        gf: &gvir::Function,
        tmap: &[TypeId],
    ) -> Function {
        let unknown = self.types_unknown();
        let id = self.intern_func(&gf.id);

        // Value table: index = .gvir value id (1-based; slot 0 is the
        // reserved opaque/absent slot). Sized from counts, not from any
        // single id's raw value, so a fuzzed huge `register`/`id` can't
        // drive an allocation bomb — same guard shape as
        // `TypeTable::import_package`'s cap.
        let max_id = value_id_ceiling(gf);
        let mut values = vec![
            ValueInfo {
                ty: unknown,
                kind: ValueKind::Opaque
            };
            max_id + 1
        ];

        let mut params = Vec::with_capacity(gf.params.len());
        for p in &gf.params {
            if p.id != 0
                && let Some(slot) = values.get_mut(p.id as usize)
            {
                *slot = ValueInfo {
                    ty: resolve_ty(tmap, unknown, p.r#type),
                    kind: ValueKind::Param,
                };
                params.push(ValueId(p.id));
            }
        }

        for a in &gf.aux {
            let kind = match a.kind.as_str() {
                "Const" => ValueKind::Const(lower_const(a)),
                "Global" => ValueKind::Global(a.repr.clone()),
                "Function" => ValueKind::FuncRef(self.intern_func(&a.repr)),
                "Builtin" => ValueKind::Builtin(a.repr.clone()),
                "FreeVar" => ValueKind::FreeVar,
                _ => ValueKind::Opaque,
            };
            if a.id != 0
                && let Some(slot) = values.get_mut(a.id as usize)
            {
                *slot = ValueInfo {
                    ty: resolve_ty(tmap, unknown, a.r#type),
                    kind,
                };
            }
        }

        // Register slots get ValueKind::Instr up front, before lowering
        // any instruction body, so forward references (e.g. Phi edges
        // from a loop back-edge) resolve to the right type.
        for b in &gf.blocks {
            for ins in &b.instrs {
                if ins.register != 0
                    && let Some(slot) = values.get_mut(ins.register as usize)
                {
                    *slot = ValueInfo {
                        ty: resolve_ty(tmap, unknown, ins.r#type),
                        kind: ValueKind::Instr,
                    };
                }
            }
        }

        let blocks = gf
            .blocks
            .iter()
            .map(|b| Block {
                succs: b.succs.clone(),
                instrs: b
                    .instrs
                    .iter()
                    .filter_map(|ins| self.lower_instr(pkg, gf, ins, tmap, &values))
                    .collect(),
            })
            .collect();

        Function {
            id,
            sig: resolve_ty(tmap, unknown, gf.r#type),
            params,
            values,
            blocks,
            pos: lower_pos(pkg, &gf.pos),
            opaque: ValueInfo {
                ty: unknown,
                kind: ValueKind::Opaque,
            },
        }
    }

    /// Lower one instruction. Every path returns `Some(instr)` (possibly a
    /// `Havoc`) or `None` (deliberately dropped — no analyzer-visible
    /// semantics); nothing here can panic on malformed input.
    fn lower_instr(
        &mut self,
        pkg: &gvir::Package,
        gf: &gvir::Function,
        ins: &gvir::Instruction,
        tmap: &[TypeId],
        values: &[ValueInfo],
    ) -> Option<Instr> {
        let v = |i: usize| ValueId(ins.operands.get(i).copied().unwrap_or(0));
        let vopt = |i: usize| {
            ins.operands
                .get(i)
                .copied()
                .filter(|&x| x != 0)
                .map(ValueId)
        };
        let dst = (ins.register != 0).then_some(ValueId(ins.register));
        let pos = lower_pos(pkg, &ins.pos);

        let op = match ins.kind.as_str() {
            "Alloc" => {
                let Some(d) = dst else {
                    return Some(self.havoc(gf, ins, dst, pos));
                };
                Op::Alloc {
                    dst: d,
                    heap: matches!(&ins.sem, Some(Sem::Alloc(a)) if a.heap),
                }
            }
            "BinOp" => {
                let Some(d) = dst else {
                    return Some(self.havoc(gf, ins, dst, pos));
                };
                match &ins.sem {
                    Some(Sem::Binop(b)) => match binop_kind(&b.op) {
                        Some(kind) => Op::BinOp {
                            dst: d,
                            kind,
                            lhs: v(0),
                            rhs: v(1),
                        },
                        None => return Some(self.havoc(gf, ins, dst, pos)),
                    },
                    _ => return Some(self.havoc(gf, ins, dst, pos)),
                }
            }
            "UnOp" => {
                let Some(d) = dst else {
                    return Some(self.havoc(gf, ins, dst, pos));
                };
                match &ins.sem {
                    Some(Sem::Unop(u)) => match u.op.as_str() {
                        "*" => Op::Load { dst: d, addr: v(0) },
                        "<-" => Op::Recv {
                            dst: d,
                            chan: v(0),
                            comma_ok: u.comma_ok,
                        },
                        "-" => Op::UnOp {
                            dst: d,
                            kind: UnOpKind::Neg,
                            operand: v(0),
                        },
                        "!" => Op::UnOp {
                            dst: d,
                            kind: UnOpKind::Not,
                            operand: v(0),
                        },
                        "^" => Op::UnOp {
                            dst: d,
                            kind: UnOpKind::BitNot,
                            operand: v(0),
                        },
                        _ => return Some(self.havoc(gf, ins, dst, pos)),
                    },
                    _ => return Some(self.havoc(gf, ins, dst, pos)),
                }
            }
            "Store" => Op::Store {
                addr: v(0),
                val: v(1),
            },
            "FieldAddr" | "Field" => {
                let Some(d) = dst else {
                    return Some(self.havoc(gf, ins, dst, pos));
                };
                let idx = match &ins.sem {
                    Some(Sem::Field(f)) => f.index,
                    _ => 0,
                };
                if ins.kind == "FieldAddr" {
                    Op::FieldAddr {
                        dst: d,
                        base: v(0),
                        field: idx,
                    }
                } else {
                    Op::Field {
                        dst: d,
                        base: v(0),
                        field: idx,
                    }
                }
            }
            "IndexAddr" => {
                let Some(d) = dst else {
                    return Some(self.havoc(gf, ins, dst, pos));
                };
                Op::IndexAddr {
                    dst: d,
                    base: v(0),
                    index: v(1),
                }
            }
            "Index" => {
                let Some(d) = dst else {
                    return Some(self.havoc(gf, ins, dst, pos));
                };
                Op::Index {
                    dst: d,
                    base: v(0),
                    index: v(1),
                }
            }
            "Lookup" => {
                let Some(d) = dst else {
                    return Some(self.havoc(gf, ins, dst, pos));
                };
                Op::Lookup {
                    dst: d,
                    map: v(0),
                    key: v(1),
                    comma_ok: matches!(&ins.sem, Some(Sem::Lookup(l)) if l.comma_ok),
                }
            }
            "Slice" => {
                let Some(d) = dst else {
                    return Some(self.havoc(gf, ins, dst, pos));
                };
                Op::Slice {
                    dst: d,
                    base: v(0),
                    low: vopt(1),
                    high: vopt(2),
                    max: vopt(3),
                }
            }
            "Convert" | "ChangeInterface" | "SliceToArrayPointer" | "MultiConvert" => {
                let Some(d) = dst else {
                    return Some(self.havoc(gf, ins, dst, pos));
                };
                Op::Convert { dst: d, src: v(0) }
            }
            "ChangeType" => {
                let Some(d) = dst else {
                    return Some(self.havoc(gf, ins, dst, pos));
                };
                Op::Assign { dst: d, src: v(0) }
            }
            "Extract" => {
                let Some(d) = dst else {
                    return Some(self.havoc(gf, ins, dst, pos));
                };
                Op::Extract {
                    dst: d,
                    tuple: v(0),
                    index: match &ins.sem {
                        Some(Sem::Extract(e)) => e.index,
                        _ => 0,
                    },
                }
            }
            "Phi" => {
                let Some(d) = dst else {
                    return Some(self.havoc(gf, ins, dst, pos));
                };
                Op::Phi {
                    dst: d,
                    edges: ins.operands.iter().map(|&o| ValueId(o)).collect(),
                }
            }
            "MakeInterface" => {
                let Some(d) = dst else {
                    return Some(self.havoc(gf, ins, dst, pos));
                };
                Op::MakeInterface { dst: d, src: v(0) }
            }
            "MakeChan" => {
                let Some(d) = dst else {
                    return Some(self.havoc(gf, ins, dst, pos));
                };
                Op::Make {
                    dst: d,
                    kind: MakeKind::Chan,
                    args: vec![v(0)],
                }
            }
            "MakeMap" => {
                let Some(d) = dst else {
                    return Some(self.havoc(gf, ins, dst, pos));
                };
                Op::Make {
                    dst: d,
                    kind: MakeKind::Map,
                    args: ins.operands.iter().map(|&o| ValueId(o)).collect(),
                }
            }
            "MakeSlice" => {
                let Some(d) = dst else {
                    return Some(self.havoc(gf, ins, dst, pos));
                };
                Op::Make {
                    dst: d,
                    kind: MakeKind::Slice,
                    args: vec![v(0), v(1)],
                }
            }
            "MapUpdate" => Op::Store {
                addr: v(0),
                val: v(2),
            }, // map[k]=v as opaque store
            "Return" => Op::Return {
                vals: ins.operands.iter().map(|&o| ValueId(o)).collect(),
            },
            "Jump" => Op::Jump,
            "If" => Op::Branch { cond: v(0) },
            "Panic" => Op::Panic { val: v(0) },
            "TypeAssert" => {
                let Some(d) = dst else {
                    return Some(self.havoc(gf, ins, dst, pos));
                };
                match &ins.sem {
                    Some(Sem::TypeAssert(t)) => {
                        let unknown = self.types_unknown();
                        Op::TypeAssert {
                            dst: d,
                            src: v(0),
                            asserted: resolve_ty(tmap, unknown, t.asserted),
                            comma_ok: t.comma_ok,
                        }
                    }
                    _ => return Some(self.havoc(gf, ins, dst, pos)),
                }
            }
            "Send" => Op::Send {
                chan: v(0),
                val: v(1),
            },
            // Loop primitives havoc per spec §3.3: no diagnostic, this is
            // an intentional (not "unmodeled") degrade.
            "Range" | "Next" => Op::Havoc { dst },
            // Dropped: no analyzer-visible semantics (defers are recorded
            // at the `defer` op itself).
            "DebugRef" | "RunDefers" => return None,
            "Call" | "Defer" | "Go" => {
                let Some(Sem::Call(c)) = &ins.sem else {
                    return Some(self.havoc(gf, ins, dst, pos));
                };
                let unknown = self.types_unknown();
                // SSA operand layout: non-invoke: [callee, args…]; invoke:
                // [recv, args…] (the receiver is passed through as the
                // invoke call's first argument, mirroring x/tools SSA).
                let (callee, args): (Callee, Vec<ValueId>) = if c.invoke {
                    (
                        Callee::Invoke {
                            iface: resolve_ty(tmap, unknown, c.iface_type),
                            method: c.method.clone(),
                            sig: resolve_ty(tmap, unknown, c.method_sig),
                        },
                        ins.operands.iter().map(|&o| ValueId(o)).collect(),
                    )
                } else if !c.builtin.is_empty() {
                    (
                        Callee::Builtin(c.builtin.clone()),
                        ins.operands.iter().skip(1).map(|&o| ValueId(o)).collect(),
                    )
                } else if !c.static_callee.is_empty() {
                    (
                        Callee::Static(self.intern_func(&c.static_callee)),
                        ins.operands.iter().skip(1).map(|&o| ValueId(o)).collect(),
                    )
                } else {
                    (
                        Callee::Dynamic { value: v(0) },
                        ins.operands.iter().skip(1).map(|&o| ValueId(o)).collect(),
                    )
                };
                match ins.kind.as_str() {
                    "Go" => Op::Go { callee, args },
                    "Defer" => Op::Defer { callee, args },
                    _ => self.lower_plain_call(dst, callee, args),
                }
            }
            "MakeClosure" => {
                let Some(d) = dst else {
                    return Some(self.havoc(gf, ins, dst, pos));
                };
                // operands: [fn, bindings…]; fn is a Function aux value.
                let ValueKind::FuncRef(func) = value_kind_of(values, ins, 0) else {
                    return Some(self.havoc(gf, ins, dst, pos));
                };
                Op::MakeClosure {
                    dst: d,
                    func,
                    bindings: ins.operands.iter().skip(1).map(|&o| ValueId(o)).collect(),
                }
            }
            "Select" => {
                let Some(Sem::Select(s)) = &ins.sem else {
                    return Some(self.havoc(gf, ins, dst, pos));
                };
                let Some(d) = dst else {
                    return Some(self.havoc(gf, ins, dst, pos));
                };
                Op::Select {
                    dst: d,
                    blocking: s.blocking,
                    arms: s
                        .states
                        .iter()
                        .map(|st| SelectArm {
                            dir: st.dir,
                            chan: ValueId(st.chan_operand),
                            send: (st.send_operand != 0).then_some(ValueId(st.send_operand)),
                        })
                        .collect(),
                }
            }
            _ => return Some(self.havoc(gf, ins, dst, pos)),
        };
        Some(Instr { op, pos })
    }

    /// Rewrite a plain (non-Go, non-Defer) call to its intrinsic op when
    /// the callee is one of the modeled `sync` lock methods or the `close`
    /// builtin; otherwise a plain `Op::Call`. `args` still includes the
    /// receiver for a lock method (operand layout: [callee, receiver,
    /// …]), which is exactly the value `Op::Lock` wants as `mu`.
    fn lower_plain_call(&self, dst: Option<ValueId>, callee: Callee, args: Vec<ValueId>) -> Op {
        if let Callee::Static(f) = &callee
            && let Some(kind) = lock_kind(self.func_name(*f))
        {
            return Op::Lock {
                kind,
                mu: args.first().copied().unwrap_or(ValueId(0)),
            };
        }
        if let Callee::Builtin(name) = &callee
            && name == "close"
        {
            return Op::CloseChan {
                chan: args.first().copied().unwrap_or(ValueId(0)),
            };
        }
        Op::Call { dst, callee, args }
    }

    /// Shared fallback for both "instruction kind not modeled at all" and
    /// "a modeled kind arrived without the register it needs" (`dst` is
    /// already `None` in the latter case, so `Op::Havoc { dst }` is
    /// exactly `Op::Havoc { dst: None }` there). Pushes a diagnostic
    /// naming the kind so degraded input is still observable.
    fn havoc(
        &mut self,
        gf: &gvir::Function,
        ins: &gvir::Instruction,
        dst: Option<ValueId>,
        pos: Option<Pos>,
    ) -> Instr {
        self.push_diagnostic(format!(
            "{}: unmodeled instruction kind {:?}",
            gf.id, ins.kind
        ));
        Instr {
            op: Op::Havoc { dst },
            pos,
        }
    }
}

/// Upper bound for a function's per-value-id table. `.gvir` assigns ids
/// densely 1..=N (params, then aux values, then one per value-producing
/// instruction — see `gvir.proto`'s `Function` doc), so this count is a
/// safe upper bound on the legitimate maximum id. Bounding by count
/// rather than by any single id's raw value keeps a fuzzed input (e.g.
/// one instruction with `register: u32::MAX`) from allocating an
/// oversized table; any id beyond the cap just doesn't get a slot and
/// degrades to the shared opaque fallback via `Function::value`.
fn value_id_ceiling(gf: &gvir::Function) -> usize {
    let instr_count: usize = gf.blocks.iter().map(|b| b.instrs.len()).sum();
    gf.params.len() + gf.aux.len() + instr_count
}

fn resolve_ty(tmap: &[TypeId], unknown: TypeId, local: u32) -> TypeId {
    tmap.get(local as usize).copied().unwrap_or(unknown)
}

/// Maps the static callee of a `sync.Mutex`/`sync.RWMutex` lock method to
/// its `Op::Lock` intrinsic kind; `None` for every other function name.
fn lock_kind(name: &str) -> Option<LockKind> {
    match name {
        "(*sync.Mutex).Lock" | "(*sync.RWMutex).Lock" => Some(LockKind::Lock),
        "(*sync.Mutex).Unlock" | "(*sync.RWMutex).Unlock" => Some(LockKind::Unlock),
        "(*sync.RWMutex).RLock" => Some(LockKind::RLock),
        "(*sync.RWMutex).RUnlock" => Some(LockKind::RUnlock),
        _ => None,
    }
}

/// Look up the `ValueKind` of the value id at `ins.operands[idx]` in the
/// already-built per-function value table. Bounds-checked and total: a
/// missing operand or an out-of-range id degrades to `ValueKind::Opaque`
/// rather than panicking (fuzzed input).
fn value_kind_of(values: &[ValueInfo], ins: &gvir::Instruction, idx: usize) -> ValueKind {
    let id = ins.operands.get(idx).copied().unwrap_or(0) as usize;
    values
        .get(id)
        .map_or(ValueKind::Opaque, |vi| vi.kind.clone())
}

fn lower_const(a: &gvir::AuxValue) -> ConstVal {
    use gvir::const_value::Value as CV;
    match a.r#const.as_ref().and_then(|c| c.value.as_ref()) {
        Some(CV::Bool(b)) => ConstVal::Bool(*b),
        Some(CV::Int(i)) => ConstVal::Int(*i),
        Some(CV::BigInt(s)) => ConstVal::BigInt(s.clone()),
        Some(CV::FloatBits(f)) => ConstVal::Float(*f),
        Some(CV::Str(s)) => ConstVal::Str(s.clone()),
        Some(CV::Nil(_)) => ConstVal::Nil,
        Some(CV::Complex(s)) => ConstVal::Complex(s.clone()),
        None => ConstVal::Opaque,
    }
}

/// Resolve a `Position` through `pkg.files`. `pos` itself absent (the
/// whole field unset) yields `None`; a present position with an
/// unresolvable file index (0, or out of range on fuzzed input) still
/// yields `Some` — line/col remain useful even when the file is unknown.
fn lower_pos(pkg: &gvir::Package, pos: &Option<gvir::Position>) -> Option<Pos> {
    let p = pos.as_ref()?;
    let file = if p.file != 0 {
        pkg.files
            .get((p.file - 1) as usize)
            .map(|f| f.path.clone())
            .unwrap_or_default()
    } else {
        String::new()
    };
    Some(Pos {
        file,
        line: p.line,
        col: p.col,
    })
}

fn binop_kind(op: &str) -> Option<BinOpKind> {
    Some(match op {
        "+" => BinOpKind::Add,
        "-" => BinOpKind::Sub,
        "*" => BinOpKind::Mul,
        "/" => BinOpKind::Div,
        "%" => BinOpKind::Rem,
        "&" => BinOpKind::And,
        "|" => BinOpKind::Or,
        "^" => BinOpKind::Xor,
        "<<" => BinOpKind::Shl,
        ">>" => BinOpKind::Shr,
        "&^" => BinOpKind::AndNot,
        "==" => BinOpKind::Eq,
        "!=" => BinOpKind::Neq,
        "<" => BinOpKind::Lt,
        "<=" => BinOpKind::Leq,
        ">" => BinOpKind::Gt,
        ">=" => BinOpKind::Geq,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_pkg(instrs: Vec<gvir::Instruction>) -> gvir::Package {
        gvir::Package {
            import_path: "t".into(),
            types: vec![gvir::Type {
                id: 1,
                repr: "int".into(),
                kind: gvir::TypeKind::Basic as i32,
                name: "int".into(),
                ..Default::default()
            }],
            functions: vec![gvir::Function {
                id: "t.F".into(),
                params: vec![
                    gvir::Param {
                        id: 1,
                        name: "x".into(),
                        r#type: 1,
                    },
                    gvir::Param {
                        id: 2,
                        name: "y".into(),
                        r#type: 1,
                    },
                ],
                blocks: vec![gvir::BasicBlock {
                    index: 0,
                    instrs,
                    succs: vec![],
                }],
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn lowers_binop_and_return() {
        let pkg = test_pkg(vec![
            gvir::Instruction {
                kind: "BinOp".into(),
                register: 3,
                r#type: 1,
                operands: vec![1, 2],
                sem: Some(gvir::instruction::Sem::Binop(gvir::BinOpSem {
                    op: "+".into(),
                })),
                ..Default::default()
            },
            gvir::Instruction {
                kind: "Return".into(),
                operands: vec![3],
                ..Default::default()
            },
        ]);
        let p = Program::from_packages(vec![pkg]);
        let f = p.func(p.lookup_func("t.F").unwrap()).expect("lowered body");
        let ops: Vec<&Op> = f.blocks[0].instrs.iter().map(|i| &i.op).collect();
        assert!(
            matches!(
                ops[0],
                Op::BinOp {
                    kind: BinOpKind::Add,
                    ..
                }
            ),
            "{ops:?}"
        );
        assert!(matches!(ops[1], Op::Return { .. }), "{ops:?}");
    }

    #[test]
    fn unknown_kind_lowers_to_havoc_not_panic() {
        let pkg = test_pkg(vec![gvir::Instruction {
            kind: "FrobnicateV9".into(),
            register: 3,
            r#type: 1,
            ..Default::default()
        }]);
        let p = Program::from_packages(vec![pkg]);
        let f = p.func(p.lookup_func("t.F").unwrap()).unwrap();
        assert!(matches!(
            f.blocks[0].instrs[0].op,
            Op::Havoc { dst: Some(_) }
        ));
        assert!(p.diagnostics().iter().any(|d| d.contains("FrobnicateV9")));
    }

    #[test]
    fn huge_register_id_does_not_allocate_a_bomb() {
        // A single instruction claiming register: u32::MAX must not drive
        // an allocation anywhere near u32::MAX entries; the out-of-range
        // id simply never gets a table slot and later resolves through
        // `Function::value`'s bounds check instead of panicking.
        let pkg = test_pkg(vec![gvir::Instruction {
            kind: "Alloc".into(),
            register: u32::MAX,
            r#type: 1,
            ..Default::default()
        }]);
        let p = Program::from_packages(vec![pkg]); // must not panic or hang
        let f = p.func(p.lookup_func("t.F").unwrap()).unwrap();
        assert!(f.values.len() < 1_000, "{}", f.values.len());
        assert!(matches!(f.blocks[0].instrs[0].op, Op::Alloc { .. }));
        assert_eq!(f.value(ValueId(u32::MAX)).kind, ValueKind::Opaque);
    }

    #[test]
    fn lowers_static_call_and_lock_intrinsics() {
        // aux id 3 = Function "(*sync.Mutex).Lock"; call it with operand
        // order [callee, receiver]; also a plain static call to t.G.
        let pkg = gvir::Package {
            import_path: "t".into(),
            types: vec![gvir::Type {
                id: 1,
                repr: "*sync.Mutex".into(),
                kind: gvir::TypeKind::Pointer as i32,
                ..Default::default()
            }],
            functions: vec![gvir::Function {
                id: "t.F".into(),
                params: vec![gvir::Param {
                    id: 1,
                    name: "mu".into(),
                    r#type: 1,
                }],
                blocks: vec![gvir::BasicBlock {
                    index: 0,
                    instrs: vec![
                        gvir::Instruction {
                            kind: "Call".into(),
                            operands: vec![0, 1],
                            sem: Some(gvir::instruction::Sem::Call(gvir::CallSem {
                                static_callee: "(*sync.Mutex).Lock".into(),
                                ..Default::default()
                            })),
                            ..Default::default()
                        },
                        gvir::Instruction {
                            kind: "Call".into(),
                            operands: vec![0],
                            sem: Some(gvir::instruction::Sem::Call(gvir::CallSem {
                                static_callee: "t.G".into(),
                                ..Default::default()
                            })),
                            ..Default::default()
                        },
                        gvir::Instruction {
                            kind: "Return".into(),
                            ..Default::default()
                        },
                    ],
                    succs: vec![],
                }],
                ..Default::default()
            }],
            ..Default::default()
        };
        let p = Program::from_packages(vec![pkg]);
        let f = p.func(p.lookup_func("t.F").unwrap()).unwrap();
        let ops: Vec<&Op> = f.blocks[0].instrs.iter().map(|i| &i.op).collect();
        assert!(
            matches!(
                ops[0],
                Op::Lock {
                    kind: LockKind::Lock,
                    ..
                }
            ),
            "{ops:?}"
        );
        assert!(
            matches!(
                ops[1],
                Op::Call {
                    callee: Callee::Static(_),
                    ..
                }
            ),
            "{ops:?}"
        );
    }

    #[test]
    fn lowers_builtin_close_to_closechan() {
        // Call with sem.builtin = "close", operands [callee, ch] → Op::CloseChan { chan }.
        let pkg = gvir::Package {
            import_path: "t".into(),
            types: vec![gvir::Type {
                id: 1,
                repr: "chan struct{}".into(),
                kind: gvir::TypeKind::Chan as i32,
                ..Default::default()
            }],
            functions: vec![gvir::Function {
                id: "t.F".into(),
                params: vec![gvir::Param {
                    id: 1,
                    name: "ch".into(),
                    r#type: 1,
                }],
                blocks: vec![gvir::BasicBlock {
                    index: 0,
                    instrs: vec![
                        gvir::Instruction {
                            kind: "Call".into(),
                            operands: vec![0, 1],
                            sem: Some(gvir::instruction::Sem::Call(gvir::CallSem {
                                builtin: "close".into(),
                                ..Default::default()
                            })),
                            ..Default::default()
                        },
                        gvir::Instruction {
                            kind: "Return".into(),
                            ..Default::default()
                        },
                    ],
                    succs: vec![],
                }],
                ..Default::default()
            }],
            ..Default::default()
        };
        let p = Program::from_packages(vec![pkg]);
        let f = p.func(p.lookup_func("t.F").unwrap()).unwrap();
        let op = &f.blocks[0].instrs[0].op;
        assert!(matches!(op, Op::CloseChan { chan: ValueId(1) }), "{op:?}");
    }

    #[test]
    fn param_id_zero_does_not_clobber_opaque_sentinel() {
        // `.gvir` value id 0 is the universal "absent" sentinel (mirroring
        // type id 0). A malformed encoder could claim it for a real param
        // or aux value; that must not overwrite slot 0's reserved
        // ValueKind::Opaque, or every "missing operand" lookup (which
        // falls back to ValueId(0)) would silently resolve to whatever
        // clobbered it instead of staying opaque.
        let pkg = gvir::Package {
            import_path: "t".into(),
            types: vec![gvir::Type {
                id: 1,
                repr: "int".into(),
                kind: gvir::TypeKind::Basic as i32,
                name: "int".into(),
                ..Default::default()
            }],
            functions: vec![gvir::Function {
                id: "t.F".into(),
                params: vec![gvir::Param {
                    id: 0,
                    name: "bad".into(),
                    r#type: 1,
                }],
                aux: vec![gvir::AuxValue {
                    id: 0,
                    kind: "Global".into(),
                    repr: "also_bad".into(),
                    r#type: 1,
                    ..Default::default()
                }],
                blocks: vec![gvir::BasicBlock {
                    index: 0,
                    instrs: vec![gvir::Instruction {
                        kind: "Return".into(),
                        ..Default::default()
                    }],
                    succs: vec![],
                }],
                ..Default::default()
            }],
            ..Default::default()
        };
        let p = Program::from_packages(vec![pkg]);
        let f = p.func(p.lookup_func("t.F").unwrap()).unwrap();
        assert_eq!(
            f.value(ValueId(0)).kind,
            ValueKind::Opaque,
            "slot 0 must stay the reserved opaque sentinel"
        );
        assert!(
            f.params.is_empty(),
            "id-0 param must not be registered as a real param"
        );
    }
}
