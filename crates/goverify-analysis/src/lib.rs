//! Analysis engine: SCC scheduler, pre-pass, summary instantiation,
//! bounded fixpoint (phase 2; parent spec §2).

mod effects;
mod summary;

pub use effects::{ChanOp, Effects, LockOp, Spawns};
pub use summary::{
    BoundClause, Clause, IfaceVar, PlaceholderFormula, Provenance, Summary, instantiate_requires,
};
