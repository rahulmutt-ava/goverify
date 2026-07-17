//! Solver abstraction (parent spec §8). Phase 2 ships the trait and a
//! stub; Z3Native and SmtLib2Process arrive in phase 3. Decl/Term are
//! opaque SMT-LIB2 fragments for now — phase 3 replaces their innards
//! with the typed term language behind the same trait.

mod printer;
mod sort;
mod term;

pub use printer::{Logic, Query};
pub use sort::{CtorDecl, DatatypeDecl, Sort, SortError, ptr_datatype, ptr_sort};
pub use term::Term as AstTerm;
pub use term::{BvBinOp, BvCmpOp, ptr_is_nil, ptr_nil};

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
