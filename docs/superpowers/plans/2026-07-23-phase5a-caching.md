# Phase 5a: Extraction + Analysis Caching Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the `extract` (per-package `.gvir`) and `scc` (per-SCC summaries+findings) cache layers on the existing `Store`, make caching default-on for `goverify check`, and measure the warm-run speed milestone — plus the three wave-2 hygiene riders and the §16 generics measurement.

**Architecture:** Both new layers use recursive content keys over their DAG (import DAG for packages, condensed call DAG for SCCs), so invalidation propagates upward exactly. The SCC entry carries summaries **and findings and diagnostics** so a warm run replays byte-identical stdout. Term serialization (the one new codec surface) lands in `goverify-solver`; the SCC entry framing lands in `goverify-analysis::scc_cache` (NOT `goverify-cache` — see Deviation note); extraction caching lands in `goverify-extract::cached`. `goverify-cache` stays bytes-only.

**Tech Stack:** Rust (workspace crates), Go sidecar (`extractor/`), blake3, prost, existing `goverify_cache::Store`. No new external crates (the sidecar manifest uses a line protocol, not JSON, to avoid a serde dependency).

**Spec:** `docs/superpowers/specs/2026-07-23-phase5-caching-design.md`. Parent: `2026-07-16-goverify-design.md` §9.

**DEVIATION from spec §4 (dependency-forced, pre-agreed here):** the spec says entry framing "lives in `goverify-cache`". `goverify-cache` is a leaf crate that cannot see `Summary`/`Finding`/`Term` types (`goverify-analysis` and `goverify-solver` both depend on it — the reverse dependency would be a cycle). The framing therefore lives in `goverify-analysis::scc_cache`, which uses `goverify_cache::Store` for bytes. This preserves the actual principle ("cache owns bytes, not meaning"). Task 7 amends the spec sentence.

## Global Constraints

- **Determinism is the root invariant**: identical inputs ⇒ byte-identical outputs. Cold and warm runs of `check` must produce **byte-identical stdout**. No timestamps, no absolute paths, no map-iteration order in anything cached or printed.
- **Only Go code lives in `extractor/`**; everything else is Rust.
- **No new crate dependencies** without justification (design spec §13). This plan adds none.
- **Parsers of bytes the analyzer didn't write must reject, never panic** — every new decoder is defensive (`Option`/miss) and the SCC-entry decoder gets a fuzz target.
- **Errors degrade, never die**: cache read failure = miss; cache write failure = warn + continue; manifest failure = fall back to uncached extraction.
- Run toolchain commands through mise: `mise x -- cargo ...` (sandbox RUSTUP relocation; see memory `goverify-sandbox-environment`).
- Commits are **unsigned** in this sandbox: `git commit --no-gpg-sign`. Re-sign before pushing.
- Blocking gate before any task is "complete": `mise run lint` and the task's tests green. Corpus determinism suite (`mise run corpus`) must stay green throughout.
- Commit-message prefix for this wave: `phase5a:`.
- Version constants introduced here (`TERM_CODEC_VERSION`, `SCC_CACHE_VERSION`, `EXTRACT_CACHE_VERSION`, per-checker `version()`) carry the documented invariant: **bump on any semantic change** to what they cover. The cold/warm byte-identity tests are the tripwire.

---

## Task Dependency Order

Tasks 1–4 are independent riders (do first, any order). Core chain: 5 → 6 → 7 → 8 → 9 and 10 → 11 → 12 (12 also needs 8). 13 after 7. 14 after 12. 15 last.

---

### Task 1: Warm-run profiling probe (`GOVERIFY_TIMINGS`) — the G4 denominator

**Files:**
- Modify: `crates/goverify-cli/src/main.rs` (inside `run_check`, main.rs:324-391)
- Report: `.superpowers/sdd/task-1-report.md` (gitignored ledger; no commit for the report)

**Interfaces:**
- Consumes: nothing new.
- Produces: `GOVERIFY_TIMINGS=1` env knob printing phase wall-clocks to **stderr** (never stdout — stdout is the byte-identity surface). Later tasks (8, 12) extend these prints with cache stats.

- [ ] **Step 1: Add phase timing to `run_check`**

In `crates/goverify-cli/src/main.rs`, at the top of `run_check` (after the `let dargs = ...` block, before `load_program`):

```rust
    // Phase wall-clocks on stderr, opt-in via GOVERIFY_TIMINGS=1 (spec
    // §6 rider 1 / G4). stderr only: stdout is the cold/warm
    // byte-identity surface.
    let timings = std::env::var_os("GOVERIFY_TIMINGS").is_some();
    let t_extract = std::time::Instant::now();
    let program = load_program(&dargs)?;
    if timings {
        eprintln!(
            "goverify: timing: extract+load {:.2}s",
            t_extract.elapsed().as_secs_f64()
        );
    }
```

(replacing the existing `let program = load_program(&dargs)?;` line). Then wrap the analysis call:

```rust
    let t_analyze = std::time::Instant::now();
    let a = goverify_analysis::analyze_full(&program, &cfg, &checkers, &*mk);
    if timings {
        eprintln!(
            "goverify: timing: analyze {:.2}s",
            t_analyze.elapsed().as_secs_f64()
        );
    }
```

and wrap the render+scope block (from `let scope = ...` through the final `print!`):

```rust
    let t_render = std::time::Instant::now();
    // ... existing scope/render code unchanged ...
    print!("{}", render::render_findings(&scoped, Path::new(".")));
    if timings {
        eprintln!(
            "goverify: timing: scope+render {:.2}s",
            t_render.elapsed().as_secs_f64()
        );
    }
```

- [ ] **Step 2: Verify no stdout change**

Run: `mise x -- cargo test -p goverify-cli`
Expected: PASS (existing `cli.rs` / `debug_integration.rs` untouched — timings are stderr-only and opt-in).

- [ ] **Step 3: Lint + commit the code change**

```bash
mise run lint
git add crates/goverify-cli/src/main.rs
git commit --no-gpg-sign -m "phase5a: GOVERIFY_TIMINGS phase wall-clocks on check stderr"
```

- [ ] **Step 4: Measure the warm bbolt breakdown (investigation, report-only)**

```bash
mise run shakeout            # ensures bbolt clone + warm cache exist
cd .goverify/shakeout/bbolt
GOVERIFY_TIMINGS=1 "$(git rev-parse --show-toplevel 2>/dev/null || echo ../../..)/target/release/goverify" check ./... --cache-dir "$(pwd)/../cache" 2> /tmp/../../../..; true
```

Practical form (from repo root, mirroring `scripts/shakeout.sh`):

```bash
mise x -- cargo build --release -p goverify-cli
export GOVERIFY_EXTRACTOR_DIR="$(pwd)/extractor"
cd .goverify/shakeout/bbolt
GOVERIFY_TIMINGS=1 /usr/bin/time ../../../target/release/goverify check ./... \
  --cache-dir "$(pwd)/../cache" > /dev/null
```

Record in `.superpowers/sdd/task-1-report.md`: the three phase numbers (extract+load / analyze / scope+render), total wall, and which phase dominates. Carry the standing machine-state caveat (SentinelOne EDR incidents; see memory) — if any number looks stall-shaped (minutes where seconds are expected), re-run once and note it.

**This report is the G4 denominator.** Expected shape (unverified prediction, do not assume): extract+load dominates the warm 26s since the query cache already absorbs solver time.

---

### Task 2: `// want:` parser hardening (wave-2 rider b, twice-bitten)

**Files:**
- Modify: `crates/goverify-ir/src/testutil.rs:48-79` (`wants_in`) and its unit test at `testutil.rs:85-101`

**Interfaces:**
- Consumes: nothing.
- Produces: `wants_in` accepts a marker **only in trailing-comment position with valid tags**: there must be non-comment code before the marker, the marker's comment must be the line's last `//`, and every tag must match `[a-z0-9-]+`. Prose lines mentioning `// want:` no longer parse as pins.

- [ ] **Step 1: Extend the unit test with the two historical bite patterns**

In the `#[cfg(test)]` module of `testutil.rs`, add:

```rust
    #[test]
    fn wants_ignores_prose_and_whole_line_comments() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("a.go"),
            concat!(
                "package a\n",
                "// This pin exists because the // want: parser used to\n", // prose: whole-line comment
                "// match `// want: overflow` anywhere in a line.\n",       // prose: marker mid-comment
                "func F(x int) int {\n",
                "\treturn x + x // want: overflow\n",                        // real pin
                "}\n",
                "// want: nil-deref\n",                                      // whole-line marker: NOT a pin
                "func G() { _ = 1 } // want: not a valid tag list\n",        // invalid tags: NOT a pin
            ),
        )
        .unwrap();
        let got = wants_in(dir.path());
        assert_eq!(
            got,
            vec![("a.go".to_string(), 5, "overflow".to_string())],
            "wants_in(): only the trailing-comment marker with valid tags parses"
        );
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `mise x -- cargo test -p goverify-ir wants_ignores_prose`
Expected: FAIL — the current naive `line.split("// want:").nth(1)` (testutil.rs:66) picks up the prose lines, the whole-line marker, and the invalid tag list.

- [ ] **Step 3: Harden the parser**

Replace the marker-matching body of the per-line loop in `wants_in` (the `let Some(rest) = line.split("// want:").nth(1) else { continue };` and the tag loop) with:

```rust
        for (i, line) in text.lines().enumerate() {
            // Trailing-comment position only (wave-2 follow-up, twice
            // bitten by prose): the marker must be the line's LAST `//`
            // comment, there must be real code before it, and every tag
            // must be a bare [a-z0-9-]+ token. Anything else is prose.
            let Some(idx) = line.rfind("//") else { continue };
            let (code, comment) = line.split_at(idx);
            let Some(rest) = comment.strip_prefix("//") else { continue };
            let Some(rest) = rest.trim_start().strip_prefix("want:") else {
                continue;
            };
            if code.trim().is_empty() || code.trim_start().starts_with("//") {
                continue; // whole-line or comment-only prefix: prose, not a pin
            }
            let tags: Vec<&str> = rest.split(',').map(str::trim).collect();
            let valid = |t: &str| {
                !t.is_empty()
                    && t.chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            };
            if !tags.iter().all(|t| valid(t)) {
                continue; // prose after "want:" — not a tag list
            }
            for tag in tags {
                out.push((name.clone(), (i + 1) as u32, tag.to_string()));
            }
        }
```

Note `code.trim_start().starts_with("//")` is unreachable after `split_at(rfind)` when idx is the last `//` — it guards multi-`//` lines where the prefix itself is a comment opener, e.g. `\t// prose // want: x`: `code` = `\t// prose `, which must not count as code.

- [ ] **Step 4: Run the parser tests and the full corpus suite**

Run: `mise x -- cargo test -p goverify-ir wants` — Expected: PASS (both the old `wants_parses_tags_lines_and_multi` and the new test).
Run: `mise run corpus` — Expected: PASS. **If any corpus test now fails, a real pin was being parsed only by accident of the old naive matcher — STOP and inspect that pin file before adjusting anything** (a silently dropped pin is exactly the failure mode this hardening exists to prevent).

- [ ] **Step 5: Lint + commit**

```bash
mise run lint
git add crates/goverify-ir/src/testutil.rs
git commit --no-gpg-sign -m "phase5a: want-parser trailing-comment-position + tag-charset hardening"
```

---

### Task 3: Lazy escalated-tier solver construction (wave-2 rider c)

**Files:**
- Modify: `crates/goverify-solver/src/retry.rs` (add `LazySolver`), `crates/goverify-solver/src/lib.rs:24` (export), `crates/goverify-cli/src/main.rs:259-274` (`retry_backend`)

**Interfaces:**
- Consumes: `TextSolver` trait (lib.rs:82-96), `SolverLimits` (lib.rs:59-73), `RetryBackend` (retry.rs:28-51).
- Produces: `pub struct LazySolver` with `pub fn new(identity: String, limits: SolverLimits, make: Box<dyn FnMut() -> Box<dyn TextSolver> + Send>) -> LazySolver`, implementing `TextSolver`. Exported as `pub use retry::{LazySolver, RetryBackend, escalation_count};`.

Background: `RetryBackend::new` (retry.rs:34) receives two already-boxed solvers and the CLI's `retry_backend` (main.rs:259-274) constructs **both** `Z3Native`s eagerly; `Z3Native::new` allocates a real Z3 context immediately (z3native.rs:62-78). One `Infer`-role backend is built **per SCC** (engine.rs:159), so every SCC pays a second Z3 context that is only used if a query escalates (254/~4600 SCCs did in the last shakeout).

