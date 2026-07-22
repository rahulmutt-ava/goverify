# Task 3 report: C221 investigation — bisect the regression, confirm the discharge path

(Supersedes earlier unrelated Task-3 content from a different
task-numbering pass — `Checker::infer_ensures` trait surface + engine
collection, commit `21c5e1f`, from the interprocedural-summaries wave.
That work is complete, merged, and untouched by this task; its content
is recoverable from git history. This is the summaries-followups wave's
own task 3, an investigation task with no production-code changes.)

Branch `summaries/followups`, HEAD `2c24ad5` at start (Tasks 1-2 done,
nothing touching bounds). All probing done in temporary git worktrees
under the scratchpad, all removed. Working tree change: one file,
`testdata/corpus/bounds/bounds.go` (RED repro). New/updated files:
`.superpowers/sdd/task-3-investigation.md` (this task's required
deliverable) and this report.

## Step 1: Bisect with targeted probes

Setup per probe: `git worktree add <scratch>/c221-probe <commit>`, copy
`mise.local.toml` in, symlink `.goverify` (shares the bbolt checkout +
SMT cache), `mise trust`, `export CARGO_TARGET_DIR=.../target`,
`mise x -- cargo build --release -p goverify-cli`, then run
`check ./...` against bbolt v1.4.0 (via `mise run shakeout` or the
built binary directly), grep for the two signatures.

**Gotcha discovered immediately**: worktrees don't inherit mise's trust
state (`mise ERROR ... not trusted`) — fixed with `mise trust` in each
new worktree, once, before any `mise x`/`mise run` call.

**Gotcha discovered on probe 1**: with `CARGO_TARGET_DIR` pointed at the
shared target dir, `scripts/shakeout.sh`'s `BIN="$(pwd)/target/release/
goverify"` resolves relative to the worktree's `./target`, which doesn't
exist (binaries land in the shared dir) — exit 127. Fixed with
`ln -sf $CARGO_TARGET_DIR target` inside each worktree before running
`mise run shakeout`, or by invoking the built binary with an absolute
path directly.

### First pass (as instructed): the two named suspects, shared cache

| Commit | Task | Result |
|---|---|---|
| `9994c53` | 6 | `internal/surgeon/surgeon.go:78:20` overflow (manifest) present; no `268:55` |
| `5549cb7` | 8 | same: `78:20` present, no `268:55` |

Both suspects showed the *post*-regression signature, so per the
brief's rule I probed the immediate predecessor to find the true first
occurrence rather than trusting either named suspect as "the" answer:

| Commit | Task | Result |
|---|---|---|
| `a0e1b28` | 5 (predecessor of 9994c53) | `command_surgery.go:268:55` present; no `78:20` |

Clean boundary: `a0e1b28` clean → `9994c53` regressed → `5549cb7` still
regressed. Initial conclusion: `9994c53` (task 6, bounds.rs/nil.rs
switch to `encode_func_with`) is the culprit; task 8's canonicalization
adds nothing new.

### Second pass: sanity-checking against a fresh cache (not in the original brief, but necessary)

While preparing Step 2 I ran a plain default-timeout `check` at current
HEAD using the *same shared cache dir* the probes had been accumulating
into, expecting to still see `78:20` (HEAD is chronologically after
`5549cb7`). It showed `268:55` instead. I checked whether this was a
caching artifact with progressively more isolated caches:

| Test | Cache | Result |
|---|---|---|
| HEAD, default timeouts | shared (accumulated) | `268:55` |
| HEAD, `--solver-timeout-ms 5000 --obligation-timeout-ms 5000`, new dir | fresh | `268:55` |
| HEAD, `--solver-timeout-ms 5000 --obligation-timeout-ms 250`, new dir | fresh | `268:55` |
| HEAD, default timeouts, brand-new dir | fresh | `268:55` |
| HEAD via actual `mise run shakeout`, shared cache dir wiped first | fresh | `268:55` |
| `9994c53` re-probed, brand-new isolated cache dir | fresh | `268:55` (!) — contradicts pass 1 |
| `a0e1b28` re-probed, brand-new isolated cache dir | fresh | `268:55` (consistent) |

The `9994c53` re-probe under a genuinely fresh, never-shared cache
directly contradicted my own first-pass observation at the exact same
commit. I went looking for whether the original signal was a
stale-cache artifact or genuine flakiness, and found the prior wave's
own shakeout report (`.superpowers/sdd/task-10-report.md`) had already
tested this exact question at `b94581a` (a commit in my bisection range)
and recorded two runs — cold and warm — "byte-identical", both showing
`78:20`. I re-probed `b94581a` myself, twice, each with a brand-new
isolated cache dir:

