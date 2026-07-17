//! Query-cache layer (parent spec §9.3): key = blake3 over the canonical
//! SMT-LIB2 text ⊕ solver identity ⊕ limits (length-prefixed fields);
//! value = SatResult + model text for Sat. Unknown IS cached — it is
//! deterministic per solver build, which is exactly why identity and
//! limits are in the key.

use std::path::PathBuf;

use crate::store::Store;

pub struct QueryKeyParts<'a> {
    pub canonical: &'a str,
    pub solver_identity: &'a str,
    pub timeout_ms: u32,
    pub mem_mb: u32,
}

/// Length-prefixed field hashing: `("ab","c")` must never collide with
/// `("a","bc")` (phase-1 final-review deferred lesson).
pub fn query_key(parts: &QueryKeyParts) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    for field in [parts.canonical.as_bytes(), parts.solver_identity.as_bytes()] {
        h.update(&(field.len() as u64).to_le_bytes());
        h.update(field);
    }
    h.update(&parts.timeout_ms.to_le_bytes());
    h.update(&parts.mem_mb.to_le_bytes());
    *h.finalize().as_bytes()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CachedOutcome {
    Sat { model: Option<String> },
    Unsat,
    Unknown,
}

const VERSION: u8 = 1;
const LAYER: &str = "query";

fn encode(v: &CachedOutcome) -> Vec<u8> {
    let mut out = vec![VERSION];
    match v {
        CachedOutcome::Unsat => out.push(0),
        CachedOutcome::Sat { model } => {
            out.push(1);
            match model {
                None => out.push(0),
                Some(m) => {
                    out.push(1);
                    out.extend((m.len() as u32).to_le_bytes());
                    out.extend(m.as_bytes());
                }
            }
        }
        CachedOutcome::Unknown => out.push(2),
    }
    out
}

/// Bytes from disk: decode defensively, None on any mismatch (corrupt
/// entry = miss, parent §11). Trailing garbage is also a miss.
fn decode(b: &[u8]) -> Option<CachedOutcome> {
    match b {
        [VERSION, 0] => Some(CachedOutcome::Unsat),
        [VERSION, 2] => Some(CachedOutcome::Unknown),
        [VERSION, 1, 0] => Some(CachedOutcome::Sat { model: None }),
        [VERSION, 1, 1, rest @ ..] => {
            let (len, rest) = rest.split_first_chunk::<4>()?;
            let len = u32::from_le_bytes(*len) as usize;
            if rest.len() != len {
                return None;
            }
            Some(CachedOutcome::Sat {
                model: Some(String::from_utf8(rest.to_vec()).ok()?),
            })
        }
        _ => None,
    }
}

pub struct QueryCache {
    store: Store,
}

impl QueryCache {
    pub fn open(root: PathBuf) -> QueryCache {
        QueryCache {
            store: Store::open(root),
        }
    }

    pub fn get(&self, key: &[u8; 32]) -> Option<CachedOutcome> {
        decode(&self.store.get(LAYER, key)?)
    }

    pub fn put(&self, key: &[u8; 32], v: &CachedOutcome) -> std::io::Result<()> {
        self.store.put(LAYER, key, &encode(v))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts<'a>(c: &'a str, id: &'a str) -> QueryKeyParts<'a> {
        QueryKeyParts {
            canonical: c,
            solver_identity: id,
            timeout_ms: 100,
            mem_mb: 1024,
        }
    }

    #[test]
    fn key_is_stable_and_field_sensitive() {
        let k1 = query_key(&parts("(check-sat)\n", "z3-4.12"));
        assert_eq!(k1, query_key(&parts("(check-sat)\n", "z3-4.12")), "stable");
        assert_ne!(
            k1,
            query_key(&parts("(check-sat)\n", "z3-4.13")),
            "identity in key"
        );
        assert_ne!(
            k1,
            query_key(&QueryKeyParts {
                timeout_ms: 200,
                ..parts("(check-sat)\n", "z3-4.12")
            }),
            "limits in key"
        );
    }

    #[test]
    fn key_fields_are_length_prefixed() {
        // ("ab","c…") vs ("a","bc…") must not collide.
        assert_ne!(query_key(&parts("ab", "c")), query_key(&parts("a", "bc")));
    }

    #[test]
    fn outcome_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let c = QueryCache::open(dir.path().to_path_buf());
        let key = [3u8; 32];
        for v in [
            CachedOutcome::Unsat,
            CachedOutcome::Unknown,
            CachedOutcome::Sat { model: None },
            CachedOutcome::Sat {
                model: Some("((p0 ptr-nil))".into()),
            },
        ] {
            c.put(&key, &v).unwrap();
            assert_eq!(c.get(&key), Some(v));
        }
    }

    #[test]
    fn corrupt_entry_is_a_miss_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let c = QueryCache::open(dir.path().to_path_buf());
        let key = [4u8; 32];
        c.put(&key, &CachedOutcome::Unsat).unwrap();
        // Truncate / garble the underlying file.
        let hex: String = key.iter().map(|b| format!("{b:02x}")).collect();
        let path = dir.path().join("query").join(&hex[..2]).join(&hex);
        for bytes in [&b""[..], &b"\xff\xff\xff"[..], &[1, 1][..]] {
            std::fs::write(&path, bytes).unwrap();
            assert_eq!(c.get(&key), None, "bytes {bytes:?} must be a miss");
        }
    }
}
