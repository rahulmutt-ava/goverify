//! Analyzer-owned SSA-style IR + call graph (phase 2).

mod callgraph;
mod dump;
mod func;
mod lower;
mod op;
mod program;
#[doc(hidden)]
pub mod testutil;
mod types;

pub use callgraph::{CallGraph, Sccs};
pub use dump::{dump_callgraph, dump_function, dump_sccs};
pub use func::{Block, ConstVal, Function, Instr, Pos, ValueId, ValueInfo, ValueKind};
pub use op::{BinOpKind, Callee, LockKind, MakeKind, Op, SelectArm, UnOpKind};
pub use program::{FuncId, MethodInfo, Program};
pub use types::{FieldInfo, TypeId, TypeKind, TypeTable};
