//! Nil, bounds, leaks, races — plugins over the engine.
//!
//! `NilTracer` (phase 3) is the tracer's embryo: entry-block unconditional
//! derefs + constant-nil call args. Phase 4 grows it (and its siblings)
//! into real path-sensitive checkers behind the same `Checker` trait (see
//! docs/superpowers/specs/2026-07-16-goverify-design.md §15).

mod nil;

pub use nil::NilTracer;
