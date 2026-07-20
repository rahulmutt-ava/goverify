# FP/Encoding Fix-Wave Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Eliminate the five characterized FP/encoding mechanisms from the phase-4 bbolt shakeout (156 classes / 393 findings) by fixing semantics in the shared SMT encoding layer.

**Architecture:** All fixes land where IR lowers to SMT terms (`goverify-analysis::encode::op_def` and a new load-forwarding pass) plus the NilChecker discharge path (dominating-deref assumptions), so both the findings pass and `infer_requires` stop producing spurious facts at the source. Spec: `docs/superpowers/specs/2026-07-20-fp-encoding-fixwave-design.md`.

**Tech Stack:** Rust (crates `goverify-analysis`, `goverify-checkers`), Z3 via `goverify-solver`, Go corpus modules under `testdata/corpus/`.

## Global Constraints

- Run every toolchain command through mise: `mise x -- cargo ...`, `mise run <task>` (sandbox relocates RUSTUP_HOME via `mise.local.toml`; bare `cargo` may not exist).
- Commits are unsigned in this sandbox: always `git commit --no-gpg-sign`.
- Determinism is the root invariant: no timestamps, no absolute paths, no map-iteration order reaching any output. Use `BTreeMap`, not `HashMap`, anywhere order can reach the encoding.
- Only Go code lives in `extractor/`; everything else is Rust.
- Degrade, never die: every fallible Term construction uses the `.ok()?` / `let ... else { continue }` pattern; recursion over IR read from `.gvir` bytes must be depth-capped (fuzzed input may contain cycles).
- No `.gvir` schema change in this wave (`proto/gvir/v1/gvir.proto` untouched, no version bumps).
- Solver `Term` constructors return `Result`: `Term::eq/not/or/bv_*` need `.ok()?`; `ptr_is_nil(t)` returns `Result<Term>`.
- The whole-body encoding asserts SSA defining equalities unconditionally; block guards (`g<b>`) carry reachability. Non-nil facts on SSA dsts are asserted unconditionally too (constraining a dst never constrains its base).

## File Structure

| File | Change |
|---|---|
| `crates/goverify-analysis/src/encode.rs` | new `op_def` arms (fixes 1, 3, 4), load-forwarding pass (fix 2a) |
| `crates/goverify-analysis/src/dom.rs` | NEW — topo order + immediate dominators over the cut DAG (fix 2b) |
| `crates/goverify-analysis/src/lib.rs` | export `dom` items |
| `crates/goverify-checkers/src/shared.rs` | `checked_deref_assumptions` helper; `assume` callback param on `propagate_requires` / `call_site_obligations` (fix 2b) |
| `crates/goverify-checkers/src/nil.rs` | wire assumptions into all NilChecker queries (fix 2b) |
| `crates/goverify-checkers/src/bounds.rs` | pass no-op `assume` callback (bounds untouched by this wave) |
| `testdata/corpus/nil/nil.go` | fix-2 red/green cases |
| `testdata/corpus/knownfp/knownfp.go` | pin flips + fix-3 green case |
| `docs/threat-model.md` | "Deliberate under-approximations" section |
| `docs/shakeout-phase4-bbolt.md` | re-run addendum |
| Fix 5 location | determined by Task 7's investigation (likely subsumed by fixes 1/2) |

---

### Task 1: Fix 1 — never-nil Alloc/FieldAddr/IndexAddr

**Files:**
- Modify: `crates/goverify-analysis/src/encode.rs` (imports ~line 14; `op_def` before the `_ => None` arm ~line 695; tests `mod tests` at ~line 896)
- Modify: `testdata/corpus/knownfp/knownfp.go` (BuildSurgeryOptions pin, ~line 44-60)

**Interfaces:**
- Consumes: `op_def(p, func, block, op, enc) -> Option<Term>` (existing), `ptr_is_nil(Term) -> Result<Term>`, `ptr_sort() -> Sort` from `goverify_solver`.
- Produces: encoding-level guarantee later tasks rely on: every `Alloc`/`FieldAddr`/`IndexAddr` dst with Ptr sort carries a `¬is_nil` assert.

- [ ] **Step 1: Write the failing encode test**

In `encode.rs`'s existing `mod tests` (line ~896), add. Build the program the way the neighboring tests do — reuse `types_program()`'s type table (type id 5 is `*T`) and the `testpkg::{block, func, instr, pkg}` builders used by the tests around it; the instruction under test is a `FieldAddr` with dst register 2 and base = param 0 (clone the construction from the nearest existing test that encodes a `FieldAddr`, e.g. any test using `instr("FieldAddr")`, keeping its `sem`/operand shape):

