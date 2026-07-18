//! BoundsChecker (phase-4 spec §4): path-sensitive index/slice-bounds,
//! division, and narrowing-conversion safety on the gated SSA encoding,
//! mirroring nil.rs's two-pass shape (`shared::propagate_requires`/
//! `shared::call_site_obligations` factor out the parts that are
//! byte-for-byte identical across checkers). Task 8 covers
//! `Index`/`IndexAddr`/`Slice` sites (tag `"bounds"`); Task 9 adds
//! `Op::BinOp` div/rem sites (tags `"div-zero"`/`"overflow"`) and
//! narrowing/sign-changing `Op::Convert` sites (tag `"overflow"`).

use goverify_analysis::{
    Checker, Clause, EncodedFunc, Formula, Obligation, Summary, array_len, encode_func, int_repr,
    seq_datatype,
};
use goverify_ir::{BinOpKind, FuncId, Function, MakeKind, Op, Pos, Program, ValueId, ValueKind};
use goverify_solver::{BvCmpOp, Query, SatResult, Term};

use crate::shared::{
    call_site_obligations, own_preconditions, params_only, propagate_requires, push_clause,
};

pub struct BoundsChecker;

/// index violation: i < 0 ∨ i >= len  (signed: Go index is int)
fn index_violation(idx: Term, len: Term) -> Option<Term> {
    let neg = Term::bv_cmp(BvCmpOp::Slt, idx.clone(), Term::bv_lit(64, 0)).ok()?;
    let over = Term::bv_cmp(BvCmpOp::Sle, len, idx).ok()?;
    Term::or(vec![neg, over]).ok()
}

/// slice violation: low < 0 ∨ high < low ∨ cap < high [∨ cap < max].
/// `bv_cmp` has no Sgt: `x > y` is encoded as `bvslt y x` (so `high >
/// cap` becomes `(bvslt cap high)`, `max > cap` becomes `(bvslt cap
/// max)`).
fn slice_violation(low: Term, high: Term, cap: Term, max: Option<Term>) -> Option<Term> {
    let neg_low = Term::bv_cmp(BvCmpOp::Slt, low.clone(), Term::bv_lit(64, 0)).ok()?;
    let high_lt_low = Term::bv_cmp(BvCmpOp::Slt, high.clone(), low).ok()?;
    let high_over_cap = Term::bv_cmp(BvCmpOp::Slt, cap.clone(), high).ok()?;
    let mut disjuncts = vec![neg_low, high_lt_low, high_over_cap];
    if let Some(m) = max {
        disjuncts.push(Term::bv_cmp(BvCmpOp::Slt, cap, m).ok()?);
    }
    Term::or(disjuncts).ok()
}

/// len term for an index/slice base: Seq values via seq-len accessor,
/// arrays (direct or, as go/ssa commonly emits, via pointer —
/// `array_len` peels that one level) via their static type length; None
/// = unmodelable (silent).
fn len_of(p: &Program, func: &Function, enc: &EncodedFunc, base: ValueId) -> Option<Term> {
    if let Some(len) = array_len(p.types(), func.value(base).ty) {
        return Some(Term::bv_lit(64, len as u128));
    }
    let b = enc.value(base)?.clone();
    if b.sort() != &seq_datatype().sort() {
        return None;
    }
    Term::dt_get(&seq_datatype(), "seq-val", "seq-len", b).ok()
}

/// cap term for a slice base: Seq values via seq-cap accessor; arrays
/// (direct or via pointer) have no separate capacity in Go, so their
/// static length stands in for cap too. None = unmodelable (silent) —
/// same shape as `len_of`.
fn cap_of(p: &Program, func: &Function, enc: &EncodedFunc, base: ValueId) -> Option<Term> {
    if let Some(len) = array_len(p.types(), func.value(base).ty) {
        return Some(Term::bv_lit(64, len as u128));
    }
    let b = enc.value(base)?.clone();
    if b.sort() != &seq_datatype().sort() {
        return None;
    }
    Term::dt_get(&seq_datatype(), "seq-val", "seq-cap", b).ok()
}

/// One site (bounds, div-zero, or overflow): the block it's in, the
/// `Clause`/`Obligation` tag it reports under, its violation term, its
/// source position, and every IR value the violation was actually built
/// from (used only by `obligations`' groundness test — see
/// `expressible`). A single `Site` shape covers all three families so
/// `infer_requires`/`obligations` only need one loop apiece.
struct Site {
    block: usize,
    tag: &'static str,
    violation: Term,
    pos: Option<Pos>,
    values: Vec<ValueId>,
}

