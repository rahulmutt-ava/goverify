//! Nil, bounds, leaks, races — plugins over the engine.
//!
//! `NilChecker` (phase 4, spec §4) is path-sensitive: it encodes each
//! function's gated SSA and Sat-gates every deref site's nil path, with
//! requires propagating bottom-up through call sites via the SCC
//! fixpoint (see docs/superpowers/specs/2026-07-16-goverify-design.md
//! §15).

mod bounds;
mod leak;
mod nil;
mod shared;
#[cfg(test)]
mod testfix;

pub use bounds::BoundsChecker;
pub use leak::LeakChecker;
pub use nil::NilChecker;

/// The production checker set — single source for the CLI's checker
/// vec and for validating `//goverify:ignore` names (phase-6 spec §5):
/// both the CLI and `goverify-spec`'s `compile_program` need the same
/// list, so it lives here rather than being duplicated at each call
/// site.
pub fn default_checkers() -> Vec<&'static dyn goverify_analysis::Checker> {
    vec![&NilChecker, &BoundsChecker, &LeakChecker]
}
