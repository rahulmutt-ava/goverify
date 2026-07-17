//! Analyzer-owned SSA-style IR + call graph (phase 2).

mod func;
mod lower;
mod op;
mod program;
mod types;

pub use func::{Block, ConstVal, Function, Instr, Pos, ValueId, ValueInfo, ValueKind};
pub use op::{BinOpKind, Callee, LockKind, MakeKind, Op, SelectArm, UnOpKind};
pub use program::{FuncId, MethodInfo, Program};
pub use types::{FieldInfo, TypeId, TypeKind, TypeTable};