/// Every `Index`/`IndexAddr`/`Slice` instruction in `func`, each with its
/// violation term built via `index_violation`/`slice_violation` +
/// `len_of`/`cap_of`. Missing/unmodelable terms degrade the site to
/// silence (never panics, never reports on a guess) — degrade, never
/// die.
fn bounds_sites(p: &Program, func: &Function, enc: &EncodedFunc) -> Vec<Site> {
    let mut out = Vec::new();
    for (bi, b) in func.blocks.iter().enumerate() {
        for ins in &b.instrs {
            match &ins.op {
                Op::IndexAddr { base, index, .. } | Op::Index { base, index, .. } => {
                    let Some(idx) = enc.value(*index).cloned() else {
                        continue;
                    };
                    let Some(len) = len_of(p, func, enc, *base) else {
                        continue;
                    };
                    let Some(violation) = index_violation(idx, len) else {
                        continue;
                    };
                    out.push(Site {
                        block: bi,
                        tag: "bounds",
                        violation,
                        pos: ins.pos.clone(),
                        values: vec![*index, *base],
                    });
                }
                Op::Slice {
                    base,
                    low,
                    high,
                    max,
                    ..
                } => {
                    let Some(cap) = cap_of(p, func, enc, *base) else {
                        continue;
                    };
                    let mut values = vec![*base];
                    let low_t = match low {
                        Some(l) => {
                            let Some(t) = enc.value(*l).cloned() else {
                                continue;
                            };
                            values.push(*l);
                            t
                        }
                        None => Term::bv_lit(64, 0), // Go default: low = 0
                    };
                    let high_t = match high {
                        Some(h) => {
                            let Some(t) = enc.value(*h).cloned() else {
                                continue;
                            };
                            values.push(*h);
                            t
                        }
                        None => {
                            // Go default: high = len(base).
                            let Some(t) = len_of(p, func, enc, *base) else {
                                continue;
                            };
                            t
                        }
                    };
                    let max_t = match max {
                        Some(m) => {
                            let Some(t) = enc.value(*m).cloned() else {
                                continue;
                            };
                            values.push(*m);
                            Some(t)
                        }
                        None => None,
                    };
                    let Some(violation) = slice_violation(low_t, high_t, cap, max_t) else {
                        continue;
                    };
                    out.push(Site {
                        block: bi,
                        tag: "bounds",
                        violation,
                        pos: ins.pos.clone(),
                        values,
                    });
                }
                _ => continue,
            }
        }
    }
    out
}

/// Every `Div`/`Rem` `BinOp` in `func`. Two independent violation
/// families share the same instruction:
///
/// - `"div-zero"` (both `Div` and `Rem`, any signedness): `rhs == 0` at
///   the operand width.
/// - `"overflow"` (signed `Div` only — Go defines `MinInt % -1` as `0`,
///   no trap, so `Rem` never overflows; unsigned division can never
///   overflow either): `lhs == MIN(w) ∧ rhs == -1`, both at the operand
///   width, `MIN(w)` = the `1 << (w-1)` bit pattern, `-1` = all-ones.
///
/// Signedness/width come from the LHS operand type, exactly mirroring
/// `encode.rs`'s own `binop_term` (never the dst — a comparison's dst
/// would be Bool, but Div/Rem's dst always shares the operand type).
fn divrem_sites(p: &Program, func: &Function, enc: &EncodedFunc) -> Vec<Site> {
    let mut out = Vec::new();
    for (bi, b) in func.blocks.iter().enumerate() {
        for ins in &b.instrs {
            let Op::BinOp { kind, lhs, rhs, .. } = &ins.op else {
                continue;
            };
            if !matches!(kind, BinOpKind::Div | BinOpKind::Rem) {
                continue;
            }
            let Some((w, signed)) = int_repr(p.types(), func.value(*lhs).ty) else {
                continue;
            };
            let Some(lhs_t) = enc.value(*lhs).cloned() else {
                continue;
            };
            let Some(rhs_t) = enc.value(*rhs).cloned() else {
                continue;
            };
            if let Ok(zero_violation) = Term::eq(rhs_t.clone(), Term::bv_lit(w, 0)) {
                out.push(Site {
                    block: bi,
                    tag: "div-zero",
                    violation: zero_violation,
                    pos: ins.pos.clone(),
                    values: vec![*rhs],
                });
            }
            if signed && matches!(kind, BinOpKind::Div) {
                let min = 1u128 << (w - 1);
                let ones = u128::MAX >> (128 - w);
                let lhs_is_min = Term::eq(lhs_t, Term::bv_lit(w, min));
                let rhs_is_neg1 = Term::eq(rhs_t, Term::bv_lit(w, ones));
                if let (Ok(a), Ok(b)) = (lhs_is_min, rhs_is_neg1)
                    && let Ok(violation) = Term::and(vec![a, b])
                {
                    out.push(Site {
                        block: bi,
                        tag: "overflow",
                        violation,
                        pos: ins.pos.clone(),
                        values: vec![*lhs, *rhs],
                    });
                }
            }
        }
    }
    out
}

/// `value` re-based to width `ws`: two's-complement-truncated so a
/// negative dst-width bound (e.g. int8's MIN = -128) prints as its
/// correct src-width bit pattern (`ws` = 64 ⇒ `-128` becomes
/// `18446744073709551488`) — the literal a src-width term can be
/// directly compared against.
fn lit_sext(ws: u32, value: i128) -> Term {
    Term::bv_lit(ws, (value as u128) & (u128::MAX >> (128 - ws)))
}

