# Phase 5b: CI surface — SARIF/JSON output, baselines, --diff-base

Date: 2026-07-24
Status: approved (brainstorm 2026-07-24)
Parent spec: `2026-07-16-goverify-design.md` (§10 CLI and CI surface,
§14 roadmap item 5)
Predecessor: `2026-07-23-phase5-caching-design.md` (phase 5a, merged
2026-07-24 @ `66dc5e2`)

## 1. Goal and scope

The second half of roadmap phase 5: the reporting surface that makes
`goverify` usable as a CI gate. Three features, one wave — they share
the finding-fingerprint concept, so they are designed together:

- `check --format sarif|json` — machine output for CI; SARIF for
  GitHub code scanning.
- `goverify baseline write` + automatic baseline suppression on
  `check` — adopt goverify on a codebase with existing findings and
  gate only on new ones.
- `check --diff-base <git-ref>` — analyze everything (the phase-5a
  caches make that cheap), report only findings attributable to
  functions changed since `<ref>` or their transitive callers.

Baseline: local `main` @ `66dc5e2`; bbolt shakeout at 457 findings.

**Decisions fixed by this brainstorm:**

- Fingerprint "obligation shape" = `tag ⊕ message ⊕ ordinal` (§2) —
  position-independent and encoding-independent, with an
  occurrence-index disambiguator for identical siblings (the
  SARIF-style tradeoff: inserting a new identical sibling above an
  existing one shifts ordinals; accepted).
- `--diff-base` determines changed functions by **extracting the base
  ref and comparing per-function content hashes** (§5) — not by
  parsing `git diff` line ranges. Semantically exact; reuses the
  func-hash machinery phase 5a built for scc cache keys.
- **serde + serde_json** join the workspace (§13 of the parent spec
  requires justification: three consumers — SARIF emission, native
  JSON emission, baseline write **and read**; the read is parsing a
  possibly-human-edited file, exactly where a hand-rolled parser would
  be the riskiest code in the wave).
- Baselines apply **automatically when `.goverify/baseline.json`
  exists**; `--no-baseline` opts out. No stale-entry pruning surface
  this wave (queued).
- All new code lives in `goverify-cli` (its ARCHITECTURE.md charter:
  argument parsing, orchestration, rendering), except a
  transitive-callers helper on `goverify-ir`'s call graph and a
  per-function content-hash exposure from `goverify-analysis` — each
  added to the crate that owns the underlying data.

## 2. Finding fingerprints (`goverify-cli/src/fingerprint.rs`)

The parent spec pins `hash(checker ⊕ stable function ID ⊕ obligation
shape)`. This spec defines obligation shape and the concrete scheme:

```
fp = "v1:" + hex(blake3(checker ⊕ func ⊕ tag ⊕ message ⊕ ordinal)[..16])
```

- Fields are length-prefixed before hashing (no separator injection),
  the same discipline as the cache keys.
- `func` is the Finding's stable function ID (the ssa id) — already
  position-independent.
- `message` is checker-produced text built from tags and function
  names; checkers must keep messages free of source positions (they
  are today; this becomes a stated invariant).
- `ordinal` = this finding's index among findings with identical
  `(checker, func, tag, message)`, ordered by the Finding `Ord`
  (position order). Distinguishes N same-shaped findings in one
  function so baselining one does not suppress the others.
- The `"v1:"` prefix versions the scheme in-band; a future scheme
  change is a new prefix, never a silent re-keying.

Fingerprints are computed in the CLI after scope filtering and before
baseline filtering, carried as a `(Finding, fingerprint)` pair —
`Finding` itself does not change (fingerprinting is a reporting
concern; the analysis layer stays pure). Ordinals are stable across
the whole filter chain because scope and diff-base both filter at
function granularity: a group of identical siblings shares one `func`,
so no filter before the baseline can split a group and shift its
ordinals.

## 3. Output formats: `check --format human|sarif|json`

Default `human` (the existing renderer, unchanged). New sibling
modules `sarif.rs` and `json.rs` beside `render.rs`. All formats write
to stdout; timings and diagnostics stay on stderr. Findings are
already sorted before rendering; both machine formats are
**byte-identical across runs** and join the corpus determinism suite.