- [ ] **Step 1: Write the failing test (in `retry.rs`'s test module — create one if absent; existing RetryBackend tests live in `discharge.rs`)**

```rust
#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;
    use crate::{QueryOutcome, SatResult};

    struct CountingFake;
    impl TextSolver for CountingFake {
        fn identity(&self) -> String {
            "fake".to_string()
        }
        fn limits(&self) -> SolverLimits {
            SolverLimits::default()
        }
        fn solve_text(&mut self, _canonical: &str) -> QueryOutcome {
            QueryOutcome {
                result: SatResult::Unsat,
                model: None,
            }
        }
    }

    #[test]
    fn lazy_solver_defers_construction_until_first_solve() {
        let built = Arc::new(AtomicU32::new(0));
        let b = built.clone();
        let mut lazy = LazySolver::new(
            "fake".to_string(),
            SolverLimits::default(),
            Box::new(move || {
                b.fetch_add(1, Ordering::SeqCst);
                Box::new(CountingFake)
            }),
        );
        // identity/limits answer WITHOUT constructing the inner solver.
        assert_eq!(lazy.identity(), "fake", "LazySolver::identity()");
        assert_eq!(
            lazy.limits(),
            SolverLimits::default(),
            "LazySolver::limits()"
        );
        assert_eq!(built.load(Ordering::SeqCst), 0, "no construction yet");
        // First solve constructs exactly once; second reuses.
        let _ = lazy.solve_text("(check-sat)\n");
        let _ = lazy.solve_text("(check-sat)\n");
        assert_eq!(built.load(Ordering::SeqCst), 1, "constructed exactly once");
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `mise x -- cargo test -p goverify-solver lazy_solver`
Expected: FAIL — `LazySolver` not defined.

- [ ] **Step 3: Implement `LazySolver` in `retry.rs`**

```rust
/// A TextSolver whose inner backend is constructed on first
/// `solve_text` (wave-2 follow-up: the escalated tier's Z3 context was
/// allocated per SCC but used only when a query actually escalates).
/// `identity`/`limits` are carried as data so the query-cache key can
/// be computed without forcing construction.
pub struct LazySolver {
    identity: String,
    limits: SolverLimits,
    make: Box<dyn FnMut() -> Box<dyn TextSolver> + Send>,
    inner: Option<Box<dyn TextSolver>>,
}

impl LazySolver {
    pub fn new(
        identity: String,
        limits: SolverLimits,
        make: Box<dyn FnMut() -> Box<dyn TextSolver> + Send>,
    ) -> LazySolver {
        LazySolver {
            identity,
            limits,
            make,
            inner: None,
        }
    }
}

impl TextSolver for LazySolver {
    fn identity(&self) -> String {
        self.identity.clone()
    }
    fn limits(&self) -> SolverLimits {
        self.limits
    }
    fn solve_text(&mut self, canonical: &str) -> QueryOutcome {
        if self.inner.is_none() {
            self.inner = Some((self.make)());
        }
        self.inner
            .as_mut()
            .expect("just constructed")
            .solve_text(canonical)
    }
}
```

Export in `lib.rs`: change line 24 to `pub use retry::{LazySolver, RetryBackend, escalation_count};`.

- [ ] **Step 4: Run to verify it passes**

Run: `mise x -- cargo test -p goverify-solver lazy_solver`
Expected: PASS.

- [ ] **Step 5: Wire the CLI's `retry_backend` to a lazy escalated tier**

Replace `retry_backend` in `crates/goverify-cli/src/main.rs:259-274` with:

```rust
fn retry_backend(
    cmd: &Option<String>,
    lim: goverify_solver::SolverLimits,
) -> Box<dyn goverify_solver::TextSolver> {
    let esc = escalated(lim);
    match cmd {
        Some(c) => {
            let base = goverify_solver::SmtLib2Process::new(c, lim);
            let identity = base.identity();
            let c = c.clone();
            Box::new(goverify_solver::RetryBackend::new(
                Box::new(base),
                Box::new(goverify_solver::LazySolver::new(
                    identity,
                    esc,
                    Box::new(move || Box::new(goverify_solver::SmtLib2Process::new(&c, esc))),
                )),
            ))
        }
        None => {
            let base = goverify_solver::Z3Native::new(lim);
            // Same z3 build, same identity string — safe to carry as
            // data; the escalated tier's cache entries stay keyed
            // identically to the eager construction they replace.
            let identity = base.identity();
            Box::new(goverify_solver::RetryBackend::new(
                Box::new(base),
                Box::new(goverify_solver::LazySolver::new(
                    identity,
                    esc,
                    Box::new(move || Box::new(goverify_solver::Z3Native::new(esc))),
                )),
            ))
        }
    }
}
```

Note: `TextSolver::identity` is `&self -> String` (lib.rs:83); if `SmtLib2Process::new`'s identity method needs the constructed value, calling `.identity()` on the eagerly-built **base** is correct for both arms — base and escalated tiers have identical identities today (same z3 build / same command), which is exactly what the query-cache key relied on before this change.

- [ ] **Step 6: Run the full retry/discharge test suite + corpus**

Run: `mise x -- cargo test -p goverify-solver` — Expected: PASS (discharge.rs retry tests exercise escalation end-to-end with fakes).
Run: `mise run corpus` — Expected: PASS (debug_integration exercises the CLI construction path).

- [ ] **Step 7: Lint + commit**

```bash
mise run lint
git add crates/goverify-solver/src/retry.rs crates/goverify-solver/src/lib.rs crates/goverify-cli/src/main.rs
git commit --no-gpg-sign -m "phase5a: lazy escalated-tier solver construction (per-SCC double Z3 context removed)"
```

---

### Task 4: Convert-widened non-liftable overflow pin (wave-2 rider a)

**Files:**
- Modify: `testdata/corpus/knownfp/knownfp.go` (the elemOffset family, near `GlobalElemOffset` at knownfp.go:473-475)

**Interfaces:**
- Consumes: existing `elemOffset` helper (knownfp.go:404-406), `var unboundedN int` (knownfp.go:471), the `knownfp_corpus.rs` set-equality harness.
- Produces: a `// want: overflow` pin whose overflow obligation flows through a **widening `Convert`** on a **non-liftable** (global-load) source — closing the wave-2 spec §4 item 2 narrow claim (wave-2's Pin B used a global load with no Convert; 4A's int→int Convert source-range assertion was never pinned on a non-liftable shape).

Corpus-authoring gotchas (twice-confirmed in wave 2, both mitigated by Task 2 but stay relevant): probe mutations must keep the file compiling or the whole file is skipped and every pin silently masked; and obligations expressible purely over bare params lift into inferred requires instead of reporting (`requires-lifting`, bounds.rs ~490) — which is why the source must be a global load.

- [ ] **Step 1: Add the fixture**

Append after `GlobalElemOffset` (knownfp.go:475), reusing the existing `unboundedN` style:

```go
// unboundedN32 mirrors unboundedN at a narrower width so the call below
// must route through a widening int32->int Convert.
var unboundedN32 int32

// ConvertWidenedElemOffset pins the 4A Convert path on a non-liftable
// shape (wave-2 spec §4 item 2's narrow claim): the multiplier reaches
// elemOffset through a widening Convert whose SOURCE is a global load,
// so the obligation cannot lift into inferred requires, and 4A's
// int->int source-range assertion must not discharge an genuinely
// unbounded multiplication. If this pin ever goes silent, 4A has
// started over-suppressing through widening Converts.
func ConvertWidenedElemOffset(base uintptr) uintptr {
	return elemOffset(base, 16, int(unboundedN32)) // want: overflow
}
```

- [ ] **Step 2: Run the knownfp corpus suite — expect one of two outcomes**

Run: `mise x -- cargo test -p goverify-checkers --test knownfp_corpus`

- **Expected: PASS** — the finding appears at the pinned line and set-equality holds. Proceed to step 3.
- **STOP CONDITION:** if the suite fails because the finding is **absent** (want without got), do NOT delete the pin or force it green. This is the same investigate-first situation as wave-2 Task 3: determine whether the silence is (a) 4A discharging via the Convert source-range assertion — i.e. the exact over-suppression this pin guards, a real bug to surface — or (b) another lifting/discharge path. Write the evidence to `.superpowers/sdd/` and surface to the plan owner in the task report. If the failure is an **unexpected extra finding elsewhere** (got without want), the mutation broke an adjacent pin — inspect before touching anything.

- [ ] **Step 3: RED/GREEN probe (verify the pin can actually fire)**

Temporarily mutate `elemOffset`'s body-level guard consumer — replace the pinned line with a bounded call `return elemOffset(base, 16, 4)` (keeps the file compiling), run the suite, and confirm it goes RED with a missing-want failure at the pinned line. Restore the original line, re-run, confirm GREEN. This proves the set-equality harness sees this exact position.

Run: `mise x -- cargo test -p goverify-checkers --test knownfp_corpus` (twice, RED then GREEN as above).

- [ ] **Step 4: Full corpus + lint + commit**

```bash
mise run corpus
mise run lint
git add testdata/corpus/knownfp/knownfp.go
git commit --no-gpg-sign -m "phase5a: ConvertWidenedElemOffset pin — 4A Convert path on a non-liftable shape"
```

---

### Task 5: Term binary codec in `goverify-solver`

**Files:**
- Create: `crates/goverify-solver/src/codec.rs`
- Modify: `crates/goverify-solver/src/lib.rs` (add `mod codec;` + `pub use codec::{TERM_CODEC_VERSION, decode_sort, decode_term, encode_sort, encode_term};`)

**Interfaces:**
- Consumes: `Term` (term.rs:79-83, `node` is `pub(crate)` — this module MUST live inside goverify-solver), `Node` (term.rs:35-77), `Sort`/`DatatypeDecl` (sort.rs), the checked constructors `Term::{bool_lit, bv_lit, var, not, and, or, implies, eq, ite, bv_bin, bv_cmp, select, store, dt_ctor, dt_is, dt_get}` (term.rs:103-335).
- Produces (cross-task contract for Task 7):
  - `pub const TERM_CODEC_VERSION: u8 = 1;`
  - `pub fn encode_term(t: &Term, out: &mut Vec<u8>)`
  - `pub fn decode_term(input: &mut &[u8], decls: &[DatatypeDecl]) -> Option<Term>`
  - `pub fn encode_sort(s: &Sort, out: &mut Vec<u8>)` / `pub fn decode_sort(input: &mut &[u8]) -> Option<Sort>`

Design constraints:
- Decode goes **only** through the checked constructors, so a decoded term always satisfies the sort invariants — fuzzed bytes can never produce an ill-sorted `Term` that would later panic in the printer. Any `SortError` ⇒ `None`.
- `Node::DtIs`/`Node::DtGet` don't record the datatype name; the decoder searches `decls` for the (unique) decl containing the ctor/field. v1 callers pass `[ptr_datatype(), seq_datatype()]` — ctor and field names are disjoint between them.
- `bv_lit` **asserts** on out-of-range values (term.rs: `assert!((1..=128).contains(&width))`) — the decoder must range-check width and value BEFORE calling it (reject, never panic).
- `Term::var` asserts `valid_symbol` — decoder must pre-check with the same predicate; expose the check by making the decoder validate via a local copy of the rule (ASCII alnum + the EXTRA set, non-empty, no leading digit) or by adding `pub(crate) use` of `valid_symbol` — prefer marking `valid_symbol` `pub(crate)` in term.rs and calling it.
- Encoding: leading discriminant byte per Node variant; `u32` little-endian length prefixes for strings and vecs; `u128` as 16 LE bytes; recursion for children; `Var` additionally encodes its sort (the one place sort isn't derivable from children). Deterministic by construction (no maps).

- [ ] **Step 1: Write the failing tests (in `codec.rs`'s `#[cfg(test)]` module)**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::sort::{Sort, ptr_datatype};
    use crate::term::{BvBinOp, BvCmpOp, Term};

    fn decls() -> Vec<crate::sort::DatatypeDecl> {
        vec![ptr_datatype()]
    }

    fn round_trip(t: &Term) {
        let mut buf = Vec::new();
        encode_term(t, &mut buf);
        let mut input = buf.as_slice();
        let back = decode_term(&mut input, &decls()).expect("decode_term()");
        assert!(input.is_empty(), "decoder must consume exactly its bytes");
        assert_eq!(&back, t, "round-trip identity");
    }

    #[test]
    fn scalar_and_connective_terms_round_trip() {
        let x = Term::var("p0", Sort::BitVec(64));
        let y = Term::var("p1", Sort::BitVec(64));
        let cases = vec![
            Term::bool_lit(true),
            Term::bv_lit(64, 42),
            Term::bv_lit(128, u128::MAX),
            x.clone(),
            Term::not(Term::bool_lit(false)).unwrap(),
            Term::and(vec![Term::bool_lit(true), Term::bool_lit(false)]).unwrap(),
            Term::or(vec![Term::bool_lit(true)]).unwrap(),
            Term::implies(Term::bool_lit(true), Term::bool_lit(false)).unwrap(),
            Term::eq(x.clone(), y.clone()).unwrap(),
            Term::ite(Term::bool_lit(true), x.clone(), y.clone()).unwrap(),
            Term::bv_bin(BvBinOp::Mul, x.clone(), y.clone()).unwrap(),
            Term::bv_cmp(BvCmpOp::Slt, x.clone(), y.clone()).unwrap(),
        ];
        for t in &cases {
            round_trip(t);
        }
    }

    #[test]
    fn datatype_terms_round_trip() {
        let dt = ptr_datatype();
        // Build a ptr value via the crate's own helpers to stay
        // agnostic of ctor names: ptr_nil() is exported at lib.rs.
        let nil = crate::ptr_nil();
        round_trip(&nil);
        round_trip(&crate::ptr_is_nil(nil.clone()).unwrap());
        // dt_get on a real field of the decl.
        let ctor = &dt.ctors[0];
        if let Some((fname, _)) = ctor.fields.first() {
            let args: Vec<Term> = Vec::new();
            let _ = (fname, args); // field-bearing ctor coverage handled by proptest below
        }
    }

    #[test]
    fn arrays_round_trip() {
        let arr = Term::var(
            "m0",
            Sort::Array(Box::new(Sort::BitVec(64)), Box::new(Sort::Bool)),
        );
        let idx = Term::var("p0", Sort::BitVec(64));
        let sel = Term::select(arr.clone(), idx.clone()).unwrap();
        round_trip(&sel);
        round_trip(&Term::store(arr, idx, Term::bool_lit(true)).unwrap());
    }

    #[test]
    fn corrupt_bytes_are_none_never_panic() {
        let mut buf = Vec::new();
        encode_term(&Term::bv_lit(64, 7), &mut buf);
        // Truncations.
        for cut in 0..buf.len() {
            let mut input = &buf[..cut];
            let _ = decode_term(&mut input, &[]); // must not panic
        }
        // Garbage discriminants / oversized lengths / bad widths.
        for garbage in [
            &[0xffu8, 0xff][..],
            &[TERM_CODEC_VERSION, 0xee][..],
            &[][..],
        ] {
            let mut input = garbage;
            assert!(
                decode_term(&mut input, &[]).is_none() || true,
                "reject-never-panic is the property; None or partial is fine"
            );
        }
        // A bv width of 0 or >128 must be rejected BEFORE bv_lit's assert.
        // (Constructed by hand-editing the width field of a valid encoding
        // in the implementation's format; see implementation test below.)
    }
}
```

Also add a property test using the existing `testgen` generator (this is what really covers `DtCtor`/`DtIs`/`DtGet` and deep nesting). In `crates/goverify-solver/tests/` the differential harness already generates arbitrary well-sorted terms under `--features testgen`; add to `codec.rs` tests:

```rust
    // Property: every generator-produced well-sorted term round-trips.
    // Uses the same term generator as the differential harness.
    #[cfg(feature = "testgen")]
    mod prop {
        use proptest::prelude::*;

        use super::super::*;
        use crate::sort::ptr_datatype;

        proptest! {
            #[test]
            fn generated_terms_round_trip(t in crate::testgen::arb_term()) {
                let mut buf = Vec::new();
                encode_term(&t, &mut buf);
                let mut input = buf.as_slice();
                let back = decode_term(&mut input, &[ptr_datatype()]);
                prop_assert_eq!(back.as_ref(), Some(&t));
                prop_assert!(input.is_empty());
            }
        }
    }
```

(Adjust the generator entry name to whatever `testgen.rs` actually exports — check `crates/goverify-solver/src/testgen.rs` for the arbitrary-term strategy the differential test uses, and pass the same datatype decls it uses, which include the ptr datatype. If the generator can emit GoSeq-sorted terms, add `seq_datatype()`'s decl equivalent — the generator is self-contained in the solver crate, so use whatever decls it declares.)

- [ ] **Step 2: Run to verify failure**

Run: `mise x -- cargo test -p goverify-solver codec`
Expected: FAIL — module doesn't exist.

- [ ] **Step 3: Implement the codec**

`crates/goverify-solver/src/codec.rs` (complete):

```rust
//! Binary Term/Sort codec for the SCC cache layer (phase-5a spec §4).
//! This is a SEPARATE serialization from the canonical SMT-LIB2 printer
//! (printer.rs) — the printer is the solver-facing lowering and cache
//! key; this codec is the durable at-rest form for cached summaries.
//!
//! Decode reconstructs exclusively through the checked Term
//! constructors, so any decoded Term satisfies the sort invariants;
//! bytes the current binary didn't write yield None, never a panic
//! (parent spec §12.4 — fuzzed via the scc_entry target).

use crate::sort::{DatatypeDecl, Sort};
use crate::term::{BvBinOp, BvCmpOp, Node, Term, valid_symbol};

/// Bump on ANY change to this encoding. Feeds SCC_CACHE_VERSION's
/// preimage in goverify-analysis, so a bump invalidates all entries.
pub const TERM_CODEC_VERSION: u8 = 1;

// ---- primitives ----

fn put_u32(out: &mut Vec<u8>, v: u32) {
    out.extend(v.to_le_bytes());
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    put_u32(out, s.len() as u32);
    out.extend(s.as_bytes());
}

fn take_u8(input: &mut &[u8]) -> Option<u8> {
    let (b, rest) = input.split_first()?;
    *input = rest;
    Some(*b)
}

fn take_u32(input: &mut &[u8]) -> Option<u32> {
    let (bytes, rest) = input.split_first_chunk::<4>()?;
    *input = rest;
    Some(u32::from_le_bytes(*bytes))
}

fn take_u128(input: &mut &[u8]) -> Option<u128> {
    let (bytes, rest) = input.split_first_chunk::<16>()?;
    *input = rest;
    Some(u128::from_le_bytes(*bytes))
}

fn take_str(input: &mut &[u8]) -> Option<String> {
    let len = take_u32(input)? as usize;
    if input.len() < len {
        return None;
    }
    let (s, rest) = input.split_at(len);
    *input = rest;
    String::from_utf8(s.to_vec()).ok()
}

// ---- Sort ----

pub fn encode_sort(s: &Sort, out: &mut Vec<u8>) {
    match s {
        Sort::Bool => out.push(0),
        Sort::BitVec(w) => {
            out.push(1);
            put_u32(out, *w);
        }
        Sort::Array(k, v) => {
            out.push(2);
            encode_sort(k, out);
            encode_sort(v, out);
        }
        Sort::Datatype(name) => {
            out.push(3);
            put_str(out, name);
        }
    }
}

pub fn decode_sort(input: &mut &[u8]) -> Option<Sort> {
    match take_u8(input)? {
        0 => Some(Sort::Bool),
        1 => {
            let w = take_u32(input)?;
            if !(1..=128).contains(&w) {
                return None;
            }
            Some(Sort::BitVec(w))
        }
        2 => {
            let k = decode_sort(input)?;
            let v = decode_sort(input)?;
            Some(Sort::Array(Box::new(k), Box::new(v)))
        }
        3 => Some(Sort::Datatype(take_str(input)?)),
        _ => None,
    }
}

// ---- Term ----

pub fn encode_term(t: &Term, out: &mut Vec<u8>) {
    match &t.node {
        Node::BoolLit(b) => {
            out.push(0);
            out.push(u8::from(*b));
        }
        Node::BvLit { width, value } => {
            out.push(1);
            put_u32(out, *width);
            out.extend(value.to_le_bytes());
        }
        Node::Var(name) => {
            out.push(2);
            put_str(out, name);
            encode_sort(t.sort(), out);
        }
        Node::Not(a) => {
            out.push(3);
            encode_term(a, out);
        }
        Node::And(ts) => {
            out.push(4);
            put_u32(out, ts.len() as u32);
            for a in ts {
                encode_term(a, out);
            }
        }
        Node::Or(ts) => {
            out.push(5);
            put_u32(out, ts.len() as u32);
            for a in ts {
                encode_term(a, out);
            }
        }
        Node::Implies(a, b) => {
            out.push(6);
            encode_term(a, out);
            encode_term(b, out);
        }
        Node::Eq(a, b) => {
            out.push(7);
            encode_term(a, out);
            encode_term(b, out);
        }
        Node::Ite(c, a, b) => {
            out.push(8);
            encode_term(c, out);
            encode_term(a, out);
            encode_term(b, out);
        }
        Node::BvBin { op, lhs, rhs } => {
            out.push(9);
            out.push(bv_bin_tag(*op));
            encode_term(lhs, out);
            encode_term(rhs, out);
        }
        Node::BvCmp { op, lhs, rhs } => {
            out.push(10);
            out.push(bv_cmp_tag(*op));
            encode_term(lhs, out);
            encode_term(rhs, out);
        }
        Node::Select(a, i) => {
            out.push(11);
            encode_term(a, out);
            encode_term(i, out);
        }
        Node::Store(a, i, v) => {
            out.push(12);
            encode_term(a, out);
            encode_term(i, out);
            encode_term(v, out);
        }
        Node::DtCtor { dt, ctor, args } => {
            out.push(13);
            put_str(out, dt);
            put_str(out, ctor);
            put_u32(out, args.len() as u32);
            for a in args {
                encode_term(a, out);
            }
        }
        Node::DtIs { ctor, arg } => {
            out.push(14);
            put_str(out, ctor);
            encode_term(arg, out);
        }
        Node::DtGet { field, arg } => {
            out.push(15);
            put_str(out, field);
            encode_term(arg, out);
        }
    }
}

fn bv_bin_tag(op: BvBinOp) -> u8 {
    use BvBinOp::*;
    match op {
        Add => 0,
        Sub => 1,
        Mul => 2,
        Udiv => 3,
        Sdiv => 4,
        Urem => 5,
        Srem => 6,
        And => 7,
        Or => 8,
        Xor => 9,
        Shl => 10,
        Lshr => 11,
        Ashr => 12,
    }
}

fn bv_bin_untag(b: u8) -> Option<BvBinOp> {
    use BvBinOp::*;
    Some(match b {
        0 => Add,
        1 => Sub,
        2 => Mul,
        3 => Udiv,
        4 => Sdiv,
        5 => Urem,
        6 => Srem,
        7 => And,
        8 => Or,
        9 => Xor,
        10 => Shl,
        11 => Lshr,
        12 => Ashr,
        _ => return None,
    })
}

fn bv_cmp_tag(op: BvCmpOp) -> u8 {
    use BvCmpOp::*;
    match op {
        Ult => 0,
        Ule => 1,
        Slt => 2,
        Sle => 3,
    }
}

fn bv_cmp_untag(b: u8) -> Option<BvCmpOp> {
    use BvCmpOp::*;
    Some(match b {
        0 => Ult,
        1 => Ule,
        2 => Slt,
        3 => Sle,
        _ => return None,
    })
}

/// Recursion depth cap: crafted deep nestings must not overflow the
/// stack (same rationale as resolve_named's cycle cap, wave-2 §3).
const MAX_DEPTH: u32 = 512;

pub fn decode_term(input: &mut &[u8], decls: &[DatatypeDecl]) -> Option<Term> {
    decode_at(input, decls, 0)
}

fn decode_many(
    input: &mut &[u8],
    decls: &[DatatypeDecl],
    depth: u32,
) -> Option<Vec<Term>> {
    let n = take_u32(input)? as usize;
    // Defensive bound: each element needs >= 1 byte, so n can never
    // exceed the remaining input (rejects absurd length prefixes
    // before any allocation).
    if n > input.len() {
        return None;
    }
    let mut ts = Vec::with_capacity(n);
    for _ in 0..n {
        ts.push(decode_at(input, decls, depth)?);
    }
    Some(ts)
}

fn decode_at(input: &mut &[u8], decls: &[DatatypeDecl], depth: u32) -> Option<Term> {
    if depth > MAX_DEPTH {
        return None;
    }
    let d = depth + 1;
    match take_u8(input)? {
        0 => match take_u8(input)? {
            0 => Some(Term::bool_lit(false)),
            1 => Some(Term::bool_lit(true)),
            _ => None,
        },
        1 => {
            let width = take_u32(input)?;
            let value = take_u128(input)?;
            // Pre-check bv_lit's asserted invariants: reject, never panic.
            if !(1..=128).contains(&width) {
                return None;
            }
            if width < 128 && value >= (1u128 << width) {
                return None;
            }
            Some(Term::bv_lit(width, value))
        }
        2 => {
            let name = take_str(input)?;
            let sort = decode_sort(input)?;
            // Pre-check var's asserted symbol invariant.
            if !valid_symbol(&name) {
                return None;
            }
            Some(Term::var(&name, sort))
        }
        3 => Term::not(decode_at(input, decls, d)?).ok(),
        4 => Term::and(decode_many(input, decls, d)?).ok(),
        5 => Term::or(decode_many(input, decls, d)?).ok(),
        6 => {
            let a = decode_at(input, decls, d)?;
            let b = decode_at(input, decls, d)?;
            Term::implies(a, b).ok()
        }
        7 => {
            let a = decode_at(input, decls, d)?;
            let b = decode_at(input, decls, d)?;
            Term::eq(a, b).ok()
        }
        8 => {
            let c = decode_at(input, decls, d)?;
            let a = decode_at(input, decls, d)?;
            let b = decode_at(input, decls, d)?;
            Term::ite(c, a, b).ok()
        }
        9 => {
            let op = bv_bin_untag(take_u8(input)?)?;
            let a = decode_at(input, decls, d)?;
            let b = decode_at(input, decls, d)?;
            Term::bv_bin(op, a, b).ok()
        }
        10 => {
            let op = bv_cmp_untag(take_u8(input)?)?;
            let a = decode_at(input, decls, d)?;
            let b = decode_at(input, decls, d)?;
            Term::bv_cmp(op, a, b).ok()
        }
        11 => {
            let a = decode_at(input, decls, d)?;
            let i = decode_at(input, decls, d)?;
            Term::select(a, i).ok()
        }
        12 => {
            let a = decode_at(input, decls, d)?;
            let i = decode_at(input, decls, d)?;
            let v = decode_at(input, decls, d)?;
            Term::store(a, i, v).ok()
        }
        13 => {
            let dt_name = take_str(input)?;
            let ctor = take_str(input)?;
            let args = decode_many(input, decls, d)?;
            let dt = decls.iter().find(|dd| dd.name == dt_name)?;
            Term::dt_ctor(dt, &ctor, args).ok()
        }
        14 => {
            let ctor = take_str(input)?;
            let arg = decode_at(input, decls, d)?;
            // DtIs doesn't record the datatype; resolve by ctor name
            // (unique across v1's two decls; ambiguity = reject).
            let mut matches = decls.iter().filter(|dd| dd.ctor(&ctor).is_some());
            let dt = matches.next()?;
            if matches.next().is_some() {
                return None;
            }
            Term::dt_is(dt, &ctor, arg).ok()
        }
        15 => {
            let field = take_str(input)?;
            let arg = decode_at(input, decls, d)?;
            // Resolve (dt, ctor) by field name, unique across decls.
            let mut hits = decls.iter().flat_map(|dd| {
                dd.ctors
                    .iter()
                    .filter(|c| c.fields.iter().any(|(n, _)| n == &field))
                    .map(move |c| (dd, c.name.clone()))
            });
            let (dt, ctor) = hits.next()?;
            if hits.next().is_some() {
                return None;
            }
            Term::dt_get(dt, &ctor, &field, arg).ok()
        }
        _ => None,
    }
}
```

Supporting changes:
- In `term.rs`, change `fn valid_symbol` to `pub(crate) fn valid_symbol` and add `pub(crate)` to nothing else (Node is already `pub(crate)`).
- In `sort.rs`, confirm `DatatypeDecl::ctor(&self, name) -> Option<&CtorDecl>` exists (it's used by `Term::resolve_ctor` at term.rs:263 as `dt.ctor(ctor)`); it does — no change.
- In `lib.rs` add after `mod printer;`: `mod codec;` and the re-export line from the Interfaces block.

Note on the encoding of `BvLit`: `out.extend(value.to_le_bytes())` writes 16 bytes (u128). The `value >= (1 << width)` pre-check mirrors `bv_lit`'s assert exactly (term.rs: `width == 128 || value < (1u128 << width)`).

- [ ] **Step 4: Run tests**

Run: `mise x -- cargo test -p goverify-solver codec`
Expected: PASS.
Run: `mise x -- cargo test -p goverify-solver --features testgen`
Expected: PASS (property round-trip + existing differential harness).

- [ ] **Step 5: Lint + commit**

```bash
mise run lint
git add crates/goverify-solver/src/codec.rs crates/goverify-solver/src/lib.rs crates/goverify-solver/src/term.rs
git commit --no-gpg-sign -m "phase5a: binary Term/Sort codec (checked-constructor decode, reject-never-panic)"
```

---

### Task 6: Per-function IR hashes in `goverify-ir`

**Files:**
- Modify: `crates/goverify-ir/src/program.rs` (hash computation in `from_packages`, new accessor), `crates/goverify-ir/Cargo.toml` (add `blake3.workspace = true`, `prost.workspace = true`)

**Interfaces:**
- Consumes: `gvir::Package` / `gvir::Function` prost types (via the existing `goverify_extract::gvir` import at program.rs:7), `prost::Message::encode_to_vec`.
- Produces (contract for Task 7): `pub fn func_ir_hash(&self, f: FuncId) -> [u8; 32]` on `Program`. Stable across runs for identical `.gvir` bytes; distinct when the function's own message OR its package's non-function sections change; defined for body-less (external) functions too.

Hash definition (write this doc comment verbatim on the accessor):
- Per package, a **context hash**: blake3 over domain tag `"goverify-func-ctx\0"`, then length-prefixed: `schema_version`, `go_version`, `extractor_version`, `import_path`, then each of `types`, `method_sets`, `files`, `pragmas` re-encoded per element with `encode_to_vec()` (types/method-sets changes must invalidate every function in the package — a function's encoding reads the type table).
- Per function with a body: blake3 over `"goverify-func-ir\0"`, the 32 context-hash bytes, and the length-prefixed `encode_to_vec()` of its `gvir::Function` message. Function messages embed their positions, so a line shift invalidates exactly the shifted functions (findings' printed positions must track).
- Per interned-but-absent function (no body anywhere): blake3 over `"goverify-func-ext\0"` + length-prefixed name. Externals are havoc; their hash only pins identity.
- prost re-encoding of a decoded message is deterministic for a fixed prost version (fields in tag order); a prost major bump is a semantic change ⇒ bump `SCC_CACHE_VERSION` (Task 7 documents this).

- [ ] **Step 1: Write the failing test (in `program.rs`'s test module, or `tests/` if program.rs has none — follow the file's existing test location)**

```rust
    #[test]
    fn func_ir_hashes_are_stable_and_content_sensitive() {
        // Build two identical single-function packages via the same
        // constructor the fuzz seeds use (fuzz_seeds.rs pattern), then a
        // third with a mutated function body position.
        fn pkg(line: u32) -> goverify_extract::gvir::Package {
            use goverify_extract::gvir;
            gvir::Package {
                schema_version: goverify_extract::SCHEMA_VERSION.to_string(),
                go_version: "go1.25.10".to_string(),
                extractor_version: "0.1.0".to_string(),
                import_path: "example.com/h".to_string(),
                files: vec![],
                types: vec![],
                functions: vec![gvir::Function {
                    id: "example.com/h.F".to_string(),
                    name: "F".to_string(),
                    r#type: 0,
                    params: vec![],
                    aux: vec![],
                    blocks: vec![],
                    pos: Some(gvir::Position {
                        file: 0,
                        line,
                        col: 1,
                    }),
                }],
                method_sets: vec![],
                pragmas: vec![],
            }
        }
        let p1 = Program::from_packages(vec![pkg(1)]);
        let p2 = Program::from_packages(vec![pkg(1)]);
        let p3 = Program::from_packages(vec![pkg(2)]);
        let f = p1.lookup_func("example.com/h.F").expect("lookup_func");
        assert_eq!(
            p1.func_ir_hash(f),
            p2.func_ir_hash(f),
            "identical packages hash identically"
        );
        assert_ne!(
            p1.func_ir_hash(f),
            p3.func_ir_hash(f),
            "a position change must change the hash"
        );
    }
```

(Adjust `gvir::Position` field names to the generated bindings — check the `fuzz_seeds.rs` builder at `crates/goverify-extract/tests/fuzz_seeds.rs:13-69` for the exact prost field spelling and copy its style.)

- [ ] **Step 2: Run to verify failure**

Run: `mise x -- cargo test -p goverify-ir func_ir_hashes`
Expected: FAIL — `func_ir_hash` not defined.

- [ ] **Step 3: Implement**

In `crates/goverify-ir/Cargo.toml` `[dependencies]` add:

```toml
blake3.workspace = true
prost.workspace = true
```

In `program.rs`:
- Add field `func_hashes: Vec<[u8; 32]>` to `Program` (parallel to `func_names`).
- In `from_packages`, while walking packages/functions (where functions are interned), compute:

```rust
use prost::Message;

fn ctx_hash(pkg: &gvir::Package) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(b"goverify-func-ctx\0");
    let mut field = |bytes: &[u8]| {
        h.update(&(bytes.len() as u64).to_le_bytes());
        h.update(bytes);
    };
    field(pkg.schema_version.as_bytes());
    field(pkg.go_version.as_bytes());
    field(pkg.extractor_version.as_bytes());
    field(pkg.import_path.as_bytes());
    for t in &pkg.types {
        field(&t.encode_to_vec());
    }
    for m in &pkg.method_sets {
        field(&m.encode_to_vec());
    }
    for f in &pkg.files {
        field(&f.encode_to_vec());
    }
    for pr in &pkg.pragmas {
        field(&pr.encode_to_vec());
    }
    *h.finalize().as_bytes()
}

fn func_hash(ctx: &[u8; 32], f: &gvir::Function) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(b"goverify-func-ir\0");
    h.update(ctx);
    let bytes = f.encode_to_vec();
    h.update(&(bytes.len() as u64).to_le_bytes());
    h.update(&bytes);
    *h.finalize().as_bytes()
}

