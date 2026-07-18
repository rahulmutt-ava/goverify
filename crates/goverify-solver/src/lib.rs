//! Solver abstraction (parent spec §8). `TextSolver` is the backend
//! contract (Z3Native, SmtLib2Process); `Solver` is the incremental,
//! typed-term interface the analysis engine drives, adapted onto any
//! `TextSolver` by `TermSolver`. `discharge_query` (phase-3 spec §8) is
//! the single entry point that renders canonical text once, consults the
//! query cache, and falls back to the backend on a miss.

mod discharge;
mod printer;
mod process;
mod reader;
mod sort;
mod term;
#[cfg(any(test, feature = "testgen"))]
#[doc(hidden)]
pub mod testgen;
mod z3native;

pub use discharge::discharge_query;
pub use printer::{Logic, Query};
pub use process::SmtLib2Process;
pub use reader::{ReadError, SExpr, parse_query, parse_response, parse_sexpr};
pub use sort::{CtorDecl, DatatypeDecl, Sort, SortError, ptr_datatype, ptr_sort};
pub use term::Term; // Term is now THE typed term; the AstTerm alias is gone.
pub use term::{BvBinOp, BvCmpOp, ptr_is_nil, ptr_nil};
pub use z3native::Z3Native;

/// A declaration for the incremental Solver interface (parent spec §8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decl {
    Const(String, Sort),
    Datatype(DatatypeDecl),
}

/// A satisfying model: the solver's textual rendering, display-only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Model(pub String);

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

/// Answers Unknown to everything (⇒ no report). Implements both solver
/// interfaces so tests and the engine default need no backend.
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

impl TextSolver for StubSolver {
    fn identity(&self) -> String {
        "stub".into()
    }
    fn limits(&self) -> SolverLimits {
        SolverLimits::default()
    }
    fn solve_text(&mut self, _canonical: &str) -> QueryOutcome {
        QueryOutcome {
            result: SatResult::Unknown,
            model: None,
        }
    }
}

/// Adapter: the incremental Solver interface over any TextSolver. Each
/// check renders ONE canonical query from the accumulated frames + the
/// assumptions (single-lowering rule holds: rendering is Query's).
pub struct TermSolver {
    backend: Box<dyn TextSolver>,
    logic: Logic,
    decls: Vec<Decl>,
    asserts: Vec<Term>,
    frames: Vec<(usize, usize)>,
    last_model: Option<Model>,
}

impl TermSolver {
    pub fn new(backend: Box<dyn TextSolver>, logic: Logic) -> TermSolver {
        TermSolver {
            backend,
            logic,
            decls: Vec::new(),
            asserts: Vec::new(),
            frames: Vec::new(),
            last_model: None,
        }
    }

    fn to_query(&self, assumptions: &[Term]) -> Query {
        let mut datatypes = Vec::new();
        let mut consts = Vec::new();
        for d in &self.decls {
            match d {
                Decl::Const(n, s) => consts.push((n.clone(), s.clone())),
                Decl::Datatype(dt) => datatypes.push(dt.clone()),
            }
        }
        let mut asserts = self.asserts.clone();
        asserts.extend_from_slice(assumptions);
        Query {
            logic: self.logic,
            datatypes,
            consts,
            asserts,
        }
    }
}

impl Solver for TermSolver {
    fn declare(&mut self, decl: Decl) {
        self.decls.push(decl);
    }
    fn assert(&mut self, term: Term) {
        self.asserts.push(term);
    }
    fn check_sat_assuming(&mut self, assumptions: &[Term]) -> SatResult {
        let out = self
            .backend
            .solve_text(&self.to_query(assumptions).canonical_text());
        self.last_model = match (&out.result, out.model) {
            (SatResult::Sat, Some(m)) => Some(Model(m)),
            _ => None,
        };
        out.result
    }
    fn model(&self) -> Option<Model> {
        self.last_model.clone()
    }
    fn push(&mut self) {
        self.frames.push((self.decls.len(), self.asserts.len()));
    }
    fn pop(&mut self) {
        if let Some((d, a)) = self.frames.pop() {
            self.decls.truncate(d);
            self.asserts.truncate(a);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scripted(Vec<SatResult>);
    impl TextSolver for Scripted {
        fn identity(&self) -> String {
            "scripted".into()
        }
        fn limits(&self) -> SolverLimits {
            SolverLimits::default()
        }
        fn solve_text(&mut self, _c: &str) -> QueryOutcome {
            QueryOutcome {
                result: self.0.remove(0),
                model: None,
            }
        }
    }

    #[test]
    fn stub_solver_always_answers_unknown() {
        let mut s = StubSolver;
        s.declare(Decl::Const("x".into(), Sort::Bool));
        s.push();
        s.assert(Term::var("x", Sort::Bool));
        assert_eq!(s.check_sat_assuming(&[]), SatResult::Unknown);
        assert!(s.model().is_none(), "Unknown must never expose a model");
        s.pop();
    }

    #[test]
    fn term_solver_frames_pop_asserts() {
        let mut s = TermSolver::new(
            Box::new(Scripted(vec![SatResult::Unsat, SatResult::Sat])),
            Logic::QfBv,
        );
        s.push();
        s.assert(Term::bool_lit(false));
        assert_eq!(s.check_sat_assuming(&[]), SatResult::Unsat);
        s.pop();
        assert_eq!(
            s.check_sat_assuming(&[]),
            SatResult::Sat,
            "popped assert must be gone"
        );
    }
}
