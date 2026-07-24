//! Library surface of the goverify CLI (the binary lives in main.rs).
//! Exists so `fuzz/` can reach the baseline parser — parsers of bytes
//! the analyzer didn't write must reject, never panic (parent spec
//! §12.4) — and holds the pure, reusable pieces of the reporting
//! layer. Orchestration (formats dispatch, git, rendering) stays
//! bin-side.

pub mod baseline;
pub mod fingerprint;