/// The narrowing/sign-changing `Convert` violation over `x` (the SOURCE
/// term, always at width `ws`; `(ws, ss)` = src (width, signed), `(wd,
/// sd)` = dst): the four shapes the brief specifies, selected by the
/// (src-signed, dst-signed) pair — the caller has already gated entry to
/// only `wd < ws` (narrowing) or `wd == ws && sd != ss` (sign-changing
/// same-width convert).
fn convert_violation(ws: u32, ss: bool, wd: u32, sd: bool, x: Term) -> Option<Term> {
    let min_d = -(1i128 << (wd - 1));
    let max_d_signed = (1i128 << (wd - 1)) - 1;
    let max_d_unsigned = (1i128 << wd) - 1;
    match (ss, sd) {
        (true, true) => {
            // signed -> signed narrow: x < MIN_d ∨ MAX_d < x.
            let lo = Term::bv_cmp(BvCmpOp::Slt, x.clone(), lit_sext(ws, min_d)).ok()?;
            let hi = Term::bv_cmp(BvCmpOp::Slt, lit_sext(ws, max_d_signed), x).ok()?;
            Term::or(vec![lo, hi]).ok()
        }
        (false, false) => {
            // unsigned -> unsigned narrow: MAX_d <u x.
            Term::bv_cmp(BvCmpOp::Ult, lit_sext(ws, max_d_unsigned), x).ok()
        }
        (true, false) => {
            // signed -> unsigned: x < 0 ∨ (narrowing ? MAX_d <s x : nothing more).
            let neg = Term::bv_cmp(BvCmpOp::Slt, x.clone(), Term::bv_lit(ws, 0)).ok()?;
            if wd < ws {
                let hi = Term::bv_cmp(BvCmpOp::Slt, lit_sext(ws, max_d_signed), x).ok()?;
                Term::or(vec![neg, hi]).ok()
            } else {
                Some(neg)
            }
        }
        (false, true) => {
            // unsigned -> signed: MAX_d <u x, MAX_d = 2^(wd-1) - 1 zero-extended.
            Term::bv_cmp(BvCmpOp::Ult, lit_sext(ws, max_d_signed), x).ok()
        }
    }
}

/// Every narrowing (`wd < ws`) or sign-changing same-width (`wd == ws &&
/// sd != ss`) `Convert` in `func`, tagged `"overflow"`; a widening or
/// same-signedness-same-width convert is always safe and never produces
/// a site (silent, per the brief's skip condition). Non-int operands
/// (missing `int_repr` on either side) havoc: silent, never a guess.
fn convert_sites(p: &Program, func: &Function, enc: &EncodedFunc) -> Vec<Site> {
    let mut out = Vec::new();
    for (bi, b) in func.blocks.iter().enumerate() {
        for ins in &b.instrs {
            let Op::Convert { dst, src } = &ins.op else {
                continue;
            };
            let Some((ws, ss)) = int_repr(p.types(), func.value(*src).ty) else {
                continue;
            };
            let Some((wd, sd)) = int_repr(p.types(), func.value(*dst).ty) else {
                continue;
            };
            if !(wd < ws || (wd == ws && sd != ss)) {
                continue; // widening, or same signedness at the same width: safe
            }
            let Some(x) = enc.value(*src).cloned() else {
                continue;
            };
            let Some(violation) = convert_violation(ws, ss, wd, sd, x) else {
                continue;
            };
            out.push(Site {
                block: bi,
                tag: "overflow",
                violation,
                pos: ins.pos.clone(),
                values: vec![*src],
            });
        }
    }
    out
}

/// Every bounds/div-zero/overflow site in `func`: the union `Site`
/// collection `infer_requires`/`obligations` both iterate once over.
fn all_sites(p: &Program, func: &Function, enc: &EncodedFunc) -> Vec<Site> {
    let mut out = bounds_sites(p, func, enc);
    out.extend(divrem_sites(p, func, enc));
    out.extend(convert_sites(p, func, enc));
    out
}

/// Human-readable noun phrase for a site's tag, used to build
/// `obligations`'s per-site message.
fn site_description(tag: &str) -> &'static str {
    match tag {
        "div-zero" => "division by zero",
        "overflow" => "integer overflow",
        _ => "index/slice out of bounds",
    }
}

/// A bare function parameter or manifest constant: `encode_func` ties
/// both down (a defining const-eq assert, or the calling convention's
/// own `p<i>` naming) even though the *term itself* is never
/// syntactically free-vars-empty — same lesson as nil.rs's
/// `is_const_nil`, generalized past "nil" to any constant.
fn is_ground_or_param(func: &Function, v: ValueId) -> bool {
    matches!(func.value(v).kind, ValueKind::Param | ValueKind::Const(_))
}

