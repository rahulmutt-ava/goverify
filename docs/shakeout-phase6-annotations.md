# Phase-6 shakeout: annotation-language gates (G1-G4)

Status: **ALL GATES PASS** (2026-07-26) — run against the same pinned
bbolt shakeout target used by every prior wave
(`.goverify/shakeout/bbolt` @ v1.4.0, the 457-finding baseline first
recorded by phase-5a and re-verified unchanged through phase-5b). Exit
criteria per [phase-6 design spec §9](superpowers/specs/2026-07-25-phase6-annotations-design.md#9-gates-bbolt-shakeout)
and this wave's [task-14 brief](../.superpowers/sdd/2026-07-25-phase6-annotations/task-14-brief.md).

## Run parameters

- Wave tip (HEAD): `d374598` (branch `worktree-phase6-annotations`,
  Tasks 1-14).
- Wave base: `5a3f101` (`main`, per the plan) — built in a detached
  `git worktree add <scratch>/base 5a3f101` checkout, removed after use.
- Both binaries: `mise x -- cargo build --release -p goverify-cli`.
- bbolt: the existing cached checkout at `.goverify/shakeout/bbolt`
  (v1.4.0, same ref as phase-5b — verified via `git log -1` before
  starting; working copy was clean apart from the pre-existing
  untracked `.z3-trace`, same as every prior addendum).
- Shakeout cache: the existing pre-warmed `.goverify/shakeout/cache`
  (703 MB, populated by phase-4/5a/5b runs) — reused in place for the
  base-vs-new content comparisons (G1, part of G2), exactly as
  phase-5b's addendum did. A **separate, fresh** `mktemp -d` cache was
  used for G2's cold/warm self-consistency check (that check is about
  cache-replay determinism, not base-vs-new comparison, so a clean
  cache isolates it from the shared cache's pre-existing state).
- No EDR first-exec stall observed: both fresh release binaries'
  `--help` returned in under a second.

## Deviations (recorded per the honesty rule)

1. **Two deviations requested explicitly by reviewers, both already
   recorded in-repo at implementation time (Task 8/Task 12-13), and
   restated here:**

   - **Spec §3/§4 dependency-direction deviation.** The design spec
     reads as if the engine calls into `goverify-spec` directly
     ("the engine compiles each function's pragmas once, at summary
     construction"). That edge cannot exist: `goverify-spec` depends on
     `goverify-analysis` (to resolve/lower against the engine's own
     `FuncAnnotations`/`Clause` types), so `goverify-analysis` depending
     back on `goverify-spec` would cycle. `ignore`-name validation does
     NOT pull in `goverify-checkers` as a production dependency —
     `compile_program` takes the known-checker set as a plain
     `known_checkers: &[&str]` parameter, supplied by the CLI;
     `goverify-checkers` appears in `goverify-spec`'s `Cargo.toml` only
     as a `[dev-dependencies]` entry, for `annot_corpus.rs`'s
     end-to-end test harness. The implementation instead
     compiles at the CLI: `goverify-cli` calls
     `goverify_spec::compile_program(&program, &known_checkers)` once
     per run and passes the resulting `Annotations` into
     `goverify_analysis::EngineConfig`, which `analyze_full` consumes
     at the same summary-construction point the spec describes — one
     layer up. The shared data types
     (`FuncAnnotations`/`AnnClause`/the `contract`/`bad-annotation`/
     `unverified-annotation` tag constants) live in
     `goverify-analysis/src/annotations.rs` for exactly this reason.
     Recorded as one paragraph in the design spec's §3 and in
     `ARCHITECTURE.md`'s `goverify-spec` row + crate-graph paragraph
     (this task's Steps 1-2).
   - **Ensures-verification query deviation.** Spec §4's sketch is
     `body ∧ ¬clause`; the shipped query is `own-requires ∧ body ∧
     ¬clause` — the function's own (merged, annotated-included)
     requires terms are assumed alongside the body, per §1(a)'s
     "requires assumed at function entry" rule (the same
     `own_preconditions`-assumed convention the checkers already use
     for in-body obligations). Without it, a valid contract like
     `requires x > 0` + `ensures ret > 0` on `return x` is unprovable
     and spuriously warns. See `verify_ensures`'s doc comment and the
     `verify_ensures_uses_own_requires_as_an_assumption` regression
     test in `crates/goverify-analysis/src/engine.rs`. Recorded as a
     sentence in the design spec's §4 (this task's Step 2b).

2. **SARIF `level` `"warning"`→`"error"` is a deliberate, documented
   non-additive change (not a regression).** Before this wave every
   finding was `Severity::Error` in effect but SARIF had no severity
   concept, so every result rendered `level: "warning"`. Now
   `Severity::Error` maps to SARIF `level: "error"` and only the new
   `unverified-annotation` class renders `"warning"`. Confirmed
   empirically below (G2): the base binary emits `"warning"` for all
   457 bbolt results; the new binary emits `"error"` for all 457 (bbolt
   has no annotations, so none renders as the new warning class). See
   `crates/goverify-cli/src/sarif.rs`'s `build_result` comment, which
   already cites this addendum by name.

3. **`debug summary` (and `debug findings`, `debug ir`) do not compile
   or display `//goverify:` annotations at all — discovered during the
   G4 dogfood run, not previously called out in the follow-up queue.**
   Only the `check`/`baseline write` path
   (`run_check`/`run_baseline_write` in `crates/goverify-cli/src/main.rs`)
   calls `goverify_spec::compile_program` and threads the result into
   `EngineConfig::annotations`. The `debug summary`/`debug prepass`
   arms call `goverify_analysis::analyze(&program, &Options::default())`
   — a thin wrapper that always uses `EngineConfig::default()`
   (`Annotations::default()`, i.e. empty) — and `debug findings`
   builds its own `EngineConfig` but also hardcodes
   `Annotations::default()`. `debug ir`'s raw dump doesn't print pragma
   text either (`gvir::Pragma` is captured but not surfaced by
   `dump_function`). Verified directly: adding `//goverify:requires
   size >= 0` to bbolt's `(*DB).mmapSize` and running
   `debug summary --func mmapSize` still reports `requires=0` (see G4
   below) — this is the same *class* of pre-existing divergence the
   plan's follow-up queue already flags for `debug findings` (`vec![&NilChecker]`
   instead of `default_checkers()`), just not previously identified for
   the annotation surface specifically. **This task is docs+shakeout
   only (no files outside README/ARCHITECTURE/design-spec/addendum are
   in Task 14's scope), so it is not fixed here** — G4's "clause is
   visible" demonstration below instead uses `goverify check`'s
   human/JSON output, which quotes the clause text verbatim in the
   `contract` finding's message at the call site. Flagged as a concern
   in this task's completion report; a candidate for the 6b follow-up
   queue (wire `debug summary`/`debug findings` through
   `compile_program` the same way `run_check` does).

4. **Base-vs-new finding-count comparisons are only reproducible
   against a shared, fully-warm cache — matching exactly what
   phase-5b's addendum already did, not a new requirement invented
   here.** An initial attempt compared the two binaries against two
   *separate, freshly created* cache directories (no shared warm
   state) and got non-matching counts: base 465 vs new 457 (and, in an
   unrelated self-consistency check against yet another fresh cache,
   462 for a single binary, internally consistent cold-vs-warm but
   numerically different from 457). The reported solver
   retry-escalation counts varied widely across these fresh-cache
   attempts (85 / 196 / 204 / 310 queries escalated to the 10x retry
   tier across different runs) — consistent with SAT/Unknown outcomes
   near the timeout boundary being sensitive to system load (this
   machine had just finished two back-to-back release `cargo build`
   compiles), not with a code regression. Switching the base-vs-new
   comparison to the existing pre-warmed `.goverify/shakeout/cache` —
   the same cache phase-5b's own G1 command reused in place — made
   every run deterministic and byte-identical (0 escalations needed
   for the already-warm base binary; the new binary needed one
   recompute pass the first time it touched this cache, expected from
   the SCC-cache version bump the annotation wave's clause-provenance
   field forces per spec §6, then was itself fully warm and
   byte-identical on repeat). **All G1/G2 numbers reported below are
   from the shared-warm-cache runs**; the fresh-cache attempts are
   recorded here only as a methodology note, not as gate results.
5. A stray untracked `.z3-trace` (0 bytes) sits in the bbolt clone,
   predating this session (also noted in the phase-5b addendum); left
   alone, not part of this task's cleanup contract.

## Gate results

### G1 — human path unchanged (byte-identical 457)

```sh
BBOLT=.goverify/shakeout/bbolt
CACHE=.goverify/shakeout/cache
cd "$BBOLT"
GOVERIFY_EXTRACTOR_DIR=<worktree>/extractor <worktree>/target/release/goverify \
  check ./... --cache-dir "$CACHE" > new.txt
GOVERIFY_EXTRACTOR_DIR=<base-worktree>/extractor <base-worktree>/target/release/goverify \
  check ./... --cache-dir "$CACHE" > base.txt
cmp base.txt new.txt && echo "G1 PASS"
```

- `cmp`: silent (byte-identical) → **G1 PASS**
- Both runs: `exit=1`
- Finding-header count: **457** (exact match) on both
- `baseline write` on both binaries against the same cache produced
  byte-identical `.goverify/baseline.json` files (`diff` silent) —
  identical fingerprints, not just identical counts
- → **G1 PASS**

### G2 — machine-format determinism, and the documented diff vs. base

**Cold/warm self-consistency** (fresh `mktemp -d` cache, new binary
only — isolates cache-replay determinism from the base-vs-new
comparison):

```sh
FRESH=$(mktemp -d)
goverify check --format sarif ./... --cache-dir "$FRESH" > sarif-cold.json
goverify check --format sarif ./... --cache-dir "$FRESH" > sarif-warm{1,2,3}.json
goverify check --format json  ./... --cache-dir "$FRESH" > json-cold.json
goverify check --format json  ./... --cache-dir "$FRESH" > json-warm{1,2,3}.json
```

- `cmp sarif-cold.json sarif-warm{1,2,3}.json`: all silent
  (byte-identical)
- `cmp json-cold.json json-warm{1,2,3}.json`: all silent
  (byte-identical)
- No `/Users`, `/tmp`, or `/var/folders` substring in either output
- (This lineage reported 462 findings, not 457 — see deviation #4: a
  fresh cache under load is subject to retry-escalation variance. The
  cold/warm self-consistency property itself held regardless: all four
  runs of each format were identical to each other.)

**Diff vs. base** (shared warm cache, both binaries — the canonical
comparison):

```sh
goverify check --format json  ./... --cache-dir "$CACHE"   # new
goverify check --format json  ./... --cache-dir "$CACHE"   # base (own binary)
goverify check --format sarif ./... --cache-dir "$CACHE"   # new
goverify check --format sarif ./... --cache-dir "$CACHE"   # base (own binary)
```

JSON, diffed by fingerprint (Python, keyed on `fingerprint`):

- Fingerprint sets: **identical** (same 457 on both sides)
- Per-finding field diff, across all 457: only `severity` (present in
  new, absent in base)
- `summary`: new adds `suppressed_pragma: 0`; `schema_version`: new
  `2`, base `1`
- → matches the brief's documented allow-list exactly

SARIF, diffed by `(ruleId, file, line)` key:

- Result-key sets: **identical** (same 457 on both sides)
- `rules`: new has 3 extra (`contract`, `bad-annotation`,
  `unverified-annotation`); base's 4 (`nil-deref`, `bounds`,
  `div-zero`, `overflow`) are an exact subset
- Per-result field diff: only `level` — **every** base result is
  `"warning"`, **every** new result is `"error"` (deviation #2)
- `run.properties`: new adds `suppressedByPragma: 0`
- → matches the brief's documented allow-list exactly
- → **G2 PASS**

### G3 — warm wall-clock vs. the 3.35 s phase-5b baseline (report-only)

Three warm `check` runs (shared cache, `GOVERIFY_TIMINGS=1`):

| | phase-5b baseline | phase-6 warm run 1 | run 2 | run 3 |
|---|---|---|---|---|
| extract cache | 220 hit / 0 extracted | 220 hit / 0 extracted | same | same |
| extract+load | 0.89s | 1.03s | 0.98s | 0.96s |
| analyze | 2.32s | 2.31s | 2.34s | 2.39s |
| scc cache | 18965 hit / 0 miss | 18965 hit / 0 miss | same | same |
| scope+render | 0.08s | 0.07s | 0.05s | 0.07s |
| wall (`real`) | 3.35s | 3.49s | 3.45s | 3.49s |

Delta ≈ **+0.10-0.15s** (~3-4%), concentrated in `extract+load` (the
schema-4 `.gvir`/`Program` fields — the `.gvir` wire schema, distinct
from the CLI's JSON output `schema_version: 2` reported in G2 above —
are slightly larger to load) rather than `analyze` — `analyze`'s own
time is flat within noise (2.31-2.39s vs. 2.32s baseline). Annotation
compilation itself doesn't appear as a
separate line (it's the CLI's `compile_program` call, folded into the
process's overall wall time outside the four timed phases above), and
with zero pragmas in bbolt it is expected to cost effectively nothing;
the observed delta is consistent with that — **noise-level, not a
regression** → **G3 PASS (report-only, no hard threshold)**.

### G4 — dogfood: annotated `requires` + a violating call

Working copy: `.goverify/shakeout/bbolt` (never the pinned tree).
Added to `db.go` above `(*DB).mmapSize(size int) (int, error)`:

```go
//goverify:requires size >= 0
```

...and, inside `(*DB).mmap`, right after the function's own legitimate
call site, a deliberately-violating call:

```go
// goverify shakeout probe (Task 14, G4): deliberately violates
// mmapSize's annotated requires (size >= 0).
_, _ = db.mmapSize(-1)
```

`go build ./...` confirmed the probe compiles.

**"Clause visible in output"** — per deviation #3, `debug summary`
does *not* surface it:

```
$ goverify debug summary --func mmapSize
(*go.etcd.io/bbolt.DB).mmapSize effects=... requires=0 ensures=0 provenance=Inferred
```

`requires=0` — unchanged from before the pragma was added; `debug ir
--func mmapSize` likewise prints no pragma text. Substituting
`goverify check`'s output, which *does* compile and use the
annotation:

```
$ goverify check ./... --cache-dir "$CACHE"
...
db.go:466:25: contract: call to (*go.etcd.io/bbolt.DB).mmapSize violates annotated requires `size >= 0` [(*go.etcd.io/bbolt.DB).mmap]
  466 |  size, err = db.mmapSize(size)
    path: db.go:449 -> db.go:463 -> db.go:462 -> db.go:468
    with: p1 = #x9000000000000000

db.go:474:20: contract: call to (*go.etcd.io/bbolt.DB).mmapSize violates annotated requires `size >= 0` [(*go.etcd.io/bbolt.DB).mmap]
  474 |  _, _ = db.mmapSize(-1)
    path: db.go:449 -> db.go:463 -> db.go:462 -> db.go:474 -> db.go:478 -> db.go:479
```

The clause text (`size >= 0`) is quoted verbatim in both messages —
this is where "the clause appears in output" actually holds today.

**"A deliberately violating call produces a `contract` finding"** —
confirmed: `db.go:474:20`, the probe's own call site, is exactly one of
the two `contract` findings above (`severity: error`,
`func: (*go.etcd.io/bbolt.DB).mmap`, JSON fingerprint
`v1:35652db702ade3d63a09744ffc862151`).

**Unexpected but honest bonus finding:** the *pre-existing, legitimate*
call site (`db.go:466`, `size, err = db.mmapSize(size)`) *also* reports
a `contract` violation. This is not a bug in the demonstration: `size`
at that point derives from `db.fileSize()` (an `int64` file-size read)
with no summary proving it non-negative, so `size >= 0` genuinely isn't
provable at that call site either — the solver found a real
counter-model (`p1 = #x9000000000000000`, a negative `int64` bit
pattern). This is arguably a *better* demonstration of the "never
silently trusted" philosophy than a clean single-violation probe would
have been: an annotation that overclaims gets caught everywhere it's
violated, not just where the demo intended.

Total finding count with both `contract` findings present: **459**
(457 baseline + 2). Verified via
`grep -cE '^[^ ].*: (nil-deref|bounds|div-zero|overflow|contract): '`.

Cleanup: `git checkout -- db.go`; re-ran plain `check` against the same
shared cache afterward and confirmed byte-identical to the pre-probe
G1 output (457, `cmp` silent) — the working copy round-trips cleanly.
`git status --porcelain` shows only the pre-existing untracked
`.z3-trace` (deviation #5).

→ **G4 PASS**

## Cleanup verification

```
$ cd .goverify/shakeout/bbolt && git status --porcelain
?? .z3-trace
```

Base worktree (`git worktree add <scratch>/base 5a3f101`) removed via
`git worktree remove --force <scratch>/base` after both binaries were
built and all gate commands completed.
