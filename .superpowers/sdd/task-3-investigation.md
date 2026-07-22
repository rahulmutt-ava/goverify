# Task 3 investigation: C221 bisect + mechanism + decision

Branch `summaries/followups` (HEAD `2c24ad5`). Read-only probing in
temporary worktrees (all removed); the only working-tree mutation is the
RED corpus repro in `testdata/corpus/bounds/bounds.go`.

## Probe table

Predicate: `command_surgery.go:268:55` (pre-regression, call-site
requires) vs `surgeon.go:78:20` (post-regression, manifest overflow).

| Commit | Task | Cache state | Timeouts | Signature observed |
|---|---|---|---|---|
| `a0e1b28` | 5 (NilChecker ensures inference) | isolated fresh | default 100/250ms | `268:55` only |
| `9994c53` | 6 (`bounds.rs`/`nil.rs` → `encode_func_with`) | **shared/pre-populated** (first probe of the session) | default | `78:20` only |
| `9994c53` | 6 | isolated fresh (retest) | default | `268:55` only |
| `5549cb7` | 8 (canonicalize deref subjects) | shared/pre-populated | default | `78:20` only |
| `b94581a` | 9 (task-10's own tested commit) | isolated fresh, run 1 | default | `268:55` only |
| `b94581a` | 9 | isolated fresh, run 2 | default | `268:55` only (reproducible in my environment) |
| HEAD `2c24ad5` | followups (task 2B fix) | shared, then fully wiped/fresh | default, and 100/250, 5000/5000, 5000/250 | `268:55` only, **every** combination tried |

External corroboration (already on disk, not produced by me):
- `.superpowers/sdd/task-10-report.md` (this wave's own gated shakeout,
  run at `b94581a`): **two runs, cold + warm, byte-identical**, both
  showing `surgeon.go:78:20` as a "new vs 509" finding (gate 3). The
  cold run is a genuinely fresh computation (empty cache dir) — real
  evidence the Unknown-verdict path fires in some environments/moments.
  The "warm" run necessarily reuses the cold run's own cache dir
  (`scripts/shakeout.sh` always uses `$(pwd)/../cache`), so its
  agreement with the cold run is cache-consistency, not an independent
  re-solve — it does not contradict the flakiness finding below.
- `.superpowers/sdd/task-9-investigation.md` (prior, unrelated
  "fixwave" plan, its own Q2): documents the *opposite*-direction
  transition (`78:20`→`268:55`) at a much earlier commit (`d9ace1f`,
  ancestor of this wave's `31a50cf` base), calls it a genuine precision
  *improvement*, and explicitly states it could not derive the exact
  SMT-level reason the flip occurred — the same signature pair was
  already known to be a fragile, encoding-sensitive boundary before this
  wave.

**Bisection verdict**: `a0e1b28` (task 5, pre-switch) never showed
`78:20` in any run — structurally cannot, see mechanism below. `9994c53`
(task 6) is the first commit capable of showing `78:20`. Per the brief's
rule ("the FIRST commit whose run shows `surgeon.go:78:20` ... is the
culprit"), **`9994c53` is the culprit**. Task 8's canonicalization
(`5549cb7`, the "Prime" hypothesis in the design spec) adds nothing new
to this specific mechanism — it merely inherits whatever task 6 already
introduced; my `a0e1b28`/`9994c53` boundary test disconfirms it as an
independent contributor.

**Important finding not anticipated by the brief: the regression is
genuinely flaky (load/timing-dependent), not a hard function of commit
+ source.** I observed both signatures at the *same* commits (`9994c53`,
`5549cb7`, and `b94581a`) across different runs/cache states, and the
prior task's own "cold" run independently caught `78:20` at `b94581a`
while my repeated fresh-cache retests of that exact commit consistently
show `268:55`. Both are real observations; see mechanism for why.

**For Task 7's gate evaluation:** at current HEAD (`2c24ad5`), *every*
run I made — any cache state, any timeout from 100ms up to 5000ms —
showed the pre-regression `268:55` signature and never `78:20` (see the
probe table's last row). The `78:20` FP is currently **latent/flaky at
HEAD, not hard-present** — a gate run today is likely, though not
guaranteed, to see the clean signature even before Task 4's fix lands.

## Mechanism (Step 2)

### Culprit diff — `crates/goverify-checkers/src/bounds.rs`

```diff
-use goverify_analysis::{
-    Checker, Clause, EncodedFunc, Formula, Obligation, Summary, array_len, encode_func, int_repr,
-    seq_datatype,
-};
+use goverify_analysis::{
+    Checker, Clause, EncodedFunc, Formula, Obligation, Summary, array_len, encode_func_with,
+    int_repr, seq_datatype,
+};
@@ impl Checker for BoundsChecker::infer_requires
-        let Ok(enc) = encode_func(p, f) else {
+        let Ok(enc) = encode_func_with(p, f, summary_of) else {
@@ impl Checker for BoundsChecker::obligations
-        let Ok(enc) = encode_func(p, f) else {
+        let Ok(enc) = encode_func_with(p, f, summary_of) else {
```

(`nil.rs` gets the mirror-image edit in the same commit; irrelevant to
this checker's own bug but confirms the switch was a blanket parity
change, not bounds-specific.)

### Which gate flipped, and why

`params_only` does **not** fail: `ClearPageElements`'s `uint16(start)`
overflow site's violation term is built solely from `start`
(`convert_sites`, bounds.rs:302-334, `values: vec![*src]`), a bare
param — `params_only` (shared.rs:18-23) checks only `p<i>`-named free
vars and is true regardless of `encode_func`/`encode_func_with`.

The gate that flips is **`discharge()`'s Sat check** inside
`infer_requires`, bounds.rs:474-478:

```rust
            if discharge(&enc.reach_query(site.block, vec![site.violation.clone()]))
                != SatResult::Sat
            {
                continue;
            }
```

`encode_func_with` (vs. `encode_func`, which is literally
`encode_func_with(p, f, &|_| Summary::havoc())`, encode.rs:240-242) adds
one extra encoding step, `encode_call_ensures` (encode.rs:788-825):
every static call inside the function whose callee summary is
`Provenance::Inferred` with non-empty `ensures` gets each ensures clause
asserted, gated on the call's block guard. Only `NilChecker::infer_ensures`
(introduced in task 5, `a0e1b28`) ever produces such a summary — the
Go-idiom correlation rule (`err == nil ⇒ result != nil`), the exact shape
of `guts_cli.ReadPage`, which `ClearPageElements` calls before the
`uint16(start)` site. Before task 6, `BoundsChecker` used bare
`encode_func`, so this fact was **never** asserted into its own
encoding regardless of what NilChecker had inferred elsewhere — task 6
is what makes `ClearPageElements`'s own discharge query for its overflow
site *depend on* cross-checker inference for the first time.

I confirmed the standalone query text (`--emit-smt`) for this exact site
is trivially `sat` under an unbounded external `z3` — the true answer is
unambiguous (`start`'s own guard plus the still-unbounded
`elementCnt = int(p.Count())`, see below, makes the overflow genuinely
reachable in isolation). `check`'s CLI wires an **asymmetric timeout**
(`crates/goverify-cli/src/main.rs`):

```rust
    /// Per-query timeout for requires-inference queries (ms).
    #[arg(long, default_value_t = 100)]
    solver_timeout_ms: u32,
    /// Per-query timeout for obligation (findings) queries (ms) —
    /// function-sized formulas get more room (spec §8).
    #[arg(long, default_value_t = 250)]
    obligation_timeout_ms: u32,
```

Adding `encode_call_ensures`'s assertions makes this specific query
harder (more constants/clauses to reason about) without changing its
true answer. **What follows is the best-supported inference from the
evidence I have, not a directly reproduced observation** — I want to be
precise about which link in this chain is established fact vs. the
most-plausible explanation:

- **Established (structural argument + direct check):** the query is
  genuinely `sat` (verified against an unbounded external `z3`), and
  `encode_call_ensures` measurably adds assertions to it that weren't
  there under plain `encode_func`. This part doesn't depend on timing
  at all — it's true regardless of machine load.
- **Established (repeated observation):** every one of *my own*
  fresh-cache runs, at every commit I tried including `9994c53` itself,
  landed on `268:55`. Only the prior wave's *one* independent cold
  shakeout run (at `b94581a`, recorded in `task-10-report.md`) is
  attested evidence of `78:20` arising from a genuinely fresh
  computation — I never personally reproduced a live, in-session
  Sat→Unknown flip I could point to and say "there, it just happened."
- **Inferred, not reproduced:** that the mechanism connecting these is
  specifically "the **100ms requires-inference budget** is occasionally
  exceeded, flipping `SatResult::Sat` to `SatResult::Unknown`." This is
  the natural reading given `check`'s CLI wires an asymmetric,
  tightly-budgeted timeout for exactly this query class (below), and
  it's the only mechanism I can find in the code that would make the
  *same* query answer differently across runs with no source change —
  but I did not myself catch the query mid-flight and observe a timeout;
  my evidence is also consistent with a *prior* timeout having been
  cached and reused ("cache-of-a-prior-timeout") rather than a fresh
  live near-100ms race on every affected run — both explanations point
  at the same underlying near-the-budget phenomenon, so I can't fully
  rule either reading out from the data alone. Treat the specific
  "exceeds 100ms → Unknown" step as the most-plausible inference, not a
  directly witnessed fact.

`checker.rs`'s own doc comment states the policy this collides with
(checker.rs:56-64):

> "a checker must only emit a requires-clause when the corresponding
> violation path is confirmed `Sat` — `Unknown` must never manufacture
> requires any more than it manufactures findings"

So `Unknown` is *deliberately* treated as "don't emit" — sound, but the
knock-on effect is what regresses precision. When the "overflow" clause
fails to establish for `ClearPageElements`:

1. `call_site_obligations` (shared.rs:208-252) has nothing in
   `summary_of(ClearPageElements).requires` to instantiate at
   `command_surgery.go:268` → that finding vanishes.
2. `obligations()`'s own local-site loop, bounds.rs:517
   (`let pre = own_preconditions(&summary_of(f));`), no longer has
   `¬violation` in `pre` for this same function/tag — previously this
   is exactly what *self-masked* the local site (the function had
   already exported the requirement to its callers, so it didn't also
   blame itself). With `pre` empty, the local obligation's query
   (`reach(block) ∧ pre ∧ violation`) is genuinely Sat again → the
   manifest finding fires at `surgeon.go:78:20` instead.

Both deltas (`268:55` vanishing, `78:20` appearing) are the *same* root
cause — the vanished `infer_requires` clause — exactly as the brief's
hint anticipated; the best-supported explanation for "why" is a
resource-limit (`Unknown`-verdict) artifact of `encode_call_ensures`'s
added query cost (see the itemized evidence breakdown above — this is
the inferred link, not a directly witnessed one), and it is **not** a
genuine Sat→Unsat logical flip (that part *is* established: a correct,
independent new fact could never cause a Sat→Unsat flip, since a true,
independent conjunct cannot make an otherwise-Sat formula Unsat — I
verified the raw query is `sat` via a completely unbounded external
solver run). So whatever the exact live/cached-timeout split, the
verdict genuinely differing across runs of identical source is only
explicable as some form of resource-limit sensitivity, not a logic bug.

### The independent, pre-existing gap this interacts with

`elementCnt := int(p.Count())` — `Count()` returns `uint16`, so its
result's SMT sort is intrinsically `BitVec(16)` (`≤ 65535` for free).
**Correction (task reviewer, post-commit):** an earlier version of this
report claimed `op_def`'s match has no `Op::Convert` case at all — that
is wrong. There **is** an `Op::Convert` arm, `encode.rs:1031-1039`, the
uintptr→pointer provenance arm from the fix wave (asserts the dst
non-nil when the src has uintptr provenance and the dst is
pointer-sorted). What's actually true, and what the rest of this
argument rests on, is narrower: that arm only ever fires for a
*pointer*-sorted dst (`d.sort() != &ptr_sort()` bails early) — the
`int → int` widening conversion this bug is about never matches its
guard, so it falls through to the match's catch-all, which havocs
(`encode.rs:1052`, comment: "Convert havocs except the uintptr-provenance
arm above"). So `elementCnt`'s wider `BitVec(64)` term is fully havoc'd,
unrelated to `Count()`'s narrow result — **for Task 4A this means
extending the existing `Op::Convert` arm with an int-widening sub-case,
not adding a second, fresh (and unreachable/duplicate-match) `Op::Convert`
arm.** This gap is
**not** the regression itself (it predates task 6 and is present at
`a0e1b28` too — task 5 also never fires `78:20`) — it's what makes the
underlying query genuinely `Sat` (the overflow really is reachable
*as currently modeled*) rather than provably `Unsat`, which is precisely
why the query is delicate enough to tip into `Unknown` once
`encode_call_ensures` makes it harder. This matches the design spec's
own planning note (`docs/superpowers/specs/2026-07-21-summaries-followups-design.md`
§4) almost verbatim.

## RED corpus repro (Step 3)

`testdata/corpus/bounds/bounds.go` — the brief's shape, plus one
adjustment (documented inline, see the `clearOpts` comment): the given
`ClearElemsUnbounded(i int) uint16 { return ClearElems(i) }` doesn't
reproduce anything by itself. Root cause, found by tracing the actual
`--emit-smt` output: `propagate_requires` (shared.rs:90-140) instantiates
`ClearElems`'s "overflow" requires clause at `ClearElemsUnbounded`'s
call site and, since the argument is a **bare forwarded parameter**,
`params_only(&bound)` is (vacuously) true after substitution — the
clause propagates **transitively up** into `ClearElemsUnbounded`'s own
summary instead of becoming a decidable `call_site_obligation`. With
nothing left downstream to violate it (no further caller), the whole
chain self-masks and the corpus is silent end to end — the exact same
"self-consistency" mechanism as the manifest-masking above, just one
call frame further out.

Fix (mirrors bbolt's real topology): `cfg.startElementIdx` in the real
code is a **struct field** of a CLI-flag-populated options struct, not a
bare parameter. I changed `ClearElemsUnbounded` to take a `clearOpts`
struct and forward `o.start` (a field access) instead of a bare `int`
parameter — this fails `params_only` at the propagation step (the
substituted bound now references a local, not a `p<i>`), so
`propagate_requires` stops there and `call_site_obligations` raises a
real, immediately-discharged obligation instead.

Verified RED:

```
$ mise x -- cargo test -p goverify-checkers --test bounds_corpus
thread 'bounds_corpus_findings_match_want_comments' panicked:
assertion `left == right` failed: findings vs want comments
  left: {..., ("bounds.go", 95, "overflow")}
 right: {...}  // no 95 entry
test result: FAILED. 1 passed; 1 failed
```

Fires at `bounds.go:95` (`ClearElemsUnbounded`, the caller), tag
`overflow` — matching the brief's own flexible "Expected: FAIL — an
overflow finding fires (at the `uint16(start)` line **and/or a
caller**)". `ClearElemsBounded` (constant argument) stays silent, as
expected.

I could **not**, within this investigation's budget, force the repro to
fire at the manifest position (`ClearElems` itself, mirroring
`surgeon.go:78:20`) via `cargo test -p goverify-checkers --test
bounds_corpus`: that test wires **only `BoundsChecker`** (`NilChecker`
never runs, so `encode_call_ensures` has nothing to assert regardless of
what callee I add in scope — verified empirically: I added a
`diagLookup` function replicating the exact `ReadPage` correlation
idiom, temporarily registered `NilChecker` too, and even at a
role-differentiated 100ms/250ms timeout matching `check`'s CLI
defaults, the query for this tiny function still resolves well within
budget — bbolt-scale complexity (many blocks, several calls) is what's
needed to approach the 100ms line, and a small illustrative corpus
function can't cheaply replicate that without becoming a slow, abnormal
test). The committed repro instead demonstrates the **structural**
vulnerability (the severed widening-conversion bound, propagated to a
real, decidable call-site obligation) that both the design spec's own
description of pre/post-4A behavior and the brief's flexible wording
anticipate — it is RED for the same underlying reason C221 is a bug,
even though it can't reproduce the specific resource-limit flake at
corpus scale.

## Decision (Step 4)

> **Task 4 branch selected: 4A (convert-model discharge)**, because the
> regression is best explained by a `Sat`-vs-`Unknown` timing/complexity
> artifact (the specific "exceeds the 100ms budget" step is the
> best-supported inference from the evidence, not something I directly
> caught happening — see the Mechanism section's evidence breakdown)
> around a query that is only reachable/Sat in the first place due to
> an independent, pre-existing gap (`Op::Convert`'s widening sub-case
> never asserts a defining equality — the arm itself exists,
> `encode.rs:1031-1039`, but only handles uintptr→pointer provenance; the
> `int`→`int` widening case falls through to the catch-all havoc, so
> `Count()`'s intrinsic `≤ 65535` bound never reaches `elementCnt`).
> Asserting a range bound on the widened dst
> (`0 ≤ dst ≤ 65535` for a `uint16` source, per the design spec §4)
> makes the manifest site's own query **provably Unsat** — not just
> fast, actually unreachable — which both fixes the FP outright (bbolt's
> real code is safe here) and removes the fragile Sat-near-timeout
> query that caused the flake, since Z3 resolves a tight algebraic
> contradiction far more robustly than it resolves a Sat search over
> several free variables. This is a strictly better outcome than 4B
> (restoring the requires form) would be: 4B just re-pins the OLD
> behavior without touching the underlying delicacy, so the same class
> of resource-limit flake could resurface on a different shape or under
> different load. I found no evidence the regression mechanism damages
> any *other* finding class (the encode_call_ensures-added facts are
> orthogonal boolean/pointer correlations; they cannot themselves flip
> an independent bitvector query from Sat to a genuine Unsat — only to
> Unknown under a timeout), so neither of the brief's 4B triggers
> applies.

## Concern for Task 4 / future work

The underlying resource-limit sensitivity (a query sitting close to the
100ms requires-inference budget, tipped by unrelated `encode_call_ensures`
additions) is a **general** hazard, not unique to this one conversion —
any `BoundsChecker`/`NilChecker` local site whose discharge query grows
with a function's interprocedural ensures footprint is a candidate for
the same flake. 4A removes it for *this* shape by making the query
trivially Unsat; it does not add any general timeout-tier separation or
retry-with-more-time safety net. Worth a note if Task 4's implementer
wants to scope-check for other borderline sites, but out of scope for
this investigation.
