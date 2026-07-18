# goverify

An SMT-backed static analyzer for Go, in the spirit of
[Infer](https://fbinfer.com/): bottom-up compositional function
summaries, constraints discharged with Z3, aggressive content-addressed
caching. Bug-finder first — high-confidence reports, false positives
are the enemy.

**Status:** early development. Phases 1-4 of the
[design](docs/superpowers/specs/2026-07-16-goverify-design.md) are
implemented: extraction pipeline, IR/call-graph/analysis engine, the Z3
solver layer, and the nil + bounds checkers behind `goverify check`.
Caching's full stack (summary/extraction caches, baselines, SARIF) and
the concurrency checkers land in later phases.

## Quickstart

Requires [mise](https://mise.jdx.dev). Everything else is pinned.

```sh
mise install          # pinned Rust, Go, protoc, buf, gitleaks, …
mise run setup        # one-time: git hooks (secret scan on commit)
mise run build
mise run test
```

## Checking a module

Build the binary once from this checkout, then run it from *inside*
the target Go module — `check` resolves patterns in the current
directory and shells out to `go` and the extractor sidecar at runtime,
so `go` must be on `PATH`:

```sh
# from this checkout:
mise run build   # or: cargo build -p goverify-cli

# from the target Go module:
cd /path/to/some/go/module
/path/to/goverify/target/debug/goverify check ./...
```

Exit codes: 0 clean, 1 findings, 2 analyzer error. Findings render as
labeled source spans with the violating path and the callee requirement
that fired. `--solver-timeout-ms`/`--obligation-timeout-ms` tune the
per-query budgets (timeouts suppress reports, never invent them).

The first `cargo build` compiles a statically-linked Z3 (~20 minutes,
one-time, cached afterwards).

Extract `.gvir` IR artifacts from a Go module directly (developer
command; same directory/`PATH` requirements as `check` above):

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

### Findings (single-checker debug tracer)

Lower-level than `check`: runs only the nil checker, against `debug`'s
gvir-dir/temp-extract conventions rather than `check`'s own flags.

```sh
goverify debug findings            # analyze CWD, print nil-tracer findings
goverify debug findings --emit-smt /tmp/smt   # dump canonical SMT-LIB2 artifacts
goverify debug findings --solver-cmd z3       # portable backend instead of built-in Z3
```

## Development

Named tasks (run `mise tasks` for the full list): `build`, `test`,
`lint`, `fmt`, `corpus` (full extractor pipeline + determinism suite),
`bench`, `audit`, `secrets`, `fuzz`, `proto-gen`, `shakeout` (manual —
`check` run over pinned bbolt).

Corpus expectations live as `// want: <tag>` comments on the annotated
line (`testdata/corpus/*`), checked by the `corpus` task's checker
suites — tags are `nil-deref`, `bounds`, `div-zero`, `overflow`.

- [ARCHITECTURE.md](ARCHITECTURE.md) — crate boundaries and why
- [docs/threat-model.md](docs/threat-model.md) — security stance
- [AGENTS.md](AGENTS.md) — front door for AI coding agents

## License

Apache-2.0 — see [LICENSE](LICENSE).
