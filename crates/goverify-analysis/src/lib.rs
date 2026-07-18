//! Analysis engine: SCC scheduler, pre-pass, summary instantiation,
//! bounded fixpoint (phase 2; parent spec §2).

mod checker;
mod effects;
mod encode;
mod engine;
mod prepass;
mod summary;
#[cfg(test)]
mod testpkg;

pub use checker::{Checker, Finding, Obligation, TraceStep};
pub use effects::{ChanOp, Effects, Loc, LockOp, Root, Spawns, collect};
pub use encode::{
    EncodedFunc, array_len, cut_back_edges, encode_func, int_repr, seq_datatype, sort_of,
};
pub use engine::{
    Analysis, BackendRole, EngineConfig, Options, analyze, analyze_full, dump_findings,
    dump_prepass, dump_summaries,
};
pub use prepass::{Domains, value_clean};
pub use summary::{
    BoundClause, Clause, Formula, IfaceVar, Provenance, Summary, iface_var_name,
    instantiate_requires,
};