/// True iff `v`'s value is safe to use in a *local* manifest obligation
/// even though its own encoded term isn't syntactically free-vars-empty:
/// a function parameter, a manifest constant, or the destination of a
/// `Make`/`Slice` op **whose own length/cap-determining inputs are
/// themselves ground consts or params-only** (review finding, Task 8
/// fast-follow — the unqualified "any Make/Slice dst" version admitted
/// a site like `f(m, n int) []int { s := make([]int, m, n); return
/// s[1:] }` as a *local* obligation on `f`, when `m`/`n` being
/// unconstrained params means this is exactly what `infer_requires`
/// should have surfaced as `f`'s own requires instead — bypassing the
/// self-consistency check `own_preconditions` gives every OTHER site,
/// see nil.rs's `wrapper_does_not_self_report`).
///
/// `Make`'s determining inputs are its `len`/`cap` args directly (a
/// missing 2-arg-`make` cap lowers to the `ValueId(0)` sentinel, which
/// isn't a real value to check — see lower.rs's `v(1)`/`vopt`
/// convention). `Slice`'s are `low`/`high`/`max` (each checked
/// directly — Go's slice-expression operands are never anything but a
/// scalar int) plus `base`, RECURSIVELY: a `base` whose type resolves
/// via `array_len` (an array, or, as go/ssa commonly emits for a
/// compile-time-constant `make()`, a *pointer* to one) never has its
/// own term read by `len_of`/`cap_of` at all — the array's length is a
/// static type fact, independent of whatever value the pointer holds —
/// so its groundness is moot; otherwise `base` must itself satisfy this
/// same test (e.g. `BadSlice`'s corpus case: reslicing a `make()`'d
/// slice is a `Slice`-of-a-`Slice`, base = the first `Slice`'s dst).
/// Depth-capped against a malformed/cyclic `.gvir` recursing forever —
/// degrades to "not expressible" past the cap, never overflows the
/// stack (degrade, never die).
const EXPRESSIBLE_MAX_DEPTH: u32 = 16;

fn expressible(p: &Program, func: &Function, v: ValueId) -> bool {
    expressible_at(p, func, v, EXPRESSIBLE_MAX_DEPTH)
}

fn expressible_at(p: &Program, func: &Function, v: ValueId, depth: u32) -> bool {
    if is_ground_or_param(func, v) {
        return true;
    }
    let Some(depth) = depth.checked_sub(1) else {
        return false;
    };
    for b in &func.blocks {
        for ins in &b.instrs {
            match &ins.op {
                Op::Make {
                    dst,
                    kind: MakeKind::Slice,
                    args,
                } if *dst == v => {
                    return args
                        .iter()
                        .all(|&a| a == ValueId(0) || is_ground_or_param(func, a));
                }
                Op::Slice {
                    dst,
                    base,
                    low,
                    high,
                    max,
                } if *dst == v => {
                    let base_ok = array_len(p.types(), func.value(*base).ty).is_some()
                        || expressible_at(p, func, *base, depth);
                    if !base_ok {
                        return false;
                    }
                    for opt in [low, high, max] {
                        if let Some(a) = opt
                            && !is_ground_or_param(func, *a)
                        {
                            return false;
                        }
                    }
                    return true;
                }
                _ => {}
            }
        }
    }
    false
}

