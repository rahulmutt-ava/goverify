# goverify

An SMT-backed static analyzer for Go, in the spirit of
[Infer](https://fbinfer.com/): bottom-up compositional function
summaries, constraints discharged with Z3, aggressive content-addressed
caching. Bug-finder first — high-confidence reports, false positives
are the enemy.

**Status:** early development. Phase 1 (extraction pipeline) and phase 2
(IR, call graph, and the analysis engine — pre-pass and summaries, no
real solver backend yet) of the
[design](docs/superpowers/specs/2026-07-16-goverify-design.md) are
implemented; checkers land in later phases.

## Quickstart

Requires [mise](https://mise.jdx.dev). Everything else is pinned.

```sh
mise install          # pinned Rust, Go, protoc, buf, gitleaks, …
mise run setup        # one-time: git hooks (secret scan on commit)
mise run build
mise run test
```

Extract `.gvir` IR artifacts from a Go module (developer command; the
`check` command arrives with the first checkers). Build the binary once
from this checkout, then run it from *inside* the target Go module — the
`extract` command resolves patterns in the current directory, and it
shells out to `go` and the extractor sidecar at runtime, so `go` must be
on `PATH`:

```sh
# from this checkout:
mise run build   # or: cargo build -p goverify-cli

# from the target Go module:
cd /path/to/some/go/module
/path/to/goverify/target/debug/goverify extract -o /tmp/gvir ./...
```

Inspect the analyzer's view of a module without writing `.gvir` files
yourself — `debug` extracts to a temp dir on the fly when `--gvir-dir` is
omitted (same directory/`PATH` requirements as `extract` above):

```sh
cd /path/to/some/go/module
/path/to/goverify/target/debug/goverify debug ir ./...
```

Other `debug` subcommands (`callgraph`, `sccs`, `prepass`, `summary`)
take the same arguments; `--func` filters by substring match on the
function's SSA id.

### Findings (phase-3 tracer)

```sh
goverify debug findings            # analyze CWD, print nil-tracer findings
goverify debug findings --emit-smt /tmp/smt   # dump canonical SMT-LIB2 artifacts
goverify debug findings --solver-cmd z3       # portable backend instead of built-in Z3
```

The first `cargo build` compiles a statically-linked Z3 (~20 minutes,
one-time, cached afterwards).

## Development

Named tasks (run `mise tasks` for the full list): `build`, `test`,
`lint`, `fmt`, `corpus` (full extractor pipeline + determinism suite),
`bench`, `audit`, `secrets`, `fuzz`, `proto-gen`.

- [ARCHITECTURE.md](ARCHITECTURE.md) — crate boundaries and why
- [docs/threat-model.md](docs/threat-model.md) — security stance
- [AGENTS.md](AGENTS.md) — front door for AI coding agents

## License

Apache-2.0 — see [LICENSE](LICENSE).
