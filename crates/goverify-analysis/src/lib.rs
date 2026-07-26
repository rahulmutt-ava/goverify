//! Analysis engine: SCC scheduler, pre-pass, summary instantiation,
//! bounded fixpoint (phase 2; parent spec §2).

pub mod annotations;
mod checker;
mod dom;
mod effects;
mod encode;
mod engine;
mod prepass;
mod scc_cache;
mod summary;
#[cfg(test)]
mod testpkg;

pub use annotations::{
    AnnClause, Annotations, BAD_ANNOTATION, CONTRACT, FuncAnnotations, UNVERIFIED_ANNOTATION,
};
pub use checker::{Checker, Finding, Obligation, Severity, TraceStep};
pub use dom::{dominators, strictly_dominates};
pub use effects::{
    ChanOp, Effects, Loc, LockOp, Root, Spawns, closure_bindings, collect, cyclic_blocks,
};
pub use encode::{
    EncodedFunc, array_len, cut_back_edges, encode_func, encode_func_with, guard_values, int_repr,
    model_bindings, seq_datatype, sort_of, violating_path,
};
pub use engine::{
    Analysis, BackendRole, EngineConfig, Options, analyze, analyze_full, dump_findings,
    dump_prepass, dump_summaries,
};
pub use prepass::{Domains, value_clean};
pub use scc_cache::{CacheConfigKey, MemberEntry, SccCache, SccEntry, decode_entry_bytes};
pub use summary::{
    BoundClause, Clause, Formula, IfaceVar, Provenance, Summary, havoc_with, iface_var_name,
    instantiate_ensures, instantiate_requires, merge_annotations,
};