**SARIF 2.1.0** (pinned version):

- One `run`; `tool.driver.name` = `goverify`,
  `tool.driver.semanticVersion` = crate version; `rules` = the checker
  tags (`nil-deref`, `bounds`, `div-zero`, `overflow`) with short
  descriptions.
- Each result: `ruleId` = tag, `level` = `warning`, `message.text` =
  the finding message with model bindings appended (`with: p0 =
  (ptr-nil)`), `locations[0].physicalLocation` with **relative** URI +
  `region` (line, col), one `codeFlow` lowered from `trace`,
  `partialFingerprints: {"goverify/v1": fp}`.
- **No timestamps, no absolute paths, no invocation environment** —
  determinism is the root invariant and SARIF's optional provenance
  fields all violate it.
- Suppressed-by-baseline results are **omitted** (not emitted with
  SARIF `suppressions`); the suppressed count goes in
  `run.properties`.

**Native JSON**:

```json
{
  "schema_version": 1,
  "findings": [
    {"fingerprint": "...", "checker": "...", "tag": "...",
     "func": "...", "file": "...", "line": 0, "col": 0,
     "message": "...", "trace": [{"file": "...", "line": 0}],
     "model": [["p0", "(ptr-nil)"]]}
  ],
  "summary": {"total": 0, "suppressed_by_baseline": 0,
              "diff_base_scoped": false}
}
```

Serialization via serde derive structs — field order fixed by
declaration, escaping owned by serde_json. The human renderer's
`sanitize` stays human-path-only (terminal escape hygiene); machine
formats rely on JSON string escaping and carry raw field values.

## 4. Baselines (`goverify-cli/src/baseline.rs`)

**Writing** — new subcommand:

```
goverify baseline write [same args as check]
```

Runs the identical pipeline (extract → analyze → scope), then writes
`.goverify/baseline.json` in the checked module's root:

```json
{
  "schema_version": 1,
  "entries": [
    {"fingerprint": "v1:...", "checker": "...", "tag": "...",
     "func": "...", "message": "..."}
  ]
}
```

Entries sorted by fingerprint; output deterministic. The readable
fields exist for humans reviewing baseline diffs; **matching uses the
fingerprint only**.

**Applying** — on every `check`:

- If `.goverify/baseline.json` exists at the module root, load it and
  drop findings whose fingerprint appears in it. `--baseline <path>`
  overrides the location; `--no-baseline` disables.
- Filtering runs after scope (and after diff-base scoping, §5), before
  render. **The exit code gates on the post-filter count**: all
  findings suppressed → exit 0.
- The human footer and JSON `summary.suppressed_by_baseline` report
  the count; SARIF puts it in `run.properties`.

**Error handling** — a deliberate, documented exception to
degrade-never-die: a malformed or unparseable baseline file is a
**hard actionable error, exit 2** (naming the path and the parse
failure). Rationale: it is user-authored gate configuration, not
analyzed code; silently proceeding unfiltered would flood CI with
hundreds of pre-existing findings and misreport the gate. The parser
is reject-never-panic (serde_json + schema/version validation) and
gets a fuzz target (§6).

Unknown `schema_version` or unknown fingerprint-scheme prefix → same
hard error, naming both versions (parent spec §11 style). Baseline
entries that no longer match any finding are ignored silently this
wave (prune surface queued, §8).

## 5. `check --diff-base <git-ref>` (`goverify-cli/src/diff.rs`)

Analyze the full closure as usual; scope the *report* to functions
changed since `<ref>`, plus their transitive callers (their
obligations depend on changed summaries).

Mechanics:

1. `git worktree add --detach <tmpdir> <ref>` — shell out to the `git`
   CLI (no libgit2 dependency; git is required only when the flag is
   used). Missing git binary, unknown ref, or a dirty failure →
   actionable error naming the ref, exit 2.
2. Extract the base worktree through the normal extraction path. The
   phase-5a extract cache applies (content-keyed, so the base tree's
   packages cache like any other); repeat `--diff-base` invocations
   against the same ref are warm.
