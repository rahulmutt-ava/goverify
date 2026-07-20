# FP/encoding fix-wave design

2026-07-20. Follow-up to the phase-4 bbolt shakeout triage
(`docs/shakeout-phase4-bbolt.md`, design
`2026-07-19-shakeout-triage-design.md`). Chosen over roadmap phase 5
(caching) because the shakeout measured a 96.2% FP rate: precision is
the binding constraint on the tool's usefulness, not speed.

## 1. Goal and scope

Eliminate five of the six FP/encoding mechanisms the triage distilled —
the analyzer's own modeling bugs, not facts about bbolt:

| # | Mechanism | Classes | Findings |
|---|-----------|---------|----------|
| 1 | Same-function dominating check not carried forward | 62 | 121 |
| 2 | Address-of stack-local / composite-literal / slice-element / value field | 48 | 111 |
| 3 | Unsafe-pointer / pointer-arithmetic derived value | 35 | 111 |
| 4 | Stdlib constructor documented never-nil | 8 | 42 |
| 5 | Nil-map range is legal | 3 | 8 |
|   | **Total in scope** | **156** | **393** |

**Out of scope** (explicit non-goals, each its own future item):

- Mechanism 6 "other/misc" (29 classes, 41 findings) — heterogeneous
  one-off gaps, each needing individual diagnosis.
- The requires-lifting pass (78 classes, 124 findings) — general
  interprocedural postcondition inference/substitution. Fixes 2b and 4
  below are deliberate stepping stones toward it, not a slice of it.
- Roadmap phase 5 proper: caching stack, SARIF, diff-base, speed
  milestone.
- Upstream-reporting the 33 bbolt TPs.

## 2. Approach

All fixes land in the **shared encoding layer** — where the IR lowers
to SMT terms (`goverify-analysis::encode`, plus the checker discharge
path and one extractor-side deref-site classification). Rationale: the
dominant FP surface form on bbolt was *"call to X violates its
nil-deref requirement"* — a spurious precondition inferred for the
callee and propagated to every caller through the SCC fixpoint. Only a
fix at the encoding layer stops the bogus requires clauses at the
source; suppressing at finding time (pre-pass patterns) or post-hoc
filtering would leave the spurious requires propagating.

