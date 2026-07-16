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
| `goverify-ir` | analyzer-owned SSA-style IR, call graph | x/tools SSA quirks (isolated at the extractor boundary) |
| `goverify-analysis` | SCC scheduler, pre-pass, summary instantiation, fixpoint | what to check (checkers plug in) |
| `goverify-checkers` | nil, bounds, leaks, races | engine machinery, solver details |
| `goverify-solver` | `Solver` trait, Z3 backend, canonical SMT-LIB2 printer | summary semantics |
| `goverify-spec` | summary/annotation format: parse, serialize, validate | inference |
| `goverify-cache` | content-addressed store, invalidation, locking | what the bytes mean |

Checkers depend on `goverify-analysis` + `goverify-solver` +
`goverify-spec` and nothing else — that boundary is what keeps checker
versioning able to invalidate only its own cached results.

## The Go sidecar (`extractor/`)

The one non-Rust component — the same trade Infer makes reusing
clang/javac: only the Go toolchain can type-check Go reliably. It is
content-hashed and built on demand by `goverify-extract`; its output is
canonicalized so identical source bytes give byte-identical `.gvir`.
That determinism is the root of the entire caching design.