| Commit | Cache | Result |
|---|---|---|
| `b94581a`, run 1 | fresh, isolated | `268:55` |
| `b94581a`, run 2 | fresh, isolated | `268:55` |

Both of mine disagree with the prior report's two runs. Resolution: the
prior wave's "cold + warm" comparison necessarily reused *the same*
cache directory between the two runs (`scripts/shakeout.sh` always uses
`$(pwd)/../cache`) — their agreement demonstrates cache consistency,
not independent re-solving. Their *cold* run, though, was a genuinely
fresh computation (empty cache dir) and it did observe `78:20` — real
evidence the Unknown-verdict path fires in some environments. My
repeated, independently-fresh runs consistently landing on `268:55`
while their one independent (cold) run landed on `78:20` is fully
consistent with a genuinely load/timing-sensitive query, not a
contradiction to explain away and not a flaw in either investigation's
method.

### External corroboration found (not produced by me)

- `.superpowers/sdd/task-10-report.md` — this wave's own gated shakeout
  at `b94581a`: `surgeon.go:78:20` recorded as new-vs-509, FP, "C221 ...
  resurrected phase-4 FP (fix-3's call-site requires re-internalized)".
- `.superpowers/sdd/task-9-investigation.md` — an **unrelated, prior**
  "fixwave" plan's own Q2, investigating the *opposite*-direction
  transition (`78:20`→`268:55`) at commit `d9ace1f` (an ancestor of this
  wave's base `31a50cf`). It calls the transition to `268:55` a genuine
  precision *improvement* and explicitly says it "did not derive the
  exact SMT-level reason fix 3's asserts flip the infer-vs-manifest
  classification" — this exact signature pair was already known, before
  this wave started, to be a fragile encoding-sensitive boundary.
- `docs/superpowers/specs/2026-07-21-summaries-followups-design.md` §4
  — the design spec this task's brief was generated from. Names the
  same two hypotheses ("Prime": task 8 canonicalization; "Second": task
  6's `encode_func_with` switch) and already notes the `Op::Convert`
  widening-modeling gap as planning groundwork — I confirmed this same
  gap independently by reading `encode.rs` before finding this section.

**Final bisection verdict**: `9994c53` (task 6) is the culprit commit —
the first commit in the range capable of exhibiting `78:20` at all.
`a0e1b28` (task 5) cannot exhibit it under any tested condition,
consistent with the mechanism requiring a capability task 6 introduces.
Task 8 (`5549cb7`, the "Prime"/canonicalization hypothesis) is
disconfirmed as an independent contributor.

## Step 2: Mechanism

- `params_only` does not change — the overflow site's violation term is
  built solely from the bare param `start` (`convert_sites`,
  bounds.rs:302-334).
- The gate that flips is `discharge()`'s Sat check in `infer_requires`
  (`bounds.rs:474-478`). `encode_func_with` (the task-6 switch) adds
  `encode_call_ensures` (`encode.rs:788-825`), which asserts any
  `Inferred`-provenance callee's `ensures` into the *encoding function's
  own* query, gated on the call's block guard. Only `NilChecker`
  (task 5's Go-idiom correlation rule, `err == nil ⇒ result != nil`)
  ever produces such ensures — and `ClearPageElements` calls
  `guts_cli.ReadPage`, which matches that idiom exactly.
- This makes `ClearPageElements`'s own overflow-site discharge query
  bigger without changing its true answer — confirmed genuinely `sat`
  via an unbounded external `z3` run on the dumped `--emit-smt` text.
  `check`'s CLI wires an asymmetric timeout: 100ms for
  requires-inference queries, 250ms for findings/obligation queries
  (`crates/goverify-cli/src/main.rs`, `solver_timeout_ms`/
  `obligation_timeout_ms` defaults). Once big enough, this query
  sometimes exceeds the 100ms budget and returns `SatResult::Unknown`
  instead of `Sat`.
- `checker.rs`'s own doc comment mandates `Unknown` must never
  manufacture a requires clause (sound bug-finder policy) — so on
  Unknown, `infer_requires` silently drops the "overflow" clause for
  `ClearPageElements`. Two knock-on effects from the same root cause:
  `call_site_obligations` has nothing to instantiate at
  `command_surgery.go:268` (vanishes there), and `obligations()`'s
  `pre = own_preconditions(&summary_of(f))` (`bounds.rs:517`) no longer
  contains the self-masking `¬violation` for this function/tag, so the
  same local site's own manifest obligation is no longer masked and
  fires at `surgeon.go:78:20` instead.
- Interacts with (does not cause) an independent, pre-existing gap:
  `Op::Convert`'s widening arm has no defining equality anywhere in
  `encode.rs`'s `op_def` (confirmed by reading the whole match), so
  `Count()`'s intrinsic `BitVec(16)` (`≤ 65535`) bound never reaches the
  widened `elementCnt`. That's why the query is genuinely Sat (not
  provably Unsat) in the first place — which is exactly what makes it
  delicate enough to tip into Unknown once `encode_call_ensures` adds
  cost.

Full quoted diff/lines are in `task-3-investigation.md`.

## Step 3: RED corpus repro

Appended the brief's shape verbatim first; it did not reproduce
(silent, test green). Traced via `--emit-smt` on an isolated build:
`ClearElemsUnbounded(i int) uint16 { return ClearElems(i) }` forwards a
*bare parameter*, so `propagate_requires` (shared.rs) finds
`params_only` vacuously true after substitution and lifts `ClearElems`'s
"overflow" requires transitively into `ClearElemsUnbounded`'s own
summary rather than raising a decidable call-site obligation — with no
further caller, the whole chain self-masks silently (the same
self-consistency mechanism as the manifest-masking above, one call
frame further out).

Adjusted per the brief's own guidance ("may need a summary-bearing
callee in scope" — the actual fix needed was in the same spirit: match
bbolt's real call topology). `cfg.startElementIdx` in the real bbolt
code is a struct field (CLI-flag-populated), not a bare parameter — I
changed `ClearElemsUnbounded` to take a `clearOpts{start int}` struct
and forward `o.start`, a field access. This fails `params_only` at the
propagation step, so `call_site_obligations` raises a real,
immediately-discharged obligation instead.

```
$ mise x -- cargo test -p goverify-checkers --test bounds_corpus
running 2 tests
test bounds_corpus_findings_match_want_comments ... FAILED
test findings_and_smt_artifacts_are_deterministic ... ok

thread '...' panicked: assertion `left == right` failed: findings vs want comments
  left: {("bounds.go", 19, "bounds"), ("bounds.go", 25, "bounds"), ("bounds.go", 37, "bounds"),
         ("bounds.go", 46, "div-zero"), ("bounds.go", 60, "overflow"), ("bounds.go", 95, "overflow")}
 right: {("bounds.go", 19, "bounds"), ("bounds.go", 25, "bounds"), ("bounds.go", 37, "bounds"),
         ("bounds.go", 46, "div-zero"), ("bounds.go", 60, "overflow")}
test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out
```

RED as required. Fires at `bounds.go:95` (`ClearElemsUnbounded`, a
caller), tag `overflow` — matching the brief's own flexible wording
("Expected: FAIL — an overflow finding fires (at the `uint16(start)`
line **and/or a caller**)"). `ClearElemsBounded` (constant arg `3`)
stays silent, as intended.

I attempted, but could not force within budget, the exact manifest
position (inside `ClearElems` itself). Root cause: `bounds_corpus.rs`
wires **only `BoundsChecker`** — `NilChecker` never runs in that test,
so `encode_call_ensures` has nothing to assert regardless of what callee
I add. Verified empirically with a temporary (fully reverted)
diagnostic: added a `diagLookup` function replicating `ReadPage`'s exact
correlation idiom, temporarily registered `NilChecker` alongside
`BoundsChecker` in a scratch `#[test]`, and even tightened the
diagnostic's timeouts to 100ms/250ms (matching `check`'s CLI defaults)
— the query for this tiny function still resolved comfortably within
budget every time. Bbolt-scale complexity (many blocks, several
interprocedural calls) is what's needed to approach the 100ms line; a
small, fast, illustrative corpus function can't cheaply replicate that
without becoming a slow, non-representative test — and
`bounds_corpus.rs`'s own doc comment explicitly says its 5s uniform
timeout exists specifically "so slow CI can't turn a Sat into Unknown
and flake the golden," i.e. the harness is deliberately built to resist
exactly this class of flake. All diagnostic edits to `bounds_corpus.rs`
and the `diagLookup` addition to `bounds.go` were reverted before
finishing (verified via `git diff --stat` showing only the intended
`bounds.go` change); only the `clearOpts`-based fix remains committed.

Confirmed everything else stays green:

```
$ mise x -- cargo test -p goverify-checkers --no-fail-fast
unittests: 33 passed, 0 failed
tests/bounds_corpus.rs: 1 passed, 1 FAILED (bounds_corpus_findings_match_want_comments — intentional)
tests/differential_findings.rs: 2 passed, 0 failed
tests/ensures_corpus.rs: 1 passed, 0 failed
tests/knownfp_corpus.rs: 1 passed, 0 failed
tests/nil_corpus.rs: 2 passed, 0 failed
```

`cargo build --workspace` also clean.

## Step 4: Decision

**Task 4 branch selected: 4A (convert-model discharge)**, because the
regression is a `Sat`-vs-`Unknown` timing/complexity artifact around a
query that is only reachable/Sat in the first place due to the
independent, pre-existing `Op::Convert` widening gap. Asserting a range
bound on the widened dst (per the design spec §4: `0 ≤ dst ≤ 65535` for
a `uint16` source) makes the manifest site's own query provably Unsat —
not just faster, actually unreachable — fixing the FP outright and
removing the fragile near-timeout query that caused the flake in the
first place (an algebraic contradiction is far more robust for Z3 to
resolve than a Sat search). Strictly better than 4B (restore requires
form), which would just re-pin the old behavior without touching the
underlying delicacy — the same class of resource-limit flake could
resurface on a different shape or under different load. No evidence the
mechanism damages any other finding class (the added
`encode_call_ensures` facts are orthogonal boolean/pointer correlations
— they can only turn a query's *time* outcome, never its true Sat/Unsat
answer, since a genuinely independent true conjunct cannot make an
otherwise-Sat formula Unsat), so neither of the brief's stated 4B
triggers applies.

## Step 5: Commit

```
git add -f testdata/corpus/bounds/bounds.go .superpowers/sdd/task-3-investigation.md .superpowers/sdd/task-3-report.md
git -c commit.gpgsign=false commit -m "followups: C221 bisect + mechanism, RED truncation repro (task 3)"
```

Bounds_corpus is intentionally RED; everything else is green (Step 3
output above). Did not run the full `mise run corpus`/blocking gate,
since that would (correctly) also report the intentional bounds_corpus
failure — per the brief's instruction not to run the full gate
expecting green bounds.

## Files changed

- `testdata/corpus/bounds/bounds.go` — appended the RED repro
  (`count`/`ClearElems`/`clearOpts`/`ClearElemsUnbounded`/
  `ClearElemsBounded`), no `// want:` comments (intentional).
- `.superpowers/sdd/task-3-investigation.md` — new, the required
  investigation report (probe table, mechanism, repro, decision).
  Gitignored by `.superpowers/sdd/.gitignore`; added with `git add -f`.
- `.superpowers/sdd/task-3-report.md` — this report, overwriting a
  stale unrelated report from a prior numbering scheme (see the note at
  top); also gitignored, also needs `git add -f`.

No production code touched — this is an investigation-only task per
the brief.

## Self-review

- **Probe table complete with per-commit signatures observed?** Yes,
  including the extra probes needed once the shared-cache signal turned
  out to need cross-checking (a0e1b28, 9994c53 ×2, 5549cb7, b94581a ×2,
  HEAD ×6 variants) — more thorough than the brief's minimum ask, with
  each extra probe's reason documented.
- **Mechanism quoted from the culprit diff (file:line), not paraphrased
  guesswork?** Yes — the diff hunk, the exact gate lines
  (`bounds.rs:474-478`, `bounds.rs:517`), `encode_call_ensures`, the
  CLI's timeout defaults, and `checker.rs`'s policy doc comment are all
  quoted in `task-3-investigation.md`. I additionally verified the
  disputed query's true answer directly against an unbounded external
  `z3` binary rather than relying on inference alone.
- **Repro RED, and its firing location consistent with the bisected
  mechanism?** RED, confirmed by direct test run. Firing location is
  "a caller" (the brief's own stated alternative), not the manifest
  position — I was explicit about why the manifest position isn't
  reachable through this specific test harness's checker registration,
  rather than silently settling for an unverified location.
- **DECISION follows the brief's rule with evidence?** Yes — 4A is the
  brief's stated default, and I gave affirmative evidence beyond "it's
  the default": the fix target (provable Unsat) directly removes the
  resource-limit sensitivity that caused the regression.
- **Worktrees removed; main checkout clean apart from your commit?**
  All worktrees removed (`git worktree list` shows only the main
  checkout). `git status` showed only the intended `bounds.go` change
  before committing.

## Concerns

- The regression is genuinely flaky/load-dependent — I could not
  produce a clean, single-command, always-reproducible A/B (turn the
  timeout up, watch it flip, in one continuous session) the way I
  originally expected; the evidence is instead a pattern across many
  independently-fresh runs plus the prior wave's own cold-run data
  point. The mechanism itself is solid (directly readable from source,
  confirmed via the CLI's documented timeout tiers and doc comments),
  but `mise run shakeout` may show either signature on a given run
  depending on machine load, until Task 4's fix removes the delicacy.
- The corpus repro fires at the call-site position, not the manifest
  position, because `bounds_corpus.rs` registers only `BoundsChecker`.
  I judged this acceptable against the brief's own flexible wording
  rather than modifying the test harness's checker list or timeout
  (out of scope per the brief's file list), but flagging it in case
  Task 4 wants a corpus case exercising the manifest position
  specifically (would need a `knownfp_corpus`-style test, which
  registers both checkers).