Alternatives rejected: pre-pass suppression (cannot stop requires
propagation; suppressed facts don't compose with other constraints);
full heap-SSA/points-to memory model (principled, subsumes fixes 1–3,
but phase-sized and overlaps the future lifting work).

## 3. The five fixes

### Fix 1 — never-nil addresses (mechanism 2)

Today `Op::Alloc`, `Op::FieldAddr`, and `Op::IndexAddr` fall through
`op_def`'s `_ => None` arm and havoc their destination, so `&local`,
`&buf[i]`, `&s.field`, and composite-literal backing allocations are
modeled as freely nilable pointers. New `op_def` cases:

- `Alloc { dst }`: assert `¬is_nil(dst)` unconditionally. A Go
  allocation (stack or heap) never yields a nil address.
- `FieldAddr { dst, .. }` / `IndexAddr { dst, .. }`: assert
  `¬is_nil(dst)`. The op itself faults on a nil/invalid base *before*
  producing a value, so any execution that continues holds a non-nil
  result. The base's own nilability remains a separate obligation at
  that site — this fix never weakens it.

### Fix 2 — same-function dominating facts (mechanism 1)

Two cooperating pieces; both are needed to cover the mechanism's
variants (a prior explicit nil-check needs 2a; a prior dereference or
promoted-method call needs 2b, with 2a identifying "same value").

**2a — load forwarding.** Repeated `Op::Load`s of the same address get
their destinations equated, so a fact proven about the first read of a
field applies to a later re-read instead of the re-read being a fresh
unconstrained value. "Same address" is decided by value-numbering the
pure address ops: two `FieldAddr`s with the same (value-numbered) base
and field index compute the same address, likewise `IndexAddr` with
equal base and index. Forwarding invariant: **never forward across an
instruction that could write the address** — an intervening `Store`
whose target may alias the loaded address (conservatively: same field
index, or a store through an address the value-numbering cannot
distinguish) or a `Havoc` kills the equation. An intervening **call
does not invalidate forwarding** — this is a deliberate
under-approximation (see §4): bbolt's canonical C015a shape has calls
between the check and the re-read, and the field is never actually
reassigned.

**2b — checked-deref assumptions.** When discharging an obligation at
deref site *s*, conjoin `guard(b') ⇒ ¬is_nil(subj')` for every
nil-deref site that **strictly dominates** *s* in the cut DAG: a model
that reaches *s* already traversed those dereferences without
panicking, so their subjects were non-nil on that execution. The
obligation's own site is never assumed, so a genuine first-failure
site still fires. Requires a dominator computation over the cut DAG (a
small new helper in `goverify-analysis`; the cut DAG is acyclic, so
this is a standard forward-dominance pass). Applies to `NilChecker`
discharge queries (both `infer_requires` and `obligations`); the
bounds checker is untouched by this wave.

### Fix 3 — unsafe-pointer arithmetic (mechanism 3)

Any pointer produced by a `uintptr → pointer` conversion is asserted
non-nil at its defining `Convert`. Blanket rule, not base-tracking
dataflow: in real Go these conversions appear exclusively in
`unsafe.Pointer(uintptr(base) + offset)` idioms off a live base, and
producing nil would require exact 64-bit wraparound. Documented
under-approximation (§4). This covers `UnsafeIndex`,
`LeafPageElement`/`BranchPageElement`, and the `db.page`/`tx.page`
mmap-arithmetic family.

### Fix 4 — never-nil extern constructor table (mechanism 4)

A small curated static table in `goverify-analysis` mapping
fully-qualified function names to "result 0 is non-nil", consulted by
`op_def` for `Op::Call` with a matching `Callee::Static` (and applied
whether or not the callee's body was analyzed). Seed contents: every
constructor named by the 8 triaged mechanism-4 classes (e.g.
`flag.NewFlagSet` from C003; the full list is enumerated during
planning from the committed
`docs/shakeout-phase4-bbolt-findings.tsv` rows of those classes),
each verified against its stdlib documentation before inclusion. This is a lookup table, not
postcondition inference — kept as data so the phase-6 annotation
language can externalize it later.

### Fix 5 — nil-map range (mechanism 5)

Ranging over a nil map is legal Go: zero iterations, no dereference
occurs. The range subject is today classified as a deref site
(extractor-side aux classification); stop classifying it as one. No
`.gvir` schema change expected — this is a classification fix, not a
shape change. If inspection during planning shows the site is instead
synthesized on the Rust side, the fix moves there; the invariant is
the same either way: **a map range header raises no nil-deref
obligation**.

## 4. Soundness stance

Consistent with the design's bug-finder invariant (findings only on
`Sat`; false positives are the enemy), three of the fixes are genuine
documented under-approximations, and two are simply correct Go
semantics:

| Fix | Nature | Blind spot accepted |
|-----|--------|---------------------|
| 1 | Correct semantics | none |
| 2a | Under-approximation | a callee that really does mutate the re-read field between check and re-read |
| 2b | Correct semantics (per-model no-panic reasoning) | none — later-site findings masked by a dominating first failure reappear once the first is fixed |
| 3 | Under-approximation | deliberate uintptr wraparound producing nil |
| 4 | Trusts stdlib docs | a stdlib constructor changing behavior contrary to its documentation |
| 5 | Correct semantics | none |

The three accepted blind spots get a short "deliberate
under-approximations" section added to `docs/threat-model.md` so the
analyzer's blind spots stay enumerable rather than folklore.

## 5. Verification

The agreed bar: **bbolt re-run + corpus.**

1. **Corpus (red/green per mechanism).** Each mechanism's
   `testdata/corpus/knownfp` pin flips from `// want: nil-deref` to
   expected-clean and moves into the regular corpus. Each fix also
   lands a *red* case proving no over-suppression:
   - fix 1: a deref of a genuinely nilable pointer (e.g. an unchecked
     param) still fires even when address-of values are nearby;
   - fix 2a: a `Store` between the two loads still invalidates
     forwarding (re-read stays independently checkable);
   - fix 2b: a first deref with no dominator still fires; a
     *non*-dominating earlier deref (parallel branch) grants no
     assumption;
   - fix 3: a nil-derived ordinary (non-uintptr) pointer still fires;
   - fix 4: an extern constructor *not* in the table stays nilable;
   - fix 5: an actual deref of a nil map's element pointer (distinct
     from the range header) still fires.

   Mechanism 1 had no minimal repro under the old snapshot (the
   knownfp file records why); with load forwarding in place the shape
   becomes expressible, so its green case lands fresh in this wave.

2. **bbolt shakeout re-run** with the recorded phase-4 run parameters.
   Gate: all 156 in-scope classes produce zero findings; the 33 TP
   findings still present; no new finding classes introduced. Results
   recorded as an addendum to `docs/shakeout-phase4-bbolt.md` with
   before/after totals.

3. **Standing suites** all green: corpus determinism suite, solver
   differential harness, `mise run lint` / `test` / `secrets` /
   `audit`.

## 6. Component touch points

| Component | Change |
|-----------|--------|
| `goverify-analysis::encode` (`op_def`) | fixes 1, 3, 4 (new cases); fix 2a (value-numbering + load-forwarding equalities) |
| `goverify-analysis` (new helper) | cut-DAG dominators for fix 2b |
| `goverify-checkers::nil` discharge path | fix 2b assumption conjuncts |
| `extractor/` (Go) or `goverify-ir::lower` | fix 5 deref-site classification |
| `docs/threat-model.md` | §4 under-approximations section |
| `testdata/corpus` | pin flips + red/green cases |
| `docs/shakeout-phase4-bbolt.md` | re-run addendum |

Checker/crate boundaries are unchanged: no new crate dependencies, no
`.gvir` schema bump expected, solver layer untouched.
