# goverify

An SMT-backed static analyzer for Go, in the spirit of
[Infer](https://fbinfer.com/): bottom-up compositional function
summaries, constraints discharged with Z3, aggressive content-addressed
caching. Bug-finder first — high-confidence reports, false positives
are the enemy.

**Status:** early development. Phases 1-6 of the
[design](docs/superpowers/specs/2026-07-16-goverify-design.md) are
implemented: extraction pipeline, IR/call-graph/analysis engine, the Z3
solver layer, the nil + bounds checkers behind `goverify check`, the
summary/extraction caches, the CI-facing surface — SARIF/JSON output,
findings baselines (`goverify baseline write`), `--diff-base` PR-scoped
reporting — and the core `//goverify:` annotation language (see
[Annotations](#annotations) below). See
[docs/shakeout-phase5b-ci-surface.md](docs/shakeout-phase5b-ci-surface.md)
and
[docs/shakeout-phase6-annotations.md](docs/shakeout-phase6-annotations.md)
for the acceptance-gate results at bbolt scale. The concurrency
checkers land in later phases.

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

### CI usage

```sh
goverify check --format sarif ./...      # GitHub code scanning
goverify baseline write ./...            # adopt on an existing codebase
goverify check ./...                     # …now reports only new findings
goverify check --diff-base origin/main ./...   # PR-scoped report
```

`--format sarif`/`--format json` are deterministic byte-for-byte across
cold/warm cache runs and emit no absolute paths. `baseline write`
records the current finding set; a later `check` reports only findings
absent from the baseline (exit 0 once fully suppressed). `--diff-base
<rev>` restricts the report to functions changed since `<rev>`, or in
their transitive callers (comment-only edits report nothing).

## Annotations

`//goverify:` doc-comment pragmas on a function/method declaration let
you state a contract in source instead of relying on inference. Three
pragmas ship today (phase-6 spec §2):

```go
//goverify:requires p != nil && n >= 0
//goverify:ensures err == nil ==> ret != nil
//goverify:ignore nil-deref        // trailing // comment allowed
```

- **`requires`** is assumed at the function's own entry (kills in-body
  false positives the contract covers) and checked at every call site —
  a violating call reports a `contract` finding (error severity) at the
  *caller*, quoting the clause text.
- **`ensures`** always enters the function's summary (callers rely on
  it), and is verified best-effort against the body after the SCC
  fixpoint settles: proven → silent; unprovable or unknown →
  `unverified-annotation` (the only warning-severity finding). An
  annotation is therefore never silently trusted and never silently
  dropped — it either verifies quietly or gets flagged.
- **`ignore <checker>`** suppresses findings from the named checker for
  that function's own body (a `(function, checker)` pair) — but only at
  the CLI's suppression layer; the engine itself always analyzes as if
  unannotated when a pragma is bad. Because `contract` findings
  attribute to the *caller*, a callee's `ignore` cannot suppress its
  callers' contract violations.

Multiple pragmas per declaration are allowed; repeated `requires`/
`ensures` lines become separate clauses. Expressions support
comparisons, `&&`/`||`/`!`, `==>` (implication), `len()`/`cap()`,
`old()`, and literals — no arithmetic and no field selection yet (both
parsed-and-rejected, reserved for a later wave).

**Severity.** Every finding carries a `severity` (`error` or
`warning`); only `unverified-annotation` is a warning today. Exit code
is 1 iff an unsuppressed error-severity finding remains — warnings
render but never fail the run on their own. `--deny warnings` promotes
warnings to effective-errors (exit code, JSON `severity`, and SARIF
`level` all reflect the promotion) — the one CI ratchet knob.

**`ignore` vs. baseline — in-source vs. external suppression.** A
`//goverify:ignore` pragma lives with the code and suppresses at the
source (SARIF `"inSource"`); `goverify baseline write` records
fingerprints externally in `.goverify/baseline.json` (SARIF
`"external"`) so an adopted codebase's *existing* findings don't fail
CI while new ones still do. Both suppression counts are reported
separately (human summary: `pragma: N finding(s) suppressed` /
`baseline: N finding(s) suppressed`; JSON:
`suppressed_pragma`/`suppressed_by_baseline`), and both are
severity-blind — a suppressed warning is still counted.

**Never-silent rule.** A pragma that fails to parse, names an unknown
identifier or checker, or uses an unsupported construct (`effects`,
`pure`, field selection) never gets dropped quietly — it produces a
`bad-annotation` error finding at the pragma's position, and the
function it's attached to is analyzed as if unannotated (degrade,
never die).

## Development

Named tasks (run `mise tasks` for the full list): `build`, `test`,
`lint`, `fmt`, `corpus` (full extractor pipeline + determinism suite),
`bench`, `audit`, `secrets`, `fuzz`, `proto-gen`, `shakeout` (manual —
`check` run over pinned bbolt).

Corpus expectations live as `// want: <tag>` comments on the annotated
line (`testdata/corpus/*`), checked by the `corpus` task's checker
suites — checker tags are `nil-deref`, `bounds`, `div-zero`,
`overflow`, `contract`; the `bad-annotation`/`unverified-annotation`
annotation-only fixtures (`testdata/corpus/annot/`) use bespoke
assertions instead of `// want:` pins, since those findings anchor at
the pragma line.

- [ARCHITECTURE.md](ARCHITECTURE.md) — crate boundaries and why
- [docs/threat-model.md](docs/threat-model.md) — security stance
- [AGENTS.md](AGENTS.md) — front door for AI coding agents

## License

Apache-2.0 — see [LICENSE](LICENSE).
