//! Analysis engine: SCC scheduler, pre-pass, summary instantiation,
//! bounded fixpoint (phase 2; parent spec §2).

mod effects;
mod engine;
mod prepass;
mod summary;
#[cfg(test)]
mod testpkg;

pub use effects::{ChanOp, Effects, LockOp, Spawns, collect};
pub use engine::{
    Analysis, Finding, Options, analyze, analyze_with_solver, discharge, dump_prepass,
    dump_summaries,
};
pub use prepass::{Domains, value_clean};
pub use summary::{
    BoundClause, Clause, IfaceVar, PlaceholderFormula, Provenance, Summary, instantiate_requires,
};
