# Phase 5a: extraction + analysis caching (the speed milestone)

Date: 2026-07-23
Status: approved (brainstorm 2026-07-23)
Parent spec: `2026-07-16-goverify-design.md` (§9 caching, §14 roadmap
item 5, §16 generics blowup)
Predecessor: `2026-07-22-followups-wave2-design.md`

## 1. Goal and scope

Roadmap phase 5 splits into two sub-projects. This spec is the first:
the **extraction cache** and the **analysis (summary+findings) cache**,
caching switched on by default, and the warm-run speed milestone.
The second sub-project — the reporting surface (SARIF/JSON, baselines,
`--diff-base`) — gets its own spec afterwards; it depends on stable
finding fingerprints, not on these caches.

Baseline: local `main` @ `606fa00`; bbolt shakeout at 457 findings
(458-header baseline file, tx.go:558:11 retry-discharged at HEAD).

Today `goverify check` re-runs the Go sidecar over the whole import
closure into a tempdir and re-analyzes every SCC on every invocation;
the only persistence is the opt-in query cache. Warm bbolt shakeout is
~26 s with that cache (cold ~207 s). The parent spec's promise —
"editing one function re-analyzes only its callers upward" and a < 5 s
warm target — needs the two missing layers.

**Decisions fixed by this brainstorm:**

- The < 5 s warm target is **report-only** this wave: measured against
  a phase breakdown, recorded in the shakeout addendum, gap queued if
  missed. It is not a merge gate (nobody has profiled the 26 s yet).
- Caching becomes **default-on** for `check` at
  `$XDG_CACHE_HOME/goverify` (fallback `~/.cache/goverify`), the same
  root and 0700 discipline the sidecar build dir already uses.
  `--cache-dir` overrides (hermetic project-local mode); `--no-cache`
  disables all layers including the query cache. `debug findings`
  keeps opt-in `--cache-dir` (a tracer wants fresh solves).
- Riders: the §16 generics-blowup measurement (report-only) and the
  wave-2 follow-up queue's three code items (§6 below). The
  SentinelOne EDR exclusion remains an IT ticket, not a wave task.

## 2. Cache architecture

The existing `Store` (blake3 keys, atomic tempfile+rename, advisory
per-layer lock, corrupt/truncated = miss) gains two layers beside
`query`. The store itself does not change.

| Layer | Unit | Key (all fields length-prefixed) | Value |
|---|---|---|---|
| `extract` | Go package | extractor content-hash ⊕ Go version ⊕ import path ⊕ sorted (file name, file content hash) pairs ⊕ sorted dep keys | the package's `.gvir` bytes |
| `scc` | call-graph SCC | encoder+checker version consts ⊕ solver identity ⊕ both timeout tiers ⊕ retry factor ⊕ sorted member function-IR hashes ⊕ sorted external-callee SCC keys | summaries + findings + diagnostics for all members (private versioned binary) |
| `query` | SMT query | unchanged | unchanged |

