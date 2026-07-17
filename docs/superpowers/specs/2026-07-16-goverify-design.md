# goverify — SMT-backed static analyzer for Go

**Date:** 2026-07-16
**Status:** Approved design
**Repo:** `goverify` (fresh; Rust workspace)

## 1. Goal and philosophy

`goverify` is an Infer-inspired static analyzer for Go. It discovers bugs by
generating SMT constraints from Go code and discharging them with Z3, using
bottom-up compositional function summaries so that whole dependency trees are
analyzed once, cached aggressively, and instantiated cheaply at call sites.

Decided priorities:

- **Bug-finder first.** Under-approximate; report only high-confidence issues;
  tolerate missed bugs. False positives are the enemy. Proof/verification
  capability is a growth path enabled by the SMT-native summary format, not a
  v1 requirement.
- **Blazing fast via aggressive caching.** All artifacts are deterministic and
  content-addressed. Warm incremental runs on a large repo (avalanchego-scale)
  after a one-function edit: **< 5 seconds** target. Cold runs: minutes,
  parallelized per package.
- **Rust core.** The entire analyzer is Rust. The only Go code is a small
  vendored extractor sidecar (see §3) — the same trade Infer makes by reusing
  clang/javac frontends.
- **Standard-format constraints.** Every solver query has a canonical SMT-LIB2
  rendering; that text is the deterministic artifact and the cache key. The
  solver backend is a trait; v1 specializes for Z3 via the C API.
- **CLI and CI are co-equal v1 targets.** Human terminal output, SARIF/JSON,
  baselines, and diff-aware reporting all ship in v1.

### v1 checkers (phased within v1)

1. **Nil-safety** — nil pointer/interface/map/slice dereference (phase A)
2. **Integer overflow / index bounds** (phase A — shares value machinery)
3. **Goroutine leaks** — goroutines blocked forever on channel/WaitGroup ops (phase B)
4. **Data races** — RacerD-style lock-set analysis (phase C)

### Non-goals for v1

- Soundness guarantees / "verified" badges
- Full heap separation logic (biabduction); the summary language covers
  interface-level heap facts only
- IDE/LSP integration
- Windows support (macOS + Linux first)

## 2. Architecture overview

```
go source ──▶ extractor (Go sidecar) ──▶ .gvir artifacts (protobuf, per package)
                                              │
        ┌─────────────────────────────────────┘
        ▼
   Rust core: IR load ──▶ call-graph SCC order ──▶ per-function analysis
                                                    ├─ cheap pre-pass (no SMT)
                                                    └─ constraint gen ──▶ Solver trait ──▶ Z3
                                                          │
                                                          ▼
                                              summaries (.gvspec, per package)
                                                          │
                                                          ▼
                                              findings ──▶ terminal / SARIF / JSON
```

**Engine architecture: compositional summaries + cheap pre-pass.** Functions
are analyzed bottom-up in call-graph dependency order, each exactly once,
against symbolic inputs, producing a summary. Call sites instantiate callee
summaries instead of re-analyzing. A fast, non-SMT intraprocedural pre-pass
classifies trivially-clean functions (no derefs of possibly-nil values, no
unchecked indexing, no concurrency ops) and skips constraint generation for
them entirely — most functions in real code are trivially clean, and skipping
them is a large part of the speed budget.

### Cargo workspace crates

| Crate | Purpose |
|---|---|
| `goverify-cli` | argument parsing, orchestration, output rendering |
| `goverify-extract` | drives the Go sidecar; protobuf schema; loads `.gvir` |
| `goverify-ir` | analyzer-owned SSA-style IR + call graph |
| `goverify-analysis` | engine: SCC scheduler, pre-pass, summary instantiation, fixpoint |
| `goverify-checkers` | nil, bounds, leaks, races — plugins over the engine |
| `goverify-solver` | `Solver` trait; Z3 backend; canonical SMT-LIB2 printer; subprocess backend |
| `goverify-spec` | summary/annotation format: parse, serialize, validate |
| `goverify-cache` | content-addressed store, invalidation, locking |

