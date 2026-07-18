//! Nil, bounds, leaks, races — plugins over the engine.
//!
//! `NilChecker` (phase 4, spec §4) is path-sensitive: it encodes each
//! function's gated SSA and Sat-gates every deref site's nil path, with
//! requires propagating bottom-up through call sites via the SCC
//! fixpoint (see docs/superpowers/specs/2026-07-16-goverify-design.md
//! §15).

mod nil;

pub use nil::NilChecker;
