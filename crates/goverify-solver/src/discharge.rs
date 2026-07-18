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
}
