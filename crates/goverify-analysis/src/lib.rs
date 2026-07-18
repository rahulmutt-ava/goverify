//! Analysis engine: SCC scheduler, pre-pass, summary instantiation,
//! bounded fixpoint (phase 2; parent spec §2).

mod effects;
mod engine;
mod prepass;
mod summary;
#[cfg(test)]
mod testpkg;

pub use effects::{ChanOp, Effects, Loc, LockOp, Root, Spawns, collect};
pub use engine::{
    Analysis, Finding, Options, analyze, analyze_with_solver, discharge, dump_prepass,
    dump_summaries,
};
pub use prepass::{Domains, value_clean};
pub use summary::{
    BoundClause, Clause, Formula, IfaceVar, Provenance, Summary, iface_var_name,
    instantiate_requires,
};