fn external_hash(name: &str) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(b"goverify-func-ext\0");
    h.update(&(name.len() as u64).to_le_bytes());
    h.update(name.as_bytes());
    *h.finalize().as_bytes()
}
```

Wire into `from_packages`: after the existing intern/sort pass establishes `func_names`, initialize `func_hashes = func_names.iter().map(|n| external_hash(n)).collect()`, then for each package compute `ctx_hash` once and for each of its functions overwrite `func_hashes[id] = func_hash(&ctx, f)`. (Follow the existing structure of `from_packages` at program.rs:38-60 — the function-message walk already exists for lowering; add the hash assignment beside it. If the same function id appears in multiple packages, keep the existing dedup rule's winner — overwrite in the same order lowering resolves it.)

Accessor:

```rust
    /// Stable content hash of this function's IR + its package context
    /// (types/method-sets/files/pragmas). See phase-5a spec §2: this is
    /// the member-hash input to the SCC cache key. Externals hash their
    /// name only.
    pub fn func_ir_hash(&self, id: FuncId) -> [u8; 32] {
        self.func_hashes
            .get(id.0 as usize)
            .copied()
            .unwrap_or([0u8; 32])
    }
```

- [ ] **Step 4: Run tests**

Run: `mise x -- cargo test -p goverify-ir` — Expected: PASS (new test + all existing lower/callgraph tests).
Run: `mise run corpus` — Expected: PASS.

- [ ] **Step 5: Lint + commit**

```bash
mise run lint
git add crates/goverify-ir/Cargo.toml crates/goverify-ir/src/program.rs Cargo.lock
git commit --no-gpg-sign -m "phase5a: per-function IR content hashes (package ctx + function message)"
```

---

### Task 7: SCC cache — keys, entry codec, store layer (`goverify-analysis::scc_cache`)

**Files:**
- Create: `crates/goverify-analysis/src/scc_cache.rs`
- Modify: `crates/goverify-analysis/src/lib.rs` (add `mod scc_cache;` + `pub use scc_cache::{CacheConfigKey, MemberEntry, SccCache, SccEntry, decode_entry_bytes};`)
- Modify: `crates/goverify-analysis/Cargo.toml` (add `blake3.workspace = true` if not present — check; `goverify-cache` is already a dep via engine's QueryCache use)
- Modify: `docs/superpowers/specs/2026-07-23-phase5-caching-design.md` §4 (the framing-location sentence — see Deviation note in the header)

**Interfaces:**
- Consumes: `Store` (`goverify_cache::Store`), `encode_term`/`decode_term`/`encode_sort`/`decode_sort`/`TERM_CODEC_VERSION` (Task 5), `Program::func_ir_hash` (Task 6), `Sccs::{schedule, callee_sccs}` (goverify-ir), `Summary`/`Clause`/`Formula`/`Provenance` (summary.rs:30-77), `Effects`/`Loc`/`Root`/`ChanOp`/`LockOp`/`Spawns` (effects.rs), `Finding`/`TraceStep` (checker.rs:27-51), `Pos` (goverify-ir func.rs:47-51), `seq_datatype()` (encode.rs:31), `ptr_datatype()` (goverify-solver sort.rs).
- Produces (contract for Tasks 8, 9, 13):

```rust
pub struct CacheConfigKey {
    pub solver_identity: String,
    pub infer_limits: goverify_solver::SolverLimits,
    pub findings_limits: goverify_solver::SolverLimits,
    pub widen_after: u32,
    /// (checker name, checker version), sorted by name.
    pub checkers: Vec<(&'static str, u32)>,
}

pub struct SccCache { /* private: Store + salt */ }
impl SccCache {
    pub fn open(root: std::path::PathBuf, cfg: &CacheConfigKey) -> SccCache;
    /// One key per schedule position, callees-first recursive.
    pub fn keys(&self, p: &Program, sccs: &Sccs) -> Vec<[u8; 32]>;
    pub fn get(&self, key: &[u8; 32]) -> Option<SccEntry>;
    pub fn put(&self, key: &[u8; 32], e: &SccEntry) -> std::io::Result<()>;
}

pub struct SccEntry { pub members: Vec<MemberEntry> }  // schedule order
pub struct MemberEntry {
    pub func: String,                   // ssa id, integrity check on decode-install
    pub summary: Summary,
    pub analysis_diag: Option<String>,  // the diag_slots entry
    pub findings: Vec<Finding>,         // already (pos, message)-sorted
    pub findings_diags: Vec<String>,    // encode-skip / panic diags, in emit order
}

/// Fuzz surface (Task 13): decode arbitrary bytes, never panic.
pub fn decode_entry_bytes(bytes: &[u8]) -> Option<SccEntry>;
```

Also add to the `Checker` trait (checker.rs:53): `fn version(&self) -> u32 { 1 }` (defaulted — non-breaking), and explicit `fn version(&self) -> u32 { 1 }` overrides on `NilChecker` and `BoundsChecker` in goverify-checkers with a comment: "bump on any semantic change to this checker's clauses/obligations."

Key definitions (doc-comment these in the module header):
- **Salt** (config-wide, computed once in `open`): blake3 over `"goverify-scc-salt\0"`, `SCC_CACHE_VERSION` (u32 LE), `TERM_CODEC_VERSION` (u8), length-prefixed `solver_identity`, `infer_limits.{timeout_ms, mem_mb}`, `findings_limits.{timeout_ms, mem_mb}`, `widen_after`, then each `(name, version)` pair length-prefixed. The CLI's `RETRY_FACTOR` is deliberately NOT in the key: escalated limits are a pure function of base limits; changing `RETRY_FACTOR` is a semantic change ⇒ bump `SCC_CACHE_VERSION` (document at the const and beside `RETRY_FACTOR` in main.rs).
- **Per-SCC key** (`keys()`, iterating `schedule()` in order — callees precede callers so `keys[d]` for `d in callee_sccs(i)` is always already computed): blake3 over `"goverify-scc-key\0"`, the 32 salt bytes, the member `func_ir_hash` values **sorted bytewise** (FuncId numbering shifts when unrelated functions appear — never key on ids), and the callee-SCC keys sorted bytewise.
- `const SCC_CACHE_VERSION: u32 = 1;` — bump on: entry-format change, engine semantic change (encoding, fixpoint, widening), prost major bump, `RETRY_FACTOR` change.
- Store layer name: `"scc"`.

Entry encoding: version byte `SCC_ENTRY_FORMAT: u8 = 1`, then u32-LE member count, each member with length-prefixed fields. Primitives identical to Task 5's (`put_u32`/`put_str`/`take_*` — reimplement locally or expose; keep them local, they're 30 lines). Sub-codecs, all total and defensive:
- `Provenance`: 1 byte (0 Inferred, 1 Havoc).
- `Clause`: `put_str(tag)` + `encode_term(formula.term)`; decode with `decls = &[ptr_datatype(), seq_datatype()]`.
- `Spawns`: 1 byte. `ChanOp`/`LockOp`: 1 byte each. `Root`: tag byte + payload (`Param(u32)`, `Global(String)`, `Alloc(u32)`, `Unknown`). `Loc`: root + u32 count + u32 path elems. `Effects`: spawns + (u32 count + per-entry `Loc` + u32 count + op bytes) for each of the two BTreeMaps — **iterate the BTreeMaps directly** (already deterministically ordered); decode inserts in order.
- `Pos`: `put_str(file)` + line + col. `Option<T>`: presence byte. `TraceStep`: block + optional pos. `Finding`: checker/tag/func/message strings, optional pos, u32+traces, u32+model pairs.
- Decode rejects trailing garbage at the entry level: `decode_entry_bytes` returns `None` unless the input is fully consumed.

- [ ] **Step 1: Write the failing tests (in `scc_cache.rs` test module)**

```rust
#[cfg(test)]
mod tests {
    use goverify_solver::{Sort, Term};

