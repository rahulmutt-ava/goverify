//! Function body representation: the lowered SSA-style IR (spec §3).
//!
//! Value ids are per-function and correspond 1:1 to the `.gvir` value
//! numbering (params, then aux values, then instruction registers), so
//! `ValueId(0)` never denotes a real value — it is reserved as the
//! "opaque"/absent slot returned when an operand or register is missing
//! or out of range in fuzzed input.

use crate::op::Op;
use crate::program::FuncId;
use crate::types::TypeId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ValueId(pub u32);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConstVal {
    Bool(bool),
    Int(i64),
    BigInt(String),
    Float(u64),
    Str(Vec<u8>),
    Nil,
    Complex(String),
    Opaque,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValueKind {
    Param,
    FreeVar,
    Const(ConstVal),
    Global(String),
    FuncRef(FuncId),
    Builtin(String),
    Instr,
    Opaque,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValueInfo {
    pub ty: TypeId,
    pub kind: ValueKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pos {
    pub file: String,
    pub line: u32,
    pub col: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instr {
    pub op: Op,
    pub pos: Option<Pos>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub instrs: Vec<Instr>,
    /// Successor block indices as raw wire values — NOT validated against
    /// `Function::blocks.len()`. Any consumer that indexes `blocks` with
    /// these must bounds-check (fuzzed input can carry arbitrary ids); see
    /// `effects::cyclic_blocks` for the filtering pattern.
    pub succs: Vec<u32>,
}

/// Lowered function body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Function {
    pub id: FuncId,
    pub sig: TypeId,
    pub params: Vec<ValueId>,
    pub values: Vec<ValueInfo>,
    pub blocks: Vec<Block>,
    pub pos: Option<Pos>,
    /// Fallback returned by `value()` for any id that isn't a real slot in
    /// `values` (out-of-range or the reserved 0 slot). Fuzzed input can
    /// reference arbitrary ids, so this lookup must be total, never panic.
    pub(crate) opaque: ValueInfo,
}

impl Function {
    /// Total lookup: out-of-range ids degrade to a shared Opaque/Unknown
    /// value rather than panicking.
    pub fn value(&self, v: ValueId) -> &ValueInfo {
        self.values.get(v.0 as usize).unwrap_or(&self.opaque)
    }
}
