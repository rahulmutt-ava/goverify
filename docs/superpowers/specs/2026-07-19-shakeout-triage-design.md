# Phase-4 shakeout triage — design

Date: 2026-07-19
Status: approved
Closes: spec §7 exit criterion for phase 4 (`docs/superpowers/specs/2026-07-16-goverify-design.md`), tracked as residual-ledger item 1 in `.superpowers/sdd/progress.md`.

## 1. Goal and scope

Triage every finding `goverify check` produces over pinned bbolt (v1.4.0)
into a class-level report with a recorded false-positive rate and the
phase-5 planning observations the ledger asks for. The last recorded run
produced 1006 findings (830 in methods); the expectation is that a
make-from-param local-obligation class dominates.

**In scope:** capture, bucketing tooling, verdicts, the filled-in report,
a per-finding appendix, and corpus cases pinning representative FP classes
as known-FP markers.

**Out of scope:** fixing any FP class. In particular the anticipated
substitution-based requires-lifting is phase-5 design input, not work here.
No product-code changes at all (no new CLI flags, no renderer changes).

## 2. Pipeline

Four stages: capture → bucket → triage → synthesize.

### 2.1 Capture

Re-run `mise run shakeout` at current `main`, teeing stdout to a capture
file (uncommitted; it is derived output). Two runs, wall-clocked: cold
cache (fresh `--cache-dir`) and warm (immediate re-run), filling the
skeleton's run-parameters section. Exit 1 is the expected outcome; exit 2
(analyzer error) blocks the task and is surfaced instead of triaged.

If the finding count differs from the ledger's 1006 (toolchain drift since
that run), proceed — the report records this run's numbers and notes the
delta.

### 2.2 Bucket — `scripts/shakeout_bucket.awk`

A committed awk script (matching the repo's existing bash/scripts surface;
no new language) parses the capture into one TSV row per finding:

```
pos  tag  func  message  source_line  has_trace  model_bindings
```

The renderer's format is pinned by goldens (`render.rs`): a header line
`file:line:col: tag: message [func]`, a gutter source line plus caret line,
and optional `    path: ...` / `    with: ...` continuation lines.

Initial machine bucketing keys on `(tag, normalized source line)`, where
normalization replaces identifiers and numeric literals with placeholders
so `b.pages[id]` and `tx.meta[i]` share a bucket shape. These coarse
classes are the unit of triage; refinement happens in stage 3.

**Sanity gate:** the script fails loudly (nonzero exit, message on stderr)
if the parsed row count disagrees with the number of finding headers seen,
or if any block has an unrecognized shape. No silent truncation in an
exit-criterion artifact.

### 2.3 Triage — one subagent per class

Each class is triaged by a dedicated subagent that receives: the class
definition (tag + pattern + count), deterministically chosen
representatives, each representative's full rendered block (source line,
trace path, model bindings), and access to the pinned bbolt checkout under
`.goverify/shakeout/bbolt`.

**Representative selection:** all findings if the class has ≤5; otherwise
5 chosen by sorting positions and taking first, last, and evenly spaced
interior points. Deterministic — no RNG.

**Evidence standard:** the subagent must read the bbolt source at each
representative site (and enough surrounding context — callers, invariants)
and cite file:line evidence for every verdict. Verdicts without citations
are rejected and the subagent re-run.

**Verdict taxonomy** (richer than the skeleton's TP/FP/unclear because the
FP *reason* is the phase-5 payload):

| Verdict | Meaning |
|---|---|
| TP | violating path actually reachable — a real bbolt bug (expected rare) |
| FP/requires-lifting | safe because callers guarantee a precondition the analyzer could not lift (expected dominant: make-from-param) |
| FP/invariant | safe due to a data-structure invariant outside the analyzer's model |
| FP/encoding | the analyzer's encoding is wrong at this site — a goverify bug |
| unclear | not determinable from local reading |

A class verdict is the unanimous representative verdict. If representatives
disagree, the controller splits the class on the distinguishing feature the
subagent identifies and re-triages the subclasses — **one refinement round
only**; a still-mixed class is recorded as mixed with its sampled ratio.

**Second opinion:** the dominant class (by count) gets an independent
second subagent triaging a disjoint representative sample; disagreement
between the two escalates to the controller for adjudication in the report.
Its verdict carries most of the report's weight, so it is not trusted to a
single pass.

Any FP/encoding verdict is a goverify bug: recorded in the report as a
fix-wave candidate for the plan owner, not fixed here (scope rule above).

### 2.4 Synthesize

The controller assembles the report and appendix from the TSV plus the
class verdicts. All totals are arithmetic over the TSV so they are
spot-checkable.

## 3. Committed artifacts

1. **`docs/shakeout-phase4-bbolt.md`** — the existing skeleton, filled in.
   The per-finding table becomes a per-class table: class id, tag, pattern,
   count, verdict, reason, representative positions, note. Totals section
   records finding count, per-verdict counts, FP rate, unclear count, and
   cold/warm wall clock. The dispatch-precision section addresses the
   carried Task-10 watch item (shared-signature over-approximation) and
   §16's dynamic-dispatch question with what the bbolt run actually shows.
   The phase-5 observations section quantifies the requires-lifting class:
   count, canonical example with trace, and what substitution-based lifting
   must handle to kill it.
2. **`docs/shakeout-phase4-bbolt-findings.tsv`** — full per-finding
   appendix: the stage-2 columns plus `class` and `verdict` (inherited from
   the class; rows in a still-mixed class carry `mixed`). This is what makes "all findings triaged" auditable and
   gives phase 5 a diff base for re-runs.
3. **`scripts/shakeout_bucket.awk`** — the parser/bucketer, reproducible
   for phase-5 re-comparison.
4. **Corpus cases** — one minimal Go repro per confirmed FP class
   (unanimous class verdict; mixed and unclear classes are not pinned), with
   `// want:` lines pinning **current** (wrong) behavior and a
   `KNOWN-FP(phase-5)` comment. The determinism suite stays green now, and
   phase 5 inherits red/green flip targets. TP findings (real bbolt bugs),
   if any, are recorded in the report only; upstreaming is a separate
   decision for the user.

## 4. Error handling

- Parser: loud failure on count mismatch or unrecognized block shape.
- Subagents: verdicts must carry file:line citations; missing evidence →
  reject and re-run. A subagent that cannot reach a verdict returns
  `unclear` rather than guessing.
- Shakeout exit 2: blocker, surfaced to the user, no triage.
- Degrade-never-die does not apply to the report itself: a hole in the
  pipeline fails the task rather than shipping an incomplete
  exit-criterion artifact.

## 5. Verification and exit-criteria mapping

- Report totals recomputable from the committed TSV.
- Every class verdict cites representative evidence in the report.
- Corpus additions pass `mise run corpus`; full blocking gate
  (`lint`, `test`, `secrets`, `audit`) stays green.
- Spec §7 exit criteria: **all findings triaged** — every TSV row carries a
  class and verdict; **FP rate recorded** — totals section; **every fixed
  FP lands a corpus case** — satisfied vacuously (no fixes in scope), with
  KNOWN-FP pins as the forward-looking substitute, stated explicitly in the
  report so the criterion's disposition is auditable;
  **dispatch-precision observations recorded** — dedicated section.