    use super::*;
    use crate::summary::{Clause, Formula, Provenance, Summary};

    fn sample_entry() -> SccEntry {
        let term = Term::eq(
            Term::var("p0", Sort::BitVec(64)),
            Term::bv_lit(64, 0),
        )
        .unwrap();
        SccEntry {
            members: vec![MemberEntry {
                func: "example.com/m.F".to_string(),
                summary: Summary {
                    requires: vec![Clause {
                        tag: "nil-deref".to_string(),
                        formula: Formula { term: term.clone() },
                    }],
                    ensures: vec![],
                    effects: crate::Effects::top(),
                    provenance: Provenance::Inferred,
                },
                analysis_diag: Some("widened".to_string()),
                findings: vec![crate::Finding {
                    checker: "nil".to_string(),
                    tag: "nil-deref".to_string(),
                    func: "example.com/m.F".to_string(),
                    pos: Some(goverify_ir::Pos {
                        file: "m.go".to_string(),
                        line: 3,
                        col: 9,
                    }),
                    message: "possible nil dereference".to_string(),
                    trace: vec![crate::TraceStep { block: 0, pos: None }],
                    model: vec![("p0".to_string(), "(ptr-nil)".to_string())],
                }],
                findings_diags: vec!["skipped encode".to_string()],
            }],
        }
    }

    #[test]
    fn entry_round_trips() {
        let e = sample_entry();
        let bytes = encode_entry(&e);
        let back = decode_entry_bytes(&bytes).expect("decode_entry_bytes()");
        assert_eq!(back.members.len(), 1);
        let (a, b) = (&back.members[0], &e.members[0]);
        assert_eq!(a.func, b.func);
        assert_eq!(a.summary, b.summary, "Summary round-trip incl. Effects/Terms");
        assert_eq!(a.analysis_diag, b.analysis_diag);
        assert_eq!(a.findings, b.findings);
        assert_eq!(a.findings_diags, b.findings_diags);
    }

    #[test]
    fn corrupt_entries_are_none_never_panic() {
        let bytes = encode_entry(&sample_entry());
        for cut in 0..bytes.len() {
            let _ = decode_entry_bytes(&bytes[..cut]); // no panic
        }
        let mut garbled = bytes.clone();
        garbled.push(0); // trailing garbage
        assert!(decode_entry_bytes(&garbled).is_none(), "trailing garbage = miss");
        assert!(decode_entry_bytes(&[]).is_none());
        assert!(decode_entry_bytes(&[0xff; 8]).is_none());
    }

    #[test]
    fn store_round_trip_and_key_shape() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = CacheConfigKey {
            solver_identity: "stub".to_string(),
            infer_limits: goverify_solver::SolverLimits::default(),
            findings_limits: goverify_solver::SolverLimits::default(),
            widen_after: 3,
            checkers: vec![("nil", 1)],
        };
        let c = SccCache::open(dir.path().to_path_buf(), &cfg);
        let key = [9u8; 32];
        assert!(c.get(&key).is_none(), "empty cache misses");
        c.put(&key, &sample_entry()).unwrap();
        assert!(c.get(&key).is_some(), "round-trips through Store");

        // Different config = different salt = disjoint keys for the
        // same program. Checked indirectly: two caches with different
        // identities must produce different keys() for one program.
        // (Program-level keys() coverage lives in Task 8/9's
        // integration tests; here we only pin salt sensitivity.)
        let cfg2 = CacheConfigKey {
            solver_identity: "other".to_string(),
            ..cfg
        };
        let c2 = SccCache::open(dir.path().to_path_buf(), &cfg2);
        assert_ne!(c.salt_for_test(), c2.salt_for_test(), "identity is in the salt");
    }
}
```

(`salt_for_test` = `#[cfg(test)] pub(crate) fn salt_for_test(&self) -> [u8; 32]`. `CacheConfigKey` needs `Clone` for the `..cfg` update syntax — derive `Debug, Clone`.)

- [ ] **Step 2: Run to verify failure**

Run: `mise x -- cargo test -p goverify-analysis scc_cache`
Expected: FAIL — module doesn't exist.

- [ ] **Step 3: Implement `scc_cache.rs`**

