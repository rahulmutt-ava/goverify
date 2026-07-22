# Follow-ups wave 2: retry tier, cycle guard, pins, and open verdicts

Date: 2026-07-22
Status: approved (brainstorm 2026-07-22)
Predecessors: `2026-07-20-interprocedural-summaries-design.md`,
`2026-07-21-summaries-followups-design.md`

## 1. Goal

Clear the plan-owner follow-up queue left by the summaries follow-up
wave (ledger, `.superpowers/sdd/progress.md`, WAVE COMPLETE entry of
2026-07-22). Eight items, one branch, investigation-gated where
applicable, shakeout-gated overall. Baseline: local `main` @ `9727cc0`,
bbolt shakeout baseline 457 findings (452 unique).

The queue, in the ledger's words:

1. MayNilVia laundering-boundary pin (Go-idiom under-approximation,
   transitive through wrappers)
2. unbounded-elemOffset positive-polarity overflow pin (guards 4A
   over-suppression)
3. `ensures_corpus` missing from mise `corpus` task
4. manifest-position C221 repro (needs dual-checker harness)
5. shared cycle-guarded Named resolver (pre-existing, prior-wave item)
6. tx.go:558:11 pre-4A re-probe (close causal question)
7. near-timeout query hazard (retry-at-higher-timeout tier)
8. test-binary link-time anomaly (20m56s, inferred linker-bound)

Two items change behavior (5, 7); the rest are pins, test infra,
hygiene, and report-only investigations.

## 2. Retry-at-higher-timeout tier (item 7)

### Problem

Queries landing near the 100 ms Infer timeout
(`SolverLimits::default`, `goverify-solver/src/lib.rs:67`) occasionally
return Unknown, and Unknown ⇒ silence: inferred requires are soundly
dropped, findings flicker between runs (the C221/task-3 mechanism;
tx.go:558:11 is the suspected sibling). Sound but flaky — the
best-supported inference for two shakeout deltas across two waves.

### Design

A retrying wrapper around the solver backend, constructed inside
`mk_backend`, invisible to the engine and checkers (approach chosen
over engine-level retry, which would thread a second backend through
every checker closure, and over raising the base timeout, which makes
every hard query pay).

- **Policy**: run the query at the base tier (100 ms). If the result
  is Unknown, re-issue the identical query once at the escalated tier
  (1000 ms) and use that result. Sat/Unsat at any tier is final.
  Unknown at the top tier stays Unknown — bug-finder semantics
  unchanged (Unknown ⇒ no report; timeouts never create false
  positives).
- **Cache composition**: retry sits above the cache lookup for each
  tier. `timeout_ms` is already part of the query key
  (`goverify-cache/src/query.rs`), so each tier caches independently:
  a pre-existing cached Unknown@100ms still triggers escalation, which
  then consults/fills the @1000ms entry. No key-schema change, no
  invalidation. This also repairs the C221-era trap where a cached
  first-run Unknown replayed forever as fake "determinism."
- **Scope**: applied uniformly to both `BackendRole::Infer` and
  `::Findings` — one code path, one semantics. Pre-agreed fallback if
  the shakeout gate shows unacceptable wall-clock cost: restrict to
  `Infer` at the construction site (a condition, not a redesign).
- **Honesty clause**: wall-clock timeouts are inherently machine- and
  load-sensitive. The tier narrows the flake window (a query must now
  straddle 1000 ms, not 100 ms, to flake) but cannot eliminate it.
  This spec claims mitigation, not determinism.

### Tests

Unit, against a scripted `TextSolver` fake answering Unknown at 100 ms
and Unsat at 1000 ms: (a) escalation fires; (b) the escalated result
wins; (c) both tiers hit the cache correctly (Unknown@100ms cached,
Unsat@1000ms cached, second run resolves from cache without a solver
call); (d) Unknown at both tiers stays Unknown. Plus a pin that
Sat/Unsat at the base tier never escalates.

### Success criteria

Unit pins green. Shakeout arrivals vs the 457 baseline attributed to
recovered near-timeout queries (G1/G3); expected candidate:
tx.go:558:11 (see §5).

## 3. Shared cycle-guarded Named resolver (item 5)

### Problem

`int_repr`, `array_len`, `array_len_direct`, and `sort_of`'s Named
handling (`goverify-analysis/src/encode.rs:46`, `:84`, `:97`, `:108`)
follow `Named → underlying` chains by unguarded self-recursion. A
crafted `.gvir` with a Named cycle (`type A = Named{underlying: A}`)
aborts by stack overflow — violating "parsers of bytes the analyzer
didn't write must reject, never panic" and "errors degrade, never
die." Pre-existing (flagged in the prior two waves' final reviews),
fuzz-reachable.

### Design

One shared helper in `encode.rs`:

```rust
fn resolve_named(types: &TypeTable, t: TypeId) -> Option<TypeId>
```

walks the Named chain with a cycle guard — a step cap at the
type-table size (simpler than a visited-set, allocation-free, and any
chain longer than the table necessarily repeats) — and returns the first
non-Named `TypeId`, or `None` on a cycle. The four resolvers call it
once and match on the resolved kind, eliminating their self-recursion
through Named entirely. On cycle: `None`, which every caller already
treats as unresolvable → havoc/skip — a hostile `.gvir` degrades like
any other unencodable type.

### Tests

