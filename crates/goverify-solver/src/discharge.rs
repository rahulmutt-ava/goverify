//! The single solver-layer entry point (phase-3 spec §8): renders the
//! canonical text exactly once, keys the query cache with it, and on a
//! miss drives the backend with those same bytes.

use std::path::Path;

use goverify_cache::{CachedOutcome, QueryCache, QueryKeyParts, query_key};

use crate::printer::Query;
use crate::{QueryOutcome, SatResult, TextSolver};

pub fn discharge_query(
    q: &Query,
    backend: &mut dyn TextSolver,
    cache: Option<&QueryCache>,
    emit_dir: Option<&Path>,
) -> QueryOutcome {
    let out = discharge_one(q, backend, cache, emit_dir);
    if out.result == SatResult::Unknown
        && let Some(esc) = backend.escalation()
    {
        // Exactly one escalation: discharge_one never re-consults
        // escalation(), so nested RetryBackends still retry once.
        // emit_dir is None — the canonical bytes were already written.
        crate::retry::note_escalation();
        return discharge_one(q, esc, cache, None);
    }
    out
}

fn discharge_one(
    q: &Query,
    backend: &mut dyn TextSolver,
    cache: Option<&QueryCache>,
    emit_dir: Option<&Path>,
) -> QueryOutcome {
    let text = q.canonical_text();
    let limits = backend.limits();
    if let Some(dir) = emit_dir {
        // Deterministic filename = content hash; best-effort (diagnostic
        // surface only, never affects verdicts).
        let name = format!("{}.smt2", blake3::hash(text.as_bytes()).to_hex());
        let _ = std::fs::create_dir_all(dir);
        let _ = std::fs::write(dir.join(name), &text);
    }
    let key = cache.map(|_| {
        query_key(&QueryKeyParts {
            canonical: &text,
            solver_identity: &backend.identity(),
            timeout_ms: limits.timeout_ms,
            mem_mb: limits.mem_mb,
        })
    });
    if let (Some(c), Some(k)) = (cache, key.as_ref())
        && let Some(hit) = c.get(k)
    {
        return match hit {
            CachedOutcome::Sat { model } => QueryOutcome {
                result: SatResult::Sat,
                model,
            },
            CachedOutcome::Unsat => QueryOutcome {
                result: SatResult::Unsat,
                model: None,
            },
            CachedOutcome::Unknown => QueryOutcome {
                result: SatResult::Unknown,
                model: None,
            },
        };
    }
    let out = backend.solve_text(&text);
    if let (Some(c), Some(k)) = (cache, key.as_ref()) {
        let v = match &out {
            QueryOutcome {
                result: SatResult::Sat,
                model,
            } => CachedOutcome::Sat {
                model: model.clone(),
            },
            QueryOutcome {
                result: SatResult::Unsat,
                ..
            } => CachedOutcome::Unsat,
            QueryOutcome {
                result: SatResult::Unknown,
                ..
            } => CachedOutcome::Unknown,
        };
        let _ = c.put(k, &v); // cache write failure degrades to slower, never wrong
    }
    out
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;
    use crate::printer::{Logic, Query};
    use crate::term::Term;
    use crate::{QueryOutcome, SatResult, SolverLimits, TextSolver};

    /// Scripted backend: answers Sat, counts calls.
    struct Counting(&'static AtomicU32);

    impl TextSolver for Counting {
        fn identity(&self) -> String {
            "counting:1".into()
        }
        fn limits(&self) -> SolverLimits {
            SolverLimits::default()
        }
        fn solve_text(&mut self, _c: &str) -> QueryOutcome {
            self.0.fetch_add(1, Ordering::SeqCst);
            QueryOutcome {
                result: SatResult::Sat,
                model: Some("(model)".into()),
            }
        }
    }

    fn q(b: bool) -> Query {
        Query::for_asserts(Logic::QfBv, vec![Term::bool_lit(b)])
    }

    #[test]
    fn cache_hit_skips_backend() {
        static CALLS: AtomicU32 = AtomicU32::new(0);
        let dir = tempfile::tempdir().unwrap();
        let cache = goverify_cache::QueryCache::open(dir.path().to_path_buf());
        let mut backend = Counting(&CALLS);
        let a = discharge_query(&q(true), &mut backend, Some(&cache), None);
        let b = discharge_query(&q(true), &mut backend, Some(&cache), None);
        assert_eq!(
            CALLS.load(Ordering::SeqCst),
            1,
            "second call must be a cache hit"
        );
        assert_eq!(a, b, "hit reproduces outcome incl. model");
        assert_eq!(a.result, SatResult::Sat);
    }

    #[test]
    fn distinct_queries_distinct_entries() {
        static CALLS: AtomicU32 = AtomicU32::new(0);
        let dir = tempfile::tempdir().unwrap();
        let cache = goverify_cache::QueryCache::open(dir.path().to_path_buf());
        let mut backend = Counting(&CALLS);
        discharge_query(&q(true), &mut backend, Some(&cache), None);
        discharge_query(&q(false), &mut backend, Some(&cache), None);
        assert_eq!(CALLS.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn emit_smt_writes_canonical_bytes() {
        static CALLS: AtomicU32 = AtomicU32::new(0);
        let dir = tempfile::tempdir().unwrap();
        let mut backend = Counting(&CALLS);
        let query = q(true);
        discharge_query(&query, &mut backend, None, Some(dir.path()));
        let files: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(files.len(), 1);
        let content = std::fs::read_to_string(files[0].as_ref().unwrap().path()).unwrap();
        assert_eq!(content, query.canonical_text(), "artifact == solved bytes");
    }

    #[test]
    fn no_cache_no_emit_still_solves() {
        static CALLS: AtomicU32 = AtomicU32::new(0);
        let out = discharge_query(&q(true), &mut Counting(&CALLS), None, None);
        assert_eq!(out.result, SatResult::Sat);
    }

    /// One scripted tier: fixed answer, counts calls, distinct limits.
    struct Tier {
        limits: SolverLimits,
        answer: SatResult,
        calls: &'static AtomicU32,
    }

    impl TextSolver for Tier {
        fn identity(&self) -> String {
            "tier-fake".into()
        }
        fn limits(&self) -> SolverLimits {
            self.limits
        }
        fn solve_text(&mut self, _c: &str) -> QueryOutcome {
            self.calls.fetch_add(1, Ordering::SeqCst);
            QueryOutcome {
                result: self.answer,
                model: None,
            }
        }
    }

    fn tier(timeout_ms: u32, answer: SatResult, calls: &'static AtomicU32) -> Box<Tier> {
        Box::new(Tier {
            limits: SolverLimits {
                timeout_ms,
                mem_mb: 1024,
            },
            answer,
            calls,
        })
    }

    #[test]
    fn unknown_escalates_once_and_escalated_result_wins() {
        static BASE: AtomicU32 = AtomicU32::new(0);
        static ESC: AtomicU32 = AtomicU32::new(0);
        let before = crate::escalation_count();
        let mut b = crate::RetryBackend::new(
            tier(100, SatResult::Unknown, &BASE),
            tier(1000, SatResult::Unsat, &ESC),
        );
        let out = discharge_query(&q(true), &mut b, None, None);
        assert_eq!(out.result, SatResult::Unsat, "escalated result wins");
        assert_eq!(BASE.load(Ordering::SeqCst), 1, "base tier ran once");
        assert_eq!(ESC.load(Ordering::SeqCst), 1, "escalated tier ran once");
        assert!(
            crate::escalation_count() > before,
            "escalation counter must advance"
        );
    }

    #[test]
    fn definitive_base_answer_never_escalates() {
        static BASE: AtomicU32 = AtomicU32::new(0);
        static ESC: AtomicU32 = AtomicU32::new(0);
        let mut b = crate::RetryBackend::new(
            tier(100, SatResult::Unsat, &BASE),
            tier(1000, SatResult::Unsat, &ESC),
        );
        let out = discharge_query(&q(true), &mut b, None, None);
        assert_eq!(out.result, SatResult::Unsat);
        assert_eq!(ESC.load(Ordering::SeqCst), 0, "no wasted escalated query");
    }

    #[test]
    fn unknown_at_both_tiers_stays_unknown() {
        static BASE: AtomicU32 = AtomicU32::new(0);
        static ESC: AtomicU32 = AtomicU32::new(0);
        let mut b = crate::RetryBackend::new(
            tier(100, SatResult::Unknown, &BASE),
            tier(1000, SatResult::Unknown, &ESC),
        );
        let out = discharge_query(&q(true), &mut b, None, None);
        assert_eq!(
            out.result,
            SatResult::Unknown,
            "bug-finder semantics: still silent"
        );
        assert_eq!(
            ESC.load(Ordering::SeqCst),
            1,
            "exactly one escalation, no ladder"
        );
    }

    /// The C221-era trap, repaired: each tier caches under its own
    /// limits-bearing key, so a cached Unknown@base still triggers the
    /// escalation, and a cached Unsat@escalated resolves it with ZERO
    /// solver calls on the second run.
    #[test]
    fn retry_composes_with_cache_per_tier() {
        static BASE1: AtomicU32 = AtomicU32::new(0);
        static ESC1: AtomicU32 = AtomicU32::new(0);
        static BASE2: AtomicU32 = AtomicU32::new(0);
        static ESC2: AtomicU32 = AtomicU32::new(0);
        let dir = tempfile::tempdir().unwrap();
        let cache = goverify_cache::QueryCache::open(dir.path().to_path_buf());
        let mut b1 = crate::RetryBackend::new(
            tier(100, SatResult::Unknown, &BASE1),
            tier(1000, SatResult::Unsat, &ESC1),
        );
        let first = discharge_query(&q(true), &mut b1, Some(&cache), None);
        assert_eq!(first.result, SatResult::Unsat);
        let mut b2 = crate::RetryBackend::new(
            tier(100, SatResult::Unknown, &BASE2),
            tier(1000, SatResult::Unsat, &ESC2),
        );
        let second = discharge_query(&q(true), &mut b2, Some(&cache), None);
        assert_eq!(second.result, SatResult::Unsat, "resolved from cache");
        assert_eq!(
            BASE2.load(Ordering::SeqCst),
            0,
            "base tier answered by cache"
        );
        assert_eq!(
            ESC2.load(Ordering::SeqCst),
            0,
            "escalated tier answered by cache"
        );
    }
}