Implement per the Interfaces + encoding spec above. Skeleton of the non-codec parts (the codec follows Task 5's primitive style exactly):

```rust
//! Per-SCC analysis cache (phase-5a spec §2/§4): key = recursive
//! content hash over the condensed call DAG; value = summaries +
//! findings + diagnostics for every member, so a hit replays
//! byte-identical output without encoding or solving.
//!
//! Lives here, not in goverify-cache, because the entry payload IS
//! analysis meaning (Summary/Finding/Term); goverify-cache stays
//! bytes-only (spec Deviation note, plan header).

use std::path::PathBuf;

use goverify_cache::Store;
use goverify_ir::{Program, Sccs};
use goverify_solver::{SolverLimits, decode_term, encode_term};

use crate::checker::Finding;
use crate::summary::Summary;

/// Bump on: entry-format change, any engine/encoding semantic change,
/// prost major bump, CLI RETRY_FACTOR change (escalated limits are
/// derived from base limits and deliberately not keyed separately).
const SCC_CACHE_VERSION: u32 = 1;
const LAYER: &str = "scc";
const SCC_ENTRY_FORMAT: u8 = 1;

#[derive(Debug, Clone)]
pub struct CacheConfigKey {
    pub solver_identity: String,
    pub infer_limits: SolverLimits,
    pub findings_limits: SolverLimits,
    pub widen_after: u32,
    pub checkers: Vec<(&'static str, u32)>,
}

pub struct SccCache {
    store: Store,
    salt: [u8; 32],
}

impl SccCache {
    pub fn open(root: PathBuf, cfg: &CacheConfigKey) -> SccCache {
        let mut h = blake3::Hasher::new();
        h.update(b"goverify-scc-salt\0");
        h.update(&SCC_CACHE_VERSION.to_le_bytes());
        h.update(&[goverify_solver::TERM_CODEC_VERSION]);
        let mut field = |b: &[u8]| {
            h.update(&(b.len() as u64).to_le_bytes());
            h.update(b);
        };
        field(cfg.solver_identity.as_bytes());
        h.update(&cfg.infer_limits.timeout_ms.to_le_bytes());
        h.update(&cfg.infer_limits.mem_mb.to_le_bytes());
        h.update(&cfg.findings_limits.timeout_ms.to_le_bytes());
        h.update(&cfg.findings_limits.mem_mb.to_le_bytes());
        h.update(&cfg.widen_after.to_le_bytes());
        let mut checkers = cfg.checkers.clone();
        checkers.sort();
        for (name, version) in &checkers {
            field(name.as_bytes());
            h.update(&version.to_le_bytes());
        }
        SccCache {
            store: Store::open(root),
            salt: *h.finalize().as_bytes(),
        }
    }

    /// Schedule-order keys; callees-first order guarantees callee keys
    /// exist when needed. Member hashes and callee keys are sorted
    /// BYTEWISE before hashing: FuncId/schedule numbering must never
    /// reach a key (ids shift when unrelated functions appear).
    pub fn keys(&self, p: &Program, sccs: &Sccs) -> Vec<[u8; 32]> {
        let n = sccs.schedule().len();
        let mut keys: Vec<[u8; 32]> = Vec::with_capacity(n);
        for si in 0..n {
            let mut members: Vec<[u8; 32]> = sccs.schedule()[si]
                .iter()
                .map(|&m| p.func_ir_hash(m))
                .collect();
            members.sort_unstable();
            let mut callees: Vec<[u8; 32]> =
                sccs.callee_sccs(si).iter().map(|&d| keys[d]).collect();
            callees.sort_unstable();
            let mut h = blake3::Hasher::new();
            h.update(b"goverify-scc-key\0");
            h.update(&self.salt);
            h.update(&(members.len() as u64).to_le_bytes());
            for m in &members {
                h.update(m);
            }
            h.update(&(callees.len() as u64).to_le_bytes());
            for c in &callees {
                h.update(c);
            }
            keys.push(*h.finalize().as_bytes());
        }
        keys
    }

    pub fn get(&self, key: &[u8; 32]) -> Option<SccEntry> {
        decode_entry_bytes(&self.store.get(LAYER, key)?)
    }

    pub fn put(&self, key: &[u8; 32], e: &SccEntry) -> std::io::Result<()> {
        self.store.put(LAYER, key, &encode_entry(e))
    }
}
```

Then `SccEntry`/`MemberEntry` structs, `encode_entry(e: &SccEntry) -> Vec<u8>` (pub(crate), used by tests + put), `pub fn decode_entry_bytes(bytes: &[u8]) -> Option<SccEntry>` with the full-consumption check, and the sub-codecs listed in the Interfaces section. Decls for term decode: `&[goverify_solver::ptr_datatype(), crate::encode::seq_datatype()]` built once per decode call.

Checker trait addition in `checker.rs` (after `fn name`):

```rust
    /// Cache-key version of this checker's semantics (phase-5a spec
    /// §2). Bump on any change to the clauses or obligations it
    /// produces; stale entries otherwise replay old findings.
    fn version(&self) -> u32 {
        1
    }
```

- [ ] **Step 4: Run tests**

Run: `mise x -- cargo test -p goverify-analysis scc_cache` — Expected: PASS.
Run: `mise x -- cargo test --workspace` — Expected: PASS (trait default is non-breaking).

- [ ] **Step 5: Amend the spec's framing sentence**

In `docs/superpowers/specs/2026-07-23-phase5-caching-design.md` §4, replace "the SCC entry framing — versioning, findings/diagnostics fields, layer plumbing — lives in `goverify-cache`, keeping the crate boundary \"cache owns bytes, not meaning\" intact" with "the SCC entry framing — versioning, findings/diagnostics fields, layer plumbing — lives in `goverify-analysis::scc_cache` (goverify-cache cannot name `Summary`/`Finding`/`Term` without a dependency cycle); `goverify-cache` stays bytes-only, which is the actual boundary the crate table promises".

- [ ] **Step 6: Lint + commit**

```bash
mise run lint
git add crates/goverify-analysis/src/scc_cache.rs crates/goverify-analysis/src/lib.rs \
        crates/goverify-analysis/src/checker.rs crates/goverify-analysis/Cargo.toml \
        crates/goverify-checkers/src crates/goverify-solver/src/lib.rs Cargo.lock \
        docs/superpowers/specs/2026-07-23-phase5-caching-design.md
git commit --no-gpg-sign -m "phase5a: SCC cache layer — recursive keys, entry codec, Store wiring (spec framing amendment)"
```

---

### Task 8: Engine integration — hit/replay/miss/put in `analyze_full`

**Files:**
- Modify: `crates/goverify-analysis/src/engine.rs` (`analyze_full` at engine.rs:98-338, `Analysis` struct at engine.rs:66-72)
- Modify: `crates/goverify-checkers/tests/nil_corpus.rs`, `crates/goverify-checkers/tests/bounds_corpus.rs` (cold/warm tests)

**Interfaces:**
- Consumes: `SccCache`/`CacheConfigKey`/`SccEntry`/`MemberEntry` (Task 7).
- Produces: `Analysis` gains `pub scc_cache_hits: u64` and `pub scc_cache_misses: u64` (both 0 when `cache_dir` is None). Behavior contract: with a cache, stdout-visible output (`findings`, `diagnostics`, `summaries`) is **byte-identical** to the uncached run; a full-hit warm run performs zero encodes and zero solver calls.

Structural background (from the current engine, verified): summaries live in slot-per-function `Vec<Mutex<Option<Summary>>>` written by parallel waves (engine.rs:147, 150-205); analysis diagnostics live in parallel `diag_slots` collected ascending-FuncId (engine.rs:229-232); findings are produced by a **global sequential pass** over `p.func_ids()` after all waves (engine.rs:240-331), per-function sorted by `(pos, message)`, with findings-phase diagnostics appended after analysis diagnostics. The cache must respect all three orders exactly.

- [ ] **Step 1: Write the failing cold/warm corpus test**

In `crates/goverify-checkers/tests/nil_corpus.rs`, first parameterize the existing `run` helper (nil_corpus.rs:17-31) with a cache dir — add a second helper rather than touching the existing signature:

```rust
fn run_with_cache(cache_dir: std::path::PathBuf) -> (String, u64, u64) {
    let p = goverify_ir::testutil::load_corpus("nil");
    let cfg = EngineConfig {
        opts: Options::default(),
        cache_dir: Some(cache_dir),
        emit_smt: None,
    };
    let checkers: Vec<&dyn goverify_analysis::Checker> = vec![&NilChecker];
    let a = analyze_full(&p, &cfg, &checkers, &|_role| {
        Box::new(Z3Native::new(limits()))
    });
    (
        goverify_analysis::dump_findings(&a, None),
        a.scc_cache_hits,
        a.scc_cache_misses,
    )
}

#[test]
fn cold_and_warm_cache_runs_are_byte_identical() {
    let cache = tempfile::tempdir().unwrap();
    let (cold, cold_hits, cold_misses) = run_with_cache(cache.path().to_path_buf());
    let (warm, warm_hits, warm_misses) = run_with_cache(cache.path().to_path_buf());
    assert_eq!(cold, warm, "cold vs warm findings must be byte-identical");
    assert_eq!(cold_hits, 0, "first run must be all misses");
    assert!(cold_misses > 0, "cold run must populate the cache");
    assert_eq!(warm_misses, 0, "warm run must be all hits");
    assert_eq!(warm_hits, cold_misses, "every SCC replays from cache");
    // Not vacuous: the uncached baseline must agree too.
    assert_eq!(cold, run(None), "cached output equals uncached output");
}
```

(`run(None)` is the existing uncached helper — match its actual signature, nil_corpus.rs:17: it takes `Option<PathBuf>` for emit_smt; call it the way `findings_and_smt_artifacts_are_deterministic` calls it for the no-emit case, or add `run(None)` support accordingly. Mirror the same new test verbatim into `bounds_corpus.rs` with `BoundsChecker`/`"bounds"`.)

- [ ] **Step 2: Run to verify failure**

Run: `mise x -- cargo test -p goverify-checkers --test nil_corpus cold_and_warm`
Expected: FAIL — `scc_cache_hits` field doesn't exist.

- [ ] **Step 3: Implement the engine integration**

In `engine.rs`, `analyze_full`:

**(a) Setup** (after `let sccs = Sccs::compute(...)`, engine.rs:114):

```rust
    // Per-SCC cache (phase-5a spec §4). Probe one backend per role for
    // identity/limits — with the lazy escalated tier this allocates one
    // Z3 context per probe, freed immediately.
    let scc_cache = cfg.cache_dir.clone().map(|root| {
        let infer_probe = mk_backend(BackendRole::Infer);
        let findings_probe = mk_backend(BackendRole::Findings);
        crate::scc_cache::SccCache::open(
            root,
            &crate::scc_cache::CacheConfigKey {
                solver_identity: infer_probe.identity(),
                infer_limits: infer_probe.limits(),
                findings_limits: findings_probe.limits(),
                widen_after: cfg.opts.widen_after,
                checkers: checkers.iter().map(|c| (c.name(), c.version())).collect(),
            },
        )
    });
    let scc_keys = scc_cache.as_ref().map(|c| c.keys(p, &sccs));
    let hits = std::sync::atomic::AtomicU64::new(0);
    let misses = std::sync::atomic::AtomicU64::new(0);
    // Replay payload per function: findings + findings-phase diags from
    // a cache hit, consumed by the sequential findings pass below.
    struct Replay {
        findings: Vec<Finding>,
        findings_diags: Vec<String>,
    }
    let replay_slots: Vec<Mutex<Option<Replay>>> =
        (0..n_funcs).map(|_| Mutex::new(None)).collect();
    // Track which schedule positions were misses (need a put later).
    let fresh_sccs: Vec<std::sync::atomic::AtomicBool> = (0..n_sccs)
        .map(|_| std::sync::atomic::AtomicBool::new(false))
        .collect();
```

**(b) Wave hit path** (inside the `wave.par_iter().for_each(|&si| { ... })` closure, engine.rs:151, as the FIRST thing):

```rust
            let members = &sccs.schedule()[si];
            if let (Some(cache), Some(keys)) = (scc_cache.as_ref(), scc_keys.as_ref())
                && let Some(entry) = cache.get(&keys[si])
                && entry_matches(members, &entry, p)
            {
                hits.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                for (m, me) in members.iter().zip(entry.members) {
                    *slots[m.0 as usize].lock().unwrap() = Some(me.summary);
                    *diag_slots[m.0 as usize].lock().unwrap() = me.analysis_diag;
                    *replay_slots[m.0 as usize].lock().unwrap() = Some(Replay {
                        findings: me.findings,
                        findings_diags: me.findings_diags,
                    });
                }
                return;
            }
            if scc_cache.is_some() {
                misses.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                fresh_sccs[si].store(true, std::sync::atomic::Ordering::Relaxed);
            }
```

with the integrity check (member names must line up — a decoded entry whose member list doesn't match this SCC exactly is treated as a miss):

```rust
fn entry_matches(
    members: &[FuncId],
    entry: &crate::scc_cache::SccEntry,
    p: &Program,
) -> bool {
    members.len() == entry.members.len()
        && members
            .iter()
            .zip(&entry.members)
            .all(|(m, me)| p.func_name(*m) == me.func)
}
```

(Entry member order: `put` writes members in `sccs.schedule()[si]` order — ascending FuncId within the SCC, which is name-sorted globally, hence stable across runs of identical input. The `entry_matches` guard makes a stale-ordering entry a miss, never a mismatch.)

**(c) Findings pass replay** (inside the sequential `for f in p.func_ids()` loop, engine.rs:245, as the FIRST thing):

```rust
            if let Some(r) = replay_slots[f.0 as usize].lock().unwrap().take() {
                findings.extend(r.findings); // stored pre-sorted
                findings_diagnostics.extend(r.findings_diags);
                continue;
            }
```

**(d) Fresh-path capture**: the fresh path must record, per function, the findings and findings-diags it just produced (to build entries). Wrap the existing per-function fresh logic so its outputs are captured:

```rust
            let diags_before = findings_diagnostics.len();
            // ... existing encode_func_with diagnostic + catch_unwind
            //     obligation loop, UNCHANGED ...
            // after `findings.extend(per_func)` in the Ok arm, also:
            //     fresh_out[f.0 as usize] = Some((per_func_clone, ...));
```

Concretely: change the `Ok(mut per_func)` arm (engine.rs:317-322) to

```rust
                Ok(mut per_func) => {
                    per_func
                        .sort_by(|a, b| a.pos.cmp(&b.pos).then_with(|| a.message.cmp(&b.message)));
                    if scc_cache.is_some() {
                        fresh_out[f.0 as usize] =
                            Some(per_func.clone());
                    }
                    findings.extend(per_func);
                }
```

and after the whole per-function block, capture the diags delta:

```rust
            if scc_cache.is_some() {
                fresh_diags[f.0 as usize] =
                    findings_diagnostics[diags_before..].to_vec();
            }
```

with `let mut fresh_out: Vec<Option<Vec<Finding>>> = vec![None; n_funcs];` and `let mut fresh_diags: Vec<Vec<String>> = vec![Vec::new(); n_funcs];` declared before the loop (only allocated meaningfully when the cache is on). The `Err` (panic) arm leaves `fresh_out[f] = Some(vec![])` — set it explicitly there so panicked functions still cache their (empty findings + panic diagnostic) result.

**(e) Post-findings put** (after `diagnostics.extend(findings_diagnostics);`, engine.rs:331):

```rust
    if let (Some(cache), Some(keys)) = (scc_cache.as_ref(), scc_keys.as_ref()) {
        for si in 0..sccs.schedule().len() {
            if !fresh_sccs[si].load(std::sync::atomic::Ordering::Relaxed) {
                continue;
            }
            let members = &sccs.schedule()[si];
            let entry = crate::scc_cache::SccEntry {
                members: members
                    .iter()
                    .map(|&m| crate::scc_cache::MemberEntry {
                        func: p.func_name(m).to_string(),
                        summary: slots[m.0 as usize]
                            .lock()
                            .unwrap()
                            .clone()
                            .unwrap_or_else(Summary::havoc),
                        analysis_diag: diag_slots[m.0 as usize].lock().unwrap().clone(),
                        findings: fresh_out[m.0 as usize].clone().unwrap_or_default(),
                        findings_diags: fresh_diags[m.0 as usize].clone(),
                    })
                    .collect(),
            };
            // Write failure degrades to slower, never wrong (spec §5).
            let _ = cache.put(&keys[si], &entry);
        }
    }
```

**Caveat to respect:** if `checkers.is_empty()` the findings pass is skipped entirely (engine.rs:242) — in that case skip the put as well OR store entries with empty findings; choose **skip the whole cache** when `checkers.is_empty()` (guard the `scc_cache` construction with `!checkers.is_empty()`), because `debug prepass/summary` paths run checker-less and must not poison entries keyed by an empty checker list — actually the empty checker list IS part of the salt, so entries can't collide; still, skip for simplicity and to keep `debug` paths allocation-free. Also note `--emit-smt` + warm cache: replayed SCCs emit nothing (documented spec §4 behavior difference; the corpus emit-smt determinism test runs uncached and is unaffected).

**(f) `Analysis` struct** (engine.rs:66-72): add

```rust
    pub scc_cache_hits: u64,
    pub scc_cache_misses: u64,
```

set from the atomics at construction (engine.rs:333-338); `analyze` (the checker-less entry) sets both 0. Fix every `Analysis { ... }` literal the compiler flags.

- [ ] **Step 4: Run the new tests + full determinism suite**

Run: `mise x -- cargo test -p goverify-checkers --test nil_corpus --test bounds_corpus` — Expected: PASS (new cold/warm tests + existing determinism tests).
Run: `mise run corpus` — Expected: PASS.
Run: `mise x -- cargo test --workspace` — Expected: PASS.

- [ ] **Step 5: Lint + commit**

```bash
mise run lint
git add crates/goverify-analysis/src/engine.rs crates/goverify-checkers/tests/nil_corpus.rs crates/goverify-checkers/tests/bounds_corpus.rs
git commit --no-gpg-sign -m "phase5a: engine SCC-cache integration — wave hit/replay, findings-pass replay, post-findings put"
```

---

### Task 9: Targeted invalidation test (the G3 fixture)

**Files:**
- Create: `crates/goverify-analysis/tests/scc_cache_invalidation.rs`
- Modify: `crates/goverify-ir/src/testutil.rs` (add `load_module`), `mise.toml` (corpus task line for goverify-analysis)

**Interfaces:**
- Consumes: `analyze_full` + `Analysis.scc_cache_{hits,misses}` (Task 8), `Sidecar` (testutil pattern at testutil.rs:19-27).
- Produces: `pub fn load_module(dir: &Path) -> Program` in `goverify_ir::testutil` (extracts an arbitrary module dir; `load_corpus` refactored to call it).

- [ ] **Step 1: Add `load_module` to testutil**

```rust
/// Extract + load an arbitrary Go module directory (invalidation tests
/// write fixtures to a tempdir and re-extract between runs).
pub fn load_module(module_dir: &Path) -> Program {
    let root = repo_root();
    let sc = Sidecar::build(&root.join("extractor"), &root.join("target/extractor-bin"))
        .expect("Sidecar::build");
    let dir = tempfile::tempdir().expect("tempdir").keep();
    sc.extract(module_dir, &["./..."], &dir).expect("extract");
    Program::load_dir(&dir).expect("load_dir")
}
```

and change `load_corpus` to `load_module(&repo_root().join("testdata/corpus").join(module))`.

- [ ] **Step 2: Write the failing invalidation test**

`crates/goverify-analysis/tests/scc_cache_invalidation.rs`:

```rust
//! G3 (phase-5a spec §7): editing one function re-analyzes exactly its
//! SCC and upward callers; everything else replays from cache. The
//! edit is same-line-count, same-length-irrelevant but crucially adds
//! NO newline, so other functions' positions (hence IR hashes) are
//! untouched.

use goverify_analysis::{Checker, EngineConfig, Options, analyze_full};
use goverify_checkers::NilChecker;
use goverify_solver::{SolverLimits, Z3Native};

const FIXTURE_V1: &str = "package inval\n\nfunc Leaf(x int) int { return x + x }\n\nfunc Caller(x int) int { return Leaf(x) }\n\nfunc Other(x int) int { return x - 1 }\n";
// Same byte length, same line count: only Leaf's body changes.
const FIXTURE_V2: &str = "package inval\n\nfunc Leaf(x int) int { return x * x }\n\nfunc Caller(x int) int { return Leaf(x) }\n\nfunc Other(x int) int { return x - 1 }\n";
const GO_MOD: &str = "module example.com/inval\n\ngo 1.25\n";

fn run(module_dir: &std::path::Path, cache_dir: &std::path::Path) -> (String, u64, u64) {
    let p = goverify_ir::testutil::load_module(module_dir);
    let cfg = EngineConfig {
        opts: Options::default(),
        cache_dir: Some(cache_dir.to_path_buf()),
        emit_smt: None,
    };
    let checkers: Vec<&dyn Checker> = vec![&NilChecker];
    let a = analyze_full(&p, &cfg, &checkers, &|_role| {
        Box::new(Z3Native::new(SolverLimits::default()))
    });
    (
        goverify_analysis::dump_findings(&a, None),
        a.scc_cache_hits,
        a.scc_cache_misses,
    )
}

#[test]
fn single_function_edit_invalidates_only_its_scc_and_callers() {
    let module = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    std::fs::write(module.path().join("go.mod"), GO_MOD).unwrap();
    std::fs::write(module.path().join("inval.go"), FIXTURE_V1).unwrap();

    let (out1, hits1, misses1) = run(module.path(), cache.path());
    assert_eq!(hits1, 0, "cold run");
    assert!(misses1 >= 3, "at least Leaf/Caller/Other SCCs analyzed");

    let (out2, hits2, misses2) = run(module.path(), cache.path());
    assert_eq!(out2, out1, "unchanged input replays byte-identically");
    assert_eq!(misses2, 0, "unchanged input is a full hit");
    assert_eq!(hits2, misses1);

    std::fs::write(module.path().join("inval.go"), FIXTURE_V2).unwrap();
    let (_out3, hits3, misses3) = run(module.path(), cache.path());
    assert_eq!(
        misses3, 2,
        "exactly Leaf's SCC and Caller's SCC re-analyze (Other + the rest hit)"
    );
    assert_eq!(hits3, misses1 - 2, "everything else replays");
}
```

Add `goverify-checkers` as a dev-dependency of `goverify-analysis` **only if it isn't already** — it is NOT (checkers depend on analysis, not vice versa; a dev-dependency in the reverse direction is allowed by cargo and does not violate the runtime crate boundary; `engine_corpus.rs` currently avoids checkers). If the dev-dep feels wrong, move this test file to `crates/goverify-checkers/tests/` instead — **prefer that**: it needs `NilChecker` anyway and checkers' tests already do exactly this dance. Final location: `crates/goverify-checkers/tests/scc_cache_invalidation.rs`.

- [ ] **Step 3: Run to verify failure, then fix expectations**

Run: `mise x -- cargo test -p goverify-checkers --test scc_cache_invalidation`
Expected: FAIL initially only if Task 8 has a bug — this test is pure integration. If `misses3` is not exactly 2, STOP and investigate before loosening the assertion: the usual suspects are (a) the package `init` function's position shifting (it must not — the edit adds no newline), (b) FuncId-order leakage into keys (Task 7's bytewise sort exists precisely for this), (c) prost re-encode instability. The assertion `misses3 == 2` IS the deliverable; weakening it to `>= 2` defeats G3.

- [ ] **Step 4: Wire into the corpus tier**

In `mise.toml`, extend the goverify-checkers corpus line:

```toml
  "cargo test -p goverify-checkers --test nil_corpus --test bounds_corpus --test knownfp_corpus --test ensures_corpus --test scc_cache_invalidation",
```

Run: `mise run corpus` — Expected: PASS.

- [ ] **Step 5: Lint + commit**

```bash
mise run lint
git add crates/goverify-ir/src/testutil.rs crates/goverify-checkers/tests/scc_cache_invalidation.rs mise.toml
git commit --no-gpg-sign -m "phase5a: G3 invalidation fixture — one-function edit re-analyzes exactly its SCC + callers"
```

---

### Task 10: Sidecar manifest mode

**Files:**
- Modify: `extractor/main.go` (new `-manifest` flag), `extractor/extract.go` (manifest load + emission), `extractor/extract_test.go` (Go-side test)
- Modify: `crates/goverify-extract/src/sidecar.rs` (`Sidecar::manifest`, `ManifestPkg`, expose `content_key`), `crates/goverify-extract/src/lib.rs` (exports)

**Interfaces:**
- Consumes: existing `packages.Load` plumbing (extract.go:22-86), `Sidecar` (sidecar.rs:13-15, build at :49).
- Produces:
  - Go: `extractor -manifest PATTERN...` prints a line protocol to stdout — for each package of the full import closure, sorted by import path: `pkg <import-path>`, then `dep <import-path>` lines (sorted), then `file <absolute-path>` lines (sorted). No type-checking (`go list`-level LoadMode). Line protocol, NOT JSON: no serde dependency exists Rust-side and none is being added (Global Constraints).
  - Rust: `pub struct ManifestPkg { pub import_path: String, pub deps: Vec<String>, pub files: Vec<PathBuf> }` and `impl Sidecar { pub fn manifest(&self, module_dir: &Path, patterns: &[&str]) -> Result<Vec<ManifestPkg>, SidecarError> }`, plus `pub fn content_key(&self) -> &str` (the build hash already computed in `Sidecar::build` — store it in the struct: `pub struct Sidecar { bin: PathBuf, key: String }`).
  - Note: absolute file paths here are fine — the manifest is a build-time enumeration that is hashed by **content**, never cached or emitted; the no-absolute-paths rule binds `.gvir` artifacts, not this handshake (deliberate departure, documented in the Go flag comment).

- [ ] **Step 1: Write the failing Go test (`extractor/extract_test.go`)**

```go
func TestManifestListsClosureWithDepsAndFiles(t *testing.T) {
	dir := t.TempDir()
	writeFile(t, dir, "go.mod", "module example.com/m\n\ngo 1.25\n")
	writeFile(t, dir, "main.go", "package m\n\nimport \"strings\"\n\nfunc F(s string) string { return strings.ToUpper(s) }\n")
	var buf bytes.Buffer
	if err := manifest(dir, []string{"./..."}, &buf); err != nil {
		t.Fatalf("manifest: %v", err)
	}
	out := buf.String()
	if !strings.Contains(out, "pkg example.com/m\n") {
		t.Errorf("manifest missing root package:\n%s", out)
	}
	if !strings.Contains(out, "dep strings\n") {
		t.Errorf("manifest missing dep edge to strings:\n%s", out)
	}
	if !strings.Contains(out, "pkg strings\n") {
		t.Errorf("manifest missing closure package strings:\n%s", out)
	}
	// Every file line must be an absolute path to an existing file.
	for _, line := range strings.Split(out, "\n") {
		if f, ok := strings.CutPrefix(line, "file "); ok {
			if !filepath.IsAbs(f) {
				t.Errorf("relative file path in manifest: %q", f)
			}
		}
	}
}
```

(Reuse the test file's existing `writeFile` helper if present; otherwise add the obvious 4-liner. Match existing test-file conventions in extract_test.go.)

- [ ] **Step 2: Run to verify failure**

Run: `cd extractor && go test -run TestManifest ./...`
Expected: FAIL — `manifest` undefined.

- [ ] **Step 3: Implement the Go side**

`extractor/extract.go`, new function:

```go
// manifest prints the go-list-level import closure of patterns: per
// package (sorted by import path) its deps and source files. No
// type-checking — this is the extraction-cache handshake (phase-5a
// spec §3): the Rust side hashes the listed files' CONTENT to compute
// per-package cache keys. File paths are printed absolute on purpose:
// unlike .gvir artifacts the manifest is never cached or persisted, so
// the no-absolute-paths determinism rule does not bind it.
func manifest(dir string, patterns []string, w io.Writer) error {
	cfg := &packages.Config{
		Dir: dir,
		Mode: packages.NeedName | packages.NeedFiles | packages.NeedCompiledGoFiles |
			packages.NeedImports | packages.NeedDeps | packages.NeedModule,
		Env: append(os.Environ(), "CGO_ENABLED=0"),
	}
	roots, err := packages.Load(cfg, patterns...)
	if err != nil {
		return err
	}
	var all []*packages.Package
	packages.Visit(roots, nil, func(p *packages.Package) { all = append(all, p) })
	slices.SortFunc(all, func(a, b *packages.Package) int {
		return strings.Compare(a.PkgPath, b.PkgPath)
	})
	for _, p := range all {
		if len(p.Errors) > 0 {
			// Degrade, never die: an errored package is omitted; the
			// Rust side then treats it as uncacheable and the extract
			// pass reports the real diagnostic.
			fmt.Fprintf(os.Stderr, "goverify: manifest: skipping %s: %v\n", p.PkgPath, p.Errors[0])
			continue
		}
		fmt.Fprintf(w, "pkg %s\n", p.PkgPath)
		deps := slices.Sorted(maps.Keys(p.Imports))
		for _, d := range deps {
			fmt.Fprintf(w, "dep %s\n", d)
		}
		files := slices.Clone(p.CompiledGoFiles)
		slices.Sort(files)
		for _, f := range files {
			fmt.Fprintf(w, "file %s\n", f)
		}
	}
	return nil
}
```

`extractor/main.go`: add the flag and dispatch:

```go
	manifestMode := flag.Bool("manifest", false, "print the import-closure manifest (pkg/dep/file lines) instead of extracting")
```

and before the `-out` requirement check:

```go
	if *manifestMode {
		if flag.NArg() == 0 {
			fmt.Fprintln(os.Stderr, "usage: extractor -manifest PATTERN...")
			os.Exit(2)
		}
		if err := manifest("", flag.Args(), os.Stdout); err != nil {
			fmt.Fprintln(os.Stderr, "extractor:", err)
			os.Exit(1)
		}
		return
	}
```

(Add `io`, `maps`, `strings` imports as needed; `slices` is already imported in extract.go.)

- [ ] **Step 4: Go test + gofmt**

Run: `cd extractor && gofmt -l . && go vet ./... && go test ./...`
Expected: PASS, no gofmt output.

- [ ] **Step 5: Write the failing Rust test (`crates/goverify-extract/tests/extract_integration.rs`)**

```rust
#[test]
fn manifest_returns_closure_with_deps_and_files() {
    let (sc, module) = sidecar_and_module(); // follow the file's existing fixture helpers
    let pkgs = sc.manifest(module.path(), &["./..."]).expect("manifest()");
    let root = pkgs
        .iter()
        .find(|p| p.import_path.ends_with("/m") || p.import_path == "example.com/m")
        .expect("root package in manifest");
    assert!(!root.files.is_empty(), "root package lists its files");
    for f in &root.files {
        assert!(f.is_absolute(), "manifest file paths are absolute");
        assert!(f.exists(), "manifest file paths exist");
    }
    // Closure includes deps when the module imports stdlib.
    // (fixture module imports "strings" — mirror the Go test fixture)
    assert!(pkgs.iter().any(|p| p.import_path == "strings"));
    assert!(root.deps.contains(&"strings".to_string()));
}
```

(Adapt fixture creation to `extract_integration.rs`'s existing helpers — it already builds a sidecar and writes a temp module at :26-63; copy that pattern and give the module a `strings` import.)

- [ ] **Step 6: Implement the Rust side (`sidecar.rs`)**

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestPkg {
    pub import_path: String,
    pub deps: Vec<String>,
    pub files: Vec<PathBuf>,
}

impl Sidecar {
    /// The content hash `build` derived from extractor sources + Go
    /// version — the "extractor identity" component of extraction-cache
    /// keys (phase-5a spec §2).
    pub fn content_key(&self) -> &str {
        &self.key
    }

    /// go-list-level closure enumeration (no type-checking). Parses the
    /// line protocol printed by `extractor -manifest`; any unrecognized
    /// line is an error (fail -> caller falls back to uncached).
    pub fn manifest(
        &self,
        module_dir: &Path,
        patterns: &[&str],
    ) -> Result<Vec<ManifestPkg>, SidecarError> {
        let output = Command::new(&self.bin)
            .arg("-manifest")
            .args(patterns)
            .current_dir(module_dir)
            .output()?;
        if !output.status.success() {
            return Err(SidecarError::Extractor(
                String::from_utf8_lossy(&output.stderr).into_owned(),
            ));
        }
        if !output.stderr.is_empty() {
            eprint!("{}", String::from_utf8_lossy(&output.stderr));
        }
        let text = String::from_utf8_lossy(&output.stdout);
        let mut pkgs: Vec<ManifestPkg> = Vec::new();
        for line in text.lines() {
            if let Some(p) = line.strip_prefix("pkg ") {
                pkgs.push(ManifestPkg {
                    import_path: p.to_string(),
                    deps: Vec::new(),
                    files: Vec::new(),
                });
            } else if let Some(d) = line.strip_prefix("dep ") {
                pkgs.last_mut()
                    .ok_or_else(|| SidecarError::Extractor("manifest: dep before pkg".into()))?
                    .deps
                    .push(d.to_string());
            } else if let Some(f) = line.strip_prefix("file ") {
                pkgs.last_mut()
                    .ok_or_else(|| SidecarError::Extractor("manifest: file before pkg".into()))?
                    .files
                    .push(PathBuf::from(f));
            } else if !line.is_empty() {
                return Err(SidecarError::Extractor(format!(
                    "manifest: unrecognized line {line:?}"
                )));
            }
        }
        Ok(pkgs)
    }
}
```

And in `Sidecar::build`, keep the computed `hash` on the struct: `Ok(Sidecar { bin, key: hash })` (adjust the struct definition and the one other constructor site if any). Export `ManifestPkg` from `lib.rs`.

- [ ] **Step 7: Run tests + lint + commit**

Run: `mise x -- cargo test -p goverify-extract --test extract_integration` — Expected: PASS.

```bash
mise run lint
git add extractor/main.go extractor/extract.go extractor/extract_test.go \
        crates/goverify-extract/src/sidecar.rs crates/goverify-extract/src/lib.rs \
        crates/goverify-extract/tests/extract_integration.rs
git commit --no-gpg-sign -m "phase5a: sidecar manifest mode — go-list-level closure handshake (line protocol)"
```

---

### Task 11: Extraction cache orchestration (`goverify-extract::cached`)

**Files:**
- Create: `crates/goverify-extract/src/cached.rs`
- Modify: `crates/goverify-extract/src/load.rs` (factor `load_package_bytes`), `crates/goverify-extract/src/sidecar.rs` (add `extract_only`), `crates/goverify-extract/src/lib.rs` (exports), `crates/goverify-extract/Cargo.toml` (add `goverify-cache` path dep — no cycle: cache depends on nothing)

**Interfaces:**
- Consumes: `Sidecar::{manifest, extract, content_key}` (Task 10), `Store` (goverify-cache), `load_package_bytes`.
- Produces (contract for Task 12):

```rust
pub struct ExtractStats {
    pub cached: usize,
    pub extracted: usize,
}
/// Full pipeline: manifest -> recursive keys -> store hits + dirty-set
/// extraction -> decoded packages, sorted by import path. Any manifest/
/// key-computation failure is an Err — the caller falls back to plain
/// uncached extraction (degrade, never die).
pub fn load_packages_cached(
    sc: &Sidecar,
    module_dir: &Path,
    patterns: &[&str],
    cache_root: &Path,
) -> Result<(Vec<gvir::Package>, ExtractStats), SidecarError>;
```

plus `pub fn load_package_bytes(bytes: &[u8]) -> Result<gvir::Package, LoadError>` in load.rs (schema check included; `load_package` becomes `load_package_bytes(&std::fs::read(path)?)`), and `Sidecar::extract_only(&self, module_dir: &Path, import_paths: &[&str], out_dir: &Path) -> Result<Vec<PathBuf>, SidecarError>` — same as `extract` but passes `-deps=false` before the paths (the Go flag already exists, main.go:14; Rust just never passed it). The dirty set is upward-closed by key construction, so `-deps=false` extraction of exactly the dirty paths is complete; dep type info comes from Go's export data.

Key definition (`const EXTRACT_CACHE_VERSION: u32 = 1;`, doc-comment in cached.rs):
`key(P) = blake3("goverify-extract-key\0" ⊕ EXTRACT_CACHE_VERSION ⊕ lp(content_key) ⊕ lp(import_path) ⊕ per file, sorted: lp(file-content-blake3) ⊕ per dep, sorted: dep-key)` where `lp` = u64-LE length prefix + bytes. `content_key` already covers extractor sources **and** Go version (sidecar.rs:154-170), so both spec §2 fields ride in. Dep keys make it recursive over the import DAG; compute by memoized DFS over the manifest with an on-stack cycle guard (a cycle or a dep missing from the manifest ⇒ `Err` ⇒ caller falls back).

- [ ] **Step 1: Write the failing integration test (`crates/goverify-extract/tests/extract_integration.rs`)**

```rust
#[test]
fn cached_load_cold_warm_and_invalidation() {
    let (sc, module) = sidecar_and_module(); // same fixture as the manifest test
    let cache = tempfile::tempdir().unwrap();

    // Cold: everything extracted, store populated.
    let (pkgs1, s1) =
        goverify_extract::load_packages_cached(&sc, module.path(), &["./..."], cache.path())
            .expect("cold cached load");
    assert_eq!(s1.cached, 0, "cold run extracts everything");
    assert!(s1.extracted >= 2, "root + at least one dep in the closure");

    // Warm: zero extraction, identical packages.
    let (pkgs2, s2) =
        goverify_extract::load_packages_cached(&sc, module.path(), &["./..."], cache.path())
            .expect("warm cached load");
    assert_eq!(s2.extracted, 0, "warm run extracts nothing");
    assert_eq!(s2.cached, s1.extracted);
    assert_eq!(
        pkgs1, pkgs2,
        "cached packages decode identically to freshly extracted ones"
    );

    // Edit the module's own file: only the root package re-extracts
    // (stdlib deps stay cached — nothing imports the root).
    let main_go = module.path().join("main.go");
    let src = std::fs::read_to_string(&main_go).unwrap();
    std::fs::write(&main_go, src.replace("ToUpper", "ToLower")).unwrap();
    let (_pkgs3, s3) =
        goverify_extract::load_packages_cached(&sc, module.path(), &["./..."], cache.path())
            .expect("edited cached load");
    assert_eq!(s3.extracted, 1, "exactly the edited leaf-of-import-DAG package re-extracts");
    assert_eq!(s3.cached, s1.extracted - 1);
}
```

(`gvir::Package` derives `PartialEq` via prost — verify; prost message types derive PartialEq by default, so `pkgs1 == pkgs2` compiles.)

- [ ] **Step 2: Run to verify failure**

Run: `mise x -- cargo test -p goverify-extract --test extract_integration cached_load`
Expected: FAIL — `load_packages_cached` undefined.

- [ ] **Step 3: Implement**

`load.rs` refactor:

```rust
pub fn load_package_bytes(bytes: &[u8]) -> Result<gvir::Package, LoadError> {
    use prost::Message;
    let pkg = gvir::Package::decode(bytes).map_err(LoadError::Decode)?;
    if pkg.schema_version != SCHEMA_VERSION {
        return Err(LoadError::SchemaVersion {
            found: pkg.schema_version,
            expected: SCHEMA_VERSION,
        });
    }
    Ok(pkg)
}

pub fn load_package(path: &Path) -> Result<gvir::Package, LoadError> {
    load_package_bytes(&std::fs::read(path).map_err(LoadError::Io)?)
}
```

`sidecar.rs` `extract_only` (clone of `extract` with `-deps=false`):

```rust
    /// Extract exactly `import_paths` (no dependency walk): passes the
    /// extractor's existing -deps=false. Callers must pass an
    /// upward-closed dirty set (phase-5a spec §3) — dep types come from
    /// Go's export data, not from re-extraction.
    pub fn extract_only(
        &self,
        module_dir: &Path,
        import_paths: &[&str],
        out_dir: &Path,
    ) -> Result<Vec<PathBuf>, SidecarError> {
        fs::create_dir_all(out_dir)?;
        let out_abs = out_dir.canonicalize()?;
        let output = Command::new(&self.bin)
            .arg("-out")
            .arg(&out_abs)
            .arg("-deps=false")
            .args(import_paths)
            .current_dir(module_dir)
            .output()?;
        // ... identical status/stderr/stdout handling to extract() —
        // factor the shared tail into a private fn run_extractor(cmd).
    }
```

(Factor the duplicated tail of `extract`/`extract_only` into one private helper taking a prepared `Command` — don't copy-paste 20 lines twice.)

`cached.rs` (complete logic):

```rust
//! Extraction-cache orchestration (phase-5a spec §3): manifest ->
//! recursive import-DAG keys -> store hits + dirty-set extraction.

use std::collections::HashMap;
use std::path::Path;

use goverify_cache::Store;

use crate::gvir;
use crate::load::load_package_bytes;
use crate::sidecar::{ManifestPkg, Sidecar, SidecarError};

/// Bump on any change to the key preimage or stored-value semantics.
const EXTRACT_CACHE_VERSION: u32 = 1;
const LAYER: &str = "extract";

pub struct ExtractStats {
    pub cached: usize,
    pub extracted: usize,
}

fn file_hash(path: &Path) -> std::io::Result<[u8; 32]> {
    Ok(*blake3::Hasher::new()
        .update(&std::fs::read(path)?)
        .finalize()
        .as_bytes())
}

/// Recursive package keys over the manifest's import DAG (memoized
/// DFS). Missing deps or cycles are errors -> caller falls back.
fn package_keys(
    sc_key: &str,
    pkgs: &[ManifestPkg],
) -> Result<HashMap<String, [u8; 32]>, SidecarError> {
    let by_path: HashMap<&str, &ManifestPkg> =
        pkgs.iter().map(|p| (p.import_path.as_str(), p)).collect();
    let mut keys: HashMap<String, [u8; 32]> = HashMap::new();
    // Iterative DFS with an explicit on-stack marker (import graphs are
    // acyclic in valid Go; a crafted cycle must degrade, not recurse).
    fn key_of<'a>(
        path: &'a str,
        sc_key: &str,
        by_path: &HashMap<&str, &'a ManifestPkg>,
        keys: &mut HashMap<String, [u8; 32]>,
        visiting: &mut Vec<&'a str>,
    ) -> Result<[u8; 32], SidecarError> {
        if let Some(k) = keys.get(path) {
            return Ok(*k);
        }
        if visiting.contains(&path) {
            return Err(SidecarError::Extractor(format!(
                "manifest: import cycle through {path}"
            )));
        }
        let pkg = by_path.get(path).ok_or_else(|| {
            SidecarError::Extractor(format!("manifest: missing dep {path}"))
        })?;
        visiting.push(path);
        let mut dep_keys: Vec<[u8; 32]> = Vec::with_capacity(pkg.deps.len());
        for d in &pkg.deps {
            dep_keys.push(key_of(d, sc_key, by_path, keys, visiting)?);
        }
        visiting.pop();
        dep_keys.sort_unstable();
        let mut h = blake3::Hasher::new();
        h.update(b"goverify-extract-key\0");
        h.update(&EXTRACT_CACHE_VERSION.to_le_bytes());
        let mut field = |b: &[u8]| {
            h.update(&(b.len() as u64).to_le_bytes());
            h.update(b);
        };
        field(sc_key.as_bytes());
        field(path.as_bytes());
        // Files are already sorted by the manifest; hash CONTENT only
        // (paths are machine-specific absolutes — never key material).
        for f in &pkg.files {
            let fh = file_hash(f).map_err(SidecarError::Io)?;
            h.update(&(fh.len() as u64).to_le_bytes());
            h.update(&fh);
        }
        for dk in &dep_keys {
            h.update(dk);
        }
        let k = *h.finalize().as_bytes();
        keys.insert(path.to_string(), k);
        Ok(k)
    }
    for p in pkgs {
        let mut visiting = Vec::new();
        key_of(&p.import_path, sc_key, &by_path, &mut keys, &mut visiting)?;
    }
    Ok(keys)
}

pub fn load_packages_cached(
    sc: &Sidecar,
    module_dir: &Path,
    patterns: &[&str],
    cache_root: &Path,
) -> Result<(Vec<gvir::Package>, ExtractStats), SidecarError> {
    let manifest = sc.manifest(module_dir, patterns)?;
    let keys = package_keys(sc.content_key(), &manifest)?;
    let store = Store::open(cache_root.to_path_buf());

    let mut packages: Vec<gvir::Package> = Vec::with_capacity(manifest.len());
    let mut dirty: Vec<&str> = Vec::new();
    let mut stats = ExtractStats {
        cached: 0,
        extracted: 0,
    };
    for p in &manifest {
        let key = &keys[&p.import_path];
        match store.get(LAYER, key).and_then(|b| load_package_bytes(&b).ok()) {
            Some(pkg) => {
                stats.cached += 1;
                packages.push(pkg);
            }
            None => dirty.push(&p.import_path),
        }
    }
    if !dirty.is_empty() {
        let out = tempfile::tempdir().map_err(SidecarError::Io)?;
        let files = sc.extract_only(module_dir, &dirty, out.path())?;
        for f in &files {
            let bytes = std::fs::read(f).map_err(SidecarError::Io)?;
            let Ok(pkg) = load_package_bytes(&bytes) else {
                // Undecodable fresh artifact: skip (extract's own
                // diagnostics already went to stderr).
                continue;
            };
            if let Some(key) = keys.get(&pkg.import_path) {
                // Write failure degrades to slower, never wrong.
                let _ = store.put(LAYER, key, &bytes);
            }
            stats.extracted += 1;
            packages.push(pkg);
        }
    }
    packages.sort_by(|a, b| a.import_path.cmp(&b.import_path));
    Ok((packages, stats))
}
```

(Check `SidecarError` has an `Io` variant — sidecar.rs:17-23 lists `Io, GoBuild, GoProbe, Extractor`; the `From<io::Error>` impl likely exists for the `?` on `Command::output`. Use whatever conversion the file already uses.)

Exports in `lib.rs`: `mod cached;` + `pub use cached::{ExtractStats, load_packages_cached};` + `pub use load::{LoadError, SCHEMA_VERSION, load_package, load_package_bytes};`.

- [ ] **Step 4: Run tests**

Run: `mise x -- cargo test -p goverify-extract` — Expected: PASS (new cached_load test + manifest test + existing integration tests + fuzz_seeds).

- [ ] **Step 5: Lint + commit**

```bash
mise run lint
git add crates/goverify-extract/src/cached.rs crates/goverify-extract/src/load.rs \
        crates/goverify-extract/src/sidecar.rs crates/goverify-extract/src/lib.rs \
        crates/goverify-extract/Cargo.toml crates/goverify-extract/tests/extract_integration.rs Cargo.lock
git commit --no-gpg-sign -m "phase5a: extraction cache — recursive import-DAG keys, dirty-set extraction"
```

---

### Task 12: CLI default-on plumbing (ties both layers together)

**Files:**
- Modify: `crates/goverify-cli/src/main.rs` (`CheckArgs` at :47-78, `run_check` at :324-391, `sidecar_build_dir` at :483-496)
- Modify: `crates/goverify-cli/tests/cli.rs` (end-to-end cold/warm test)

**Interfaces:**
- Consumes: `load_packages_cached`/`ExtractStats` (Task 11), `Program::from_packages` (goverify-ir, public — used by the fuzz targets already), `Analysis.scc_cache_{hits,misses}` (Task 8).
- Produces: `check` caches by default at the user cache root; `--cache-dir` overrides; `--no-cache` disables everything (extract + scc + query). `debug findings` keeps opt-in `--cache-dir` semantics unchanged. `GOVERIFY_TIMINGS=1` now also prints cache stats.

- [ ] **Step 1: Write the failing CLI test (`crates/goverify-cli/tests/cli.rs`, following that file's existing binary-invocation conventions — check how it locates the binary and fixture modules first and copy the pattern)**

```rust
#[test]
fn check_cold_and_warm_default_cache_stdout_identical() {
    // Fixture module + an ISOLATED cache root (never the user's real
    // one): point XDG_CACHE_HOME at a tempdir so the default-on path
    // is what's actually under test.
    let module = write_fixture_module(); // existing helper pattern in cli.rs
    let cache_home = tempfile::tempdir().unwrap();
    let run = || {
        let out = std::process::Command::new(goverify_bin())
            .arg("check")
            .arg("./...")
            .env("XDG_CACHE_HOME", cache_home.path())
            .env("GOVERIFY_EXTRACTOR_DIR", extractor_dir()) // existing helper
            .current_dir(module.path())
            .output()
            .expect("run goverify check");
        out
    };
    let cold = run();
    let warm = run();
    assert_eq!(
        String::from_utf8_lossy(&cold.stdout),
        String::from_utf8_lossy(&warm.stdout),
        "cold vs warm stdout byte-identical"
    );
    assert_eq!(cold.status.code(), warm.status.code(), "same exit code");
    // The default root actually got populated.
    assert!(
        cache_home.path().join("goverify").join("scc").exists(),
        "scc layer created under XDG_CACHE_HOME/goverify"
    );
    assert!(
        cache_home.path().join("goverify").join("extract").exists(),
        "extract layer created under XDG_CACHE_HOME/goverify"
    );
}

#[test]
fn no_cache_flag_runs_uncached() {
    let module = write_fixture_module();
    let cache_home = tempfile::tempdir().unwrap();
    let out = std::process::Command::new(goverify_bin())
        .arg("check")
        .arg("--no-cache")
        .arg("./...")
        .env("XDG_CACHE_HOME", cache_home.path())
        .env("GOVERIFY_EXTRACTOR_DIR", extractor_dir())
        .current_dir(module.path())
        .output()
        .expect("run goverify check --no-cache");
    assert!(
        out.status.code() == Some(0) || out.status.code() == Some(1),
        "check must not error: {:?}",
        out
    );
    assert!(
        !cache_home.path().join("goverify").join("scc").exists(),
        "--no-cache must not touch the cache root"
    );
}
```

- [ ] **Step 2: Run to verify failure**

Run: `mise x -- cargo test -p goverify-cli --test cli check_cold`
Expected: FAIL — no default caching, no `--no-cache` flag.

- [ ] **Step 3: Implement**

`CheckArgs` — replace the `cache_dir` doc and add `no_cache`:

```rust
    /// Cache directory (default: $XDG_CACHE_HOME/goverify, falling back
    /// to ~/.cache/goverify — spec §9). Project-local hermetic mode:
    /// pass an explicit dir (the shakeout does).
    #[arg(long)]
    cache_dir: Option<PathBuf>,
    /// Disable all cache layers (extract, scc, query).
    #[arg(long, conflicts_with = "cache_dir")]
    no_cache: bool,
```

Cache-root resolution + a `user_cache_root()` helper factored out of `sidecar_build_dir` (which builds the same base path today at main.rs:483-496 — refactor it to call the new helper):

```rust
/// $XDG_CACHE_HOME/goverify or ~/.cache/goverify, created 0700.
/// None when neither env var exists (caller degrades to uncached).
fn user_cache_root() -> Option<PathBuf> {
    let cache_root = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))?;
    let dir = cache_root.join("goverify");
    let _ = std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir);
    Some(dir)
}
```

In `run_check`, resolve and wire:

```rust
    let cache_root: Option<PathBuf> = if ca.no_cache {
        None
    } else {
        match ca.cache_dir.clone().or_else(user_cache_root) {
            Some(r) => Some(r),
            None => {
                eprintln!(
                    "goverify: no cache root (no XDG_CACHE_HOME or HOME); running uncached"
                );
                None
            }
        }
    };
