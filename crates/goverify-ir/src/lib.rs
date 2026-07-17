//! Analyzer-owned SSA-style IR + call graph (phase 2).

mod func;
mod program;
mod types;

pub use func::Function;
pub use program::{FuncId, MethodInfo, Program};
pub use types::{FieldInfo, TypeId, TypeKind, TypeTable};