Unit tests with a crafted self-referential Named and a 2-cycle,
asserting `None` from all four resolvers; a Named-cycle seed added to
the fuzz corpus. The RED demonstration is by construction (the old
code's recursion has no exit on these inputs), not by running the
crash — a stack-overflow RED would abort the test harness.

### Success criteria

Crafted-cycle tests green; fuzz smoke green; zero corpus/golden drift
(well-formed input behavior unchanged).

## 4. Pins and hygiene (items 1, 2, 3)

- **MayNilVia laundering-boundary pin** (item 1): corpus tripwire in
  `ensures_corpus` — a wrapper (`MayNilVia`) forwarding a
  non-idiomatic callee (`MayNil`, which can return `(nil, nil)`). The
  Go-idiom rule accepts the wrapper's extract-shaped return without
  consulting the callee's summary, minting an
  `err == nil ⇒ result non-nil` ensures the callee doesn't support — a
  declared under-approximation and false-discharge (missed-bug)
  source, documented in `threat-model.md`. The pin asserts today's
  laundering behavior under a KNOWN-boundary header so any future
  Go-idiom change trips it visibly instead of silently moving the
  soundness boundary.
- **Unbounded-elemOffset positive-polarity pin** (item 2): a corpus
  case where a widening convert's source is genuinely unbounded (no
  guard; can exceed the destination range) and the truncation finding
  **must still fire**. Fix 4A asserts only the source *type's* range;
  this pin proves it didn't discharge real overflows.
- **mise corpus task** (item 3): add `--test ensures_corpus` to the
  `goverify-checkers` line of the `corpus` task (`mise.toml:47`).
  Closes the gap where `corpus` ran a strict subset of `mise run test`.
  Lands first so all later pins run under the blocking gate.

## 5. Dual-checker harness + C221 manifest repro, investigations (items 4, 6, 8)

### Dual-checker harness + manifest-position C221 repro (item 4)

The task-3 repro fired at the `bounds.go:95` caller position because
the corpus harness registers exactly one checker per suite; real C221
manifested at the manifest site (`surgeon.go:78:20`) only under the
nil-ensures-into-bounds-encoding interaction, which needs both
checkers live in one run. Work: extend the corpus harness to accept
multiple registered checkers (test-infra only, no production code),
then add a corpus case with the real C221 shape (`uint16 → int` count,
`start < elementCnt` guard, `uint16(start)` convert) running NilDeref
+ Bounds together, pinning that the manifest-site finding stays
**discharged** post-4A — a GREEN pin guarding 4A's discharge under the
interaction that originally produced the FP.

### tx.go:558:11 pre-4A re-probe (item 6, report-only)

G3 of the prior wave left the departure's causal tie to 4A unproven
(labeled timing-flaky family). Two probes close it:

- (a) at the pre-4A base commit, run the shakeout repeatedly with a
  cold query cache — flicker ⇒ timing-flaky before 4A landed;
- (b) at HEAD with the §2 retry tier, check whether it reappears — a
  post-retry arrival confirms a near-timeout Unknown drop, not a 4A
  discharge.

Sequencing: the retry-tier task lands before this probe. Outcome is a
verdict in the shakeout addendum; no code change.

### Test-binary link-time anomaly (item 8, report-only)

The 20m56s `mise run test` outlier was inferred linker-bound, never
measured. Measure (cargo `--timings`, per-crate link phases), confirm
or refute, record verdict + recommendation in the ledger/addendum. A
fix lands only if trivial and clearly justified by the measurement;
anything heavier returns to the queue as its own item.

## 6. Task ordering

Dependency-driven, one branch off `main`:

1. Hygiene: `ensures_corpus` into the mise corpus task (item 3)
2. Pins: MayNilVia + unbounded-elemOffset (items 1, 2)
3. Cycle-guarded resolver (item 5)
4. Dual-checker harness + C221 manifest repro (item 4)
5. Retry tier (item 7)
6. tx.go:558:11 re-probe (item 6 — needs 5 landed)
7. Link-time measurement (item 8)
8. Shakeout gate + addendum

Same ledger discipline as prior waves (`.superpowers/sdd/progress.md`).

## 7. Acceptance gates

Blocking gate (`mise run lint` + `test` incl. corpus determinism +
`secrets` + `audit`) plus a full bbolt shakeout with an addendum in
`docs/shakeout-phase4-bbolt.md`, same format as prior waves:

- **G1**: retry tier verified — unit pins green; every shakeout
  *arrival* vs the 457 baseline attributed to a specific recovered
  near-timeout query. Zero unexplained arrivals.
- **G2**: cycle guard verified — crafted-cycle unit tests green, fuzz
  seed added, fuzz smoke green, zero corpus/golden drift from this
  task.
- **G3**: full diff vs the 457 baseline — every departure and arrival
  attributed to a specific change. Expected deltas: retry-tier
  attributed only — mostly arrivals (recovered requires), though a
  recovered query proving an obligation Unsat can also cause a
  departure; the C221 manifest pin and both polarity pins green.
  Tripwire for both behavior changes.
- **G4**: determinism across 3 shakeout runs — with the §2 honesty
  caveat: wall-clock timeouts bound this statistically, not
  absolutely; a tier-2 flake, if one appears, is documented as the
  known residual hazard, not explained away.
- **G5** (report-only): corpus/test/shakeout timing, including the
  measured link-time verdict (item 8) and the retry tier's wall-clock
  cost (count of escalated queries).

Verdicts the addendum must close: the tx.go:558:11 causal question
(item 6) and the link-time inference (item 8).

## 8. Invariants (unchanged)

Determinism is the root invariant (byte-identical `.gvir`, sorted
emission). Errors degrade, never die; parsers of untrusted bytes
reject, never panic (fuzz targets stay green). Go code only in
`extractor/`. No new crates.