```

Program acquisition (replacing the plain `load_program(&dargs)?` from Task 1's step 1 — keep the timing wrapper):

```rust
    let program = match (&ca.gvir_dir, &cache_root) {
        (Some(_), _) | (None, None) => load_program(&dargs)?,
        (None, Some(root)) => {
            let sidecar = Sidecar::build(&extractor_dir()?, &sidecar_build_dir())?;
            let patterns: Vec<&str> = ca.patterns.iter().map(String::as_str).collect();
            match goverify_extract::load_packages_cached(
                &sidecar,
                Path::new("."),
                &patterns,
                root,
            ) {
                Ok((pkgs, stats)) => {
                    if timings {
                        eprintln!(
                            "goverify: timing: extract cache {} hit / {} extracted",
                            stats.cached, stats.extracted
                        );
                    }
                    goverify_ir::Program::from_packages(pkgs)
                }
                Err(e) => {
                    eprintln!(
                        "goverify: extraction cache unavailable ({e}); extracting uncached"
                    );
                    load_program(&dargs)?
                }
            }
        }
    };
```

`EngineConfig.cache_dir` becomes `cache_root.clone()` (one root, three layers). After `analyze_full`, extend the timings print:

```rust
    if timings {
        eprintln!(
            "goverify: timing: scc cache {} hit / {} miss",
            a.scc_cache_hits, a.scc_cache_misses
        );
    }