Each crate has one purpose and a narrow public API; checkers depend on
`goverify-analysis` + `goverify-solver` + `goverify-spec` and nothing else.

## 3. Go extractor and `.gvir` artifacts

A small Go program (vendored in `extractor/`, the only non-Rust code; built on
demand and itself content-hashed) uses `go/packages` with full type
information plus `golang.org/x/tools/go/ssa` to emit one `.gvir` protobuf per
package containing:

- SSA functions (instructions, basic blocks, types)
- The package's type table and method sets
- Position tables (file/line/col for every instruction)
- Comment pragmas (`//goverify:...`) attached to declarations

**Determinism:** output is canonicalized — sorted maps, stable IDs derived
from package import path + object name, no timestamps or absolute paths — so
identical source bytes always produce byte-identical `.gvir`. This property is
tested in CI (§12) and is the root of the caching design (§9).

**Dependency traversal:** the extractor resolves the import DAG via
`go/packages`; every dependency package (module cache and stdlib included)
flows through the same extraction → analysis → summary pipeline. A dependency
whose content hash is already in the cache is never re-extracted or
re-analyzed.

**Version coupling:** `.gvir` embeds the Go toolchain version and extractor
schema version; mismatches are cache misses, and an extractor that cannot
handle the local Go version fails with an actionable error.

## 4. Rust IR

`goverify-ir` lowers Go SSA into a smaller analyzer-owned instruction set
(~25 ops): assign, field-addr, index-addr, load, store, call, phi, return,
jump/branch, make (chan/map/slice), send, recv, close, go, defer, panic,
type-assert, and lock/unlock recognized as intrinsics on `sync` types.
Lowering isolates checkers from `x/tools` SSA quirks and gives one stable
surface designed around what the checkers need: explicit nil-ness, bounds,
and concurrency effects.

The call graph is built over the IR (static calls precise; dynamic
interface/function-value calls resolved by implements-based approximation),
then condensed into SCCs. SCC condensation order is both the analysis
schedule and the parallelism structure (rayon over independent SCCs).
Recursive SCCs are handled with a bounded fixpoint over summaries: start
optimistic, iterate to stability, widen after k rounds (k configurable,
default 3).

## 5. Summaries (`.gvspec`)

One spec file per package; one summary per function. A summary is a set of
clauses over the function's symbolic interface (params, receiver, results,
reachable heap roots, effects):

- **Requires** — conditions the caller must establish (`arg0 != nil`), each
  tagged with the violation consequence (which checker fires, at which
  source position).
- **Ensures** — guarantees on return, keyed by path condition
  (`err == nil ⟹ ret0 != nil`). The conditional form is required for
  idiomatic Go error handling.
- **Effects** — concurrency facts: locks acquired/released, channel
  send/recv/close operations, goroutines spawned (and whether their lifetime
  is bounded), shared-location accesses with the lock-set held at access.
- **Provenance** — `inferred` | `annotated` | `builtin-model`, plus the
  content hash of the source the summary was derived from.

Clause formulas use an analyzer-owned typed first-order term language
(booleans, bitvectors, arrays, algebraic datatypes) that serializes to both
protobuf (cache) and SMT-LIB2 (solver).

**Builtin models:** hand-written `.gvspec` files for stdlib/runtime
(`make`, `append`, `copy`, `sync.Mutex`/`RWMutex`/`WaitGroup`, channel
operations, `reflect` and cgo boundaries modeled as havoc-with-declared-
effects) are embedded in the binary and versioned with it.

## 6. Annotation language (v1)

Comment pragmas, parsed by the extractor, type-checked and lowered by
`goverify-spec`:

```go
//goverify:requires p != nil && n >= 0
//goverify:ensures err == nil ==> ret != nil
//goverify:effects spawns-bounded, locks(mu)
//goverify:pure
//goverify:ignore nilcheck   // suppression; must name the checker
```

- Expressions are a **subset of Go syntax**: parameter/result/receiver names,
  field selection, comparisons, boolean operators, `len`, `cap`, integer
  literals, `==>` for implication, `old(expr)` for entry-state values.
