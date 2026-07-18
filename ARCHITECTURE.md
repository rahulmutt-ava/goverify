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
| `goverify-checkers` | `Checker` trait consumers: currently the nil tracer (entry-block requires + const-nil call obligations); phase 4 expands to bounds and the full nil/bounds checkers | engine machinery, solver details |
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