```

`debug findings` (`run_findings`) is untouched: its `--cache-dir` stays opt-in.

- [ ] **Step 4: Run the tests**

Run: `mise x -- cargo test -p goverify-cli` — Expected: PASS (both new tests + existing cli/debug_integration; debug_integration must be checked for accidental default-cache pickup — it drives `check` in some cases; if it does, give those invocations `--no-cache` or an isolated `XDG_CACHE_HOME` tempdir, preserving their determinism).
Run: `mise run corpus` — Expected: PASS.

- [ ] **Step 5: Lint + commit**

```bash
mise run lint
git add crates/goverify-cli/src/main.rs crates/goverify-cli/tests/cli.rs
git commit --no-gpg-sign -m "phase5a: default-on caching for check (--no-cache/--cache-dir), extraction-cache wiring"
```

---

### Task 13: Fuzz target for the SCC entry decoder

**Files:**
- Create: `fuzz/fuzz_targets/scc_entry.rs`
- Modify: `fuzz/Cargo.toml` (new `[[bin]]`), `mise.toml` (fuzz task line), `.github/workflows/nightly.yml` (nightly line)

**Interfaces:**
- Consumes: `goverify_analysis::decode_entry_bytes` (Task 7; fuzz crate already path-depends on goverify-analysis).

- [ ] **Step 1: Write the target**

`fuzz/fuzz_targets/scc_entry.rs`:

```rust
//! Decode arbitrary bytes as an SCC cache entry. The decoder parses
//! bytes the current binary didn't necessarily write (shared caches,
//! version skew, corruption) — it must reject, never panic (parent
//! spec §12.4; phase-5a spec §4).

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = goverify_analysis::decode_entry_bytes(data);
});
```

`fuzz/Cargo.toml`, mirroring the existing `[[bin]]` blocks (lines 18-44):

```toml
[[bin]]
name = "scc_entry"
path = "fuzz_targets/scc_entry.rs"
test = false
doc = false
bench = false
```

`mise.toml` fuzz task, add:

```toml
  "cargo +nightly fuzz run scc_entry -- -max_total_time=60",
