//! Solver abstraction (parent spec §8). Phase 2 ships the trait and a
//! stub; Z3Native and SmtLib2Process arrive in phase 3. Decl/Term are
//! opaque SMT-LIB2 fragments for now — phase 3 replaces their innards
//! with the typed term language behind the same trait.

mod printer;
mod process;
mod reader;
mod sort;
mod term;
#[cfg(any(test, feature = "testgen"))]
#[doc(hidden)]
pub mod testgen;
mod z3native;

pub use printer::{Logic, Query};
pub use process::SmtLib2Process;
pub use reader::{ReadError, SExpr, parse_query, parse_response, parse_sexpr};
pub use sort::{CtorDecl, DatatypeDecl, Sort, SortError, ptr_datatype, ptr_sort};
pub use term::Term as AstTerm;
pub use term::{BvBinOp, BvCmpOp, ptr_is_nil, ptr_nil};
pub use z3native::Z3Native;

/// A declaration (sort, const, or function) in canonical SMT-LIB2 text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decl(pub String);

/// A term in canonical SMT-LIB2 text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Term(pub String);

/// A satisfying model. Opaque in phase 2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Model;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SatResult {
    Sat,
    Unsat,
    /// Includes timeouts. Bug-finder semantics: Unknown ⇒ no report
    /// (parent spec §8) — timeouts must never create false positives.
    Unknown,
}

pub trait Solver {
    fn declare(&mut self, decl: Decl);
    fn assert(&mut self, term: Term);
    fn check_sat_assuming(&mut self, assumptions: &[Term]) -> SatResult;
    fn model(&self) -> Option<Model>;
    fn push(&mut self);
    fn pop(&mut self);
}

/// Per-query resource caps (parent spec §8: default 100 ms).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SolverLimits {
    pub timeout_ms: u32,
    pub mem_mb: u32,
}

impl Default for SolverLimits {
    fn default() -> Self {
        SolverLimits {
            timeout_ms: 100,
            mem_mb: 1024,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryOutcome {
    pub result: SatResult,
    /// Model text, Sat only. Display-only — never parsed for decisions.
    pub model: Option<String>,
}

/// A backend that consumes canonical SMT-LIB2 bytes (single-lowering
/// rule, phase-3 spec §4). `identity()` feeds the query-cache key.
pub trait TextSolver: Send {
    fn identity(&self) -> String;
    fn limits(&self) -> SolverLimits;
    fn solve_text(&mut self, canonical: &str) -> QueryOutcome;
}

/// Answers Unknown to everything: with bug-finder semantics this means
/// "report nothing", which is exactly right while no checkers exist.
pub struct StubSolver;

impl Solver for StubSolver {
    fn declare(&mut self, _decl: Decl) {}
    fn assert(&mut self, _term: Term) {}
    fn check_sat_assuming(&mut self, _assumptions: &[Term]) -> SatResult {
        SatResult::Unknown
    }
    fn model(&self) -> Option<Model> {
        None
    }
    fn push(&mut self) {}
    fn pop(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_solver_always_answers_unknown() {
        let mut s = StubSolver;
        s.declare(Decl("(declare-const x Bool)".into()));
        s.push();
        s.assert(Term("x".into()));
        assert_eq!(s.check_sat_assuming(&[]), SatResult::Unknown);
        assert!(s.model().is_none(), "Unknown must never expose a model");
        s.pop();
    }
}