- **Annotations never silently silence analysis.** An annotated `requires` is
  checked at every call site. An annotated `ensures` is verified against the
  body best-effort; if the engine cannot prove it, that is itself a finding
  (`unverified-annotation`, warning severity, promotable to error in CI
  config).
- An annotation that fails to parse or references unknown names is an
  **error finding** at the annotation site — a silently-ignored annotation is
  worse than none.
- **Third-party overrides:** repos may provide external spec files at
  `goverify/overrides/<import/path>.gvspec` to annotate dependencies whose
  source cannot carry pragmas. Override clauses have `annotated` provenance
  and are trusted the same way (call-site-checked requires; body-verified
  ensures when source is available).

## 7. Checkers

Each checker contributes: (a) the IR facts it consumes, (b) constraint
schemas, (c) summary clause kinds it reads/writes. Checkers are versioned
independently; a checker version bump invalidates only its own cached
results.

**Phase A — Nil-safety and bounds/overflow.** Pure value-flow analyses over
the shared symbolic-value machinery. Every dereference, index, slice
operation, division, and narrowing conversion emits a proof obligation;
obligations are discharged per-path with Z3 (bitvector arithmetic for
overflow; array theory for slices/maps at the interface level). Report only
when Z3 finds the violating path **satisfiable** under the function's
inferred/declared preconditions (bug-finder mode: sat = report, unsat = safe,
unknown/timeout = no report).

**Phase B — Goroutine leaks.** Built on Effects clauses. A spawned goroutine
whose blocking operations (channel send/recv on specific channels, WaitGroup
waits) cannot all be matched by obligations discharged in the spawning
environment is flagged. Channel capacity and close events are modeled as
counters/flags in SMT. Scope limit for v1: channels that stay within a
function/package boundary or flow through summarized effects; channels
escaping into arbitrary heap structures degrade to no-report.

**Phase C — Data races.** RacerD-style lock-set analysis over Effects: a
shared location accessed from ≥2 goroutines where the lock-set intersection
is empty and at least one access is a write. Primarily relational reasoning
in Rust, not SMT; Z3 is consulted only to prune infeasible access pairs
(contradictory path conditions). Same no-report-on-uncertainty stance.

## 8. Solver layer

```rust
trait Solver {
    fn declare(&mut self, sort_or_fun: Decl);
    fn assert(&mut self, term: Term);
    fn check_sat_assuming(&mut self, assumptions: &[Term]) -> SatResult; // Sat/Unsat/Unknown
    fn model(&self) -> Option<Model>;
    fn push(&mut self); fn pop(&mut self);
}
```

- **`Z3Native`** — via `z3-sys`/C API; incremental (push/pop); one solver
  instance per rayon worker; hard per-query timeout (default 100 ms,
  configurable) and memory cap.
- **`SmtLib2Process`** — pipes canonical SMT-LIB2 to any solver binary
  (`--solver-cmd`); used for portability and differential testing.
- **Canonical SMT-LIB2 printer:** sorted declarations, stable symbol names
  derived from IR IDs, fixed logic string per query class. The hash of the
  canonical text is the query-cache key; `--emit-smt <dir>` dumps the texts
  as artifacts.
- **Timeout semantics:** for bug-finding obligations, timeout/unknown ⇒ do
  not report. Timeouts must never create false positives.

## 9. Caching and determinism

Content-addressed store, default `~/.cache/goverify/`, overridable;
`.goverify/cache` project-local mode for hermetic CI. Three layers:

1. **Extraction cache** — key: `hash(source files ⊕ Go version ⊕ extractor
   version)` → `.gvir`. Stdlib and dependencies hit this once per version.
2. **Summary cache** — key: `hash(function .gvir ⊕ hashes of all consumed
   callee summaries ⊕ checker versions ⊕ applicable annotations/overrides)`
   → `.gvspec` entries. The recursive keying yields correct transitive
   invalidation: editing one function re-analyzes only its callers upward
   through the SCC DAG.
3. **Query cache** — key: `hash(canonical SMT-LIB2 ⊕ solver identity ⊕ solver
   version ⊕ timeout/memory limits)` → sat/unsat/unknown. Solver identity is
   part of the key because unknown/timeout outcomes are solver-build-dependent.
   Survives summary invalidation when identical obligations reappear.