impl Checker for BoundsChecker {
    fn name(&self) -> &'static str {
        "bounds"
    }

    fn infer_requires(
        &self,
        p: &Program,
        f: FuncId,
        summary_of: &dyn Fn(FuncId) -> Summary,
        discharge: &mut dyn FnMut(&Query) -> SatResult,
    ) -> Vec<Clause> {
        let Some(func) = p.func(f) else {
            return Vec::new();
        };
        let Ok(enc) = encode_func(p, f) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        // Own bounds/div-zero/overflow sites: only ever a requires-clause
        // when the violation is expressible purely over this function's
        // own params (a manifest/ground violation can't be
        // parameterized — it's `obligations`'s job, not a precondition
        // on the caller) and its path is confirmed reachable (Sat-gated).
        for site in all_sites(p, func, &enc) {
            if !params_only(&site.violation) {
                continue;
            }
            if discharge(&enc.reach_query(site.block, vec![site.violation.clone()]))
                != SatResult::Sat
            {
                continue;
            }
            let Ok(bound) = Term::not(site.violation) else {
                continue;
            };
            push_clause(
                &mut out,
                Clause {
                    tag: site.tag.into(),
                    formula: Formula { term: bound },
                },
            );
        }
        for tag in ["bounds", "div-zero", "overflow"] {
            propagate_requires(p, func, &enc, tag, summary_of, discharge, &mut out);
        }
        out
    }

    fn obligations(
        &self,
        p: &Program,
        f: FuncId,
        summary_of: &dyn Fn(FuncId) -> Summary,
    ) -> Vec<Obligation> {
        let Some(func) = p.func(f) else {
            return Vec::new();
        };
        let Ok(enc) = encode_func(p, f) else {
            return Vec::new();
        };
        let pre = own_preconditions(&summary_of(f));
        let mut out = Vec::new();

        // Local manifest sites: every value the violation depends on
        // must be expressible (see `expressible`) — an index/bound/
        // operand pulled from an unmodeled call (havoc) stays silent
        // (FP storm guard: we cannot say anything about an arbitrary
        // heap value).
        for site in all_sites(p, func, &enc) {
            if !site.values.iter().all(|&v| expressible(p, func, v)) {
                continue;
            }
            let mut extra = pre.clone();
            extra.push(site.violation);
            out.push(Obligation {
                tag: site.tag.into(),
                message: format!("{} in {}", site_description(site.tag), p.func_name(f)),
                pos: site.pos,
                query: enc.reach_query(site.block, extra),
            });
        }

        for tag in ["bounds", "div-zero", "overflow"] {
            out.extend(call_site_obligations(p, func, &enc, tag, &pre, summary_of));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use goverify_analysis::Checker;
    use goverify_solver::{Logic, Query, SatResult, Sort};

    use super::*;
    use crate::testfix::{
        binop_instr, block, branch_on, call_builtin, call_static, convert_instr, index_addr_instr,
        instr, int_aux, make_slice_instr, no_summaries, pkg_with_int_types, pkg_with_seq_types,
        slice_instr, z3_discharge,
    };

    /// t.F(s []int, i int): IndexAddr s[i] unconditionally in the entry
    /// block, no destination read (statement-position index).
    fn index_func(
        extra_params: Vec<goverify_extract::gvir::Param>,
        blocks: Vec<goverify_extract::gvir::BasicBlock>,
    ) -> goverify_extract::gvir::Function {
        let mut params = vec![goverify_extract::gvir::Param {
            id: 1,
            name: "s".into(),
            r#type: 3, // []int
        }];
        params.extend(extra_params);
        goverify_extract::gvir::Function {
            id: "t.F".into(),
            params,
            blocks,
            ..Default::default()
        }
    }

    #[test]
    fn index_on_slice_param_infers_bounds_requires() {
        // t.F(s []int, i int) { _ = s[i] } ⇒ requires 0 <= i < len(s):
        // formula free vars {p0, p1}, tag "bounds".
        let f = index_func(
            vec![goverify_extract::gvir::Param {
                id: 2,
                name: "i".into(),
                r#type: 1, // int
            }],
            vec![block(
                0,
                vec![index_addr_instr(3, 1, 1, 2), instr("Return")],
                vec![],
            )],
        );
        let p = pkg_with_seq_types(vec![f]);
        let fid = p.lookup_func("t.F").unwrap();
        let reqs = BoundsChecker.infer_requires(&p, fid, &no_summaries, &mut z3_discharge());
        assert_eq!(reqs.len(), 1, "one index site: {reqs:?}");
        assert_eq!(reqs[0].tag, "bounds");
        let fv = reqs[0].formula.term.free_vars();
        let mut names: Vec<&String> = fv.keys().collect();
        names.sort();
        assert_eq!(names, vec!["p0", "p1"], "over both s and i: {reqs:?}");
    }

    #[test]
    fn constant_index_under_len_guard_infers_nothing() {
        // if len(s) > 2 { _ = s[2] } — reach implies len(s) > 2, so the
        // upper-bound half of the violation (len(s) <= 2) is UNSAT once
        // "len" is properly tied to seq-len (encode.rs's new builtin
        // arm) — this is that arm's checker-level integration proof.
        let lencall = call_builtin("len", 2, 1, vec![1]);
        let gt = binop_instr(4, 2, ">", 2, 3);
        let br = branch_on(4);
        let f = index_func(
            vec![],
            vec![
                block(0, vec![lencall, gt, br], vec![1, 2]),
                block(
                    1,
                    vec![index_addr_instr(5, 1, 1, 3), instr("Return")],
                    vec![],
                ),
                block(2, vec![instr("Return")], vec![]),
            ],
        );
        let mut f = f;
        f.aux = vec![int_aux(3, 1, 2)];
        let p = pkg_with_seq_types(vec![f]);
        let fid = p.lookup_func("t.F").unwrap();
        let obs = BoundsChecker.obligations(&p, fid, &no_summaries);
        assert!(
            !obs.is_empty(),
            "the const index site is expressible: {obs:?}"
        );
        for ob in &obs {
            assert_ne!(
                z3_discharge()(&ob.query),
                SatResult::Sat,
                "the len(s) guard rules out the violation once modeled: {ob:?}"
            );
        }
    }

    #[test]
    fn array_index_uses_static_len() {
        // t.F(a [3]int, i int) { _ = a[i] } ⇒ requires 0 <= i < 3 — the
        // array's static length, no `s`-derived term at all (`len_of`'s
        // Array arm never reads `a`'s own encoded value).
        let f = goverify_extract::gvir::Function {
            id: "t.F".into(),
            params: vec![
                goverify_extract::gvir::Param {
                    id: 1,
                    name: "a".into(),
                    r#type: 4, // [3]int
                },
                goverify_extract::gvir::Param {
                    id: 2,
                    name: "i".into(),
                    r#type: 1, // int
                },
            ],
            blocks: vec![block(
                0,
                vec![index_addr_instr(3, 1, 1, 2), instr("Return")],
                vec![],
            )],
            ..Default::default()
        };
        let p = pkg_with_seq_types(vec![f]);
        let fid = p.lookup_func("t.F").unwrap();
        let reqs = BoundsChecker.infer_requires(&p, fid, &no_summaries, &mut z3_discharge());
        assert_eq!(reqs.len(), 1, "one index site: {reqs:?}");
        let fv = reqs[0].formula.term.free_vars();
        let names: Vec<&String> = fv.keys().collect();
        assert_eq!(names, vec!["p1"], "only i, never a's own term: {reqs:?}");
    }

    #[test]
    fn slice_expr_checks_low_le_high_le_cap() {
        // The violation shape prose-specified by the brief: low < 0 ∨
        // high < low ∨ cap < high. `bv_cmp` has no Sgt, so `high > cap`
        // must print as `(bvslt cap high)` — a direct, white-box check
        // of `slice_violation`'s structure.
        let l = Term::var("l", Sort::BitVec(64));
        let h = Term::var("h", Sort::BitVec(64));
        let cap = Term::var("cap", Sort::BitVec(64));
        let v = slice_violation(l, h, cap, None).expect("slice_violation builds");
        let text = Query::for_asserts(Logic::QfBv, vec![v]).canonical_text();
        assert!(text.contains("(bvslt l (_ bv0 64))"), "low < 0:\n{text}");
        assert!(text.contains("(bvslt h l)"), "high < low:\n{text}");
        assert!(
            text.contains("(bvslt cap h)"),
            "high > cap via bvslt cap h:\n{text}"
        );
    }

    #[test]
    fn manifest_out_of_bounds_obligates() {
        // t.F() { s := make([]int, 3); _ = s[5] } ⇒ local obligation,
        // Sat: base is a Make dst (not itself a param/const, but still
        // expressible — the `expressible` groundness fix) and the index
        // is a manifest constant.
        let f = goverify_extract::gvir::Function {
            id: "t.F".into(),
            aux: vec![int_aux(1, 1, 3), int_aux(2, 1, 5)],
            blocks: vec![block(
                0,
                vec![
                    make_slice_instr(3, 3, 1, 0),
                    index_addr_instr(4, 1, 3, 2),
                    instr("Return"),
                ],
                vec![],
            )],
            ..Default::default()
        };
        let p = pkg_with_seq_types(vec![f]);
        let fid = p.lookup_func("t.F").unwrap();
        let obs = BoundsChecker.obligations(&p, fid, &no_summaries);
        assert_eq!(obs.len(), 1, "one manifest-local bounds violation: {obs:?}");
        assert_eq!(obs[0].tag, "bounds");
        assert_eq!(
            z3_discharge()(&obs[0].query),
            SatResult::Sat,
            "manifest s[5] on a 3-element slice is satisfiable"
        );
    }

    #[test]
    fn slice_over_capacity_obligates() {
        // t.F() { s := make([]int, 2, 4); _ = s[1:5] } ⇒ high(5) > cap(4).
        let f = goverify_extract::gvir::Function {
            id: "t.F".into(),
            aux: vec![
                int_aux(1, 1, 2),
                int_aux(2, 1, 4),
                int_aux(3, 1, 1),
                int_aux(4, 1, 5),
            ],
            blocks: vec![block(
                0,
                vec![
                    make_slice_instr(5, 3, 1, 2),
                    slice_instr(6, 3, 5, 3, 4, 0),
                    instr("Return"),
                ],
                vec![],
            )],
            ..Default::default()
        };
        let p = pkg_with_seq_types(vec![f]);
        let fid = p.lookup_func("t.F").unwrap();
        let obs = BoundsChecker.obligations(&p, fid, &no_summaries);
        assert_eq!(obs.len(), 1, "one manifest-local slice violation: {obs:?}");
        assert_eq!(obs[0].tag, "bounds");
        assert_eq!(
            z3_discharge()(&obs[0].query),
            SatResult::Sat,
            "s[1:5] over a cap-4 slice is satisfiable"
        );
    }

    #[test]
    fn symbolic_make_reslice_stays_silent_locally() {
        // Review finding (Task 8 fast-follow): t.F() { m := t.K()
        // (unknown callee: m havocs); s := make([]int, m, m); _ = s[1:] }
        // — make's own len/cap args are neither a manifest constant nor
        // a parameter (an arbitrary havoc'd value), so `expressible`
        // must NOT treat the resulting slice as fit for a *local*
        // obligation: unlike `manifest_out_of_bounds_obligates`'s
        // ground-literal make, we truly cannot say anything about an
        // arbitrary heap value's length here (same FP-storm guard as
        // nil.rs's `havoc_subject_deref_stays_silent`).
        let f = goverify_extract::gvir::Function {
            id: "t.F".into(),
            aux: vec![int_aux(4, 1, 1)], // low = 1
            blocks: vec![block(
                0,
                vec![
                    call_static("t.K", 1, 1, vec![]), // m := t.K(): int, havoc'd
                    make_slice_instr(2, 3, 1, 1),     // s := make([]int, m, m)
                    slice_instr(3, 3, 2, 4, 0, 0),    // _ = s[1:]
                    instr("Return"),
                ],
                vec![],
            )],
            ..Default::default()
        };
        let p = pkg_with_seq_types(vec![f]);
        let fid = p.lookup_func("t.F").unwrap();
        let obs = BoundsChecker.obligations(&p, fid, &no_summaries);
        assert!(
            obs.is_empty(),
            "havoc'd make() args must not obligate locally: {obs:?}"
        );
    }

    /// t.F(a, b <ty>): BinOp `op` unconditionally in the entry block,
    /// destination unread (statement-position, mirrors `index_func`).
    fn two_param_binop_func(ty: u32, op: &str) -> goverify_extract::gvir::Function {
        goverify_extract::gvir::Function {
            id: "t.F".into(),
            params: vec![
                goverify_extract::gvir::Param {
                    id: 1,
                    name: "a".into(),
                    r#type: ty,
                },
                goverify_extract::gvir::Param {
                    id: 2,
                    name: "b".into(),
                    r#type: ty,
                },
            ],
            blocks: vec![block(
                0,
                vec![binop_instr(3, ty, op, 1, 2), instr("Return")],
                vec![],
            )],
            ..Default::default()
        }
    }

    #[test]
    fn div_by_param_infers_nonzero_requires() {
        // t.F(a, b int) { _ = a / b } ⇒ requires b != 0, tag "div-zero".
        let f = two_param_binop_func(1, "/");
        let p = pkg_with_int_types(vec![f]);
        let fid = p.lookup_func("t.F").unwrap();
        let reqs = BoundsChecker.infer_requires(&p, fid, &no_summaries, &mut z3_discharge());
        let divzero: Vec<_> = reqs.iter().filter(|c| c.tag == "div-zero").collect();
        assert_eq!(divzero.len(), 1, "one div-zero requires: {reqs:?}");
        let fv = divzero[0].formula.term.free_vars();
        let names: Vec<&String> = fv.keys().collect();
        assert_eq!(names, vec!["p1"], "requires is over b only: {reqs:?}");
    }

    #[test]
    fn guarded_div_infers_nothing() {
        // if b != 0 { a / b } ⇒ unsat: the div-zero requires must not
        // surface once the guard rules the violation (b == 0) out.
        let f = goverify_extract::gvir::Function {
            id: "t.F".into(),
            params: vec![
                goverify_extract::gvir::Param {
                    id: 1,
                    name: "a".into(),
                    r#type: 1,
                },
                goverify_extract::gvir::Param {
                    id: 2,
                    name: "b".into(),
                    r#type: 1,
                },
            ],
            aux: vec![int_aux(3, 1, 0)], // manifest zero
            blocks: vec![
                block(
                    0,
                    vec![binop_instr(4, 7, "!=", 2, 3), branch_on(4)],
                    vec![1, 2],
                ),
                block(
                    1,
                    vec![binop_instr(5, 1, "/", 1, 2), instr("Return")],
                    vec![],
                ),
                block(2, vec![instr("Return")], vec![]),
            ],
            ..Default::default()
        };
        let p = pkg_with_int_types(vec![f]);
        let fid = p.lookup_func("t.F").unwrap();
        let reqs = BoundsChecker.infer_requires(&p, fid, &no_summaries, &mut z3_discharge());
        assert!(
            reqs.iter().all(|c| c.tag != "div-zero"),
            "guarded div must not infer a requires: {reqs:?}"
        );
    }

    #[test]
    fn signed_div_minint_overflow_obligation() {
        // int8 path: a / b violated when a == -128 ∧ b == -1, tag
        // "overflow". int8 params so the literals are small.
        let f = two_param_binop_func(2, "/"); // int8
        let p = pkg_with_int_types(vec![f]);
        let fid = p.lookup_func("t.F").unwrap();
        let obs = BoundsChecker.obligations(&p, fid, &no_summaries);
        let overflow: Vec<_> = obs.iter().filter(|o| o.tag == "overflow").collect();
        assert_eq!(overflow.len(), 1, "one overflow obligation: {obs:?}");
        assert_eq!(
            z3_discharge()(&overflow[0].query),
            SatResult::Sat,
            "MinInt8 / -1 is satisfiable: {overflow:?}"
        );
    }

    #[test]
    fn unsigned_div_has_no_minint_case() {
        // uint: only div-zero emitted.
        let f = two_param_binop_func(6, "/"); // uint64
        let p = pkg_with_int_types(vec![f]);
        let fid = p.lookup_func("t.F").unwrap();
        let obs = BoundsChecker.obligations(&p, fid, &no_summaries);
        let tags: std::collections::BTreeSet<&str> = obs.iter().map(|o| o.tag.as_str()).collect();
        assert_eq!(
            tags,
            std::collections::BTreeSet::from(["div-zero"]),
            "unsigned division never raises overflow: {obs:?}"
        );
    }

    #[test]
    fn narrowing_convert_infers_range_requires() {
        // t.F(x int) int8 { return int8(x) } ⇒ requires -128 <= x <= 127
        // (tag "overflow"): violation (or (bvslt x -128) (bvslt 127 x))
        // with 64-bit literals (sign-extended two's complement for -128).
        let f = goverify_extract::gvir::Function {
            id: "t.F".into(),
            params: vec![goverify_extract::gvir::Param {
                id: 1,
                name: "x".into(),
                r#type: 1, // int
            }],
            blocks: vec![block(
                0,
                vec![convert_instr(2, 2, 1), instr("Return")], // int8(x)
                vec![],
            )],
            ..Default::default()
        };
        let p = pkg_with_int_types(vec![f]);
        let fid = p.lookup_func("t.F").unwrap();
        let reqs = BoundsChecker.infer_requires(&p, fid, &no_summaries, &mut z3_discharge());
        let overflow: Vec<_> = reqs.iter().filter(|c| c.tag == "overflow").collect();
        assert_eq!(overflow.len(), 1, "one overflow requires: {reqs:?}");
        let fv = overflow[0].formula.term.free_vars();
        let names: Vec<&String> = fv.keys().collect();
        assert_eq!(names, vec!["p0"], "requires is over x only: {reqs:?}");
        let text = Query::for_asserts(Logic::QfBv, vec![overflow[0].formula.term.clone()])
            .canonical_text();
        assert!(
            text.contains("18446744073709551488"),
            "int8 MIN sign-extended to 64 bits: {text}"
        );
        assert!(text.contains("(_ bv127 64)"), "int8 MAX at 64 bits: {text}");
    }

    #[test]
    fn unsigned_narrowing_uses_unsigned_bound() {
        // uint16(u) for u uint32 ⇒ violation (bvult 0xFFFF u): encoded
        // as Ult(lit, u) since u > 0xFFFF is the unsigned overflow.
        let f = goverify_extract::gvir::Function {
            id: "t.F".into(),
            params: vec![goverify_extract::gvir::Param {
                id: 1,
                name: "u".into(),
                r#type: 5, // uint32
            }],
            blocks: vec![block(
                0,
                vec![convert_instr(2, 4, 1), instr("Return")], // uint16(u)
                vec![],
            )],
            ..Default::default()
        };
        let p = pkg_with_int_types(vec![f]);
        let fid = p.lookup_func("t.F").unwrap();
        let reqs = BoundsChecker.infer_requires(&p, fid, &no_summaries, &mut z3_discharge());
        let overflow: Vec<_> = reqs.iter().filter(|c| c.tag == "overflow").collect();
        assert_eq!(overflow.len(), 1, "one overflow requires: {reqs:?}");
        let text = Query::for_asserts(Logic::QfBv, vec![overflow[0].formula.term.clone()])
            .canonical_text();
        assert!(
            text.contains("(bvult (_ bv65535 32) p0)"),
            "unsigned narrowing bound is 0xFFFF, unsigned compare: {text}"
        );
    }

    #[test]
    fn widening_and_same_width_convert_infers_nothing() {
        // int32(x) for x int8 ⇒ widening: no overflow site at all.
        let f = goverify_extract::gvir::Function {
            id: "t.F".into(),
            params: vec![goverify_extract::gvir::Param {
                id: 1,
                name: "x".into(),
                r#type: 2, // int8
            }],
            blocks: vec![block(
                0,
                vec![convert_instr(2, 3, 1), instr("Return")], // int32(x)
                vec![],
            )],
            ..Default::default()
        };
        let p = pkg_with_int_types(vec![f]);
        let fid = p.lookup_func("t.F").unwrap();
        let reqs = BoundsChecker.infer_requires(&p, fid, &no_summaries, &mut z3_discharge());
        assert!(
            reqs.iter().all(|c| c.tag != "overflow"),
            "widening convert must not infer anything: {reqs:?}"
        );
        let obs = BoundsChecker.obligations(&p, fid, &no_summaries);
        assert!(
            obs.iter().all(|o| o.tag != "overflow"),
            "widening convert must not obligate anything: {obs:?}"
        );
    }

    #[test]
    fn sign_change_same_width_is_overflow_site() {
        // uint64(x) for x int: violated when x < 0.
        let f = goverify_extract::gvir::Function {
            id: "t.F".into(),
            params: vec![goverify_extract::gvir::Param {
                id: 1,
                name: "x".into(),
                r#type: 1, // int
            }],
            blocks: vec![block(
                0,
                vec![convert_instr(2, 6, 1), instr("Return")], // uint64(x)
                vec![],
            )],
            ..Default::default()
        };
        let p = pkg_with_int_types(vec![f]);
        let fid = p.lookup_func("t.F").unwrap();
        let reqs = BoundsChecker.infer_requires(&p, fid, &no_summaries, &mut z3_discharge());
        let overflow: Vec<_> = reqs.iter().filter(|c| c.tag == "overflow").collect();
        assert_eq!(overflow.len(), 1, "one overflow requires: {reqs:?}");
        let text = Query::for_asserts(Logic::QfBv, vec![overflow[0].formula.term.clone()])
            .canonical_text();
        assert!(
            text.contains("(bvslt p0 (_ bv0 64))"),
            "sign-change-only violation is exactly x < 0: {text}"
        );
        assert!(
            !text.contains("bvult"),
            "same-width signed->unsigned has no second disjunct: {text}"
        );
    }
}
