# Follow-ups Wave 2 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Clear the 8-item follow-up queue from the summaries follow-up wave: a solver retry-on-Unknown tier, a cycle-guarded Named resolver, three regression pins, one mise hygiene fix, and two report-only investigations — shakeout-gated against the 457 baseline.

**Architecture:** The retry tier lives in `discharge_query` (the single solver entry point that owns the cache), activated by an opt-in `escalation()` hook on `TextSolver` (default `None`) and a `RetryBackend` pair constructed only in the CLI's `mk_backend` closures — engine, checkers, and every corpus test are untouched. The cycle guard is one shared `resolve_named` helper replacing five unguarded recursive `Named`-chain walks in `encode.rs`. Pins land in the existing corpus suites (`ensures`, `knownfp` — the latter already registers BOTH checkers, so the spec's "dual-checker harness" needs no new infra, just a fixture in the right suite).

**Tech Stack:** Rust (workspace crates `goverify-solver`, `goverify-analysis`, `goverify-ir`, `goverify-extract`, `goverify-cli`), Go corpus fixtures under `testdata/corpus/`, Z3 via `z3-sys`, prost for `.gvir`, cargo-fuzz, mise tasks.

**Spec:** `docs/superpowers/specs/2026-07-22-followups-wave2-design.md`

## Global Constraints

- **Determinism is the root invariant**: no timestamps, no absolute paths, no map-iteration order reaching output; findings dumps must stay byte-identical across runs (spec §8).
- **Errors degrade, never die**: skip with a diagnostic; parsers of untrusted bytes reject, never panic (fuzz targets stay green).
- **Bug-finder semantics**: `Unknown` ⇒ no report; timeouts must never create false positives (solver `SatResult` doc, `crates/goverify-solver/src/lib.rs:43`).
- **No new crates** (spec §8); `Cargo.lock` is committed.
- The only Go code lives in `extractor/`; Go files under `testdata/corpus/` are test fixtures, not build code — adding fixtures is fine.
- **All toolchain commands through mise**: `mise x -- cargo <…>` for direct cargo, `mise run <task>` for named tasks. Never bare `cargo`.
- **Commits are unsigned** in this sandbox (GPG policy): use `git commit --no-gpg-sign`. Re-sign before pushing (plan-owner note, not a task).
- Commit messages: lowercase, scope-prefixed, imperative — matching `followups: …` / `docs: …` history.
- Branch: `followups/wave2` off local `main` @ `6c63b36`. Keep the ledger (`.superpowers/sdd/progress.md`) updated per task, same discipline as prior waves.
- Corpus tests build the Go extractor and run Z3; they are slow-ish (~30 s warm). `mise run corpus` is the canonical corpus gate; `mise run test` includes everything.

---

### Task 1: Shakeout baseline capture + mise corpus hygiene (spec §4 item 3)

