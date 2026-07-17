//! The analyzer-owned instruction set (phase-2 spec §3.2). Checkers see
//! only these ops — x/tools SSA quirks stop at lower.rs.

use crate::func::ValueId;
use crate::program::FuncId;
use crate::types::TypeId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum BinOpKind {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    And,
    Or,
    Xor,
    Shl,
    Shr,
    AndNot,
    Eq,
    Neq,
    Lt,
    Leq,
    Gt,
    Geq,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOpKind {
    Neg,
    Not,
    BitNot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MakeKind {
    Chan,
    Map,
    Slice,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LockKind {
    Lock,
    Unlock,
    RLock,
    RUnlock,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Callee {
    Static(FuncId),
    /// Interface method call: resolved by the call graph (Task 9).
    Invoke {
        iface: TypeId,
        method: String,
        sig: TypeId,
    },
    Builtin(String),
    /// Function-value call through `value`.
    Dynamic {
        value: ValueId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectArm {
    pub dir: u32, // types.ChanDir: 1 send, 2 recv
    pub chan: ValueId,
    pub send: Option<ValueId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    Assign {
        dst: ValueId,
        src: ValueId,
    },
    Alloc {
        dst: ValueId,
        heap: bool,
    },
    Load {
        dst: ValueId,
        addr: ValueId,
    },
    Store {
        addr: ValueId,
        val: ValueId,
    },
    FieldAddr {
        dst: ValueId,
        base: ValueId,
        field: u32,
    },
    Field {
        dst: ValueId,
        base: ValueId,
        field: u32,
    },
    IndexAddr {
        dst: ValueId,
        base: ValueId,
        index: ValueId,
    },
    Index {
        dst: ValueId,
        base: ValueId,
        index: ValueId,
    },
    Lookup {
        dst: ValueId,
        map: ValueId,
        key: ValueId,
        comma_ok: bool,
    },
    Slice {
        dst: ValueId,
        base: ValueId,
        low: Option<ValueId>,
        high: Option<ValueId>,
        max: Option<ValueId>,
    },
    BinOp {
        dst: ValueId,
        kind: BinOpKind,
        lhs: ValueId,
        rhs: ValueId,
    },
    UnOp {
        dst: ValueId,
        kind: UnOpKind,
        operand: ValueId,
    },
    Convert {
        dst: ValueId,
        src: ValueId,
    },
    Extract {
        dst: ValueId,
        tuple: ValueId,
        index: u32,
    },
    Phi {
        dst: ValueId,
        edges: Vec<ValueId>,
    },
    Call {
        dst: Option<ValueId>,
        callee: Callee,
        args: Vec<ValueId>,
    },
    MakeClosure {
        dst: ValueId,
        func: FuncId,
        bindings: Vec<ValueId>,
    },
    MakeInterface {
        dst: ValueId,
        src: ValueId,
    },
    Make {
        dst: ValueId,
        kind: MakeKind,
        args: Vec<ValueId>,
    },
    Send {
        chan: ValueId,
        val: ValueId,
    },
    Recv {
        dst: ValueId,
        chan: ValueId,
        comma_ok: bool,
    },
    CloseChan {
        chan: ValueId,
    },
    Select {
        dst: ValueId,
        arms: Vec<SelectArm>,
        blocking: bool,
    },
    Go {
        callee: Callee,
        args: Vec<ValueId>,
    },
    Defer {
        callee: Callee,
        args: Vec<ValueId>,
    },
    Return {
        vals: Vec<ValueId>,
    },
    Jump,
    Branch {
        cond: ValueId,
    },
    Panic {
        val: ValueId,
    },
    TypeAssert {
        dst: ValueId,
        src: ValueId,
        asserted: TypeId,
        comma_ok: bool,
    },
    Lock {
        kind: LockKind,
        mu: ValueId,
    },
    /// The explicit "not modeled" op. dst is havoc'd when present.
    Havoc {
        dst: Option<ValueId>,
    },
}