```rust
#[test]
fn address_of_ops_assert_nonnil_dst() {
    // f(p *T): v2 = FieldAddr p .0 — fix-wave fix 1: the dst of an
    // address-of op is never nil; the encoding must carry that fact.
    let p = /* program with f(p0 *T) { v2 = FieldAddr p0 .0 } — clone
             the FieldAddr construction from the nearest existing
             encode test and keep type id 5 (*T) as v2's type */;
    let enc = encode_func(&p, FuncId(0)).unwrap();
    let d = enc.value(goverify_ir::ValueId(2)).unwrap().clone();
    let want = Term::not(goverify_solver::ptr_is_nil(d).unwrap()).unwrap();
    assert!(
        enc.asserts.contains(&want),
        "FieldAddr dst must carry a non-nil assert (fix 1)"
    );
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `mise x -- cargo test -p goverify-analysis address_of_ops_assert_nonnil_dst`
Expected: FAIL on the `assert!` (no such assert today — `FieldAddr` falls through to havoc).

- [ ] **Step 3: Implement the op_def arms**

In `encode.rs`, extend the solver import (line ~14) with `ptr_is_nil`:

```rust
use goverify_solver::{
    BvBinOp, BvCmpOp, CtorDecl, DatatypeDecl, Logic, Query, Sort, Term, ptr_datatype,
    ptr_is_nil, ptr_nil, ptr_sort,
};
```

Add to `op_def`, immediately before the `_ => None` catch-all (line ~695):

```rust
// Address-of ops never produce nil (fix-wave fix 1): a Go
// allocation, field address, or element address is a valid non-nil
// address — the op faults on a bad base before a value exists, so
// any continuing execution holds a non-nil dst. The base's own
// nilability stays a separate obligation at its own site.
Op::Alloc { dst, .. } | Op::FieldAddr { dst, .. } | Op::IndexAddr { dst, .. } => {
    let d = t(dst)?;
    if d.sort() != &ptr_sort() {
        return None;
    }
    Term::not(ptr_is_nil(d).ok()?).ok()
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `mise x -- cargo test -p goverify-analysis address_of_ops_assert_nonnil_dst`
Expected: PASS.

- [ ] **Step 5: Reconcile the crate + checker suites**

Run: `mise x -- cargo test -p goverify-analysis -p goverify-checkers`
Expected: unit tests pass. If any existing test pinned the old havoc behavior of these dsts, update it to the new expectation and say so in the commit message.

- [ ] **Step 6: Flip the mechanism-2 knownfp pin**

Run: `mise x -- cargo test -p goverify-checkers --test knownfp_corpus`
Expected: FAIL — `BuildSurgeryOptions`'s pinned finding is gone (that is the fix working; the pin still `want`s it).

In `testdata/corpus/knownfp/knownfp.go`: on the `return o.Validate()` line, delete the `// want: nil-deref` comment, and reword the pin's leading comment block from `KNOWN-FP(phase-5): FP/encoding — address-of ...` to:

```go
// FIXED (fix-wave 2026-07-20, fix 1): formerly KNOWN-FP — address-of
// stack-local / composite-literal / slice-element / value-typed field
// (mechanism group 2, 48 classes / 111 findings; exemplars C009b,
// C002b). Alloc/FieldAddr/IndexAddr dsts now carry a non-nil fact, so
// calling the promoted (&o.baseOptions).Validate() on a stack-local
// no longer reports. Kept as the green regression case.
```

Run: `mise x -- cargo test -p goverify-checkers --test knownfp_corpus --test nil_corpus --test bounds_corpus`
Expected: all PASS — the nil/bounds corpora are fix 1's red guard: every existing `// want:` on a genuinely nilable deref must still fire with address-of values now constrained.

- [ ] **Step 7: Commit**

```bash
git add crates/goverify-analysis/src/encode.rs testdata/corpus/knownfp/knownfp.go
git commit --no-gpg-sign -m "fixwave: never-nil Alloc/FieldAddr/IndexAddr dsts (fix 1, mechanism 2)"
```

---

### Task 2: Fix 2a — same-function load forwarding

**Files:**
- Modify: `crates/goverify-analysis/src/encode.rs` (`encode_func` ~line 259; new pass + tests)
- Modify: `testdata/corpus/nil/nil.go` (red/green corpus cases)

**Interfaces:**
- Consumes: `EncodedFunc { asserts, values, dag_succs, .. }`, `Op::{Assign, FieldAddr, IndexAddr, Load, Store, Havoc}`.
- Produces: `fn topo_order(dag: &[Vec<u32>]) -> Vec<usize>` — `pub(crate)`, reused by Task 3's `dom.rs`. Forwarding guarantee: two `Load`s of the same value-numbered address with no intervening `Store`/dst-less-`Havoc` (in topo walk order) have equal dst terms.

- [ ] **Step 1: Write the failing encode tests**

In `encode.rs` `mod tests` (same builder approach as Task 1; two `FieldAddr`+`Load` pairs off param 0, registers: v2=FieldAddr p0 .0, v3=Load v2, v4=FieldAddr p0 .0, v5=Load v4; the loaded field type must be modelable — use `*T` (type id 5) as the field/elem type so Load dsts get Ptr sort):

```rust
#[test]
fn repeated_field_load_is_forwarded() {
    let p = /* f(p0 *T): v2=FieldAddr p0 .0; v3=Load v2; v4=FieldAddr p0 .0; v5=Load v4 */;
    let enc = encode_func(&p, FuncId(0)).unwrap();
    let d = enc.value(goverify_ir::ValueId(5)).unwrap().clone();
    let prev = enc.value(goverify_ir::ValueId(3)).unwrap().clone();
    let want = Term::eq(d, prev).unwrap();
    assert!(enc.asserts.contains(&want), "second load forwarded to first (fix 2a)");
}

#[test]
fn store_between_loads_kills_forwarding() {
    let p = /* same, with `Store v2 <- p0` inserted between v3 and v4 */;
    let enc = encode_func(&p, FuncId(0)).unwrap();
    let d = enc.value(goverify_ir::ValueId(5)).unwrap().clone();
    let prev = enc.value(goverify_ir::ValueId(3)).unwrap().clone();
    let eq = Term::eq(d, prev).unwrap();
    assert!(!enc.asserts.contains(&eq), "a Store must invalidate forwarding");
}
```

- [ ] **Step 2: Run to verify both fail correctly**

Run: `mise x -- cargo test -p goverify-analysis load`
Expected: `repeated_field_load_is_forwarded` FAILS (no forwarding today); `store_between_loads_kills_forwarding` PASSES vacuously — that's fine, it guards the implementation.

- [ ] **Step 3: Implement the pass**

In `encode.rs`, add (near `encode_ops`):

```rust
/// Structural value number for an address: Assign-transparent;
/// FieldAddr keyed by (base key, field); IndexAddr by (base key,
/// index value). Everything else is its own root. Two addresses with
/// equal keys compute the same location in SSA (address ops are pure).
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
enum AddrKey {
    Root(ValueId),
    Field(Box<AddrKey>, u32),
    Index(Box<AddrKey>, ValueId),
}

fn key_of(keys: &BTreeMap<ValueId, AddrKey>, v: ValueId) -> AddrKey {
    keys.get(&v).cloned().unwrap_or(AddrKey::Root(v))
}

/// DFS postorder from the entry over the cut DAG, reversed — a
/// topological order (the DAG is acyclic by construction). Only blocks
/// reachable from the entry appear; unreachable blocks have guards
/// pinned false, nothing to forward.
pub(crate) fn topo_order(dag: &[Vec<u32>]) -> Vec<usize> {
    let n = dag.len();
    let mut order = Vec::with_capacity(n);
    if n == 0 {
        return order;
    }
    let mut state = vec![0u8; n];
    let mut stack: Vec<(usize, usize)> = vec![(0, 0)];
    state[0] = 1;
    while let Some(frame) = stack.last_mut() {
        let (b, i) = (frame.0, frame.1);
        if i < dag[b].len() {
            frame.1 += 1;
            let s = dag[b][i] as usize;
            if s < n && state[s] == 0 {
                state[s] = 1;
                stack.push((s, 0));
            }
        } else {
            order.push(b);
            stack.pop();
        }
    }
    order.reverse();
    order
}

/// Same-function load forwarding (fix-wave fix 2a): repeated loads of
/// the same value-numbered address see the same value when no
/// potentially-aliasing write can intervene. Blocks are walked in
/// topological order, so any Store on any path between two loads sits
/// between them in walk order (topo order respects edges; a parallel
/// branch's Store only costs precision, never soundness). A Store or a
/// dst-less Havoc (unmodeled op with unknown effect) conservatively
/// clears ALL pending forwards. Calls deliberately do NOT invalidate:
/// assuming callees don't mutate a re-read field is a documented
/// under-approximation (threat model, "deliberate under-
/// approximations") — the bug-finder invariant prefers a missed exotic
/// mutation to the shakeout's dominant FP class (mechanism 1).
fn encode_load_forwarding(func: &Function, enc: &mut EncodedFunc) {
    let mut keys: BTreeMap<ValueId, AddrKey> = BTreeMap::new();
    let mut seen: BTreeMap<AddrKey, ValueId> = BTreeMap::new();
    for b in topo_order(&enc.dag_succs) {
        let Some(block) = func.blocks.get(b) else { continue };
        for ins in &block.instrs {
            match &ins.op {
                Op::Assign { dst, src } => {
                    if let Some(k) = keys.get(src).cloned() {
                        keys.insert(*dst, k);
                    }
                }
                Op::FieldAddr { dst, base, field } => {
                    let bk = key_of(&keys, *base);
                    keys.insert(*dst, AddrKey::Field(Box::new(bk), *field));
                }
                Op::IndexAddr { dst, base, index } => {
                    let bk = key_of(&keys, *base);
                    keys.insert(*dst, AddrKey::Index(Box::new(bk), *index));
                }
                Op::Load { dst, addr } => {
                    let k = key_of(&keys, *addr);
                    if let Some(prev) = seen.get(&k) {
                        if let (Some(d), Some(pv)) =
                            (enc.values.get(dst).cloned(), enc.values.get(prev).cloned())
                            && let Ok(eq) = Term::eq(d, pv)
                        {
                            enc.asserts.push(eq);
                        }
                    } else {
                        seen.insert(k, *dst);
                    }
                }
                Op::Store { .. } | Op::Havoc { dst: None } => seen.clear(),
                _ => {}
            }
        }
    }
}
```

Call it in `encode_func` right after `encode_ops(p, func, &mut enc);` (line ~259), before the `ASSERT_CAP` check:

```rust
    encode_ops(p, func, &mut enc);
    encode_load_forwarding(func, &mut enc);
```

- [ ] **Step 4: Run the encode tests**

Run: `mise x -- cargo test -p goverify-analysis load`
Expected: both PASS.

- [ ] **Step 5: Add the corpus red/green cases**

Append to `testdata/corpus/nil/nil.go`:

```go
// --- fix-wave fix 2a: same-function dominating check carried forward ---

type inner struct{ n int }

type holder struct{ cached *inner }

func use(i *inner) int { return i.n } // infers requires: i != nil

func observe() {}

// RecheckedField mirrors bbolt C015a: the nil-check dominates the use,
// and the intervening call must not invalidate the forwarded load of
// h.cached. Green: no finding.
func RecheckedField(h *holder) int {
	if h.cached == nil {
		return 0
	}
	observe()
	return use(h.cached)
}

// StoreInvalidates: a store to the checked field between check and use
// makes the re-read genuinely unconstrained again — must still report.
func StoreInvalidates(h *holder, fresh *inner) int {
	if h.cached == nil {
		return 0
	}
	h.cached = fresh
	return use(h.cached) // want: nil-deref
}
```

- [ ] **Step 6: Run the nil corpus and reconcile**

Run: `mise x -- cargo test -p goverify-checkers --test nil_corpus`
Expected: PASS — `RecheckedField` reports nothing, `StoreInvalidates` reports at the want line. If a pre-existing corpus want changed because two loads of the same address now share a term, verify the new behavior is the intended first-iteration semantics before updating the want, and record it in the commit message.

- [ ] **Step 7: Full crate tests + commit**

Run: `mise x -- cargo test -p goverify-analysis -p goverify-checkers`
Expected: PASS.

```bash
git add crates/goverify-analysis/src/encode.rs testdata/corpus/nil/nil.go
git commit --no-gpg-sign -m "fixwave: same-function load forwarding (fix 2a, mechanism 1)"
```

---

### Task 3: cut-DAG dominators helper

**Files:**
- Create: `crates/goverify-analysis/src/dom.rs`
- Modify: `crates/goverify-analysis/src/lib.rs` (module + re-export)
- Modify: `crates/goverify-analysis/src/encode.rs` (move `topo_order` into `dom.rs`, re-import)

**Interfaces:**
- Consumes: `EncodedFunc.dag_succs: Vec<Vec<u32>>` (acyclic by construction — back edges are cut).
- Produces: `pub fn dominators(dag_succs: &[Vec<u32>]) -> Vec<Option<usize>>` (idom per block; `idom[0] == Some(0)`; `None` = unreachable) and `pub fn strictly_dominates(idom: &[Option<usize>], a: usize, b: usize) -> bool`. Task 4 consumes both from `goverify_analysis`.

- [ ] **Step 1: Write the failing tests**

Create `crates/goverify-analysis/src/dom.rs` with tests first:

```rust
//! Immediate dominators over the encoder's cut DAG (fix-wave fix 2b):
//! Cooper–Harvey–Kennedy, one pass in topological order (sufficient on
//! an acyclic graph — every pred is finalized before its successors).

use std::collections::BTreeMap;

pub(crate) use crate::encode::topo_order;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diamond_joins_at_entry() {
        // 0 -> {1,2} -> 3: idom(3) = 0; 1 and 2 do NOT dominate 3.
        let dag = vec![vec![1, 2], vec![3], vec![3], vec![]];
        let idom = dominators(&dag);
        assert_eq!(idom, vec![Some(0), Some(0), Some(0), Some(0)]);
        assert!(strictly_dominates(&idom, 0, 3));
        assert!(!strictly_dominates(&idom, 1, 3));
        assert!(!strictly_dominates(&idom, 2, 3));
        assert!(!strictly_dominates(&idom, 3, 3), "strict: never self");
    }

    #[test]
    fn chain_dominates_transitively() {
        // 0 -> 1 -> 2: both 0 and 1 strictly dominate 2.
        let dag = vec![vec![1], vec![2], vec![]];
        let idom = dominators(&dag);
        assert_eq!(idom, vec![Some(0), Some(0), Some(1)]);
        assert!(strictly_dominates(&idom, 0, 2));
        assert!(strictly_dominates(&idom, 1, 2));
    }

    #[test]
    fn unreachable_block_has_no_idom() {
        // 0 -> 1; 2 floats (unreachable — e.g. an all-in-edges-cut loop).
        let dag = vec![vec![1], vec![], vec![1]];
        let idom = dominators(&dag);
        assert_eq!(idom[2], None);
        assert!(!strictly_dominates(&idom, 2, 1), "unreachable dominates nothing");
    }
}
```

- [ ] **Step 2: Run to verify compile failure**

Run: `mise x -- cargo test -p goverify-analysis dom::`
Expected: FAIL to compile (`dominators` not defined). First make `topo_order` `pub(crate)` in `encode.rs` if Task 2 didn't already, register `mod dom;` in `lib.rs`.

- [ ] **Step 3: Implement**

Add to `dom.rs` above the tests:

```rust
/// idom per block over the cut DAG. `idom[0] = Some(0)` (the entry is
/// its own root); `None` = unreachable from the entry. One pass in
/// topological order: acyclic graph, so every processed pred is final.
pub fn dominators(dag_succs: &[Vec<u32>]) -> Vec<Option<usize>> {
    let n = dag_succs.len();
    let mut preds: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (b, ss) in dag_succs.iter().enumerate() {
        for &s in ss {
            if (s as usize) < n {
                preds[s as usize].push(b);
            }
        }
    }
    let order = topo_order(dag_succs);
    let pos: BTreeMap<usize, usize> = order.iter().enumerate().map(|(i, &b)| (b, i)).collect();
    let mut idom: Vec<Option<usize>> = vec![None; n];
    if n == 0 {
        return idom;
    }
    idom[0] = Some(0);
    for &b in order.iter().skip(1) {
        let mut new: Option<usize> = None;
        for &pd in &preds[b] {
            if idom[pd].is_none() {
                continue; // unreachable pred contributes nothing
            }
            new = Some(match new {
                None => pd,
                Some(cur) => intersect(&idom, &pos, cur, pd),
            });
        }
        idom[b] = new;
    }
    idom
}

/// CHK two-finger intersection walking idom chains by topo position.
fn intersect(
    idom: &[Option<usize>],
    pos: &BTreeMap<usize, usize>,
    mut a: usize,
    mut b: usize,
) -> usize {
    while a != b {
        let (Some(&pa), Some(&pb)) = (pos.get(&a), pos.get(&b)) else {
            return 0; // degraded input: fall back to the entry
        };
        if pa > pb {
            a = idom[a].unwrap_or(0);
        } else {
            b = idom[b].unwrap_or(0);
        }
    }
    a
}

/// True iff `a` strictly dominates `b`: a != b and a is on b's idom
/// chain. Walks at most n links (idom chains are acyclic toward the
/// entry); any degraded/None link means "don't know" = false.
pub fn strictly_dominates(idom: &[Option<usize>], a: usize, b: usize) -> bool {
    if a == b {
        return false;
    }
    let mut cur = b;
    for _ in 0..idom.len() {
        match idom.get(cur).copied().flatten() {
            Some(d) if d == a => return true,
            Some(d) if d == cur => return false, // reached the entry
            Some(d) => cur = d,
            None => return false,
        }
    }
    false
}
```

In `lib.rs` add `mod dom;` and `pub use dom::{dominators, strictly_dominates};`.

- [ ] **Step 4: Run the tests**

Run: `mise x -- cargo test -p goverify-analysis dom::`
Expected: 3 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/goverify-analysis/src/dom.rs crates/goverify-analysis/src/lib.rs crates/goverify-analysis/src/encode.rs
git commit --no-gpg-sign -m "fixwave: cut-DAG dominators helper (fix 2b groundwork)"
```

---

### Task 4: Fix 2b — checked-deref assumptions in NilChecker

**Files:**
- Modify: `crates/goverify-checkers/src/shared.rs` (new helper; `assume` param on both call-site loops)
- Modify: `crates/goverify-checkers/src/nil.rs` (wire assumptions into all four query paths)
- Modify: `crates/goverify-checkers/src/bounds.rs` (no-op callback at its two `shared::` call sites)
- Modify: `testdata/corpus/nil/nil.go` (dominating-deref green case + non-dominating red case)

**Interfaces:**
- Consumes: `goverify_analysis::{dominators, strictly_dominates}` (Task 3), `deref_sites(p, func) -> Vec<(usize, usize, ValueId, Option<Pos>)>` (nil.rs:21), `EncodedFunc::{value, guards, reach_query}`.
- Produces: `shared::checked_deref_assumptions(sites, enc, idom, block, instr) -> Vec<Term>`; changed signatures `propagate_requires(..., assume: &dyn Fn(usize, usize) -> Vec<Term>, ...)` and `call_site_obligations(..., assume: &dyn Fn(usize, usize) -> Vec<Term>)` — the callback maps (block, instr index) to the assumption conjuncts for a query raised there.

- [ ] **Step 1: Write the failing unit test**

In `nil.rs`'s `mod tests`, clone the existing `const_nil_arg_produces_obligation_other_args_dont` test (nil.rs:333) — it already builds a caller invoking a callee whose summary requires `¬nil(p0)` and discharges through `z3_discharge()`. Modify the clone into:

```rust
#[test]
fn dominating_deref_discharges_call_obligation() {
    // Caller: v3 = FieldAddr p0 .0 ; v4 = Load v3 ; v5 = FieldAddr v4 .0
    // (a dereference of the LOADED, havoc'd pointer v4) ; then
    // Call callee(v4) where callee requires ¬nil(p0).
    // Before fix 2b the call obligation is Sat (v4 is a free heap
    // value). With the dominating deref at v5 assumed to have
    // succeeded, it must discharge Unsat: no finding.
    /* build as in const_nil_arg_produces_obligation_other_args_dont,
       swapping the const-nil arg for the loaded v4 and inserting the
       v5 FieldAddr deref instruction BEFORE the call */
    let obs = /* NilChecker.obligations(...) */;
    let mut d = z3_discharge();
    assert!(
        obs.iter().all(|o| d(&o.query) != SatResult::Sat),
        "call obligation dominated by a prior deref of the same value must be Unsat (fix 2b)"
    );
}

#[test]
fn later_deref_grants_no_assumption() {
    // Same shape but the extra FieldAddr deref of v4 comes AFTER the
    // call instruction: nothing dominates the call, obligation stays Sat.
    let obs = /* same build, deref instruction moved after the call */;
    let mut d = z3_discharge();
    assert!(
        obs.iter().any(|o| d(&o.query) == SatResult::Sat),
        "an obligation with no dominating deref must survive (fix 2b red)"
    );
}
```

- [ ] **Step 2: Run to verify the first fails**

Run: `mise x -- cargo test -p goverify-checkers dominating_deref`
Expected: `dominating_deref_discharges_call_obligation` FAILS (Sat today); `later_deref_grants_no_assumption` PASSES vacuously.

- [ ] **Step 3: Implement the shared helper**

Add to `shared.rs` (new imports: `goverify_ir::{Pos, ValueId}`, `goverify_analysis::strictly_dominates`, `goverify_solver::ptr_is_nil`):

```rust
/// Checked-deref assumptions (fix-wave fix 2b): for an obligation at
/// (block, instr), every nil-deref site that strictly precedes it on
/// EVERY execution reaching it — same block earlier, or a strictly
/// dominating block — already executed without panicking, so its
/// subject was non-nil on that execution. Emitted guard-conditioned
/// (¬g_site ∨ ¬is_nil(subj)) so a dominance bug can only lose
/// precision, never invent a fact. The obligation's own site is
/// excluded by the strict ordering: a genuine first-failure site still
/// fires, and a finding masked by an earlier one reappears once the
/// earlier is fixed.
pub(crate) fn checked_deref_assumptions(
    sites: &[(usize, usize, ValueId, Option<Pos>)],
    enc: &EncodedFunc,
    idom: &[Option<usize>],
    block: usize,
    instr: usize,
) -> Vec<Term> {
    let mut out = Vec::new();
    for (bj, ij, subj, _) in sites {
        let before =
            (*bj == block && *ij < instr) || strictly_dominates(idom, *bj, block);
        if !before {
            continue;
        }
        let Some(s) = enc.value(*subj).cloned() else { continue };
        let Ok(is_nil) = ptr_is_nil(s) else { continue };
        let Ok(nonnil) = Term::not(is_nil) else { continue };
        let Some(g) = enc.guards.get(*bj).cloned() else { continue };
        let Ok(ng) = Term::not(g) else { continue };
        if let Ok(implied) = Term::or(vec![ng, nonnil]) {
            out.push(implied);
        }
    }
    out
}
```

Change both shared loops to carry instruction indices and the callback:

```rust
pub(crate) fn propagate_requires(
    p: &Program,
    func: &Function,
    enc: &EncodedFunc,
    tag: &str,
    summary_of: &dyn Fn(FuncId) -> Summary,
    discharge: &mut dyn FnMut(&Query) -> SatResult,
    assume: &dyn Fn(usize, usize) -> Vec<Term>,
    out: &mut Vec<Clause>,
) {
```

with the loop enumerating instruction indices and the query gaining the assumptions:

```rust
    for (bi, ii, ins) in func.blocks.iter().enumerate().flat_map(|(bi, b)| {
        b.instrs.iter().enumerate().map(move |(ii, i)| (bi, ii, i))
    }) {
        // ... unchanged callee/instantiation logic ...
            let mut extra = assume(bi, ii);
            extra.push(violation);
            if discharge(&enc.reach_query(bi, extra)) != SatResult::Sat {
                continue;
            }
        // ...
    }
```

`call_site_obligations` identically: add the `assume: &dyn Fn(usize, usize) -> Vec<Term>` parameter, change the inner loop to `for (ii, ins) in b.instrs.iter().enumerate()`, and build `extra` as `pre.to_vec()`, then `extra.extend(assume(bi, ii));`, then `extra.push(v);`.

- [ ] **Step 4: Wire NilChecker**

In `nil.rs`, both trait methods build the provider once per function and pass it everywhere:

```rust
        let sites = deref_sites(p, func);
        let idom = goverify_analysis::dominators(&enc.dag_succs);
        let assume = |bi: usize, ii: usize| {
            crate::shared::checked_deref_assumptions(&sites, &enc, &idom, bi, ii)
        };
```

- `infer_requires` own-site loop: keep the site index (`for (bi, ii, subject, _pos) in &sites`) and discharge `enc.reach_query(*bi, { let mut e = assume(*bi, *ii); e.push(is_nil.clone()); e })`.
- `infer_requires` tail: `propagate_requires(p, func, &enc, "nil-deref", summary_of, discharge, &assume, &mut out);`
- `obligations` manifest loop: same pattern — `let mut extra = pre.clone(); extra.extend(assume(*bi, *ii)); extra.push(is_nil);`.
- `obligations` tail: pass `&assume` to `call_site_obligations`.

In `bounds.rs`, at its `propagate_requires` and `call_site_obligations` call sites, pass a no-op: `&|_, _| Vec::new()` (bounds is untouched by this wave — spec §3 fix 2b).

- [ ] **Step 5: Run the unit tests**

Run: `mise x -- cargo test -p goverify-checkers dominating_deref later_deref`
Expected: both PASS.

- [ ] **Step 6: Corpus cases**

Append to `testdata/corpus/nil/nil.go` (types from Task 2's block):

```go
// DerefThenCall (fix 2b): the field read itself — not a nil-check —
// dominates the call; reaching the call means the deref succeeded, so
// the callee's non-nil requirement is met. Green: no finding.
func DerefThenCall(h *holder) int {
	n := h.cached.n
	return n + use(h.cached)
}

// CallThenDeref (fix 2b red): the call precedes any deref — nothing
// dominates it, the obligation must survive.
func CallThenDeref(h *holder) int {
	n := use(h.cached) // want: nil-deref
	return n + h.cached.n
}
```

Run: `mise x -- cargo test -p goverify-checkers --test nil_corpus`
Expected: PASS with exactly those wants. If a pre-existing want disappears because a dominating deref now masks it, confirm it is genuine first-failure masking (the earlier site still reports) before updating; note it in the commit.

- [ ] **Step 7: Full crate tests + commit**

Run: `mise x -- cargo test -p goverify-analysis -p goverify-checkers`
Expected: PASS.

```bash
git add crates/goverify-checkers/src/shared.rs crates/goverify-checkers/src/nil.rs crates/goverify-checkers/src/bounds.rs testdata/corpus/nil/nil.go
git commit --no-gpg-sign -m "fixwave: dominating checked-deref assumptions in NilChecker (fix 2b, mechanism 1)"
```

---

### Task 5: Fix 3 — uintptr-derived pointers are non-nil

**Files:**
- Modify: `crates/goverify-analysis/src/encode.rs` (`op_def` Convert arm + provenance helpers + tests)
- Modify: `testdata/corpus/knownfp/knownfp.go` (new green case; ReadElem pin reconciliation)

**Interfaces:**
- Consumes: `TypeKind::{Named, Basic}`; go/types Basic names: uintptr = `"uintptr"`, unsafe.Pointer = `"Pointer"` (verified against `go/types`: `types.Typ[types.UnsafePointer].Name() == "Pointer"`; no other Basic carries that name).
- Produces: encoding guarantee: a Ptr-sorted `Convert` dst with uintptr provenance carries `¬is_nil`.

- [ ] **Step 1: Write the failing encode tests**

In `encode.rs` `mod tests`, extend the test type table with two Basics — `uintptr` (kind Basic, name `"uintptr"`) and unsafe.Pointer (kind Basic, name `"Pointer"`) — alongside the existing `*T` (id 5). Then:

```rust
#[test]
fn uintptr_convert_chain_asserts_nonnil() {
    // f(p0 uintptr): v2 = Convert p0 -> unsafe.Pointer;
    //                v3 = Convert v2 -> *T.
    // The canonical (*T)(unsafe.Pointer(uintptr...)) idiom: v3 must
    // carry a non-nil assert (fix 3).
    let p = /* build with convert instrs; testfix-style Convert sem */;
    let enc = encode_func(&p, FuncId(0)).unwrap();
    let d = enc.value(goverify_ir::ValueId(3)).unwrap().clone();
    let want = Term::not(goverify_solver::ptr_is_nil(d).unwrap()).unwrap();
    assert!(enc.asserts.contains(&want), "uintptr-derived pointer non-nil (fix 3)");
}

#[test]
fn pointer_pun_stays_nilable() {
    // f(p0 *T): v2 = Convert p0 -> unsafe.Pointer; v3 = Convert v2 -> *T.
    // A plain pointer pun preserves nilability: NO non-nil assert on v3.
    let p = /* same, but p0 is *T */;
    let enc = encode_func(&p, FuncId(0)).unwrap();
    let d = enc.value(goverify_ir::ValueId(3)).unwrap().clone();
    let unwanted = Term::not(goverify_solver::ptr_is_nil(d).unwrap()).unwrap();
    assert!(!enc.asserts.contains(&unwanted), "pointer pun must stay nilable (fix 3 red)");
}
```

- [ ] **Step 2: Run to verify failure**

Run: `mise x -- cargo test -p goverify-analysis convert`
Expected: `uintptr_convert_chain_asserts_nonnil` FAILS; the red test passes vacuously.

- [ ] **Step 3: Implement**

Add helpers in `encode.rs`:

```rust
/// Basic-type name after peeling Named wrappers; None for non-basic.
fn basic_name(types: &TypeTable, t: TypeId) -> Option<&str> {
    match types.kind(t) {
        TypeKind::Named { underlying, .. } => basic_name(types, *underlying),
        TypeKind::Basic { name } => Some(name),
        _ => None,
    }
}

/// True iff `v`'s value is minted from uintptr arithmetic: v is
/// uintptr-typed, or unsafe.Pointer-typed ("Pointer" is go/types' Basic
/// name for it — no other Basic collides) and itself the dst of a
/// Convert from a uintptr-provenance value. Depth-capped: .gvir bytes
/// are untrusted and a crafted Convert cycle must degrade (to "no"),
/// never hang — parsers of bytes the analyzer didn't write reject,
/// never panic.
fn uintptr_provenance(p: &Program, func: &Function, v: ValueId, depth: u32) -> bool {
    if depth > 8 {
        return false;
    }
    match basic_name(p.types(), func.value(v).ty) {
        Some("uintptr") => true,
        Some("Pointer") => func.blocks.iter().flat_map(|b| &b.instrs).any(|ins| {
            matches!(&ins.op, Op::Convert { dst, src } if *dst == v
                && uintptr_provenance(p, func, *src, depth + 1))
        }),
        _ => false,
    }
}
```

Add the `op_def` arm (before the address-of arm from Task 1):

```rust
// A pointer minted from uintptr arithmetic is never nil (fix-wave
// fix 3): unsafe.Pointer(uintptr(base)+off) idioms compute offsets
// from live bases, and producing exactly nil would need deliberate
// 64-bit wraparound — documented under-approximation (threat model,
// "deliberate under-approximations"). A plain pointer→unsafe.Pointer
// →pointer pun keeps its nilability: its provenance is a pointer,
// not uintptr.
Op::Convert { dst, src } => {
    let d = t(dst)?;
    if d.sort() != &ptr_sort() || !uintptr_provenance(p, func, *src, 0) {
        return None;
    }
    Term::not(ptr_is_nil(d).ok()?).ok()
}
```

Note the existing `Op::Convert` in the grouped havoc arm at the top of `op_def` (`Op::Assign {..} | Op::Convert {..} | ...` — that group is in `op_values`, not `op_def`; verify `op_def` has no prior Convert arm before adding).

- [ ] **Step 4: Run the tests**

Run: `mise x -- cargo test -p goverify-analysis convert`
Expected: both PASS.

- [ ] **Step 5: Corpus green case + ReadElem pin reconciliation**

Baseline first: `mise x -- cargo test -p goverify-checkers --test knownfp_corpus` and note which findings the `ReadElem`/`elemAt` pin currently produces.

Append to `testdata/corpus/knownfp/knownfp.go`:

```go
// fix-wave fix 3 (green): the nil-deref manifestation of mechanism
// group 3 — a method with an inferred non-nil-receiver requirement is
// called on a pointer minted from uintptr arithmetic (bbolt C001's
// db.page/LeafPageElement shape). Must NOT report.
type page struct{ count uint32 }

func (p *page) Count() uint32 { return p.count }

func PageAt(buf []byte, off uintptr) uint32 {
	p := (*page)(unsafe.Pointer(uintptr(unsafe.Pointer(&buf[0])) + off))
	return p.Count()
}
```

Re-run the knownfp corpus. Reconcile the existing `ReadElem` pin by observation:
- If its `// want: bounds` finding is now gone, flip it exactly like Task 1's flip (delete the want, reword the KNOWN-FP comment to `FIXED (fix-wave 2026-07-20, fix 3)`).
- If the bounds finding persists, it is not the mechanism-3 nil-deref shape this fix targets (the pin's own comment already notes it "surfaces as a bounds obligation in this checker snapshot") — keep the want, and reword the pin comment to re-attribute the residual finding to its true family (the `&buf[0]` length / index-conversion shape, requires-lifting territory, C101-family), citing this task. Do not force the flip.

Run: `mise x -- cargo test -p goverify-checkers --test knownfp_corpus`
Expected: PASS with `PageAt` producing no finding.

- [ ] **Step 6: Full crate tests + commit**

Run: `mise x -- cargo test -p goverify-analysis -p goverify-checkers`
Expected: PASS.

```bash
git add crates/goverify-analysis/src/encode.rs testdata/corpus/knownfp/knownfp.go
git commit --no-gpg-sign -m "fixwave: uintptr-derived pointers are non-nil (fix 3, mechanism 3)"
```

---

### Task 6: Fix 4 — never-nil extern constructor table

**Files:**
- Modify: `crates/goverify-analysis/src/encode.rs` (table + `op_def` arm + test)

**Interfaces:**
- Consumes: `Callee::Static(FuncId)`, `Program::func_name(FuncId) -> &str` (fully-qualified, e.g. `flag.NewFlagSet`; methods render as `(*pkg.T).Method`).
- Produces: `const NEVER_NIL_RESULT: &[&str]` — the curated table; phase-6's annotation language externalizes it later.

- [ ] **Step 1: Write the failing encode test**

```rust
#[test]
fn table_constructor_result_is_nonnil() {
    // Two functions: "flag.NewFlagSet" (any body) and a caller whose
    // v2 = Call flag.NewFlagSet() with dst type *T. The dst must carry
    // a non-nil assert; a call to a same-shape function with a name
    // NOT in the table must not.
    let p = /* two-function program; testfix::call_static-style Call sem
             with callee id resolving to the function NAMED
             "flag.NewFlagSet"; second caller targets "pkg.NewThing" */;
    let enc = encode_func(&p, /* caller-of-flag id */).unwrap();
    let d = enc.value(goverify_ir::ValueId(2)).unwrap().clone();
    let want = Term::not(goverify_solver::ptr_is_nil(d).unwrap()).unwrap();
    assert!(enc.asserts.contains(&want), "table constructor dst non-nil (fix 4)");

    let enc2 = encode_func(&p, /* caller-of-pkg.NewThing id */).unwrap();
    let d2 = enc2.value(goverify_ir::ValueId(2)).unwrap().clone();
    let unwanted = Term::not(goverify_solver::ptr_is_nil(d2).unwrap()).unwrap();
    assert!(!enc2.asserts.contains(&unwanted), "non-table callee stays nilable (fix 4 red)");
}
```

- [ ] **Step 2: Run to verify failure**

Run: `mise x -- cargo test -p goverify-analysis table_constructor`
Expected: FAIL on the first assert.

- [ ] **Step 3: Implement**

In `encode.rs`:

```rust
/// Extern constructors documented never to return nil (fix-wave
/// fix 4). A curated lookup table, deliberately NOT inference — the
/// phase-6 annotation language externalizes it. Seeded from the
/// shakeout's mechanism-4 classes (C003, C011, C034, C073b, C122,
/// C123, C124, C207 — all 42 findings trace to flag.NewFlagSet, which
/// returns &FlagSet{...} unconditionally per its stdlib source/docs;
/// verified 2026-07-20).
const NEVER_NIL_RESULT: &[&str] = &["flag.NewFlagSet"];
```

`op_def` arm (next to the `len` builtin arm, line ~682):

```rust
Op::Call {
    dst: Some(d),
    callee: Callee::Static(fid),
    ..
} if NEVER_NIL_RESULT.contains(&p.func_name(*fid)) => {
    let dt = t(d)?;
    if dt.sort() != &ptr_sort() {
        return None;
    }
    Term::not(ptr_is_nil(dt).ok()?).ok()
}
```

- [ ] **Step 4: Run the test**

Run: `mise x -- cargo test -p goverify-analysis table_constructor`
Expected: PASS.

- [ ] **Step 5: Full crate tests + commit**

No corpus case: a real `flag` import drags the stdlib transitive closure into the corpus's whole-DAG analysis and empirically blows the run past 30 minutes (recorded in the shakeout report's Corpus pins section). The unit test covers the mechanism; Task 9's bbolt re-run validates it end-to-end against the real `flag` package.

Run: `mise x -- cargo test -p goverify-analysis -p goverify-checkers`
Expected: PASS.

```bash
git add crates/goverify-analysis/src/encode.rs
git commit --no-gpg-sign -m "fixwave: never-nil extern constructor table (fix 4, mechanism 4)"
```

---

### Task 7: Fix 5 — nil-map range raises no obligation

**Files:**
- Investigate first; likely Modify: none (subsumed) or `crates/goverify-checkers/src/nil.rs` `deref_sites` / `crates/goverify-ir/src/lower.rs` / `extractor/emit.go` per findings
- Modify: `testdata/corpus/nil/nil.go` (green case)

**Interfaces:**
- Consumes: the shakeout harness (`mise run shakeout`, pinned bbolt v1.4.0), `goverify debug` (CLI: "Inspect the analyzer's view of a module").
- Produces: the invariant later docs cite: **a map range header raises no nil-deref obligation.**

Background: `Range`/`Next` already lower to `Op::Havoc` (lower.rs:389), so the C038-family obligation is raised by the field-access chain feeding the range (`for k := range f.freemaps` → `FieldAddr`+`Load`), not by the range itself. Fixes 1/2b likely already kill it — this task verifies rather than assumes. The mechanism-5 classes to check: **C038** (5 findings, `internal/freelist/hashmap.go:237/255/271`, `shared.go:113/224`), **C362** (1 finding, `hashmap.go:126`), and the third class making up the report's 3-classes/8-findings count — identify it by grepping the committed TSV for range-header snippets (candidate: C178, `cursor.go:403`, 2 findings; confirm its verdict row attributes it to this mechanism before counting it).

- [ ] **Step 1: Reproduce against bbolt**

Run: `mise run shakeout` (first run clones pinned bbolt; warm re-run ≈ 30 s, cold ≈ 6 min). Grep the check output for the class positions above, e.g. `hashmap.go:237`.

- [ ] **Step 2: Branch on the outcome**

**If all mechanism-5 positions are clean** (expected): record in the task notes that fixes 1/2b subsume mechanism 5, and skip to Step 4.

**If any position still reports:** inspect the raising site — `mise x -- cargo run -p goverify-cli -- debug` on the bbolt module scoped to the reporting function (see `goverify debug --help` for flags; the `debug_integration` test in `crates/goverify-cli` shows working invocations). Identify which `deref_sites` entry produces it and why the own-precondition/assumption machinery misses it. Then fix at the narrowest layer that restores the invariant (most likely `deref_sites` classification in nil.rs — e.g. skip a `Load` whose only consumer is a `Range`-derived `Havoc`), with a unit test in the style of Task 4's, and re-run the shakeout grep until clean.

- [ ] **Step 3: (only if Step 2 fixed code) Run crate tests**

Run: `mise x -- cargo test -p goverify-analysis -p goverify-checkers -p goverify-ir`
Expected: PASS.

- [ ] **Step 4: Corpus green case**

Append to `testdata/corpus/nil/nil.go`:

```go
// RangeNilMap (fix 5): ranging over a nil map is legal Go — zero
// iterations, no dereference. The range header must not report.
// (Dereferencing f itself infers a requires clause on f, which is a
// summary, not a finding.)
type freeMaps struct{ maps map[uint64]bool }

func RangeNilMap(f *freeMaps) int {
	n := 0
	for k := range f.maps {
		if k > 0 {
			n++
		}
	}
	return n
}

// RangeNilMapCaller (fix 5 red): the receiver deref (f.maps reads f)
// is real — RangeNilMap's inferred requires must still propagate, so
// a nil argument still reports at the call site.
func RangeNilMapCaller() int { return RangeNilMap(nil) } // want: nil-deref
```

Run: `mise x -- cargo test -p goverify-checkers --test nil_corpus`
Expected: PASS — no finding in `RangeNilMap` itself, one at `RangeNilMapCaller`'s want line.

Update `knownfp.go`'s mechanism-5 "NOT pinned here" comment block (~line 94) to note the green case now lives in the nil corpus (`RangeNilMap`) and cite this task's outcome (subsumed by fixes 1/2b, or the specific fix made).

- [ ] **Step 5: Commit**

```bash
git add testdata/corpus/nil/nil.go testdata/corpus/knownfp/knownfp.go
# plus any code fixed in Step 2
git commit --no-gpg-sign -m "fixwave: nil-map range raises no obligation (fix 5, mechanism 5)"
```

---

### Task 8: Threat-model under-approximations section

**Files:**
- Modify: `docs/threat-model.md` (append section)

**Interfaces:**
- Consumes: the three approximations as implemented (Tasks 2, 5, 6).
- Produces: the enumerable blind-spot list the spec (§4) requires.

- [ ] **Step 1: Append the section**

Read `docs/threat-model.md` first and match its heading level and tone. Append:

```markdown
## Deliberate under-approximations (FP/encoding fix-wave, 2026-07-20)

The bug-finder invariant (findings only on Sat; false positives are
the enemy) buys precision with three enumerated blind spots. Each is a
conscious trade, not an accident — anything found missing here should
be added, not silently tolerated.

- **Load forwarding ignores calls** (`encode_load_forwarding`): two
  reads of the same address with no intervening store are modeled as
  equal even across function calls. A callee that mutates the re-read
  field between a caller's check and its use is missed at the re-read
  site. Stores and unmodeled (Havoc) effects still invalidate.
- **uintptr-derived pointers are non-nil** (`op_def` Convert arm): a
  pointer minted via uintptr arithmetic
  (`unsafe.Pointer(uintptr(base)+off)`) is assumed non-nil; deliberate
  wraparound to exactly address 0 is assumed away. Plain
  pointer→unsafe.Pointer→pointer puns keep their nilability.
- **Curated constructor trust** (`NEVER_NIL_RESULT`): externs in the
  table (currently `flag.NewFlagSet`) are trusted to return non-nil
  per their documented behavior; a stdlib behavior change contrary to
  its documentation would be missed. The phase-6 annotation language
  externalizes this table.
```

- [ ] **Step 2: Commit**

```bash
git add docs/threat-model.md
git commit --no-gpg-sign -m "fixwave: document deliberate under-approximations in threat model"
```

---

### Task 9: bbolt shakeout re-run + addendum

**Files:**
- Modify: `docs/shakeout-phase4-bbolt.md` (addendum section)
- Modify: `testdata/corpus/knownfp/knownfp.go` (package doc header refresh)

**Interfaces:**
- Consumes: `mise run shakeout` (scripts/shakeout.sh), `scripts/shakeout_bucket.awk` (triage design 2.2 bucketer), committed baseline `docs/shakeout-phase4-bbolt-findings.tsv` (columns: 1 = `file:line:col`, 2 = tag, 4 = message, 9 = `class_id`, 10 = verdict).
- Produces: the spec-§5 gate verdict, recorded as a report addendum.

- [ ] **Step 1: Full re-run**

Run: `mise run shakeout` and save the check output to the scratchpad. Record: goverify commit, bbolt ref (v1.4.0), timeouts (defaults: infer 100 ms / obligation 250 ms), findings count, cold/warm wall clock — mirroring the report's existing "Run parameters" section (phase-4 baseline: 1006 findings, cold 372 s / warm 30 s).

- [ ] **Step 2: Bucket and compare**

Bucket the new output: `awk -f scripts/shakeout_bucket.awk <output> > new-findings.tsv` (see `scripts/testdata/shakeout_bucket/` for the expected shapes). Then compute, against the baseline TSV:

1. **Vanished-class check:** for every baseline `class_id` with verdict `FP/encoding`, count new findings at the same (file:line:col, tag) signatures. Expected: ≥156 classes at zero; every FP/encoding class still producing findings must be attributable to mechanism 6 (other/misc, ≤29 classes — check each against the report's mechanism-6 description; any non-misc survivor is a fix bug, go back to its task).
2. **TP preservation:** all 33 baseline rows with verdict `TP` still present at their signatures. A missing TP is over-suppression — a gate failure; bisect which fix killed it before proceeding.
3. **No new classes:** every new finding's (file:line:col, tag) already exists in the baseline TSV. New signatures = a regression introduced by this wave; diagnose before proceeding.
4. **Requires-lifting/invariant classes** (out of scope) may shrink as a side effect of fix 2b — report the delta, don't gate on it.

- [ ] **Step 3: Write the addendum**

Append to `docs/shakeout-phase4-bbolt.md` a `## Fix-wave re-run (2026-07-20)` section: run parameters, a before/after totals table (per verdict bucket and per fixed mechanism), the gate results from Step 2 (each of the four checks with its outcome), and any mechanism-6 survivors enumerated by class id.

- [ ] **Step 4: Refresh the knownfp header**

Update `knownfp.go`'s package comment (lines 1-13): it currently says every want "is a KNOWN FP: phase 5 must make it disappear and flip the pin". Reword to reflect the post-fix-wave state: flipped pins are green regression cases; surviving wants are the still-pinned families (FP/invariant, requires-lifting, any re-attributed residuals) awaiting their own waves.

Run: `mise x -- cargo test -p goverify-checkers --test knownfp_corpus`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add docs/shakeout-phase4-bbolt.md testdata/corpus/knownfp/knownfp.go
git commit --no-gpg-sign -m "fixwave: bbolt re-run addendum — gate results vs phase-4 baseline"
```

---

### Task 10: Final gates

**Files:** none new — verification only.

- [ ] **Step 1: Run every blocking gate**

```bash
mise run fmt
mise run lint
mise run test
mise run corpus
mise run secrets
mise run audit
```

Expected: all PASS (`test` includes the corpus determinism suite; `corpus` re-runs the full extractor→.gvir pipeline + all corpus tests). Fix anything red before claiming completion; if `fmt` changed files, commit them.

- [ ] **Step 2: Commit any residue and summarize**

```bash
git status   # must be clean or contain only intended changes
git log --oneline main@{u}..main 2>/dev/null || git log --oneline -12
```

Report per-task commits and the Task 9 gate verdict as the wave's completion summary.
