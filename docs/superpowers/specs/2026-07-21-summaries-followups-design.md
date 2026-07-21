# Summaries follow-up wave — design

Date: 2026-07-21
Status: approved (brainstorm 2026-07-21)
Predecessor: `2026-07-20-interprocedural-summaries-design.md` (wave merged
to local main at 77b602f; gate addendum in `docs/shakeout-phase4-bbolt.md`)

## 1. Goal and scope

Clear the plan-owner follow-up queue left by the interprocedural-summaries
wave:

1. **C009c / DB.Begin partial miss** — `compact.go:26:23` (`tx.Commit()`
   after `tx, err := dst.Begin(true); if err != nil {...}`) still fires;
   the `err == nil ⇒ result != nil` ensures is not carried through
   `Begin`'s dispatch to `beginRWTx`/`beginTx`. Detection gap; highest
   value; goes first.
2. **C221 FP resurrection** — the wave re-internalized the `uint16(start)`
   overflow as a manifest FP at `internal/surgeon/surgeon.go:78:20` and
   dropped fix-3's narrower call-site requires at
   `cmd/bbolt/command_surgery.go:268:55`. Precision regression.
3. **Test pins** — recursive-ensures corpus case, interface-result
   non-case pin, `p<i>`-bearing ensures e2e pin.
4. **Hygiene** — spec §6 clause-order wording; de-flake the chatty-solver
   timing test; stale manifest-loop groundness comment in `nil.rs`.

Branch `summaries/followups` off local main (77b602f). Sandbox policy
unchanged: commits unsigned, re-sign before pushing.

**Structure: investigation-first.** Both analysis items get a dedicated
root-cause task with a minimal RED repro written before any fix, ending in
a decision point. The gate report's mechanisms ("multi-callee dispatch",
"canonicalization") are hypotheses, not diagnoses — prior waves' guessed
root causes have been refuted by investigation before (fix-wave task 7:
C186 not C178, widening not range-headers). Verify, don't trust.

### Non-goals (carried unchanged)

- Closure/cobra requires-lifting (C027, C216, C257, C258, C402).
- Interprocedural bounds propagation *as a general capability* — C221 may
  get a targeted type-width bound if the investigation shows it is cheap
  (that is the decision point), but no general machinery.
- The misc-FP bucket (29 classes / 41 findings); remaining
  requires-lifting survivors (86); phase-5 roadmap (caching, SARIF,
  diff-base); the upstream decision on the 33 bbolt TPs.

## 2. Task order

1. Investigate C009c (RED repro + root-cause report + decision)
2. Fix C009c (repro flips GREEN)
3. Investigate C221 (bisect + RED repro + decision)
4. Fix C221 (per decision)
5. Test pins (post-fix behavior)
6. Hygiene
7. Shakeout + gate addendum

C009c precedes C221 because it is the detection gap and its investigation
exercises the same summary machinery C221's fix may touch. Pins follow the
fixes because the recursive-ensures and `p<i>` pins must reflect post-fix
behavior; each fix task still writes its own RED repro first (TDD).

## 3. C009c — investigation and fix

### The discharge chain

For `compact.go:26:23` to go quiet, four links must hold:

1. `beginTx`/`beginRWTx` each get an inferred
   `err == nil ⇒ result non-nil` ensures.
2. `DB.Begin` — a pure dispatch wrapper whose return sites forward its
   callees' tuples — gets the same ensures. **Suspect link**, with two
   distinct failure modes visible in `infer_ensures`
   (`crates/goverify-checkers/src/nil.rs:173`):
   - If the forwarded tuple is not exploded into per-component `Extract`s
     in the IR, the arity check (`vals.len() != results.len()`) drops
     **all** candidates for `Begin`.
   - If it *is* exploded, the error component is a non-literal value, so
     the Go-idiom rule makes the correlation vacuously provable — meaning
     the break is downstream, not here.
3. The caller-side obligation at `compact.go:26` fires only if `Begin`'s
   Inferred summary carries a `nil-deref` ensures naming `r<idx>`
   (`nil.rs:307-322`).
4. The caller's encoding must assert `Begin`'s ensures at the call site so
   the `err != nil` guard discharges the query.

### Investigation protocol

- Write a minimal corpus function pair mirroring the shape: wrapper
  dispatching to two callees, forwarded tuple returns, guarded caller.
  Confirm it reproduces RED (finding fires).
- Walk links 1→4 on that repro with unit-level probes driven through the
  engine with the real solver — the debug CLI's StubSolver cannot observe
  checker state (task-9 constraint), so full check runs / engine-level
  tests are the instrument.
- The report names the broken link and the mechanism, with evidence.

### Decision point (resolved by investigation, carried in the plan)