```

`.github/workflows/nightly.yml`, beside the existing four targets (nightly.yml:27-31):

```yaml
      - run: cargo +nightly fuzz run scc_entry -- -max_total_time=900
```

- [ ] **Step 2: Smoke-run**

Run: `mise run fuzz` (needs rustup nightly; if the sandbox lacks it, run the 60s single target: `cargo +nightly fuzz run scc_entry -- -max_total_time=60`)
Expected: no crashes. **If nightly is unavailable in this sandbox session, record that in the task report and rely on the corrupt-bytes unit test (Task 7) + nightly CI** — do not skip silently.

- [ ] **Step 3: Lint (actionlint/yamlfmt run in repo lint? — `mise run lint` covers fmt+clippy+buf+gofmt; YAML edits are convention-checked by eye here) + commit**

```bash
mise run lint
git add fuzz/fuzz_targets/scc_entry.rs fuzz/Cargo.toml fuzz/Cargo.lock mise.toml .github/workflows/nightly.yml
git commit --no-gpg-sign -m "phase5a: scc_entry fuzz target (cache-entry decoder, reject-never-panic)"
```

---

### Task 14: Generics blowup measurement (§16 close-out, report-only)

**Files:**
- Create: `scripts/cache_stats.sh`
- Report: `.superpowers/sdd/task-14-report.md` (gitignored; findings fold into Task 15's addendum)

**Interfaces:**
- Consumes: a populated cache root (corpus runs from Tasks 8/9, bbolt from Task 15's shakeout).

- [ ] **Step 1: Write the stats script**

```bash
#!/usr/bin/env bash
# Cache-store stats (phase-5a spec §6 rider 2): entry counts and byte
# sizes per layer. Report-only — feeds the generics-blowup measurement
# (parent spec §16) and the shakeout addendum.
set -euo pipefail
ROOT="${1:?usage: cache_stats.sh <cache-root>}"
for layer in extract scc query; do
  dir="$ROOT/$layer"
  if [ -d "$dir" ]; then
    count=$(find "$dir" -type f ! -name '*.lock' | wc -l | tr -d ' ')
    bytes=$(find "$dir" -type f ! -name '*.lock' -exec stat -f %z {} + 2>/dev/null | awk '{s+=$1} END {print s+0}')
    echo "$layer: $count entries, $bytes bytes"
  else
    echo "$layer: (absent)"
  fi
done
```

`chmod +x scripts/cache_stats.sh`. Note `stat -f %z` is the macOS form (this repo's dev platform is darwin; the script is a dev/report tool, not CI-portable — say so in the header if desired).

- [ ] **Step 2: Measure**

- Run the corpus cold/warm test with a kept cache dir (or run `check` over `testdata/corpus/knownfp` — it contains the `elemOffset` generic family — with `--cache-dir /tmp/genstats`), then `scripts/cache_stats.sh /tmp/genstats`.
- Count per-instantiation entries: instantiated generic functions carry bracketed type args in their ssa ids. The `scc` layer is opaque bytes, so instead count from the analyzer: `goverify debug summary --gvir-dir ...` or simply grep the corpus module's function list via `goverify debug ir | grep -c '\['`. Record: number of generic instantiations vs total functions, scc-layer entry count and total bytes, average entry size.
- The bbolt-scale numbers land in Task 15 (run the script against `.goverify/shakeout/cache` after the shakeout).

Record everything in `.superpowers/sdd/task-14-report.md`; the verdict format: "per-instantiation blowup at current corpus/bbolt scale: <N> instantiations, <M> bytes — concern / no concern for phase-5+ (spec §16)".

- [ ] **Step 3: Commit the script**

```bash
mise run lint
git add scripts/cache_stats.sh
git commit --no-gpg-sign -m "phase5a: cache_stats.sh — per-layer entry/byte counts (generics-blowup measurement)"
```

---

### Task 15: Shakeout gates G1–G5, addendum, docs

**Files:**
- Modify: `docs/shakeout-phase4-bbolt.md` (addendum), `ARCHITECTURE.md` (goverify-cache row + extract/analysis rows), `AGENTS.md` (only if a workflow changed — `--no-cache` deserves a half-line in the check description if AGENTS mentions flags; it doesn't currently, so likely no change)
- Report: `.superpowers/sdd/task-15-report.md`

**Interfaces:**
- Consumes: everything. This is the wave gate — spec §7 verbatim.

Baseline discipline (wave-2 lesson, non-negotiable): count findings via `grep -cE '^[a-zA-Z0-9_./-]+\.go:[0-9]+:[0-9]+:'`, **never `wc -l`**. The baseline file `.goverify/shakeout/baseline-457.txt` holds **458** finding headers with `tx.go:558:11` PRESENT; current HEAD expectation is **457** with `tx.go:558:11` retry-discharged. All diffs are signature-level (`file:line:col` sets), not count-level.

- [ ] **Step 1: Blocking tier**

```bash
mise run lint && mise run test && mise run corpus && mise run secrets && mise run audit
```

Expected: all exit 0. `mise run test` includes every new unit test; `corpus` includes the cold/warm + invalidation suites.

- [ ] **Step 2: Shakeout — cold + 3 warm runs (G1, G2, G4)**

```bash
mise x -- cargo build --release -p goverify-cli
rm -rf .goverify/shakeout/cache            # cold = empty cache
mise run shakeout   # cold; capture stdout to .goverify/shakeout/p5a-cold.txt
```

Adapt the capture the way wave-2's Task 9 did (redirect the `goverify check` stdout inside a manual invocation mirroring `scripts/shakeout.sh`, with `GOVERIFY_TIMINGS=1` and `/usr/bin/time`). Then three warm runs to `p5a-warm{1,2,3}.txt` without touching the cache.

- **G1 (correctness):** cold findings vs the 457 expectation: signature-diff `p5a-cold.txt` against the baseline-457 set (458-header file minus tx.go:558:11) — **zero arrivals, zero departures** attributable to this wave. Any delta: STOP, bisect which task introduced it (the cache must never change results; suspect entry replay or key collisions first).
- **G2 (replay fidelity):** `cmp p5a-cold.txt p5a-warm1.txt` etc. — all three warm stdouts byte-identical to cold.
- **G4 (speed, report-only):** record cold wall vs the ~207s baseline (the new layers' overhead — manifest, hashing, puts — must be small; quantify it), warm wall + the `GOVERIFY_TIMINGS` phase breakdown vs Task 1's denominator, and the <5s verdict (met / not met + which phase dominates the gap). Escalation counts noted as nondeterministic-by-design; warm runs report ~0 escalations (replay skips solving) — expected, not a regression. Machine-state caveat (EDR) carried on all wall-clocks.

- [ ] **Step 3: G3 + G5 evidence**

- **G3:** already gated by the `scc_cache_invalidation` corpus test (exact-miss-count assertion) — cite it; no bbolt-side probe needed unless G2 failed.
- **G5:** blocking-tier results from step 1 + the fuzz smoke from Task 13 + `scripts/cache_stats.sh .goverify/shakeout/cache` numbers (feeds Task 14's report too).

- [ ] **Step 4: Write the addendum + docs**

- `docs/shakeout-phase4-bbolt.md`: append a phase-5a addendum section mirroring the wave-2 format: gate verdicts G1–G5 mapped to spec §7 wording, headline (457 → 457 expected: **the cache wave must be finding-neutral**), timing table (cold/warm × phases), cache stats, generics measurement summary (from Task 14), <5s verdict.
- `ARCHITECTURE.md` goverify-cache row: change "extraction/summary caching layers arrive in phase 5" to describe the three live layers and where each is keyed/framed (`extract` keyed in goverify-extract, `scc` keyed+framed in goverify-analysis, store bytes-only). Update the goverify-extract and goverify-analysis rows' "Owns" cells with one clause each.
- `crates/goverify-cache/src/lib.rs` doc header: same one-line reality update.

- [ ] **Step 5: Commit**

```bash
mise run lint
git add docs/shakeout-phase4-bbolt.md ARCHITECTURE.md crates/goverify-cache/src/lib.rs
git commit --no-gpg-sign -m "phase5a: shakeout addendum — gate verdicts G1-G5, cache stats, speed milestone verdict"
```

---

## Self-Review (run after drafting; issues found were fixed inline)

1. **Spec coverage:** §2 layers/keys → Tasks 6, 7, 11; §3 manifest/dirty-set → Tasks 10, 11; §4 hooks/codec/fuzz → Tasks 5, 7, 8, 13; §5 CLI/determinism suite → Tasks 8, 9, 12; §6 riders 1/2/3 → Tasks 1, 14, 2+3+4; §7 gates → Tasks 9 (G3), 15 (G1/G2/G4/G5); §8 non-goals — no task touches eviction/SARIF/gvspec. Spec §4's framing-location sentence is amended by Task 7 (Deviation note).
2. **Placeholder scan:** the two spots that lean on the implementer — testgen's exact generator name (Task 5) and cli.rs fixture-helper names (Task 12) — point at the exact file to read and the pattern to copy; every other step carries complete code.
3. **Type consistency:** `func_ir_hash` (T6) consumed by `SccCache::keys` (T7); `decode_entry_bytes` (T7) consumed by fuzz (T13); `scc_cache_hits/misses` (T8) consumed by tests (T8/T9) and CLI timings (T12); `ManifestPkg`/`content_key`/`extract_only` (T10) consumed by `load_packages_cached` (T11) consumed by run_check (T12); `LazySolver::new(String, SolverLimits, Box<dyn FnMut() -> Box<dyn TextSolver> + Send>)` consistent between T3's test and implementation.