**Files:**
- Create: `.goverify/shakeout/baseline-457.txt` (local artifact — `.goverify/` is gitignored, same as the prior wave's `baseline-461.txt`; it is referenced by path, never committed)
- Modify: `mise.toml:47` (the `[tasks.corpus]` checkers line)

**Interfaces:**
- Produces: `.goverify/shakeout/baseline-457.txt` — the pre-wave finding signatures Task 9 diffs against. One finding per line, raw `goverify check` stdout order.
- Produces: `mise run corpus` now runs `ensures_corpus` (Tasks 2, 5 rely on the corpus gate covering their suites).

- [ ] **Step 1: Create the branch**

```bash
git checkout -b followups/wave2 main
```

- [ ] **Step 2: Capture the cold-cache baseline BEFORE any code change**

The shakeout script prints findings to stdout and status to stderr; a cold cache means deleting the query-cache dir first (the C221 lesson: warm runs replay cached outcomes).

```bash
rm -rf .goverify/shakeout/cache
mise run shakeout > .goverify/shakeout/baseline-457.txt
wc -l .goverify/shakeout/baseline-457.txt
```

Expected: exit status message `shakeout: exit 1 (0 clean / 1 findings)` on stderr; `wc -l` reports **457**. If the count differs from 457, STOP — the baseline drifted from the ledger's recorded state; report to the plan owner before proceeding (do not "fix" the number).

- [ ] **Step 3: Add ensures_corpus to the mise corpus task**

In `mise.toml`, change the `goverify-checkers` line of `[tasks.corpus]`:

```toml
  "cargo test -p goverify-checkers --test nil_corpus --test bounds_corpus --test knownfp_corpus --test ensures_corpus",
```

- [ ] **Step 4: Run the corpus task to verify the new suite is picked up and green**

Run: `mise run corpus`
Expected: PASS, including an `ensures_corpus` section in the output.

- [ ] **Step 5: Commit**

```bash
git add mise.toml
git commit --no-gpg-sign -m "wave2: add ensures_corpus to mise corpus task (baseline-457 captured locally)"
```

(`.goverify/` is gitignored — the baseline stays a local artifact; record its line count in the ledger so Task 9 has a durable reference.)

---

### Task 2: MayNilVia laundering-boundary pin (spec §4 item 1)

**Files:**
- Modify: `testdata/corpus/ensures/ensures.go` (append after `NewTNamed`, before `Rec2`)
- Modify: `crates/goverify-checkers/tests/ensures_corpus.rs` (new test at end of file)

**Interfaces:**
- Consumes: existing corpus fixtures `MayNil(b bool) (*T, error)` (ensures.go:36) and the `ensures_vars(&p, &a, func) -> Vec<Vec<String>>` helper (ensures_corpus.rs:31) plus its `analysis()` fixture.
- Produces: corpus function `example.com/ensures.MayNilVia`; test `maynilvia_laundering_boundary_tripwire`.

- [ ] **Step 1: Write the failing test**

Append to `crates/goverify-checkers/tests/ensures_corpus.rs`:

```rust
/// KNOWN laundering boundary (wave-2 spec §4, threat-model.md): the
/// Go-idiom rule accepts any non-literal error component at a return
/// site WITHOUT consulting the callee's summary, so a bare forwarding
/// wrapper of a non-idiomatic callee (MayNil can return (nil, nil))
/// receives the (T, error) correlation its callee cannot support — a
/// false-discharge (missed-bug) source. This tripwire pins TODAY'S
/// behavior: if it starts failing, the Go-idiom rule changed — update
/// threat-model.md and the ensures.go comments, then flip this pin
/// deliberately.
#[test]
fn maynilvia_laundering_boundary_tripwire() {
    let (p, a) = analysis();
    assert!(
        ensures_vars(&p, &a, "example.com/ensures.MayNilVia")
            .contains(&vec!["r0".to_string(), "r1".to_string()]),
        "MayNilVia: the Go-idiom rule launders the correlation through \
         wrappers of non-idiomatic callees (KNOWN boundary): {:?}",
        ensures_vars(&p, &a, "example.com/ensures.MayNilVia")
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `mise x -- cargo test -p goverify-checkers --test ensures_corpus maynilvia`
Expected: FAIL — panic `example.com/ensures.MayNilVia` from the `lookup_func` `.expect()` (the corpus function does not exist yet).

- [ ] **Step 3: Add the corpus fixture**

In `testdata/corpus/ensures/ensures.go`, insert between `NewTNamed` (ends line 89) and the `Rec2` comment block (line 91):

```go
// MayNilVia is the laundering-boundary tripwire queued by the
// summaries follow-up wave (wave-2 spec §4): a bare forwarding wrapper
// of a NON-idiomatic callee. MayNil can return (nil, nil), so no sound
// (T, error) correlation exists for this wrapper — but the Go-idiom
// rule accepts the extract-shaped error component without consulting
// MayNil's summary and mints the clause anyway (same mechanism as
// NewTVia above, KNOWN false-discharge boundary, threat-model.md).
// The corpus pin asserts the laundered clause IS emitted, so any
// change to the Go-idiom rule flips the pin visibly instead of
// silently moving the soundness boundary.
func MayNilVia(b bool) (*T, error) {
	return MayNil(b)
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `mise x -- cargo test -p goverify-checkers --test ensures_corpus`
Expected: PASS — all ensures_corpus tests, including the new tripwire (the laundered `["r0","r1"]` clause is present) and the pre-existing `MayNil`-stays-empty assertion.

- [ ] **Step 5: Commit**

```bash
git add testdata/corpus/ensures/ensures.go crates/goverify-checkers/tests/ensures_corpus.rs
git commit --no-gpg-sign -m "wave2: MayNilVia laundering-boundary tripwire pin (Go-idiom under-approximation)"
```

---

### Task 3: Unbounded-elemOffset positive-polarity overflow pin (spec §4 item 2)

**Files:**
- Modify: `testdata/corpus/knownfp/knownfp.go` (append after `BranchElemOffset`, line 424)

**Interfaces:**
- Consumes: existing corpus function `elemOffset(base uintptr, elemSize uintptr, n int) uintptr` (knownfp.go:404) and the `// want: <tag>` comment convention checked by `knownfp_corpus_findings_match_want_comments` (set-equality of `(file, line, tag)`).
- Produces: corpus function `example.com/knownfp.UnboundedElemOffset` carrying a `// want: overflow` pin.

- [ ] **Step 1: Add the fixture WITHOUT the want comment (deliberate RED)**

In `testdata/corpus/knownfp/knownfp.go`, insert after `BranchElemOffset` (line 424):

```go
// wave-2 positive-polarity guard for task 4A (wave-2 spec §4): a
// truly-unbounded `n` — a bare int parameter, no bounded-typed source
// anywhere — must keep elemOffset's call-site "overflow" obligation
// firing. 4A's widening-convert range model asserts only the SOURCE
// TYPE's range on a conversion's dst; there is no conversion here, so
// it must have nothing to say. Guards 4A against over-suppression.
func UnboundedElemOffset(base uintptr, n int) uintptr {
	return elemOffset(base, 16, n)
}
```

- [ ] **Step 2: Run the suite to verify the finding actually fires (RED)**

Run: `mise x -- cargo test -p goverify-checkers --test knownfp_corpus`
Expected: FAIL — set-equality mismatch: `got` contains an extra `("knownfp.go", <line of the elemOffset call>, "overflow")` entry that `want` lacks. This proves the pin is non-vacuous (the obligation is live and 4A does NOT discharge it). If the suite passes instead, STOP: 4A is over-suppressing exactly what this pin guards — report as a finding, do not add the want comment to paper over it.

- [ ] **Step 3: Add the want comment (GREEN)**

Change the call line to:

```go
	return elemOffset(base, 16, n) // want: overflow
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `mise x -- cargo test -p goverify-checkers --test knownfp_corpus`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add testdata/corpus/knownfp/knownfp.go
git commit --no-gpg-sign -m "wave2: unbounded-elemOffset positive-polarity pin guards 4A over-suppression"
```

---

### Task 4: Shared cycle-guarded Named resolver (spec §3, item 5)

**Files:**
- Modify: `crates/goverify-ir/src/types.rs` (add `TypeTable::len`/`is_empty`)
- Modify: `crates/goverify-analysis/src/encode.rs:44-125` (add `resolve_named`; rewrite `int_repr`, `basic_name`, `array_len`, `array_len_direct`, `sort_of` — note the spec names four resolvers; recon found a fifth, `basic_name` at encode.rs:65, with the identical unguarded recursion; it gets the same treatment)
- Create: `crates/goverify-extract/tests/fuzz_seeds.rs`
- Create: `fuzz/seeds/ir_encode/named-cycle.bin` (generated, committed)
- Modify: `mise.toml` (`[tasks.fuzz]` `ir_encode` line gains the seeds dir)
- Test: new `#[test]` in `crates/goverify-analysis/src/encode.rs` test module

**Interfaces:**
- Consumes: `TypeTable::kind(&self, id: TypeId) -> &TypeKind` (types.rs:70); `TypeKind::Named { underlying: TypeId, .. }`; test helpers `lookup(p: &Program, repr: &str) -> TypeId` (encode.rs:1375) and `Program::from_packages`.
- Produces: `pub fn len(&self) -> usize` + `pub fn is_empty(&self) -> bool` on `TypeTable`; `pub(crate) fn resolve_named(types: &TypeTable, t: TypeId) -> Option<TypeId>` in encode.rs; checked-in fuzz seed `fuzz/seeds/ir_encode/named-cycle.bin` regenerable via `UPDATE_FUZZ_SEEDS=1`.

- [ ] **Step 1: Add `len`/`is_empty` to TypeTable**

In `crates/goverify-ir/src/types.rs`, inside `impl TypeTable` (after `repr`, line 76):

```rust
    /// Number of interned types. `resolve_named`'s cycle cap (encode.rs)
    /// needs the table size: any Named chain longer than the table
    /// necessarily revisits an id.
    pub fn len(&self) -> usize {
        self.kinds.len()
    }

    pub fn is_empty(&self) -> bool {
        self.kinds.is_empty()
    }
```

- [ ] **Step 2: Write the crafted-cycle test (RED is by construction — do NOT run it before the fix)**

A stack-overflow RED would abort the whole test binary (spec §3), so the failing state is demonstrated by construction: the current `int_repr` recurses on `Named` with no exit for a self-referential chain. Append to the encode.rs test module (near `int_repr_distinguishes_width_and_sign`, line 1463):

```rust
    /// Crafted `.gvir` Named cycles (self-loop and a 2-cycle) must
    /// degrade to unresolvable (None) in every type resolver, never
    /// recurse to a stack overflow (wave-2 spec §3; parsers of bytes
    /// the analyzer didn't write reject, never panic). RED for this
    /// test is by construction (pre-fix code has no recursion exit on
    /// these inputs) — running it pre-fix aborts the harness.
    #[test]
    fn named_cycle_degrades_to_unresolvable() {
        use goverify_extract::gvir;
        let package = gvir::Package {
            import_path: "t".into(),
            types: vec![
                // Self-cycle: underlying (`elem`) = own id.
                gvir::Type {
                    id: 1,
                    repr: "t.Self".into(),
                    kind: gvir::TypeKind::Named as i32,
                    name: "t.Self".into(),
                    elem: 1,
                    ..Default::default()
                },
                // 2-cycle: A -> B -> A.
                gvir::Type {
                    id: 2,
                    repr: "t.A".into(),
                    kind: gvir::TypeKind::Named as i32,
                    name: "t.A".into(),
                    elem: 3,
                    ..Default::default()
                },
                gvir::Type {
                    id: 3,
                    repr: "t.B".into(),
                    kind: gvir::TypeKind::Named as i32,
                    name: "t.B".into(),
                    elem: 2,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let p = Program::from_packages(vec![package]);
        let ty = p.types();
        for name in ["t.Self", "t.A", "t.B"] {
            let id = lookup(&p, name);
            assert_eq!(int_repr(ty, id), None, "int_repr({name})");
            assert_eq!(array_len(ty, id), None, "array_len({name})");
            assert_eq!(sort_of(ty, id), None, "sort_of({name})");
        }
    }
```

- [ ] **Step 3: Implement `resolve_named` and rewrite the five resolvers**

In `crates/goverify-analysis/src/encode.rs`, add above `int_repr` (line 44):

```rust
/// Follows the `Named -> underlying` chain to the first non-Named
/// TypeId. Cycle guard (wave-2 spec §3): a crafted `.gvir` can contain
/// a Named cycle, and any chain longer than the type table necessarily
/// revisits an id — cap the walk at the table size and return None on
/// overrun. Callers already treat None as unresolvable (havoc/skip),
/// so hostile input degrades exactly like any other unencodable type
/// instead of overflowing the stack.
pub(crate) fn resolve_named(types: &TypeTable, t: TypeId) -> Option<TypeId> {
    let mut cur = t;
    for _ in 0..=types.len() {
        match types.kind(cur) {
            TypeKind::Named { underlying, .. } => cur = *underlying,
            _ => return Some(cur),
        }
    }
    None
}
```

Rewrite the five resolvers to call it once (removing every `TypeKind::Named` self-recursion arm):

```rust
/// (width, signed) for integer basic types; None for anything else
/// (floats/complex are NOT ints — phase-3 final-review I1).
pub fn int_repr(types: &TypeTable, t: TypeId) -> Option<(u32, bool)> {
    match types.kind(resolve_named(types, t)?) {
        TypeKind::Basic { name } => match name.as_str() {
            "int" | "int64" => Some((64, true)),
            "int32" | "rune" => Some((32, true)),
            "int16" => Some((16, true)),
            "int8" => Some((8, true)),
            "uint" | "uint64" | "uintptr" => Some((64, false)),
            "uint32" => Some((32, false)),
            "uint16" => Some((16, false)),
            "uint8" | "byte" => Some((8, false)),
            _ => None,
        },
        _ => None,
    }
}

/// Basic-type name after peeling Named wrappers; None for non-basic.
fn basic_name(types: &TypeTable, t: TypeId) -> Option<&str> {
    match types.kind(resolve_named(types, t)?) {
        TypeKind::Basic { name } => Some(name),
        _ => None,
    }
}
```

`array_len` / `array_len_direct` (keep the existing doc comments verbatim, they still hold):

```rust
pub fn array_len(types: &TypeTable, ty: TypeId) -> Option<u64> {
    match types.kind(resolve_named(types, ty)?) {
        TypeKind::Array { len, .. } => Some(*len),
        TypeKind::Pointer { elem } => array_len_direct(types, *elem),
        _ => None,
    }
}

fn array_len_direct(types: &TypeTable, ty: TypeId) -> Option<u64> {
    match types.kind(resolve_named(types, ty)?) {
        TypeKind::Array { len, .. } => Some(*len),
        _ => None,
    }
}
```

`sort_of` (resolve first, drop its Named arm; keep the existing doc comment and the interface-as-Ptr comment verbatim):

```rust
pub fn sort_of(types: &TypeTable, t: TypeId) -> Option<Sort> {
    let t = resolve_named(types, t)?;
    if let Some((w, _)) = int_repr(types, t) {
        return Some(Sort::BitVec(w));
    }
    match types.kind(t) {
        TypeKind::Basic { name } if name == "bool" => Some(Sort::Bool),
        TypeKind::Basic { name } if name == "string" => Some(seq_datatype().sort()),
        TypeKind::Pointer { .. } => Some(ptr_sort()),
        TypeKind::Slice { .. } => Some(seq_datatype().sort()),
        // An interface value is modeled as an opaque Ptr: nil-ness is the
        // only observation the theory makes (`err != nil` guards, the
        // (T, error) ensures correlation). Everything else about it stays
        // unconstrained — over-approximate, never wrong.
        TypeKind::Interface => Some(ptr_sort()),
        _ => None,
    }
}
```

- [ ] **Step 4: Run the new test and the crate's suites**

Run: `mise x -- cargo test -p goverify-analysis named_cycle_degrades_to_unresolvable`
Expected: PASS (returns `None` for all three cyclic types, no overflow).

Run: `mise x -- cargo test -p goverify-analysis && mise x -- cargo test -p goverify-checkers`
Expected: PASS — zero behavior change on well-formed input.

- [ ] **Step 5: Create the fuzz-seed generator test**

Create `crates/goverify-extract/tests/fuzz_seeds.rs`:

```rust
//! Checked-in fuzz seeds, single-sourced from these builders.
//! `UPDATE_FUZZ_SEEDS=1` regenerates the files; otherwise the test
//! asserts they are byte-current (goldens convention, testutil.rs).

use prost::Message;

use goverify_extract::gvir;

/// A crafted package with a self-referential Named type reaching
/// `encode_func` through a parameter: the ir_encode fuzz target only
/// encodes functions, so the cycle must be reachable from one
/// (wave-2 spec §3).
fn named_cycle_package() -> gvir::Package {
    gvir::Package {
        import_path: "t".into(),
        types: vec![
            gvir::Type {
                id: 1,
                repr: "t.Self".into(),
                kind: gvir::TypeKind::Named as i32,
                name: "t.Self".into(),
                elem: 1,
                ..Default::default()
            },
            gvir::Type {
                id: 2,
                repr: "t.A".into(),
                kind: gvir::TypeKind::Named as i32,
                name: "t.A".into(),
                elem: 3,
                ..Default::default()
            },
            gvir::Type {
                id: 3,
                repr: "t.B".into(),
                kind: gvir::TypeKind::Named as i32,
                name: "t.B".into(),
                elem: 2,
                ..Default::default()
            },
        ],
        functions: vec![gvir::Function {
            id: "t.F".into(),
            params: vec![
                gvir::Param {
                    id: 1,
                    name: "p".into(),
                    r#type: 1,
                },
                gvir::Param {
                    id: 2,
                    name: "q".into(),
                    r#type: 2,
                },
            ],
            blocks: vec![gvir::BasicBlock {
                index: 0,
                instrs: vec![gvir::Instruction {
                    kind: "Return".into(),
                    ..Default::default()
                }],
                succs: vec![],
                preds: vec![],
            }],
            ..Default::default()
        }],
        ..Default::default()
    }
}

#[test]
fn named_cycle_seed_is_current() {
    let bytes = named_cycle_package().encode_to_vec();
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fuzz/seeds/ir_encode/named-cycle.bin");
    if std::env::var_os("UPDATE_FUZZ_SEEDS").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        return;
    }
    let want = std::fs::read(&path).unwrap_or_else(|e| {
        panic!("missing seed {path:?} ({e}); run with UPDATE_FUZZ_SEEDS=1")
    });
    assert_eq!(
        want, bytes,
        "named-cycle.bin drifted from its builder; regenerate with UPDATE_FUZZ_SEEDS=1"
    );
}
```

- [ ] **Step 6: Generate the seed, then verify the test is green without the env var**

```bash
UPDATE_FUZZ_SEEDS=1 mise x -- cargo test -p goverify-extract --test fuzz_seeds
mise x -- cargo test -p goverify-extract --test fuzz_seeds
```

Expected: both PASS; `fuzz/seeds/ir_encode/named-cycle.bin` exists (a few dozen bytes).

- [ ] **Step 7: Wire the seeds dir into the fuzz smoke task**

In `mise.toml` `[tasks.fuzz]`, change the ir_encode line (a seed dir passed explicitly joins the default corpus; the first dir is where new entries are written):

```toml
  "cargo +nightly fuzz run ir_encode fuzz/corpus/ir_encode fuzz/seeds/ir_encode -- -max_total_time=60",
```

- [ ] **Step 8: Run the ir_encode fuzz smoke**

Run: `mise x -- cargo +nightly fuzz run ir_encode fuzz/corpus/ir_encode fuzz/seeds/ir_encode -- -max_total_time=60`
Expected: 60 s run, zero crashes (pre-fix, the named-cycle seed would stack-overflow this target). If nightly is unavailable in the sandbox, record that in the ledger and rely on the Step 4 unit test — do not silently skip: say so in the task report.

- [ ] **Step 9: Full corpus drift check + commit**

Run: `mise run corpus`
Expected: PASS, zero golden drift (well-formed input behavior unchanged).

```bash
git add crates/goverify-ir/src/types.rs crates/goverify-analysis/src/encode.rs crates/goverify-extract/tests/fuzz_seeds.rs fuzz/seeds/ir_encode/named-cycle.bin mise.toml
git commit --no-gpg-sign -m "wave2: shared cycle-guarded resolve_named — crafted Named cycles degrade, never overflow"
```

---

### Task 5: Manifest-position C221 repro under dual checkers (spec §5 item 4)

**Files:**
- Modify: `testdata/corpus/knownfp/knownfp.go` (append at end)

**Interfaces:**
- Consumes: `knownfp_corpus` already registers `vec![&NilChecker, &BoundsChecker]` (knownfp_corpus.rs:30) — this IS the dual-checker harness the spec calls for; recon found no extension needed (YAGNI: the plan documents this discovery instead of building new infra). Set-equality of want comments doubles as the green pin.
- Produces: corpus functions `newPage8`, `count8`, `ClearPage8Elements` in `example.com/knownfp`.

- [ ] **Step 1: Add the fixture (GREEN pin — no want comments)**

Append to `testdata/corpus/knownfp/knownfp.go`. It mirrors bbolt's surgeon.go shape: an err-guarded constructor (so NilChecker infers the (T, error) correlation and its ensures thread into this function's encoding — the exact interaction behind the original C221 flake), then the guarded `uint16(start)` convert at the manifest position:

```go
// wave-2 (spec §5): manifest-position C221 repro under the
// dual-checker interaction that produced the original FP. The
// summaries-wave C221 mechanism was NilChecker ensures (from an
// err-guarded constructor call) threading into the bounds encoding of
// the SAME function via encode_func_with — reproducible only with
// both checkers registered, which is why the task-3 repro in
// bounds_corpus (BoundsChecker alone) had to fire at a caller
// position instead. Here count8's uint16 result bounds n through
// int(), so the guarded uint16(start) at the manifest position must
// stay DISCHARGED by 4A's widening-convert range model even with
// newPage8's nil-deref ensures in the encoding. GREEN pin: no want
// comment — any arrival fails the suite's set-equality.
type page8 struct{ count uint16 }

var errPage8 = &constructError{}

func newPage8(fail bool) (*page8, error) {
	if fail {
		return nil, errPage8
	}
	return &page8{count: 42}, nil
}

func count8(p *page8) uint16 { return p.count }

func ClearPage8Elements(start int, fail bool) uint16 {
	p, err := newPage8(fail)
	if err != nil {
		return 0
	}
	n := int(count8(p))
	if start < 0 || start >= n {
		return 0
	}
	return uint16(start)
}
```

(`constructError` already exists in this file — see `errPathRequired` at knownfp.go:68; reuse it rather than importing `errors`.)

- [ ] **Step 2: Run the suite — expect green**

Run: `mise x -- cargo test -p goverify-checkers --test knownfp_corpus`
Expected: PASS with no new want entries — i.e., neither an `overflow` finding at the `uint16(start)` line nor a `nil-deref` at `count8(p)`. If a nil-deref fires at the call site, the err-guard lifting failed on this shape — STOP and report (that would itself be a wave finding, not something to pin over).

- [ ] **Step 3: Non-vacuity probe — mutate the guard, expect RED, restore**

Temporarily delete the `if start < 0 || start >= n { return 0 }` guard lines, then:

Run: `mise x -- cargo test -p goverify-checkers --test knownfp_corpus`
Expected: FAIL — an unexpected `("knownfp.go", <uint16(start) line>, "overflow")` arrival. This proves the manifest-position obligation is live in the dual-checker encoding and it is the guard + 4A range model discharging it, not dead code.

Restore the guard exactly, re-run, expect PASS. Do NOT commit the mutation.

- [ ] **Step 4: Commit**

```bash
git add testdata/corpus/knownfp/knownfp.go
git commit --no-gpg-sign -m "wave2: manifest-position C221 green pin under dual-checker encoding"
```

---

### Task 6: Retry-at-higher-timeout tier (spec §2, item 7)

**Files:**
- Create: `crates/goverify-solver/src/retry.rs`
- Modify: `crates/goverify-solver/src/lib.rs` (trait method + module + re-exports)
- Modify: `crates/goverify-solver/src/discharge.rs` (escalation branch + tests)
- Modify: `crates/goverify-cli/src/main.rs` (both `mk` closures + escalation-count diagnostics)

**Interfaces:**
- Consumes: `TextSolver { identity, limits, solve_text }`, `discharge_query(q, backend, cache, emit_dir) -> QueryOutcome`, `SolverLimits`, `SatResult`; CLI `mk` closures in `run_findings` (main.rs:272) and `run_check` (main.rs:313).
- Produces: trait method `fn escalation(&mut self) -> Option<&mut dyn TextSolver>` (default `None`); `pub struct RetryBackend` with `RetryBackend::new(base: Box<dyn TextSolver>, escalated: Box<dyn TextSolver>) -> RetryBackend`; `pub fn escalation_count() -> u64`; CLI stderr line `goverify: solver: {n} queries escalated to the retry tier` (Task 9's G5 reads this).

- [ ] **Step 1: Write the failing tests**

Append to the test module in `crates/goverify-solver/src/discharge.rs`:

```rust
    /// One scripted tier: fixed answer, counts calls, distinct limits.
    struct Tier {
        limits: SolverLimits,
        answer: SatResult,
        calls: &'static AtomicU32,
    }

    impl TextSolver for Tier {
        fn identity(&self) -> String {
            "tier-fake".into()
        }
        fn limits(&self) -> SolverLimits {
            self.limits
        }
        fn solve_text(&mut self, _c: &str) -> QueryOutcome {
            self.calls.fetch_add(1, Ordering::SeqCst);
            QueryOutcome {
                result: self.answer,
                model: None,
            }
        }
    }

    fn tier(timeout_ms: u32, answer: SatResult, calls: &'static AtomicU32) -> Box<Tier> {
        Box::new(Tier {
            limits: SolverLimits {
                timeout_ms,
                mem_mb: 1024,
            },
            answer,
            calls,
        })
    }

    #[test]
    fn unknown_escalates_once_and_escalated_result_wins() {
        static BASE: AtomicU32 = AtomicU32::new(0);
        static ESC: AtomicU32 = AtomicU32::new(0);
        let before = crate::escalation_count();
        let mut b = crate::RetryBackend::new(
            tier(100, SatResult::Unknown, &BASE),
            tier(1000, SatResult::Unsat, &ESC),
        );
        let out = discharge_query(&q(true), &mut b, None, None);
        assert_eq!(out.result, SatResult::Unsat, "escalated result wins");
        assert_eq!(BASE.load(Ordering::SeqCst), 1, "base tier ran once");
        assert_eq!(ESC.load(Ordering::SeqCst), 1, "escalated tier ran once");
        assert!(
            crate::escalation_count() >= before + 1,
            "escalation counter must advance"
        );
    }

    #[test]
    fn definitive_base_answer_never_escalates() {
        static BASE: AtomicU32 = AtomicU32::new(0);
        static ESC: AtomicU32 = AtomicU32::new(0);
        let mut b = crate::RetryBackend::new(
            tier(100, SatResult::Unsat, &BASE),
            tier(1000, SatResult::Unsat, &ESC),
        );
        let out = discharge_query(&q(true), &mut b, None, None);
        assert_eq!(out.result, SatResult::Unsat);
        assert_eq!(ESC.load(Ordering::SeqCst), 0, "no wasted escalated query");
    }

    #[test]
    fn unknown_at_both_tiers_stays_unknown() {
        static BASE: AtomicU32 = AtomicU32::new(0);
        static ESC: AtomicU32 = AtomicU32::new(0);
        let mut b = crate::RetryBackend::new(
            tier(100, SatResult::Unknown, &BASE),
            tier(1000, SatResult::Unknown, &ESC),
        );
        let out = discharge_query(&q(true), &mut b, None, None);
        assert_eq!(out.result, SatResult::Unknown, "bug-finder semantics: still silent");
        assert_eq!(ESC.load(Ordering::SeqCst), 1, "exactly one escalation, no ladder");
    }

    /// The C221-era trap, repaired: each tier caches under its own
    /// limits-bearing key, so a cached Unknown@base still triggers the
    /// escalation, and a cached Unsat@escalated resolves it with ZERO
    /// solver calls on the second run.
    #[test]
    fn retry_composes_with_cache_per_tier() {
        static BASE1: AtomicU32 = AtomicU32::new(0);
        static ESC1: AtomicU32 = AtomicU32::new(0);
        static BASE2: AtomicU32 = AtomicU32::new(0);
        static ESC2: AtomicU32 = AtomicU32::new(0);
        let dir = tempfile::tempdir().unwrap();
        let cache = goverify_cache::QueryCache::open(dir.path().to_path_buf());
        let mut b1 = crate::RetryBackend::new(
            tier(100, SatResult::Unknown, &BASE1),
            tier(1000, SatResult::Unsat, &ESC1),
        );
        let first = discharge_query(&q(true), &mut b1, Some(&cache), None);
        assert_eq!(first.result, SatResult::Unsat);
        let mut b2 = crate::RetryBackend::new(
            tier(100, SatResult::Unknown, &BASE2),
            tier(1000, SatResult::Unsat, &ESC2),
        );
        let second = discharge_query(&q(true), &mut b2, Some(&cache), None);
        assert_eq!(second.result, SatResult::Unsat, "resolved from cache");
        assert_eq!(BASE2.load(Ordering::SeqCst), 0, "base tier answered by cache");
        assert_eq!(ESC2.load(Ordering::SeqCst), 0, "escalated tier answered by cache");
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `mise x -- cargo test -p goverify-solver`
Expected: COMPILE FAIL — `RetryBackend` and `escalation_count` do not exist.

- [ ] **Step 3: Implement — trait hook, RetryBackend, discharge escalation**

In `crates/goverify-solver/src/lib.rs`, extend the `TextSolver` trait (after `solve_text`, line 85):

```rust
    /// Escalated-tier backend for retry-on-Unknown (wave-2 spec §2):
    /// `discharge_query` re-issues an Unknown query once against this
    /// backend, whose own limits key its own cache entries. None (the
    /// default) = no retry — every plain backend and test fake.
    fn escalation(&mut self) -> Option<&mut dyn TextSolver> {
        None
    }
```

Add the module + re-exports (with the other `mod`/`pub use` lines):

```rust
mod retry;
```
```rust
pub use retry::{RetryBackend, escalation_count};
```

Create `crates/goverify-solver/src/retry.rs`:

```rust
//! Retry-on-Unknown tier (wave-2 spec §2): pairs a base backend with
//! an escalated-timeout twin. The retry itself lives in
//! `discharge_query`, above the per-tier cache lookups — a wrapper
//! below the cache would replay cached base-tier Unknowns forever (the
//! C221 trap). Honesty clause: wall-clock timeouts are machine- and
//! load-sensitive; the tier narrows the flake window (a query must now
//! straddle the escalated timeout to flake), it does not eliminate it.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::{QueryOutcome, SolverLimits, TextSolver};

static ESCALATIONS: AtomicU64 = AtomicU64::new(0);

/// Process-wide count of retry-tier escalations, for diagnostic
/// reporting (shakeout G5). Monotonic; never feeds verdicts or output.
pub fn escalation_count() -> u64 {
    ESCALATIONS.load(Ordering::Relaxed)
}

pub(crate) fn note_escalation() {
    ESCALATIONS.fetch_add(1, Ordering::Relaxed);
}

/// A base backend plus its escalated tier. identity/limits/solve_text
/// all delegate to the base — to `discharge_query` this IS the base
/// backend until an Unknown makes it consult `escalation()`.
pub struct RetryBackend {
    base: Box<dyn TextSolver>,
    escalated: Box<dyn TextSolver>,
}

impl RetryBackend {
    pub fn new(base: Box<dyn TextSolver>, escalated: Box<dyn TextSolver>) -> RetryBackend {
        RetryBackend { base, escalated }
    }
}

impl TextSolver for RetryBackend {
    fn identity(&self) -> String {
        self.base.identity()
    }
    fn limits(&self) -> SolverLimits {
        self.base.limits()
    }
    fn solve_text(&mut self, canonical: &str) -> QueryOutcome {
        self.base.solve_text(canonical)
    }
    fn escalation(&mut self) -> Option<&mut dyn TextSolver> {
        Some(&mut *self.escalated)
    }
}
```

In `crates/goverify-solver/src/discharge.rs`, rename the existing body to `discharge_one` and add the escalation branch. The public signature is unchanged:

```rust
pub fn discharge_query(
    q: &Query,
    backend: &mut dyn TextSolver,
    cache: Option<&QueryCache>,
    emit_dir: Option<&Path>,
) -> QueryOutcome {
    let out = discharge_one(q, backend, cache, emit_dir);
    if out.result == SatResult::Unknown
        && let Some(esc) = backend.escalation()
    {
        // Exactly one escalation: discharge_one never re-consults
        // escalation(), so nested RetryBackends still retry once.
        // emit_dir is None — the canonical bytes were already written.
        crate::retry::note_escalation();
        return discharge_one(q, esc, cache, None);
    }
    out
}

fn discharge_one(
    q: &Query,
    backend: &mut dyn TextSolver,
    cache: Option<&QueryCache>,
    emit_dir: Option<&Path>,
) -> QueryOutcome {
    // ... the entire current discharge_query body, verbatim ...
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `mise x -- cargo test -p goverify-solver`
Expected: PASS — the 4 new tests plus all pre-existing ones (`cache_hit_skips_backend` etc. use plain backends whose default `escalation()` is None: behavior identical).

- [ ] **Step 5: Wire the CLI — both roles, ×10 escalation, count diagnostic**

In `crates/goverify-cli/src/main.rs`, add near the top of the file (module level, above `run_findings`):

```rust
/// Retry-tier escalation (wave-2 spec §2): an Unknown at the base
/// timeout is re-issued once at 10x (100ms -> 1s for Infer at the
/// defaults). Applied uniformly to both backend roles; if the shakeout
/// gate shows unacceptable wall-clock cost, restricting to Infer here
/// is the pre-agreed fallback.
const RETRY_FACTOR: u32 = 10;

fn escalated(lim: goverify_solver::SolverLimits) -> goverify_solver::SolverLimits {
    goverify_solver::SolverLimits {
        timeout_ms: lim.timeout_ms.saturating_mul(RETRY_FACTOR),
        ..lim
    }
}

fn retry_backend(
    cmd: &Option<String>,
    lim: goverify_solver::SolverLimits,
) -> Box<dyn goverify_solver::TextSolver> {
    let esc = escalated(lim);
    match cmd {
        Some(c) => Box::new(goverify_solver::RetryBackend::new(
            Box::new(goverify_solver::SmtLib2Process::new(c, lim)),
            Box::new(goverify_solver::SmtLib2Process::new(c, esc)),
        )),
        None => Box::new(goverify_solver::RetryBackend::new(
            Box::new(goverify_solver::Z3Native::new(lim)),
            Box::new(goverify_solver::Z3Native::new(esc)),
        )),
    }
}
```

Replace `run_findings`'s `mk` (main.rs:272-279) with:

```rust
    let mk: Box<
        dyn Fn(goverify_analysis::BackendRole) -> Box<dyn goverify_solver::TextSolver> + Sync,
    > = Box::new(move |_role| retry_backend(&cmd, limits));
```

Replace the closure body of `run_check`'s `mk` (main.rs:313-324) with:

```rust
    > = Box::new(move |role| {
        let lim = match role {
            goverify_analysis::BackendRole::Infer => infer,
            goverify_analysis::BackendRole::Findings => oblig,
        };
        retry_backend(&cmd, lim)
    });
```

In BOTH `run_findings` and `run_check`, immediately after the `analyze_full` call's diagnostics loop, add the count diagnostic (stderr only — stdout is the deterministic findings surface):

```rust
    let esc = goverify_solver::escalation_count();
    if esc > 0 {
        eprintln!("goverify: solver: {esc} queries escalated to the retry tier");
    }
```

- [ ] **Step 6: Full drift check**

Run: `mise run corpus && mise x -- cargo test -p goverify-cli`
Expected: PASS. Corpus suites construct plain backends (no `RetryBackend`), so zero drift there by construction. `debug_integration` exercises the CLI (now retrying): its trivial queries resolve inside the base tier, so goldens should not move — if one does, the query was landing near-timeout before this change; attribute it in the commit message rather than suppressing it.

- [ ] **Step 7: Commit**

```bash
git add crates/goverify-solver/src/lib.rs crates/goverify-solver/src/retry.rs crates/goverify-solver/src/discharge.rs crates/goverify-cli/src/main.rs
git commit --no-gpg-sign -m "wave2: retry-on-Unknown escalation tier in discharge_query, CLI-constructed, per-tier cached"
```

---

### Task 7: tx.go:558:11 pre-4A re-probe (spec §5 item 6 — investigation, report-only)

**Files:**
- Modify: `.superpowers/sdd/progress.md` (verdict recorded; Task 9 folds it into the addendum)

**Interfaces:**
- Consumes: Task 6 landed (probe (b) needs the retry tier); pre-4A commit `e20aff7` (task-3 complete, before 4A landed at `b730ea8`); baseline file from Task 1.
- Produces: a verdict string for Task 9's addendum — one of: `timing-flaky-pre-4A` / `near-timeout-drop-recovered-by-retry` / `genuine-4A-discharge (re-attribute)` / `inconclusive (evidence listed)`.

- [ ] **Step 1: Probe (b) — does the finding reappear at HEAD under the retry tier?**

```bash
rm -rf .goverify/shakeout/cache
mise run shakeout > /tmp/wave2-head-cold.txt
grep -c "tx.go:558" /tmp/wave2-head-cold.txt || true
grep "escalated to the retry tier" /tmp/wave2-head-cold.stderr 2>/dev/null || true
```

(Capture stderr too: `mise run shakeout > /tmp/wave2-head-cold.txt 2>/tmp/wave2-head-cold.stderr`.) Record: present/absent, and the escalation count.

- [ ] **Step 2: Probe (a) — pre-4A flicker test, 3 cold runs**

Build a throwaway worktree at the pre-4A commit (network note: the shakeout script clones bbolt fresh under the worktree on first run):

```bash
git worktree add /private/tmp/claude-502/-Users-rahul-muttineni-goverify/d74fef91-b027-4de1-8745-f0f878d6212d/scratchpad/pre4a e20aff7
# mise.local.toml is untracked (RUSTUP_HOME sandbox relocation) — the
# worktree needs a copy or every cargo invocation there fails:
cp mise.local.toml /private/tmp/claude-502/-Users-rahul-muttineni-goverify/d74fef91-b027-4de1-8745-f0f878d6212d/scratchpad/pre4a/
cd /private/tmp/claude-502/-Users-rahul-muttineni-goverify/d74fef91-b027-4de1-8745-f0f878d6212d/scratchpad/pre4a
mise trust mise.local.toml 2>/dev/null || true
for i in 1 2 3; do
  rm -rf .goverify/shakeout/cache
  mise run shakeout > /tmp/wave2-pre4a-run$i.txt 2>/dev/null
  echo "run $i: $(grep -c 'tx.go:558' /tmp/wave2-pre4a-run$i.txt || true) occurrence(s)"
done
cd /Users/rahul.muttineni/goverify
git worktree remove --force /private/tmp/claude-502/-Users-rahul-muttineni-goverify/d74fef91-b027-4de1-8745-f0f878d6212d/scratchpad/pre4a
```

- [ ] **Step 3: Apply the decision table and record the verdict**

| pre-4A (3 cold runs) | HEAD + retry | Verdict |
|---|---|---|
| flickers (not 3/3) | either | `timing-flaky-pre-4A`: the departure was never causally tied to 4A; family label from G3 confirmed |
| stable 3/3 present | present again | `near-timeout-drop-recovered-by-retry`: the prior wave's departure was a near-timeout Unknown; retry recovers it (expected G3 arrival in Task 9) |
| stable 3/3 present | still absent | `genuine-4A-discharge`: re-attribute the departure to 4A; correct the prior addendum's "causal tie unproven" line in Task 9 |
| anything else | anything else | `inconclusive`: record the raw counts verbatim |

Append the verdict + raw evidence (per-run occurrence counts, escalation counts) to `.superpowers/sdd/progress.md` under this task's entry. No code change; no commit of analyzer behavior — commit the ledger update:

```bash
git add .superpowers/sdd/progress.md
git commit --no-gpg-sign -m "wave2: tx.go:558:11 re-probe verdict (investigation, report-only)"
```

---

### Task 8: Test-binary link-time measurement (spec §5 item 8 — investigation, report-only)

**Files:**
- Modify: `.superpowers/sdd/progress.md` (measured verdict)

**Interfaces:**
- Consumes: nothing from other tasks (independent; parallelizable with Task 7).
- Produces: a measured verdict for Task 9's G5: `linker-bound-confirmed (top unit: <name>, <s>)` or `refuted (dominant phase: <phase>)`, plus a recommendation (`accept` / trivial fix applied / queue item).

- [ ] **Step 1: Cold-build timing capture**

```bash
mise x -- cargo clean
/usr/bin/time mise x -- cargo test --workspace --all-features --no-run --timings 2>&1 | tail -5
```

Expected: a wall-clock figure for the full cold test-binary build, and `target/cargo-timings/cargo-timing-*.html` written. The anomaly under investigation was one 20m56s `mise run test`; the inference was "linker-bound" (z3-sys static link per test binary).

- [ ] **Step 2: Read the timings report**

Open the newest `target/cargo-timings/cargo-timing-*.html` (its unit table is readable as text via grep for the slowest units):

```bash
ls -t target/cargo-timings/*.html | head -1
```

Identify the top 5 units by duration and whether they are codegen/link-dominated (test binaries linking z3) or compile-dominated. Record the numbers.

- [ ] **Step 3: Warm re-link timing (the common local case)**

```bash
touch crates/goverify-solver/src/lib.rs
/usr/bin/time mise x -- cargo test --workspace --all-features --no-run 2>&1 | tail -3
```

Record the incremental figure.

- [ ] **Step 4: Verdict + ledger + commit**

Write into `.superpowers/sdd/progress.md`: confirmed/refuted for "linker-bound", the top units with durations, cold vs warm wall-clock, and the recommendation. A fix lands ONLY if it is a one-line, clearly-supported profile/config tweak justified by the measurements — anything heavier is recorded as a queue item instead (spec §5).

```bash
git add .superpowers/sdd/progress.md
git commit --no-gpg-sign -m "wave2: test-binary link-time measurement verdict (investigation, report-only)"
```

---

### Task 9: Acceptance gates + shakeout addendum (spec §7)

**Files:**
- Modify: `docs/shakeout-phase4-bbolt.md` (new wave-2 addendum section, same format as the prior two)
- Modify: `.superpowers/sdd/progress.md` (gate verdicts, wave-complete entry)
- Create: `.goverify/shakeout/wave2-cold.txt`, `.goverify/shakeout/wave2-warm1.txt`, `.goverify/shakeout/wave2-warm2.txt` (local artifacts — `.goverify/` is gitignored; the addendum quotes the counts and deltas, the raw files stay local)

**Interfaces:**
- Consumes: `.goverify/shakeout/baseline-457.txt` (Task 1); Task 7 and Task 8 verdicts from the ledger; the `goverify: solver: N queries escalated` stderr line (Task 6).
- Produces: G1–G5 verdicts in the addendum; the wave's finding-count headline.

- [ ] **Step 1: Full blocking gate**

```bash
mise run lint && mise run test && mise run secrets && mise run audit
```

Expected: all PASS (`mise run test` includes every corpus suite; record its wall-clock for G5 alongside Task 8's figures).

- [ ] **Step 2: Shakeout ×3 — cold, warm, warm — capturing stdout and stderr**

```bash
rm -rf .goverify/shakeout/cache
mise run shakeout > .goverify/shakeout/wave2-cold.txt 2>/tmp/wave2-cold.stderr
mise run shakeout > .goverify/shakeout/wave2-warm1.txt 2>/tmp/wave2-warm1.stderr
mise run shakeout > .goverify/shakeout/wave2-warm2.txt 2>/tmp/wave2-warm2.stderr
wc -l .goverify/shakeout/wave2-*.txt
grep "escalated" /tmp/wave2-cold.stderr /tmp/wave2-warm1.stderr /tmp/wave2-warm2.stderr
```

Record wall-clock per run (prefix each with `/usr/bin/time` if not already) and the escalation counts.

- [ ] **Step 3: G4 — determinism across the 3 runs**

```bash
cmp .goverify/shakeout/wave2-cold.txt .goverify/shakeout/wave2-warm1.txt && \
cmp .goverify/shakeout/wave2-warm1.txt .goverify/shakeout/wave2-warm2.txt && echo "G4: byte-identical"
```

Expected: byte-identical. If a delta appears between cold and warm, apply the spec's honesty caveat verbatim: wall-clock timeouts bound determinism statistically, not absolutely — document which query family flaked (via the differing lines) as the known residual hazard; do NOT explain it away. Note in the addendum (as the prior wave did) that warm runs replay the cache, so runs 2-3 are cache-reuse evidence, not independent reproductions.

- [ ] **Step 4: G1/G3 — full diff vs the 457 baseline, attribute every delta**

```bash
diff .goverify/shakeout/baseline-457.txt .goverify/shakeout/wave2-cold.txt || true
```

For EVERY departure and arrival, name the responsible change:
- **Arrivals**: must be retry-tier recoveries (near-timeout family; `tx.go:558:11` is the expected candidate per Task 7's probe (b)). Cross-check each arrival by confirming it also appears when re-running with `--solver-timeout-ms 1000` pre-retry semantics if ambiguity remains.
- **Departures**: only acceptable if retry-attributed (a recovered query proving an obligation Unsat — spec §7 G3 wording). The cycle guard (Task 4) and all pins (Tasks 2, 3, 5) must contribute ZERO deltas.
- Zero unexplained deltas is the gate. An unexplained delta FAILS G3 — stop and investigate before writing the addendum.

- [ ] **Step 5: Write the addendum**

Append a `## Follow-ups wave 2 addendum (2026-07-22)` section to `docs/shakeout-phase4-bbolt.md`, same structure as the prior two addenda: environment (branch, commits, baseline file), per-gate verdicts G1–G5 with the evidence inline, the finding-count headline (457 → N), the Task 7 verdict (closing the tx.go:558:11 causal question), and the Task 8 measured link-time verdict. G5 must include: corpus time, `mise run test` time, shakeout cold/warm times, and the escalation counts per run.

- [ ] **Step 6: Ledger + final commit**

Update `.superpowers/sdd/progress.md` with the gate verdicts and the wave-complete entry (commits range, unsigned-commits note).

```bash
git add docs/shakeout-phase4-bbolt.md .superpowers/sdd/progress.md
git commit --no-gpg-sign -m "wave2: bbolt shakeout re-run addendum — gate verdicts G1-G5"
```

---

## Plan self-review notes (already applied)

- **Spec coverage**: §2 → Task 6; §3 → Task 4; §4 items 1/2/3 → Tasks 2/3/1; §5 item 4 → Task 5, item 6 → Task 7, item 8 → Task 8; §6 ordering preserved (Task 5's dual-checker "harness" resolved to existing knownfp infra — documented deviation, YAGNI); §7 → Task 9; §8 constraints → Global Constraints.
- **Type consistency**: `RetryBackend::new(Box<dyn TextSolver>, Box<dyn TextSolver>)`, `escalation(&mut self) -> Option<&mut dyn TextSolver>`, `escalation_count() -> u64`, `resolve_named(&TypeTable, TypeId) -> Option<TypeId>`, `TypeTable::len() -> usize` are used with identical signatures everywhere they appear.
- **Known judgment calls surfaced to reviewers**: escalation applied to both roles (spec-agreed, fallback documented at the `RETRY_FACTOR` comment); `basic_name` added as a fifth resolver (recon finding); fuzz seed checked into a new `fuzz/seeds/` dir because `fuzz/corpus/` is machine-local/untracked.