Both new layers use **recursive content keys over their DAG**: a
package's key includes its dependencies' keys (import DAG); an SCC's
key includes its callees' keys (condensed call DAG). Invalidation
therefore propagates upward automatically and exactly. This tightens
the parent spec's extraction key — `hash(source files ⊕ Go version ⊕
extractor version)` is only sound if "source files" is read
transitively, because a package's `.gvir` embeds resolved types from
its dependencies; the recursive key makes that explicit.

Every value is a pure function of its key, so the store remains safe
to share between machines (parent §9); no sharing mechanics ship this
wave (§8 non-goals).

Two structural consequences:

- **The SCC is the analysis-cache unit, not the function.** Within an
  SCC, summaries are a joint fixpoint — per-function entries would be
  circular. Singleton SCCs (the vast majority) degenerate to
  per-function caching, which is what the upward-invalidation promise
  needs.
- **Findings and diagnostics are part of the SCC entry value.** A hit
  skips the checkers entirely, so their findings must replay from the
  entry or warm runs would silently lose them. Root invariant: **cold
  and warm runs produce byte-identical stdout** (enforced by the
  determinism suite, §5). stderr (e.g. the escalation counter, which
  reads lower on warm runs) is not part of the invariant.

## 3. Extraction cache mechanics

The sidecar grows a **manifest mode**: `go list`-level enumeration of
the full import closure, no type-checking — per package: import path,
dependency import paths, source file list. Then the Rust side
(`goverify-extract` orchestrating, `goverify-cache` storing):

1. Hashes the listed files (blake3, local reads; the whole stdlib
   closure is well under a second) and computes recursive package keys.
2. Materializes cache **hits** into the gvir dir from the store.
3. Invokes the existing extract mode restricted to the **dirty package
   set** via a new package-filter argument. The dirty set is
   upward-closed by construction (a dirty dep dirties every dependent's
   key); Go's own build cache supplies dependency export data, so
   partial type-checking stays cheap.
4. Stores the freshly extracted `.gvir` blobs under their keys.

Cold behavior is unchanged (everything is dirty; stdlib and deps then
hit the cache once per Go/extractor version). The manifest pass is
additive: if it fails, degrade to today's full uncached extraction
with a diagnostic — degrade, never die.

## 4. Analysis cache mechanics

The engine's reverse-topological SCC walk (`goverify-analysis`) gains
a hook pair per SCC:

- **Before analysis:** compute the SCC key. Function-IR hashes are the
  blake3 of each function's serialized proto message bytes, recorded
  by `goverify-ir` at load; callee SCC keys already exist because the
  walk is reverse-topological. On a **hit**: decode the entry, install
  the members' summaries into engine state (upward callers instantiate
  them exactly as if freshly inferred), append the replayed
  findings/diagnostics, skip encoding and solving.
- **On a miss:** analyze as today, then encode
  `(summaries, findings, diagnostics)` for the SCC and `put` it.

The entry encoding is the one genuinely new serialization surface:
inferred requires/ensures clauses are **terms**, so the solver term
language needs a binary encode/decode (deliberately deferred in phase
3). Term encode/decode lives in `goverify-solver` (it owns the term
types); the SCC entry framing — versioning, findings/diagnostics
fields, layer plumbing — lives in `goverify-cache`, keeping the crate
boundary "cache owns bytes, not meaning" intact. The whole entry is a
private versioned format (version-byte bump = full miss), like
`CachedOutcome` — no protobuf changes; the human-readable `.gvspec` surface stays phase 6.
Findings serialize with full trace/model text so rendering replays
byte-identically.

Decoding is defensive throughout: any mismatch is a miss and the SCC
re-analyzes. The decoder parses bytes the current binary version did
not necessarily write — reject, never panic — and gets a fuzz target
in `fuzz/` beside `ir_encode`.

Version constants (encoder version, per-checker versions) are
hand-bumped; the documented invariant is "bump on any semantic
change". The cold/warm byte-identity gate is the tripwire if someone
forgets — a stale entry replays visibly different output.

The query cache is unchanged and still earns its keep: a dirty SCC's
re-analysis hits the query layer for unchanged obligations (parent
§9's "survives summary invalidation").

## 5. CLI plumbing, error handling, determinism suite

- `check` flag surface: default cache root as in §1; `--cache-dir`
  override; `--no-cache` (mutually exclusive with `--cache-dir`).
  The shakeout script keeps its explicit project-local `--cache-dir`.
- Error handling inherits store semantics: corrupt = miss; failed
  writes warn and continue; a read-only cache root degrades to
  effectively-uncached with warnings. A semantically stale cache
  (forgotten version bump) is caught by the byte-identity gate, not at
  runtime.
- **Determinism suite extension** (runs in `mise run corpus` / CI
  blocking tier): per corpus module, run `check` twice against a fresh
  cache dir — cold, then warm — require byte-identical stdout **and** a
  nonzero warm hit count (no vacuous pass by never caching). Plus a
  **targeted invalidation test**: edit one function in a fixture,
  assert exactly its SCC and upward callers re-analyze (observable via
  a debug counter or `--emit-smt` contents), and findings remain
  correct.

## 6. Riders

1. **Profiling probe (first plan task):** break a warm bbolt `check`
   into extract / IR-load / analyze+encode / solve / render wall-clock
   phases. This is the denominator for the speed milestone and decides
   where optimization attention goes if < 5 s is missed.
2. **Generics blowup measurement (§16 close-out, report-only):** with
   the `scc` layer populated, enumerate per-instantiation summary entry
   counts and sizes on bbolt + the corpus; record in the shakeout
   addendum. Scripted stats over the store, no CLI surface.
3. **Wave-2 follow-up queue (early hygiene tasks):**
   a. Convert-widened non-liftable overflow pin — closes the wave-2
      spec §4 item 2 narrow claim (Pin B used a global load, not a
      Convert-widened shape).
   b. `// want:` parser hardening — match the marker only in
      trailing-comment position (`testutil.rs:66` naive substring
      match; twice-bitten by prose comments).
   c. Lazy escalated-tier Z3 context — `RetryBackend` construction is
      eager for both tiers per backend build; defer the escalated
      context until first escalation (per-SCC double allocation).

## 7. Acceptance gates (shakeout-gated, vs the 458-header baseline)

- **G1 correctness:** cold-run findings identical to the current 457
  set — zero arrivals/departures attributable to cache code.
- **G2 replay fidelity:** one cold run then three warm runs; all three
  warm stdouts byte-identical to the cold run's.
- **G3 invalidation:** targeted-edit test passes — one function edited
  ⇒ only its SCC + upward callers re-analyze, findings correct.
- **G4 speed (report-only):** warm bbolt wall-clock + phase breakdown
  recorded; < 5 s is the milestone, not a merge gate. Cold-run
  overhead of the new layers (manifest + hashing + puts) recorded and
  must be small relative to the ~207 s cold baseline.
- **G5 blocking tier:** lint, test (incl. the extended determinism
  suite), corpus, secrets, audit all green; fuzz smoke on the new
  entry decoder.

## 8. Non-goals (stay queued)

- Cache eviction/GC — revisit once the generics measurement yields
  size data.
- Cross-machine cache distribution wiring (CI artifact upload etc.) —
  the format is shareable by construction; no mechanics this wave.
- SARIF / baselines / `--diff-base` — phase 5b, next spec.
- `.gvspec` human-readable summary format — phase 6 (annotations).
- Summary-cache sharing across Go minor versions — full miss by
  design (parent §16).
