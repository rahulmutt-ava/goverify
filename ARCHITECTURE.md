# Architecture

Why the boundaries are where they are. The file tree is discoverable;
this documents intent. Full design: the
[spec](docs/superpowers/specs/2026-07-16-goverify-design.md).

## Pipeline

```
go source ─▶ extractor (Go sidecar) ─▶ .gvir (protobuf, per package)
                  │
                  ▼
Rust core: IR load ─▶ call-graph SCC order ─▶ per-function analysis
                                               ├─ cheap pre-pass (no SMT)
                                               └─ constraints ─▶ Solver ─▶ Z3
                                                     ▼
                                        summaries (.gvspec, per package)
                                                     ▼
                                        findings ─▶ terminal / SARIF / JSON
```

## Crates

| Crate | Owns | Deliberately does NOT own |
|---|---|---|
| `goverify-cli` | argument parsing, orchestration, rendering | any analysis logic |
| `goverify-extract` | sidecar driving, `.gvir` schema + loader | interpretation of the IR |
| `goverify-ir` | analyzer-owned SSA-style IR lowered from `.gvir`, the whole-program call graph (static/interface/function-value edges), SCC condensation for scheduling | x/tools SSA quirks (isolated at the extractor boundary) |
| `goverify-analysis` | the SCC-ordered engine: concurrency effects, the syntactic pre-pass (no SMT), summary instantiation over placeholder clauses | what to check (checkers plug in) |
| `goverify-checkers` | `Checker` trait consumers: `NilChecker` (tag `nil-deref`) and `BoundsChecker` (tags `bounds`, `div-zero`, `overflow`) | engine machinery, solver details |
| `goverify-solver` | typed QF term language (Bool, BV, Array, Ptr-ADT); canonical SMT-LIB2 printer is the single lowering — cache keys and artifacts are byte-identical to solved queries; `Z3Native` (statically-linked bundled Z3 4.16.0) and `SmtLib2Process` (external binary) backends both consume printer bytes; differential harness guards agreement | summary semantics |
| `goverify-spec` | summary/annotation format: parse, serialize, validate | inference |
| `goverify-cache` | content-addressed store (blake3, atomic rename, advisory lock, corrupt=miss); query layer keyed on canonical SMT text ⊕ solver identity ⊕ limits; extraction/summary caching layers arrive in phase 5 | what the bytes mean |

Checkers depend on `goverify-analysis` + `goverify-solver` +
`goverify-spec` and nothing else — that boundary is what keeps checker
versioning able to invalidate only its own cached results.

## The Go sidecar (`extractor/`)

The one non-Rust component — the same trade Infer makes reusing
clang/javac: only the Go toolchain can type-check Go reliably. It is
content-hashed and built on demand by `goverify-extract`; its output is
canonicalized so identical source bytes give byte-identical `.gvir`.
That determinism is the root of the entire caching design.

## Gated-SSA encoding (`goverify-analysis::encode`)

Each function is encoded once, whole-body, into a single SMT query
(design spec §3): every block gets a boolean guard term (`g<b>`, entry
`g0` asserted true); phi nodes lower to `ite` over the guards of their
incoming edges instead of per-path duplication. Back edges — successors
already on the DFS stack from the entry block — are cut before guards
are built, so a loop is encoded as its first iteration only:
reachability past a loop stays under-approximate, but any `Sat` result
still witnesses a genuine execution (nothing is invented to compensate
for what got cut). A function whose encoding exceeds the assertion cap
is skipped with a diagnostic rather than handed to the solver — degrade,
never die.

Slices and strings encode as the `GoSeq` datatype — named that and not
`Seq` because Z3's builtin sequence theory pre-claims `Seq` under
`(set-logic ALL)`; the collision doesn't error loudly, it silently
downgrades every query touching a slice/string value to `Unknown`.
`GoSeq` carries only `seq-len`/`seq-cap` (contents havoc). The
`0 <= len <= cap` invariant is asserted on `Make{Slice}` destinations
(it's `make`'s own runtime precondition, so asserting it is sound) but
deliberately NOT on `Op::Slice` destinations — a slice's derived
len/cap can legitimately violate the invariant on an out-of-bounds
slice, and asserting it there would make the whole function's query
spuriously Unsat, silently swallowing every real finding in it.

## Checkers (`goverify-checkers`)

Two checkers ship: `NilChecker` (`nil-deref`) and `BoundsChecker`
(`bounds`, `div-zero`, `overflow` — index/slice bounds, div/rem-by-zero,
narrowing or sign-changing `Convert`s). Both implement the `Checker`
trait (`goverify-analysis::checker`):

- `infer_requires` derives a function's own preconditions from its
  body during the engine's existing SCC fixpoint. A clause is only
  ever emitted once the solver confirms `Sat` on the corresponding
  violation query — `Unknown` (including timeouts) stays silent, same
  as findings. `summary_of` lets a checker read a callee's
  already-inferred requires while deriving its own, so requires
  propagate through call chains for free as the fixpoint settles.
- `obligations` raises candidate precondition violations at call
  sites, each checked against the callee's own inferred requires (an
  obligation is only ever discharged under the precondition the
  callee actually has, never a different one).
- A candidate becomes a `Finding` only when its query comes back `Sat`
  during the sequential findings pass; Unsat/Unknown stay silent
  (bug-finder invariant: false positives are the enemy).
- `Finding.trace`/`.model` are reconstructed from that same `Sat`
  model and are display-only: `trace` walks the cut DAG from the entry
  block along blocks whose guard the model set true (entry-to-somewhere,
  not a full CFG path); `model` surfaces only `p<i>` param bindings.
  Neither is consulted for verdicts, and both come back empty on any
  reconstruction failure — a finding is never dropped for lack of a
  trace.
