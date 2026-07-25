//! Compiled `//goverify:` annotation data (phase-6 spec §3, §5). Types
//! only — resolve/lower/`compile_program` live in `goverify-spec` (Task
//! 6), which is downstream of this crate; the data shapes live here so
//! both `goverify-spec` (the compiler) and the CLI/engine (the
//! consumers) share one definition.

use std::collections::BTreeMap;

use goverify_ir::{FuncId, Pos};

use crate::checker::Finding;
use crate::summary::Clause;

/// Tag for every compiled annotation `Clause` (both `requires` and
/// `ensures`): annotations are one contract source, not a per-checker
/// fact, so they share a single tag distinct from any checker name.
pub const CONTRACT: &str = "contract";
/// `Finding::checker`/`tag` for a bad-annotation diagnostic (parse/
/// resolve/lower failure, unmatched pragma, unknown `ignore` name).
pub const BAD_ANNOTATION: &str = "bad-annotation";
/// `Finding::checker`/`tag` for an annotation the engine could not
/// discharge (Task 8+): reserved here so both compiler and engine agree
/// on the string.
pub const UNVERIFIED_ANNOTATION: &str = "unverified-annotation";

/// One compiled annotated clause plus what findings need: the
/// expression source text (position-free, quotable in messages) and the
/// pragma position (finding anchor).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnnClause {
    pub clause: Clause,
    pub text: String,
    pub pos: Option<Pos>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FuncAnnotations {
    pub requires: Vec<AnnClause>,
    pub ensures: Vec<AnnClause>,
    /// Checker names suppressed within this function (validated).
    pub ignores: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Annotations {
    pub funcs: BTreeMap<FuncId, FuncAnnotations>,
    /// bad-annotation findings from compilation (parse/resolve errors,
    /// unmatched pragmas, unknown ignore names). The CLI appends these
    /// to the analysis findings — they are cheap to recompute and never
    /// enter the SCC cache.
    pub findings: Vec<Finding>,
}
