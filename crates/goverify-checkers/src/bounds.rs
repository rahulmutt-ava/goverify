//! BoundsChecker (phase-4 spec §4): path-sensitive index/slice-bounds
//! safety on the gated SSA encoding, mirroring nil.rs's two-pass shape
//! (`shared::propagate_requires`/`shared::call_site_obligations`
//! factor out the parts that are byte-for-byte identical across
//! checkers). This task covers `Index`/`IndexAddr`/`Slice` sites only;
//! `div-zero`/`overflow` are Task 9.

use goverify_analysis::{
    Checker, Clause, EncodedFunc, Formula, Obligation, Summary, array_len, encode_func,
    seq_datatype,
};
use goverify_ir::{FuncId, Function, MakeKind, Op, Pos, Program, ValueId, ValueKind};
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

/// One bounds site: the block it's in, its violation term, its source
/// position, and every IR value the violation was actually built from
/// (used only by `obligations`' groundness test — see `expressible`).
struct Site {
    block: usize,
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
        // Own index/slice sites: only ever a requires-clause when the
        // violation is expressible purely over this function's own
        // params (a manifest/ground violation can't be parameterized —
        // it's `obligations`'s job, not a precondition on the caller)
        // and its path is confirmed reachable (Sat-gated).
        for site in bounds_sites(p, func, &enc) {
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
                    tag: "bounds".into(),
                    formula: Formula { term: bound },
                },
            );
        }
        propagate_requires(p, func, &enc, "bounds", summary_of, discharge, &mut out);
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
        // must be expressible (see `expressible`) — an index/bound
        // pulled from an unmodeled call (havoc) stays silent (FP storm
        // guard: we cannot say anything about an arbitrary heap value).
        for site in bounds_sites(p, func, &enc) {
            if !site.values.iter().all(|&v| expressible(p, func, v)) {
                continue;
            }
            let mut extra = pre.clone();
            extra.push(site.violation);
            out.push(Obligation {
                tag: "bounds".into(),
                message: format!("index/slice out of bounds in {}", p.func_name(f)),
                pos: site.pos,
                query: enc.reach_query(site.block, extra),
            });
        }

        out.extend(call_site_obligations(
            p, func, &enc, "bounds", &pre, summary_of,
        ));
        out
    }
}

#[cfg(test)]
mod tests {
    use goverify_analysis::Checker;
    use goverify_solver::{Logic, Query, SatResult, Sort};

    use super::*;
    use crate::testfix::{
        binop_instr, block, branch_on, call_builtin, call_static, index_addr_instr, instr, int_aux,
        make_slice_instr, no_summaries, pkg_with_seq_types, slice_instr, z3_discharge,
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
}