All cache values are deterministic functions of their keys; the store is
safe to share between machines (e.g., as a CI cache artifact). Concurrent
runs use advisory file locking; corrupt or truncated entries are treated as
misses and rebuilt.

## 10. CLI and CI surface

One binary, `goverify`:

- `goverify check ./...` — human terminal output: findings rendered as
  labeled source spans with the violating path shown as a step-by-step trace
  (Infer-style), including the callee summary clauses that fired.
- `goverify check --format sarif|json ./...` — CI output; SARIF for GitHub
  code scanning.
- `goverify baseline write` — records current findings in
  `goverify/baseline.json`; later runs report only new findings.
  **Fingerprints are position-independent**: `hash(checker ⊕ stable function
  ID ⊕ obligation shape)`, so unrelated line shifts don't churn the baseline.
- `goverify check --diff-base <git-ref>` — analyze everything (cache makes
  this cheap) but report only findings attributable to functions changed
  since `<ref>` or their transitive callers.
- `goverify spec show <pkg>` — pretty-print inferred/annotated summaries.
- `--emit-smt <dir>` — dump canonical SMT-LIB2 query artifacts.
- Exit codes: 0 = clean, 1 = findings, 2 = analyzer error.

## 11. Error handling

Degrade, never die:

- Unparseable/untypeable package → skip with diagnostic; analysis continues;
  callers of skipped packages treat their functions as havoc-with-no-requires
  (no false positives from missing info).
- Extractor/Go-version mismatch → actionable error naming both versions.
- Solver crash → worker restarted; in-flight obligation counts as timeout.
- Corrupt cache entry → cache miss, rebuilt.
- Invalid annotation → error finding at the annotation site.
- Checker panic → caught per-function (`catch_unwind`), surfaced as an
  internal diagnostic; other findings unaffected.

## 12. Testing strategy

Validation forms, cheapest layer first:

1. **Static checks** (base of the pyramid): `rustfmt --check` and
   `clippy -D warnings` on every PR; format-on-save locally.
2. **Unit tests** per crate (term language, canonical printer, cache keys,
   annotation parser).
3. **Property tests** (proptest) for invariants: term-language →
   canonical-SMT-LIB2 → parse round-trips; cache-key stability under
   input reordering; fingerprint position-independence.
4. **Fuzz targets** (cargo-fuzz) for everything that parses bytes it didn't
   write: the annotation-expression parser, `.gvir` and `.gvspec` decoders,
   and the SMT-LIB2 response reader. Parsers must reject, never panic.
5. **Corpus tests** (the workhorse): a tree of small Go files under
   `testdata/corpus/` with `// want nilcheck "..."` expectation comments,
   run through the full extractor→Z3 pipeline. Every checker feature and
   every fixed false positive gets a corpus case.
6. **Determinism tests**: (a) analyze twice from clean, byte-compare all
   `.gvir`/`.gvspec`/SMT artifacts; (b) cold run vs. incremental run after a
   scripted edit, compare finding sets.
7. **Benchmark suite** against pinned real-world repos (first: avalanchego)
   tracking wall-clock (cold/warm), finding drift, and triaged FP rate.
8. **Differential solver tests**: Z3Native vs. SmtLib2Process(z3) must agree
   on the corpus — guards the canonical printer.

**Speed tiering.** Blocking PR tier (wall-clock budget: 10 minutes): static
checks, unit, property (bounded case counts), corpus, and the small
determinism suite. Nightly tier: fuzz campaigns, the avalanchego benchmark,
full differential-solver sweep, and mutation testing (cargo-mutants) as a
suite-strength audit. Flaky tests are quarantined out of the blocking tier
immediately, then fixed or deleted.

## 13. Toolchain and developer environment

- **Tool provisioning is mise-first, exact-pinned** (`mise use --pin`),
  committed in `mise.toml`: Rust toolchain, Go toolchain (for the extractor
  and corpus tests), protoc/buf, and dev CLIs (cargo-fuzz, cargo-mutants).
  devenv.nix is the fallback only for what mise cannot supply.
