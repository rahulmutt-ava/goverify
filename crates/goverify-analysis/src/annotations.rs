//! Compiled `//goverify:` annotation data (phase-6 spec §3, §5). Types
//! only — resolve/lower/`compile_program` live in `goverify-spec` (Task
//! 6), which is downstream of this crate; the data shapes live here so
//! both `goverify-spec` (the compiler) and the CLI/engine (the
//! consumers) share one definition.

use std::collections::BTreeMap;

use goverify_ir::{Callee, FuncId, Function, Op, Pos, Program, TypeKind};
use goverify_solver::{Query, SatResult, Term};

use crate::checker::{Finding, Obligation, Severity};
use crate::encode::EncodedFunc;
use crate::summary::{Clause, Provenance, Summary, instantiate_ensures, instantiate_requires};

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

/// Call-site obligations for callees' ANNOTATED requires (phase-6 spec
/// §4). Mirrors the checkers' shared call-site-obligation pattern, but
/// iterates the callee's `FuncAnnotations` directly rather than its
/// merged summary: a merged `Summary.requires` interleaves inferred and
/// annotated clauses (dedup rule, `merge_annotations`), so a summary
/// index no longer lines up with `FuncAnnotations.requires`'s pragma
/// texts. Walking `ann_of`'s own `FuncAnnotations` keeps every
/// `BoundClause` paired with the exact `AnnClause` (and its `text`) it
/// came from.
///
/// `pre` — the caller's own requires terms (annotated included) — is
/// asserted alongside each violation for precision parity with the
/// checkers' own call-site obligations: without it, a caller that
/// itself requires (and therefore never violates) some precondition
/// would still get a reachable-looking obligation from an unconstrained
/// encoding.
///
/// `summary_of` — the callee's own (merged) `Summary` — is consulted
/// per annotated clause to avoid double reporting (fix-wave item 4):
/// `merge_annotations`'s dedup rule drops an annotated clause from the
/// callee's merged summary whenever a checker already infers the exact
/// same formula, on the theory that the checker's own call-site
/// obligations (which walk the merged summary, e.g. `nil`'s via
/// `call_site_obligations`) already cover it under its own tag. But this
/// function walks the callee's RAW `FuncAnnotations` (never the merged
/// summary — see the doc above), so it would raise a second, redundant
/// `contract` finding for a clause the checker already flags. Skipping
/// any annotated clause whose formula matches an `Inferred` clause in
/// the callee's merged summary restores single ownership: exactly the
/// same formula-equality rule `merge_annotations` uses, checked against
/// `Provenance::Inferred` specifically because that's what a
/// checker-owned (i.e. already-deduped) clause looks like in the merged
/// summary.
pub fn contract_obligations(
    p: &Program,
    func: &Function,
    enc: &EncodedFunc,
    own: &Summary,
    ann_of: &dyn Fn(FuncId) -> Option<FuncAnnotations>,
    summary_of: &dyn Fn(FuncId) -> Summary,
) -> Vec<Obligation> {
    let pre: Vec<Term> = own
        .requires
        .iter()
        .map(|c| c.formula.term.clone())
        .collect();
    let mut out = Vec::new();
    for (bi, b) in func.blocks.iter().enumerate() {
        for ins in &b.instrs {
            let Op::Call {
                callee: Callee::Static(c),
                args,
                ..
            } = &ins.op
            else {
                continue;
            };
            let Some(fa) = ann_of(*c) else { continue };
            if fa.requires.is_empty() {
                continue;
            }
            let callee_summary = summary_of(*c);
            let arg_terms: Vec<Option<Term>> =
                args.iter().map(|a| enc.value(*a).cloned()).collect();
            // A throwaway Summary whose `requires` is JUST this callee's
            // annotated clauses, in the same order as `fa.requires` —
            // `instantiate_requires`'s output is index-parallel with its
            // input, so zipping against `fa.requires` recovers each
            // `AnnClause` (and its `text`) for the matching `BoundClause`.
            let tmp = Summary {
                requires: fa.requires.iter().map(|a| a.clause.clone()).collect(),
                ..Summary::default()
            };
            for (bc, ac) in instantiate_requires(&tmp, &arg_terms)
                .into_iter()
                .zip(&fa.requires)
            {
                // Double-reporting guard (fix-wave item 4): a checker
                // already owns this exact fact (it's why
                // `merge_annotations` deduped the annotated duplicate out
                // of the merged summary) — that checker's own call-site
                // obligations already produce a finding for it, so
                // raising a `contract` finding here too would report the
                // same violation twice.
                if callee_summary.requires.iter().any(|sc| {
                    sc.provenance == Provenance::Inferred && sc.formula == ac.clause.formula
                }) {
                    continue;
                }
                // None = unbindable (unknown arg, sort mismatch, arity
                // overflow): cannot evaluate, so no obligation — never a
                // false positive from a missing term.
                let Some(v) = bc.violation else { continue };
                let mut extra = pre.clone();
                extra.push(v);
                out.push(Obligation {
                    tag: CONTRACT.to_string(),
                    // INVARIANT (checker.rs's `Obligation::message` doc):
                    // never embed a source position here — `pos` below is
                    // the sole carrier, and this text is hashed into the
                    // finding fingerprint.
                    message: format!(
                        "call to {} violates annotated requires `{}`",
                        p.func_name(*c),
                        ac.text
                    ),
                    pos: ins.pos.clone(),
                    query: enc.reach_query(bi, extra),
                });
            }
        }
    }
    out
}

