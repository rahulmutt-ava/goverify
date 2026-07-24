//! Retry-on-Unknown tier (wave-2 spec §2): pairs a base backend with
//! an escalated-timeout twin. The retry itself lives in
//! `discharge_query`, above the per-tier cache lookups — a wrapper
//! below the cache would replay cached base-tier Unknowns forever (the
//! C221 trap). Honesty clause: wall-clock timeouts are machine- and
//! load-sensitive; the tier narrows the flake window (a query must now
//! straddle the escalated timeout to flake), it does not eliminate it.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::{QueryOutcome, SolverLimits, TextSolver};

static ESCALATIONS: AtomicU64 = AtomicU64::new(0);

/// Process-wide count of retry-tier escalations, for diagnostic
/// reporting (shakeout G5). Monotonic; never feeds verdicts or output.
pub fn escalation_count() -> u64 {
    ESCALATIONS.load(Ordering::Relaxed)
}

pub(crate) fn note_escalation() {
    ESCALATIONS.fetch_add(1, Ordering::Relaxed);
}

/// A base backend plus its escalated tier. identity/limits/solve_text
/// all delegate to the base — to `discharge_query` this IS the base
/// backend until an Unknown makes it consult `escalation()`.
pub struct RetryBackend {
    base: Box<dyn TextSolver>,
    escalated: Box<dyn TextSolver>,
}

impl RetryBackend {
    pub fn new(base: Box<dyn TextSolver>, escalated: Box<dyn TextSolver>) -> RetryBackend {
        RetryBackend { base, escalated }
    }
}

impl TextSolver for RetryBackend {
    fn identity(&self) -> String {
        self.base.identity()
    }
    fn limits(&self) -> SolverLimits {
        self.base.limits()
    }
    fn solve_text(&mut self, canonical: &str) -> QueryOutcome {
        self.base.solve_text(canonical)
    }
    fn escalation(&mut self) -> Option<&mut dyn TextSolver> {
        Some(&mut *self.escalated)
    }
}

/// A TextSolver whose inner backend is constructed on first
/// `solve_text` (wave-2 follow-up: the escalated tier's Z3 context was
/// allocated per SCC but used only when a query actually escalates).
/// `identity`/`limits` are carried as data so the query-cache key can
/// be computed without forcing construction.
pub struct LazySolver {
    identity: String,
    limits: SolverLimits,
    make: Box<dyn FnMut() -> Box<dyn TextSolver> + Send>,
    inner: Option<Box<dyn TextSolver>>,
}

impl LazySolver {
    pub fn new(
        identity: String,
        limits: SolverLimits,
        make: Box<dyn FnMut() -> Box<dyn TextSolver> + Send>,
    ) -> LazySolver {
        LazySolver {
            identity,
            limits,
            make,
            inner: None,
        }
    }
}

impl TextSolver for LazySolver {
    fn identity(&self) -> String {
        self.identity.clone()
    }
    fn limits(&self) -> SolverLimits {
        self.limits
    }
    fn solve_text(&mut self, canonical: &str) -> QueryOutcome {
        if self.inner.is_none() {
            self.inner = Some((self.make)());
        }
        self.inner
            .as_mut()
            .expect("just constructed")
            .solve_text(canonical)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;
    use crate::{QueryOutcome, SatResult};

    struct CountingFake;
    impl TextSolver for CountingFake {
        fn identity(&self) -> String {
            "fake".to_string()
        }
        fn limits(&self) -> SolverLimits {
            SolverLimits::default()
        }
        fn solve_text(&mut self, _canonical: &str) -> QueryOutcome {
            QueryOutcome {
                result: SatResult::Unsat,
                model: None,
            }
        }
    }

    #[test]
    fn lazy_solver_defers_construction_until_first_solve() {
        let built = Arc::new(AtomicU32::new(0));
        let b = built.clone();
        let mut lazy = LazySolver::new(
            "fake".to_string(),
            SolverLimits::default(),
            Box::new(move || {
                b.fetch_add(1, Ordering::SeqCst);
                Box::new(CountingFake)
            }),
        );
        // identity/limits answer WITHOUT constructing the inner solver.
        assert_eq!(lazy.identity(), "fake", "LazySolver::identity()");
        assert_eq!(
            lazy.limits(),
            SolverLimits::default(),
            "LazySolver::limits()"
        );
        assert_eq!(built.load(Ordering::SeqCst), 0, "no construction yet");
        // First solve constructs exactly once; second reuses.
        let _ = lazy.solve_text("(check-sat)\n");
        let _ = lazy.solve_text("(check-sat)\n");
        assert_eq!(built.load(Ordering::SeqCst), 1, "constructed exactly once");
    }
}