- **Link 2, arity**: infer wrapper ensures by instantiating callee
  ensures through forwarded-tuple returns — meet over return sites (for
  `Begin`, the intersection of both callees' clauses).
- **Link 2, correlation logic**: targeted repair inside `infer_ensures`.
- **Link 3/4**: encode-side fix in the obligation filter or the call-site
  ensures assertion.

Every candidate fix touches finding-suppressing paths. The bar: RED→GREEN
repro, plus gate G3's no-unexplained-departures check on the shakeout diff
(a fix must not silently discharge findings beyond its target).

### Success criteria

Repro flips GREEN; `compact.go:26:23` absent from the shakeout; no
unexplained departures vs the 461 baseline.

## 4. C221 — investigation and fix

### What regressed

Fix-3 (fp-encoding wave) converted C221's manifest overflow inside
`ClearPageElements` (`surgeon.go:78:20`, `p.SetCount(uint16(start))`)
into a call-site requires firing only at the genuinely-unbounded CLI path
(`command_surgery.go:268:55`). The summaries wave undid that: the
call-site requires vanished and the manifest FP reappeared in the callee.
The finding is an FP either way — `start` is guarded to
`[0, elementCnt)` and `elementCnt = int(p.Count()) ≤ 65535` since
`Count()` returns `uint16` — so the truncation is provably impossible.

### Hypotheses (to verify, not trust)

- **Prime**: task 8's `Assign`/`ChangeType` deref-subject
  canonicalization (`shared::canonical_value`) changed subject
  classification — if the overflow checker's manifest-vs-liftable split
  now sees `start` as params-only/expressible where it previously wasn't,
  the finding re-internalizes exactly this way.
- **Second**: F2's `encode_props` extension to the summary-bearing domain
  altered what is provable at the call site.

### Investigation protocol

- Bisect the 16 wave commits (31a50cf..77b602f) with a targeted shakeout
  grep for the two signatures. Each probe is a cold run (~163 s — checker
  changes invalidate the SMT cache); ~4 probes ≈ 11 minutes. Acceptable.
- Confirm the mechanism by reading the responsible commit against the
  overflow checker's lifting logic.
- Write a minimal RED corpus repro: guarded-truncation callee with one
  unbounded and one bounded call site, pinning "fires only at the
  unbounded site."

### Decision point: discharge outright vs restore requires form

With the mechanism in hand, assess **outright discharge** first: if
type-width bounds on call results of intrinsic-width integer returns
(`Count() → uint16` ⇒ result ≤ 65535) are already encoded or cheap to
encode, the FP disappears at both sites — best precision. If that
requires anything resembling general interprocedural bounds propagation
(an explicit non-goal), **fall back to restoring the requires form**
(fix-3 parity: fire only at the unbounded call site). The plan carries
both branches; the investigation report picks one with evidence.

### Success criteria

Either `surgeon.go:78:20` gone with no replacement (discharged), or
replaced by the `command_surgery.go:268:55` requires (fix-3 parity). The
corpus repro pins whichever outcome. No other signature deltas
attributable to this fix.

## 5. Test pins

- **Recursive-ensures corpus case**: the final review verified recursion
  self-consultation sound (simultaneous fixpoint), but no corpus function
  exercises it. Add a self-recursive callee with a provable
  `err == nil ⇒ result non-nil` ensures and a guarded caller, pinning
  that the clause is emitted and discharges the caller's deref. A
  widened-SCC sibling (recursion that trips `widen_after`) pins that the
  havoc path stays silent.
- **Interface-result non-case pin**: interface values are Ptr-sorted
  (`sort_of`) since the summaries wave; pin the boundary — an
  interface-typed result that inference must *not* claim non-nil (a
  typed-nil-prone return shape) stays clause-free. Guards the Go-idiom
  under-approximation from silently widening.
- **`p<i>`-bearing ensures e2e pin**: `instantiate_ensures` binds `p<i>`
  param vars, but no checker emits them — dead capability with zero
  coverage, including the method-receiver offset flagged unverified in
  the task-4 review. Pin with a hand-constructed `p<i>`-bearing summary
  at the engine/encode level, verifying call-site argument and receiver
  binding.

## 6. Hygiene

- **Spec §6 clause-order wording**: align the predecessor spec's clause
  enumeration wording (template×index vs index×template) with the
  implemented order. Doc-only.
- **De-flake chatty-solver timing test** (goverify-solver,
  load-sensitive): replace the wall-clock-sensitive assertion with a
  generous bound or a logical-event assertion. No behavior change.
- **Stale manifest-loop groundness comment** in `nil.rs`: rewrite the
  comment that predates canonicalization. Comment-only.

## 7. Acceptance gates

Blocking gate (`mise run lint` + `test` incl. corpus determinism +
`secrets` + `audit`) plus a full bbolt shakeout with an addendum in
`docs/shakeout-phase4-bbolt.md`, same format as prior waves:

- **G1**: `compact.go:26:23` discharged (C009c).
- **G2**: C221 resolved per the investigation's decision — signature gone,
  or relocated to the call-site requires.
- **G3**: full diff vs the 461 baseline — every departure and arrival
  attributed to a specific fix; zero unexplained deltas. This is the
  finding-suppression tripwire for both fixes.
- **G4**: determinism across 3 shakeout runs.
- **G5** (report-only): corpus/test/shakeout timing.

## 8. Invariants (unchanged)

Determinism is the root invariant (byte-identical `.gvir`, sorted
emission). Errors degrade, never die; parsers of untrusted bytes reject,
never panic (fuzz targets stay green). Go code only in `extractor/`.
Dependencies stay as they are — this wave adds no crates.