- **Z3 is pinned like everything else.** Preferred: static-link a pinned Z3
  via `z3-sys`'s bundled build so the analyzer binary carries its solver
  version (best for determinism and distribution); the pinned version string
  feeds the query-cache key (§9). If build times force dynamic linking,
  libz3 is provisioned pinned via mise/devenv.nix — never "whatever is on
  the system."
- **`Cargo.lock` is committed.** Dependencies are kept deliberately few
  (every line is a liability, supply-chain edition): prost, rayon, z3-sys,
  a diagnostics renderer, clap, blake3 — additions weigh their transitive
  cost. Renovate/Dependabot updates land on a cadence, gated by CI.
- **Native build tooling: cargo (+ `go build` for the vendored extractor),
  driven by mise tasks.** No Bazel — single-language-dominant repo, no
  remote-execution need; the determinism requirements are met by the
  analyzer's own content-addressed artifacts, not by the build system.

## 14. Security

**Threat model** — committed as `docs/threat-model.md` in phase 1 and linked
from the agent-instruction file. v1 stance, explicit:

- **Analyzed source is semi-trusted.** The extractor invokes the Go
  toolchain (`go/packages`) against the target repo; running `goverify`
  against a hostile repo is equivalent to running `go build` there. This is
  documented, not defended, in v1 (same stance as gopls/staticcheck).
- **The cache is a trust boundary when shared.** Content addressing detects
  corruption, not tampering — a writer can store a wrong value under a
  correct key. Shared caches are therefore only consumed from
  writer-trusted storage (e.g. the project's own CI cache); no
  public/community cache without signing (out of scope for v1).
- **`--solver-cmd` executes a user-supplied binary** — by design; noted so
  CI configs treat it like any other executable input.
- **Untrusted-bytes surfaces** (`.gvir`, `.gvspec`, annotation expressions,
  solver output) are parsed by fuzz-hardened decoders (§12) that reject
  malformed input without panicking.

**Repo hygiene (CI):** gitleaks in pre-commit and CI (secret scanning);
cargo-audit/osv-scanner as a CI gate plus a scheduled run (new CVEs land on
old code); clippy's correctness/security lints in the blocking tier. No
container/IaC artifacts exist, so no scanner for them until they do.

## 15. Sub-project phasing

Each phase is its own plan → implement → review cycle:

1. **Skeleton + extractor** — workspace, `mise.toml` (pinned toolchain,
   named tasks: build/test/lint/corpus/bench), protobuf schema, Go sidecar,
   `.gvir` determinism tests, CI with the blocking tier from §12, and the
   discoverability surface: README quickstart, agent-instruction file
   (AGENTS.md/CLAUDE.md pointing at task names and the threat model — not
   duplicating them), `ARCHITECTURE.md` seeded from §2's crate map (the
   *why* of the boundaries, not the file tree), and `docs/threat-model.md`.
2. **IR + engine core** — lowering, call graph/SCC scheduler, pre-pass,
   summary plumbing (stub solver).
3. **Solver layer** — term language, Z3 backend, canonical SMT-LIB2 printer,
   query cache, differential harness.
4. **Nil-safety + bounds checkers** — phase-A checkers, terminal output,
   corpus harness.
5. **Caching full stack** — summary/extraction caches, baselines, SARIF,
   diff-base; the speed milestone (< 5 s warm target measured here).
6. **Annotation language** — pragmas, override specs, unverified-annotation
   findings.
7. **Concurrency checkers** — goroutine leaks (phase B), then data races
   (phase C).

## 16. Open questions (deferred, not blocking)

- Dynamic dispatch precision: implements-based call-graph approximation may
  need call-site type narrowing if FP/FN rates warrant (revisit after
  phase 4 benchmarks).
- Summary-cache sharing across machines with different Go minor versions —
  currently a full miss by design; revisit if CI cost demands.
- Generics: v1 analyzes instantiated SSA (post-monomorphization as x/tools
  provides); per-instantiation summary blowup to be measured in phase 5.