/// Best-effort verification of a function's ANNOTATED ensures (phase-6
/// spec §4): for each clause, `own-requires ∧ body ∧ ¬clause` is
/// discharged at every return site; `Unsat` everywhere means the clause
/// is proven and stays silent (callers already trust it via
/// `encode_call_ensures`'s Annotated-clause carve-out — nothing more to
/// report). `own_requires` — this function's own (merged, annotated
/// clauses included) requires terms — is assumed at every site: the
/// design spec (§1(a)) treats requires as assumed at function entry, the
/// same way the checkers' own in-body obligations assume
/// `own_preconditions`, so proving an ensures clause without its
/// function's own precondition would reject annotations that are
/// perfectly valid under the stated contract (e.g. `requires x > 0` +
/// `ensures ret > 0` on `return x` is unprovable without assuming
/// `x > 0` first, even though it trivially holds under it). Anything
/// short of that full proof — `Sat`, `Unknown`, an unbindable clause, a
/// bodyless function, a function with no return sites, or a return
/// whose arity doesn't match the signature — yields the
/// `unverified-annotation` WARNING: the clause is still USED (trusted
/// by callers regardless of whether the engine could confirm it), so it
/// must be FLAGGED, never silently accepted or silently dropped.
pub fn verify_ensures(
    p: &Program,
    f: FuncId,
    enc: Option<&EncodedFunc>,
    ann: &FuncAnnotations,
    own_requires: &[Term],
    discharge: &mut dyn FnMut(&Query) -> SatResult,
) -> Vec<Finding> {
    let mut out = Vec::new();
    let mut warn = |ac: &AnnClause| {
        out.push(Finding {
            checker: UNVERIFIED_ANNOTATION.to_string(),
            tag: UNVERIFIED_ANNOTATION.to_string(),
            func: p.func_name(f).to_string(),
            pos: ac.pos.clone(),
            message: format!(
                "annotated ensures `{}` not proven against the body; callers still assume it",
                ac.text
            ),
            trace: Vec::new(),
            model: Vec::new(),
            severity: Severity::Warning,
        });
    };
    let (Some(func), Some(enc)) = (p.func(f), enc) else {
        // Bodyless (no body to verify against) or unencodable (e.g. past
        // the assertion cap): can't even attempt a proof.
        for ac in &ann.ensures {
            warn(ac);
        }
        return out;
    };
    let TypeKind::Signature { results, .. } = p.types().kind(func.sig) else {
        for ac in &ann.ensures {
            warn(ac);
        }
        return out;
    };
    let results_len = results.len();
    // Return sites, arity-checked: a crafted/stale `.gvir` return whose
    // value count doesn't match the signature can never be soundly bound
    // (nothing to say r<i> IS for an i past what this particular return
    // provides), so any such site marks every clause unprovable rather
    // than silently skipping just that site.
    let mut sites: Vec<(usize, Vec<goverify_ir::ValueId>)> = Vec::new();
    let mut malformed = false;
    for (bi, b) in func.blocks.iter().enumerate() {
        for ins in &b.instrs {
            if let Op::Return { vals } = &ins.op {
                if vals.len() != results_len {
                    malformed = true;
                }
                sites.push((bi, vals.clone()));
            }
        }
    }
    let arg_terms: Vec<Option<Term>> = func.params.iter().map(|&v| enc.value(v).cloned()).collect();
    for ac in &ann.ensures {
        if malformed || sites.is_empty() {
            warn(ac);
            continue;
        }
        let tmp = Summary {
            ensures: vec![ac.clause.clone()],
            ..Summary::default()
        };
        let mut proven = true;
        for (bi, vals) in &sites {
            let result_terms: Vec<Option<Term>> =
                vals.iter().map(|&v| enc.value(v).cloned()).collect();
            let bcs = instantiate_ensures(&tmp, &arg_terms, &result_terms);
            let Some(v) = bcs.into_iter().next().and_then(|bc| bc.violation) else {
                // Unbindable at this site (missing arg/result term):
                // cannot evaluate, so cannot prove — degrade to
                // unverified rather than assert on a partial binding.
                proven = false;
                break;
            };
            // Assume the function's own requires (annotated included)
            // alongside the body before checking the clause's negation:
            // requires is assumed at entry (design spec §1(a)), so a
            // clause that only holds given the stated precondition must
            // still count as proven, not unprovable.
            let mut extra = own_requires.to_vec();
            extra.push(v);
            if discharge(&enc.reach_query(*bi, extra)) != SatResult::Unsat {
                proven = false;
                break;
            }
        }
        if !proven {
            warn(ac);
        }
    }
    out
}
