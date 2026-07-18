//! Checker trait (phase-3 spec §8): the plugin surface pluggable
//! checkers implement. `infer_requires` derives a function's own
//! preconditions from its body; `obligations` raises candidate
//! precondition violations at call sites, each carrying the `Query`
//! needed to discharge it. Task 12 threads `discharge_query` through
//! `Obligation::query` to produce `Finding`s and wires this trait into
//! `analyze_full` and the CLI.

use goverify_ir::{FuncId, Pos, Program};
use goverify_solver::{Query, SatResult};

use crate::summary::{Clause, Summary};

/// A candidate precondition violation raised at a call site. `query`
/// Sat ⇒ the violation is reachable ⇒ becomes a `Finding` (bug-finder
/// semantics, parent spec §8: Unsat/Unknown must stay silent).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Obligation {
    pub tag: String,
    pub message: String,
    pub pos: Option<Pos>,
    pub query: Query,
}

/// A reported violation (parent spec §10 rendering arrives in phase 4;
/// debug output in Task 12).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Finding {
    pub checker: String,
    pub func: String,
    pub pos: Option<Pos>,
    pub message: String,
}

pub trait Checker: Sync {
    fn name(&self) -> &'static str;

    /// Derive `f`'s own preconditions from its body. `discharge` lets
    /// the checker consult the solver without owning any backend/cache
    /// plumbing (the engine owns that, Task 12); a checker must only
    /// emit a requires-clause when the corresponding violation path is
    /// confirmed `Sat` — `Unknown` must never manufacture requires any
    /// more than it manufactures findings (parent spec §8).
    fn infer_requires(
        &self,
        p: &Program,
        f: FuncId,
        discharge: &mut dyn FnMut(&Query) -> SatResult,
    ) -> Vec<Clause>;

    /// Raise obligations at `f`'s call sites against each callee's
    /// summary (as resolved by `summary_of`).
    fn obligations(
        &self,
        p: &Program,
        f: FuncId,
        summary_of: &dyn Fn(FuncId) -> Summary,
    ) -> Vec<Obligation>;
}
