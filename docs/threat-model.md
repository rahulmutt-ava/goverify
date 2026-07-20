# Threat model (v1)

Explicit v1 stance from the
[design spec](superpowers/specs/2026-07-16-goverify-design.md) §14.

## Trust boundaries

- **Analyzed source is semi-trusted.** The extractor invokes the Go
  toolchain (`go/packages`) against the target repo; running goverify
  against a hostile repo is equivalent to running `go build` there.
  Documented, not defended, in v1 — the same stance as
  gopls/staticcheck.
- **The cache is a trust boundary when shared.** Content addressing
  detects corruption, not tampering: a writer can store a wrong value
  under a correct key. Only consume shared caches from writer-trusted
  storage (e.g. the project's own CI cache). No public/community cache
  without signing (out of scope for v1).
- **The sidecar build cache executes what it holds.** It lives at
  `$XDG_CACHE_HOME/goverify/extractor-bin` (or
  `$HOME/.cache/goverify/extractor-bin`), parent directory created `0700`;
  a bare `temp_dir()/goverify-extractor-bin` path is used only as a
  last-resort fallback when neither env var is set (CWE-377 rationale
  in `sidecar_build_dir()` in `crates/goverify-cli/src/main.rs`). It holds extractor
  binaries named by content hash (extractor source ⊕ Go toolchain
  version) that goverify **executes without further verification** —
  whoever can write to this directory executes code as the user. The
  cache directory must be writable only by the user; the temp-dir
  fallback weakens this (a predictable path under a world-writable
  parent) and is deliberately last-resort.
- **`--solver-cmd` executes a user-supplied binary** — by design; CI
  configs must treat it like any other executable input.
- **Model text is now rendered — as untrusted display input, never
  parsed for verdicts.** `goverify check` (Task 11) prints `Finding.model`
  (param bindings lifted from the Sat model's own text, `checker.rs`)
  and the source-echo snippet in its terminal output. Both pass through
  `render::sanitize` (`crates/goverify-cli/src/render.rs`) first: every
  C0 control char (`< 0x20`) and DEL (`0x7f`) is stripped, so a
  crafted/corrupt model or source line can't inject ANSI escapes or
  otherwise smuggle terminal control sequences into the user's shell.
  Verdicts (Sat/Unsat/Unknown) come from the solver's own result code
  computed before any model text is read — sanitization is a display-only
  concern, not a correctness one. The cache trust boundary from the
  shared-cache clause (above) still applies to model text sourced from a
  shared cache: it is trusted as if from the original solver, sanitized
  the same as any other run's model text.
- **Untrusted-bytes surfaces** — `.gvir`, `.gvspec`, annotation
  expressions, solver output — are parsed by fuzz-hardened decoders
  (`fuzz/`) that must reject malformed input without panicking.

## Repo hygiene (enforced in CI)

- gitleaks: pre-commit hook (`mise run setup`) and the `secrets` CI job.
- cargo-audit: blocking `audit` CI job plus the nightly schedule (new
  CVEs land on old code).
- clippy correctness lints: blocking tier, `-D warnings`.

## Deliberate under-approximations (FP/encoding fix-wave, 2026-07-20)

The bug-finder invariant (findings only on Sat; false positives are
the enemy) buys precision with four enumerated blind spots. Each is a
conscious trade, not an accident — anything found missing here should
be added, not silently tolerated.

- **Load forwarding ignores calls** (`encode_load_forwarding`): two
  reads of the same address with no intervening store are modeled as
  equal even across function calls. A callee that mutates the re-read
  field between a caller's check and its use is missed at the re-read
  site. Stores and unmodeled (Havoc) effects still invalidate.
- **uintptr-derived pointers are non-nil** (`op_def` Convert arm): a
  pointer minted via uintptr arithmetic
  (`unsafe.Pointer(uintptr(base)+off)`) is assumed non-nil; deliberate
  wraparound to exactly address 0 is assumed away. Plain
  pointer→unsafe.Pointer→pointer puns keep their nilability.
- **Curated constructor trust** (`NEVER_NIL_RESULT`): externs in the
  table (currently `flag.NewFlagSet`) are trusted to return non-nil
  per their documented behavior; a stdlib behavior change contrary to
  its documentation would be missed. The phase-6 annotation language
  externalizes this table.
- **Assign/ChangeType copies can silently discharge an unrelated
  deref's requires** (`nil.rs`'s `params_only` filter composed with
  `shared::checked_deref_assumptions`, fix 2b): a same-function copy
  `q := NamedPtr(p)` (Assign/ChangeType) that is itself dereferenced
  fails `params_only` (its encoded term is its own SMT var, not `p`'s),
  so it never emits its own requires clause — but it IS a deref site,
  so `checked_deref_assumptions` grants it `¬nil(v_q)` once reached.
  The Assign equality `v_q = p0` then lets the solver derive `¬nil(p0)`
  from that grant, discharging a genuinely unrelated `p`-deref's
  requires even though nothing checked `p` itself. Net effect: `f`'s
  callers passing nil are no longer flagged, though `f` genuinely
  panics (exemplar and fix direction: `testdata/corpus/knownfp/
  knownfp.go`'s `f`/`NamedPtr` KNOWN-FN block). The in-code fix —
  canonicalizing a deref subject through same-function Assign/
  ChangeType chains before `params_only` decides expressibility — is
  queued, not yet applied.
