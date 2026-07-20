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

- **Load forwarding ignores anything that isn't a store**
  (`encode_load_forwarding`): two reads of the same address with no
  intervening store are modeled as equal. Function calls don't
  invalidate forwarding — but neither do goroutine spawns (`go`),
  deferred calls, or any other register-producing unmodeled op
  (`Havoc` with a dst); a callee/goroutine/deferred call that mutates
  the re-read field between a caller's check and its use is missed at
  the re-read site. Only a `Store` (including map updates, which lower
  to `Store`) or a dst-less unmodeled effect (`Havoc` with no dst)
  invalidates forwarding.
- **uintptr-derived pointers are non-nil** (`op_def` Convert arm,
  `uintptr_provenance`): any pointer that transits uintptr is assumed
  non-nil — including zero-valued uintptrs from parameters, calls, or
  stored handles, and exact nil→uintptr→pointer round-trips;
  deliberate wraparound is a special case. Plain
  pointer→unsafe.Pointer→pointer puns keep their nilability.
- **Curated constructor trust** (`NEVER_NIL_RESULT`): externs in the
  table (currently `flag.NewFlagSet`) are trusted to return non-nil
  per their documented behavior; a stdlib behavior change contrary to
  its documentation would be missed. The phase-6 annotation language
  externalizes this table.
- **Assign/ChangeType copies could silently discharge an unrelated
  deref's requires (fixed).** `nil.rs`'s `params_only` filter composed
  with `shared::checked_deref_assumptions` (fix 2b) used to let a
  same-function copy `q := NamedPtr(p)` (Assign/ChangeType) that was
  itself dereferenced fail `params_only` (its encoded term was its own
  SMT var, not `p`'s), so it never emitted its own requires clause —
  but it WAS a deref site, so `checked_deref_assumptions` granted it
  `¬nil(v_q)` once reached. The Assign equality `v_q = p0` then let the
  solver derive `¬nil(p0)` from that grant, discharging a genuinely
  unrelated `p`-deref's requires even though nothing checked `p`
  itself: `f`'s callers passing nil went unflagged even though `f`
  genuinely panicked. Fixed by subject canonicalization: deref subjects
  now resolve through same-function Assign/ChangeType chains
  (`shared::canonical_value`, depth-capped 64 to bound crafted-cycle/
  chain inputs) to their root value before `nil.rs` hands them to
  `params_only`, so the copy's own deref emits `¬nil(p0)` itself
  instead of leaving it to be silently absorbed (exemplar and applied
  fix: `testdata/corpus/knownfp/knownfp.go`'s `f`/`NamedPtr` block;
  live red coverage: `testdata/corpus/nil/nil.go`'s `chained`/
  `BadChained`). Two residuals remain: `Op::Convert` chains (as opposed
  to `Op::Assign`/ChangeType) stay deliberately opaque per the
  uintptr-provenance blind spot above — a pointer that transits a
  `Convert` is not canonicalized through it; and `bounds.rs` subjects
  are not canonicalized this cycle, since its violation terms are index
  expressions rather than copyable pointer subjects, so the same
  Assign-chain composition doesn't apply there.
- **Go-idiom error correlation (ensures inference).** The
  `is_nil(err) ⇒ ¬is_nil(result)` postcondition template validates per
  return site: a site whose error component is the literal nil constant
  must SMT-prove the paired result non-nil, but a site returning any
  other error expression (a sentinel global, a wrapped error) is
  accepted as returning a non-nil error without proof — the universal
  Go idiom, unprovable locally because sentinel loads are havoc'd. A
  callee that returns a nil-valued error *variable* together with a nil
  result earns a wrong ensures, and callers guarding `err != nil` get a
  wrong discharge (false negative). The unconditional `¬is_nil(result)`
  template carries no such assumption (strictly proven).