3. Compare per-function content hashes between base and current
   extraction — the same function-IR hashes the scc cache layer
   already computes, exposed via a small read-only API from
   `goverify-analysis`. **Changed set** = functions whose hash differs
   or which exist only in the current tree. (Functions deleted at HEAD
   have no current findings; nothing to report.)
4. **Report set** = changed set ∪ transitive callers of the changed
   set, via a new reverse-reachability helper on `goverify-ir`'s call
   graph (the crate that owns the graph per ARCHITECTURE.md).
5. A finding survives iff its `func` is in the report set. Composes
   with `--scope` and the baseline: all three filters apply, in order
   scope → diff-base → baseline. Exit code gates post-filter.
6. The worktree is removed on **all** exit paths (`git worktree remove
   --force`, best-effort `git worktree prune` on failure); the tmpdir
   lives under the standard temp root, never inside the repo.

Failure to extract the base tree (e.g. the base ref does not build)
is an actionable error, exit 2, naming the ref — not a silent
fall-back to reporting everything (wrong output set contradicts the
explicit request; noisy-but-honest beats quietly-wrong).

Determinism: same repo state + same ref → byte-identical output; the
report set is derived from content hashes and the sorted call graph,
never from filesystem order or diff textual layout.

## 6. Testing

- **Corpus determinism suite** extended: `--format sarif` and
  `--format json` byte-identical across repeat runs; golden files for
  SARIF, JSON, and a written baseline on corpus fixtures.
- **Fuzz target** (new, `fuzz/`): the baseline parser —
  reject-never-panic over arbitrary bytes. Joins the nightly rotation
  with the same timeout-budget discipline as phase 5a's fifth target.
- **Integration** (debug_integration.rs style):
  - baseline round-trip: write → re-check suppresses all → exit 0;
    inject a new finding → only it surfaces, exit 1.
  - ordinal behavior: two identical-shape findings in one function get
    distinct fingerprints; baselining one leaves the other reported.
  - malformed baseline → exit 2 with the path in the message.
  - diff-base: a two-commit temp git repo fixture — changed function's
    findings and its transitive caller's findings reported, untouched
    function's findings filtered; unknown ref → exit 2.
- CLI tests reuse the shared-sidecar pattern (one sidecar build per
  test binary — the per-run rebuild is a known EDR stall hazard on
  this machine, phase-5a follow-up queue).
- **Shakeout** (manual, not blocking): bbolt `baseline write` →
  `check` exits 0 with 457 suppressed; SARIF output golden-diffed
  between two warm runs.

## 7. Acceptance gates (vs the 457-finding bbolt baseline)

- **G1** — human path unchanged: `check` (no new flags) renders the
  identical 457 findings, byte-identical to pre-wave output.
- **G2** — machine-format determinism: bbolt `--format sarif` and
  `--format json` byte-identical across cold+warm repeat runs.
- **G3** — baseline exactness: after `baseline write`, `check` exits 0
  with exactly 457 suppressed; removing one entry from the file
  resurfaces exactly that finding.
- **G4** — diff-base exactness on the two-commit fixture: report set
  is exactly {changed function ∪ its transitive callers}; a whitespace
  /comment-only edit at the base produces an **empty** report set
  (hash-comparison semantics, the reason §5 chose extraction over
  line ranges).
- **G5** — timing, report-only: warm bbolt `--diff-base HEAD` wall
  time recorded (expected ≈ one extra warm extraction pass).

## 8. Non-goals (stay queued)

- Baseline pruning / stale-entry reporting (`baseline prune`).
- SARIF `suppressions` objects (results are omitted instead).
- The phase-5a follow-up queue (TOCTOU re-hash guard, cache
  GC/eviction, cached-panic replay policy, canonicalization minors,
  generics §16 fixture) — unchanged by this wave.
- `goverify spec show` (parent spec §10) — separate feature, not CI
  surface.
- Windows: `--diff-base` shells out to git and uses POSIX temp/permission
  idioms consistent with the rest of the CLI; portability is out of
  scope wave-wide.
