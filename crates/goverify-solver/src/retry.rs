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
