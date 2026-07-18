# Phase 4: Nil-safety + Bounds Checkers Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The gated-SSA discharge engine, path-sensitive nil + bounds/overflow checkers, `goverify check` with violating-path traces and real exit codes, a machine-checked want-comment corpus harness, and a bbolt shakeout.

**Architecture:** A new pure `encode` module in `goverify-analysis` turns each lowered function into one whole-function SMT encoding (block guards, phi-as-ite, back-edges cut = first-iteration analysis); every obligation is one canonical query `entry ∧ reach(site) ∧ violation` through the existing `discharge_query`/cache path. Checkers infer `requires` clauses bottom-up through the existing SCC fixpoint (the trait gains `summary_of` so requires propagate through call chains) and raise obligations discharged under the function's own preconditions. Traces are reconstructed from sat-model guard assignments.

**Tech Stack:** Rust workspace (see `ARCHITECTURE.md`), Go extractor sidecar, Z3 via z3-sys (vendored 4.16.0), proptest, cargo-fuzz, mise tasks.

**Spec:** `docs/superpowers/specs/2026-07-18-phase4-checkers-design.md` (parent: `2026-07-16-goverify-design.md`).

**Branch:** `phase4/checkers` off `main`.

## Global Constraints

- **Determinism is the root invariant**: identical inputs ⇒ byte-identical `.gvir`, findings, SMT artifacts. No map-iteration order reaching output; sort before emitting.
- **Degrade, never die**: parsers of untrusted bytes reject, never panic; analyzer errors skip with a diagnostic. Timeout/Unknown ⇒ silence — never a false positive (parent spec §8).
- **No new runtime dependencies.** No new crates — the encoder is a module in `goverify-analysis`.
- Only Go code lives in `extractor/` (plus corpus `.go` testdata).
- `.gvir` schema single-sourced in `proto/gvir/v1/gvir.proto`; any change bumps `schema_version` ("2"→"3") in `extractor/emit.go`, `crates/goverify-extract/src/load.rs`, and regenerates `extractor/gvirpb/` via `mise run proto-gen` (commit output).
- All work via mise tasks: `mise run build|test|lint|fmt|corpus|audit|secrets`. Run `mise run lint && mise run test` before each commit (corpus tests are part of `test`).
- Toolchain commands through `mise x --` in this sandbox; commits are `--no-gpg-sign` (sandbox GPG policy).
- Naming conventions frozen by earlier phases: interface vars `p<i>`/`r<i>` (`iface_var_name`), Ptr datatype `ptr-nil`/`ptr-addr`. New in this phase: block guards `g<block>`, non-param values `v<id>`, Seq datatype `seq-val(seq-len, seq-cap)`.
- Checker tags (want-comment vocabulary): `nil-deref`, `bounds`, `div-zero`, `overflow`.

---

### Task 1: Fast-follow batch (phase-1/3 deferred triage)

Seven approved items, no behavior change except error text and a deleted dead field. Three commits.

**Files:**
- Modify: `crates/goverify-extract/src/sidecar.rs`
- Modify: `crates/goverify-analysis/src/engine.rs` (delete `EngineConfig.limits`)
- Modify: `crates/goverify-cli/src/main.rs` (drop `limits` field init in `run_findings`)
- Modify: `crates/goverify-analysis/tests/engine_corpus.rs`, `crates/goverify-checkers/tests/nil_corpus.rs` (drop `limits` field init)
- Modify: `crates/goverify-solver/src/process.rs` (mem_mb doc sentence)
- Modify: `docs/superpowers/specs/2026-07-16-goverify-design.md` (§12 blocking-tier sentence)

**Interfaces:**
- Consumes: existing `cache_key(dir, go_version)`, `SidecarError`, `EngineConfig`.
- Produces: `SidecarError::GoProbe(String)` variant; `EngineConfig` without `limits` (later tasks construct `EngineConfig { opts, cache_dir, emit_smt }`).

- [ ] **Step 1: sidecar — delete the subsumed test**

In `crates/goverify-extract/src/sidecar.rs`, delete the whole test `cache_key_is_stable_across_calls_with_same_go_version` (~lines 219–230): it is a strict subset of `cache_key_is_stable_and_content_sensitive`.

- [ ] **Step 2: sidecar — length-prefix relative paths in `cache_key`, with test**

In `cache_key`, the per-file loop hashes `rel.as_bytes()` unprefixed. Replace the loop body:

```rust
    for rel in &files {
        hasher.update(&(rel.len() as u64).to_le_bytes());
        hasher.update(rel.as_bytes());
        let bytes = fs::read(dir.join(rel))?;
        hasher.update(&(bytes.len() as u64).to_le_bytes());
        hasher.update(&bytes);
    }
```

Update the doc comment's parenthetical to say paths are length-prefixed too. Add the collision-shaped test to the tests module:

```rust
    #[test]
    fn cache_key_path_boundaries_are_unambiguous() {
        // Without length-prefixed paths, ("ab.go" content "c...") and
        // ("a.go" renamed) shapes could concatenate identically. Pin
        // that moving one byte between path and content changes the key.
        let d1 = tempfile::tempdir().unwrap();
        std::fs::write(d1.path().join("ab.go"), "x").unwrap();
        let d2 = tempfile::tempdir().unwrap();
        std::fs::write(d2.path().join("a.go"), "bx").unwrap();
        assert_ne!(
            cache_key(d1.path(), GO_VERSION).unwrap(),
            cache_key(d2.path(), GO_VERSION).unwrap(),
            "path/content boundary must be domain-separated"
        );
    }
```

- [ ] **Step 3: sidecar — `GoProbe` error variant + pure `go_version` parser with tests**

Add a variant and Display arm to `SidecarError`:

```rust
pub enum SidecarError {
    Io(io::Error),
    GoBuild(String),
    GoProbe(String),
    Extractor(String),
}
```

```rust
            SidecarError::GoProbe(msg) => {
                write!(f, "probing go version (is `go` installed?): {msg}")
            }
```

Refactor `go_version` so the branchy part is pure and testable:

```rust
fn go_version() -> Result<String, SidecarError> {
    let output = Command::new("go").args(["env", "GOVERSION"]).output()?;
    version_from_output(
        output.status.success(),
        &output.stdout,
        &output.stderr,
    )
}

/// Pure half of `go_version`: interprets the probe's outcome. Split out
/// so the error branches are unit-testable without faking a `go` binary.
fn version_from_output(
    ok: bool,
    stdout: &[u8],
    stderr: &[u8],
) -> Result<String, SidecarError> {
    if !ok {
        return Err(SidecarError::GoProbe(
            String::from_utf8_lossy(stderr).into_owned(),
        ));
    }
    let version = String::from_utf8_lossy(stdout).trim().to_string();
    if version.is_empty() {
        return Err(SidecarError::GoProbe(
            "`go env GOVERSION` printed no output".to_string(),
        ));
    }
    Ok(version)
}
```

(Existing callers of `go_version()` are unchanged. The doc comment above `go_version` currently says failures surface as `GoBuild` — update it to `GoProbe`.) Tests:

```rust
    #[test]
    fn go_version_probe_failure_is_goprobe_with_stderr() {
        let err = version_from_output(false, b"", b"boom").unwrap_err();
        match err {
            SidecarError::GoProbe(msg) => assert_eq!(msg, "boom"),
            other => panic!("want GoProbe, got {other:?}"),
        }
        // Display must say "probing", not "building" (deferred triage item).
        let text = version_from_output(false, b"", b"boom").unwrap_err().to_string();
        assert!(text.contains("probing go version"), "got: {text}");
    }

    #[test]
    fn go_version_empty_output_is_goprobe() {
        let err = version_from_output(true, b"  \n", b"").unwrap_err();
        assert!(matches!(err, SidecarError::GoProbe(_)));
    }

    #[test]
    fn go_version_trims_output() {
        assert_eq!(
            version_from_output(true, b"go1.26.5\n", b"").unwrap(),
            "go1.26.5"
        );
    }
```

- [ ] **Step 4: run sidecar tests, commit**

Run: `mise x -- cargo test -p goverify-extract`
Expected: PASS (and the deleted test is gone).

```bash
git add crates/goverify-extract/src/sidecar.rs
git commit --no-gpg-sign -m "extract: length-prefix cache_key paths, GoProbe error, go_version tests (deferred batch)"
```

- [ ] **Step 5: delete `EngineConfig.limits`; document process-backend mem_mb**

In `crates/goverify-analysis/src/engine.rs`: remove the `pub limits: SolverLimits` field from `EngineConfig` and rewrite its doc comment:

```rust
/// Everything `analyze_full` needs beyond the fixpoint options: an
/// optional on-disk query cache, and an optional directory to dump every
/// canonical SMT-LIB2 query to. Solver limits live with the backend the
/// `mk_backend` closure constructs — they are part of the backend's
/// identity (query-cache key), not engine state.
#[derive(Debug, Clone, Default)]
pub struct EngineConfig {
    pub opts: Options,
    pub cache_dir: Option<PathBuf>,
    pub emit_smt: Option<PathBuf>,
}
```

Remove the now-unused `SolverLimits` import if nothing else in the file uses it. Fix the three construction sites (delete the `limits: ...` line in each):
- `crates/goverify-cli/src/main.rs` `run_findings` (`cfg` construction; keep the local `limits` binding — the backend closures still use it)
- `crates/goverify-analysis/tests/engine_corpus.rs` `run()`
- `crates/goverify-checkers/tests/nil_corpus.rs` `run()`

In `crates/goverify-solver/src/process.rs`, extend the module doc header's final line:

```rust
//! Portable text backend (parent spec §8): pipes canonical SMT-LIB2 to
//! any solver binary. Used by --solver-cmd and the differential harness.
//! Every failure — spawn, timeout, garbage output — is Unknown.
//! `SolverLimits::mem_mb` is NOT enforced by this backend (no portable
//! flag exists across solver binaries); it still participates in the
//! query-cache key via `limits()`. Only `Z3Native` enforces the cap.
```

- [ ] **Step 6: run workspace tests, commit**

Run: `mise x -- cargo test --workspace --all-features`
Expected: PASS.

```bash
git add crates/goverify-analysis/src/engine.rs crates/goverify-cli/src/main.rs \
  crates/goverify-analysis/tests/engine_corpus.rs crates/goverify-checkers/tests/nil_corpus.rs \
  crates/goverify-solver/src/process.rs
git commit --no-gpg-sign -m "analysis+solver: drop dead EngineConfig.limits, document process-backend mem_mb (deferred batch)"
```

- [ ] **Step 7: spec §12 blocking-tier wording, commit**

In `docs/superpowers/specs/2026-07-16-goverify-design.md` line ~329, change:

> **Speed tiering.** Blocking PR tier (wall-clock budget: 10 minutes): static
> checks, unit, property (bounded case counts), corpus, and the small
> determinism suite.

to:

> **Speed tiering.** Blocking PR tier (wall-clock budget: 10 minutes): static
> checks, secret scan, dependency audit, unit, property (bounded case
> counts), corpus, and the small determinism suite.

```bash
git add docs/superpowers/specs/2026-07-16-goverify-design.md
git commit --no-gpg-sign -m "docs: spec §12 blocking tier lists secrets+audit (deferred batch)"
```

---

### Task 2: gvir schema v3 — block `preds`

The encoder's phi rule needs each block's predecessor list **in go/ssa's `b.Preds` order** (phi edges align with it); that order is not recoverable from a succs scan. Add `preds` to the wire format end-to-end.

**Files:**
- Modify: `proto/gvir/v1/gvir.proto` (BasicBlock field 4)
- Modify: `extractor/emit.go` (emit preds; bump `schemaVersion` to "3")
- Modify: `extractor/extract_test.go` (schema assertion + preds test)
- Modify: `crates/goverify-extract/src/load.rs` (`SCHEMA_VERSION = "3"`)
- Modify: `crates/goverify-ir/src/func.rs` (`Block.preds`), `crates/goverify-ir/src/lower.rs` (carry through)
- Regenerate: `extractor/gvirpb/` (`mise run proto-gen`, commit)

**Interfaces:**
- Produces: `gvir::BasicBlock.preds: Vec<u32>`; `goverify_ir::Block { pub preds: Vec<u32>, ... }` — Task 3's encoder consumes `Block.preds` and relies on `Op::Phi.edges[i]` corresponding to `preds[i]`.

- [ ] **Step 1: proto + regenerate**

In `proto/gvir/v1/gvir.proto`:

```proto
message BasicBlock {
  uint32 index = 1;
  repeated Instruction instrs = 2;  // program order
  repeated uint32 succs = 3;        // successor block indexes
  repeated uint32 preds = 4;        // predecessor block indexes, in
                                    // go/ssa b.Preds order — Phi operand
                                    // i corresponds to preds[i]
}
```

Run: `mise run proto-gen` — regenerates `extractor/gvirpb/`.

- [ ] **Step 2: extractor emits preds; schema bump; failing Go test first**

Append to `extractor/extract_test.go` (this fails until emit.go changes):

```go
// TestBlockPreds pins that preds are emitted in b.Preds order: phi
// operand i must correspond to preds[i] (encoder soundness).
func TestBlockPreds(t *testing.T) {
	pkgs := extractCorpus(t, "../testdata/corpus/ops", false)
	p := pkgs["example.com/ops"]
	found := false
	for _, f := range p.Functions {
		for _, b := range f.Blocks {
			if len(b.Preds) > 0 {
				found = true
			}
			for _, pr := range b.Preds {
				if int(pr) >= len(f.Blocks) {
					t.Fatalf("%s block %d: pred %d out of range", f.Id, b.Index, pr)
				}
			}
		}
	}
	if !found {
		t.Fatal("ops corpus has branches; some block must have preds")
	}
}
```

Also update the existing schema assertion (`extract_test.go:48`) from `"2"` to `"3"`.

Run: `cd extractor && mise x -- go test ./...`
Expected: FAIL (`b.Preds` empty, schema mismatch).

In `extractor/emit.go`: `schemaVersion = "3"` (line ~26), and in the block-emission loop (~line 258) after constructing `bb`:

```go
		for _, pred := range b.Preds {
			bb.Preds = append(bb.Preds, uint32(pred.Index))
		}
```

Run: `cd extractor && mise x -- go test ./...`
Expected: PASS.

- [ ] **Step 3: Rust side — schema bump, Block.preds, lower carry-through**

`crates/goverify-extract/src/load.rs`: `pub const SCHEMA_VERSION: &str = "3";`

`crates/goverify-ir/src/func.rs` — add to `Block` (keep field order: instrs, preds, succs is fine; match existing struct style):

```rust
pub struct Block {
    pub instrs: Vec<Instr>,
    /// Predecessor block indexes in go/ssa `b.Preds` order: `Op::Phi`
    /// operand i corresponds to `preds[i]`. Raw wire values — consumers
    /// must bounds-check (same caveat as `succs`).
    pub preds: Vec<u32>,
    pub succs: Vec<u32>,
}
```

`crates/goverify-ir/src/lower.rs` — at the block-construction site (~line 112) add `preds: b.preds.clone(),`, and add `preds: vec![]` at the four synthetic-block sites flagged by the compiler (~lines 652/776/842/891 and the testpkg/testutil builders if they construct `Block`/`gvir::BasicBlock` directly — the compiler will find every site; `gvir::BasicBlock` uses `..Default::default()` in builders so those need no change).

Rust test in `crates/goverify-ir/tests/lower_corpus.rs` (append):

```rust
#[test]
fn preds_survive_lowering_and_are_in_range() {
    let p = goverify_ir::testutil::load_corpus("ops");
    let mut saw_preds = false;
    for f in p.func_ids() {
        let Some(func) = p.func(f) else { continue };
        for b in &func.blocks {
            if !b.preds.is_empty() {
                saw_preds = true;
            }
            for &pr in &b.preds {
                assert!(
                    (pr as usize) < func.blocks.len(),
                    "{}: pred {pr} out of range",
                    p.func_name(f)
                );
            }
        }
    }
    assert!(saw_preds, "ops corpus branches must yield preds");
}
```

- [ ] **Step 4: full gate, commit**

Run: `mise run lint && mise run test`
Expected: PASS (corpus determinism suite re-extracts with v3).

```bash
git add proto/gvir/v1/gvir.proto extractor/ crates/goverify-extract/src/load.rs \
  crates/goverify-ir/src/func.rs crates/goverify-ir/src/lower.rs crates/goverify-ir/tests/lower_corpus.rs
git commit --no-gpg-sign -m "gvir v3: BasicBlock.preds in b.Preds order (phi-edge alignment for the encoder)"
```

---

### Task 3: Encoder part 1 — sorts, consts, CFG guards

New module `crates/goverify-analysis/src/encode.rs`: type→sort mapping, back-edge cutting, per-block guards. Value/op encoding is Task 4; this task's `encode_func` already produces guards and param/const terms.

**Files:**
- Create: `crates/goverify-analysis/src/encode.rs`
- Modify: `crates/goverify-analysis/src/lib.rs` (declare + re-export)

**Interfaces:**
- Consumes: `goverify_ir::{Program, Function, Block, FuncId, ValueId, TypeId, TypeKind, ValueKind, ConstVal, Op}`; `goverify_solver::{Term, Sort, DatatypeDecl, CtorDecl, Query, Logic, ptr_datatype, ptr_sort, ptr_nil}`.
- Produces (used by Tasks 4–12):

```rust
pub struct EncodedFunc {
    pub datatypes: Vec<DatatypeDecl>,          // [ptr_datatype(), seq_datatype()], always
    pub consts: Vec<(String, Sort)>,           // sorted by name before Query assembly
    pub asserts: Vec<Term>,
    pub guards: Vec<Term>,                     // index-aligned: guards[b] = Term::var("g<b>", Bool)
    pub values: BTreeMap<ValueId, Term>,       // values with a modelable sort
    pub dag_succs: Vec<Vec<u32>>,              // succs with back edges removed
}
pub fn seq_datatype() -> DatatypeDecl;         // "Seq": seq-val(seq-len BV64, seq-cap BV64)
pub fn sort_of(types: &TypeTable, t: TypeId) -> Option<Sort>;
pub fn int_repr(types: &TypeTable, t: TypeId) -> Option<(u32, bool)>; // (width, signed)
pub fn cut_back_edges(f: &Function) -> Vec<Vec<u32>>;
pub fn encode_func(p: &Program, f: FuncId) -> Result<EncodedFunc, String>;
impl EncodedFunc {
    pub fn reach_query(&self, block: usize, extra: Vec<Term>) -> Query;
    pub fn value(&self, v: ValueId) -> Option<&Term>;
}
```

- Naming: guards `g<block-index>`; params `p<i>` (must equal `iface_var_name(&IfaceVar::Param(i))`); every other value `v<ValueId.0>`.

- [ ] **Step 1: failing unit tests for sorts and back-edge cutting**

Create `encode.rs` with a tests module first (the file must exist to compile; put `use` stubs and the tests, then implement). Tests to include:

```rust
#[cfg(test)]
mod tests {
    use goverify_ir::Program;
    use goverify_solver::Sort;

    use super::*;
    use crate::testpkg::{block, func, instr, pkg};

    fn types_program() -> Program {
        // Interns basic types via a tiny package. testpkg's `pkg` builder
        // gives us a TypeTable; intern reprs directly through gvir types.
        use goverify_extract::gvir;
        let package = gvir::Package {
            import_path: "t".into(),
            types: vec![
                gvir::Type { id: 1, repr: "int".into(), kind: gvir::TypeKind::Basic as i32, name: "int".into(), ..Default::default() },
                gvir::Type { id: 2, repr: "uint8".into(), kind: gvir::TypeKind::Basic as i32, name: "uint8".into(), ..Default::default() },
                gvir::Type { id: 3, repr: "bool".into(), kind: gvir::TypeKind::Basic as i32, name: "bool".into(), ..Default::default() },
                gvir::Type { id: 4, repr: "T".into(), kind: gvir::TypeKind::Struct as i32, ..Default::default() },
                gvir::Type { id: 5, repr: "*T".into(), kind: gvir::TypeKind::Pointer as i32, elem: 4, ..Default::default() },
                gvir::Type { id: 6, repr: "[]int".into(), kind: gvir::TypeKind::Slice as i32, elem: 1, ..Default::default() },
                gvir::Type { id: 7, repr: "string".into(), kind: gvir::TypeKind::Basic as i32, name: "string".into(), ..Default::default() },
                gvir::Type { id: 8, repr: "float64".into(), kind: gvir::TypeKind::Basic as i32, name: "float64".into(), ..Default::default() },
                gvir::Type { id: 9, repr: "MyInt".into(), kind: gvir::TypeKind::Named as i32, name: "MyInt".into(), underlying: 1, ..Default::default() },
            ],
            ..Default::default()
        };
        Program::from_packages(vec![package])
    }

    fn lookup(p: &Program, repr: &str) -> goverify_ir::TypeId {
        // TypeTable interns by repr; walk ids until repr matches.
        let types = p.types();
        (0..u32::MAX)
            .map(goverify_ir::TypeId)
            .take_while(|&t| !types.repr(t).is_empty() || t.0 < 64)
            .find(|&t| types.repr(t) == repr)
            .expect("type interned")
    }

    #[test]
    fn sort_of_maps_the_v1_theory_surface() {
        let p = types_program();
        let ty = p.types();
        assert_eq!(sort_of(ty, lookup(&p, "int")), Some(Sort::BitVec(64)));
        assert_eq!(sort_of(ty, lookup(&p, "uint8")), Some(Sort::BitVec(8)));
        assert_eq!(sort_of(ty, lookup(&p, "bool")), Some(Sort::Bool));
        assert_eq!(sort_of(ty, lookup(&p, "*T")), Some(goverify_solver::ptr_sort()));
        assert_eq!(sort_of(ty, lookup(&p, "[]int")), Some(seq_datatype().sort()));
        assert_eq!(sort_of(ty, lookup(&p, "string")), Some(seq_datatype().sort()));
        assert_eq!(sort_of(ty, lookup(&p, "float64")), None, "floats havoc in v1");
        assert_eq!(sort_of(ty, lookup(&p, "MyInt")), Some(Sort::BitVec(64)), "named -> underlying");
    }

    #[test]
    fn int_repr_distinguishes_width_and_sign() {
        let p = types_program();
        let ty = p.types();
        assert_eq!(int_repr(ty, lookup(&p, "int")), Some((64, true)));
        assert_eq!(int_repr(ty, lookup(&p, "uint8")), Some((8, false)));
        assert_eq!(int_repr(ty, lookup(&p, "bool")), None);
        assert_eq!(int_repr(ty, lookup(&p, "float64")), None, "I1 lesson: floats are not ints");
    }

    #[test]
    fn cut_back_edges_yields_a_dag_keeping_forward_edges() {
        // 0 -> 1, 1 -> {2, 1(self)}, 2 -> 0 (loop): both cycle edges cut.
        let f = func(
            "t.F",
            vec![
                block(0, vec![instr("Jump")], vec![1]),
                block(1, vec![instr("If")], vec![2, 1]),
                block(2, vec![instr("Jump")], vec![0]),
            ],
        );
        let p = Program::from_packages(vec![pkg("t", vec![f])]);
        let fid = p.lookup_func("t.F").unwrap();
        let dag = cut_back_edges(p.func(fid).unwrap());
        assert_eq!(dag[0], vec![1]);
        assert_eq!(dag[1], vec![2], "self-edge 1->1 cut");
        assert!(dag[2].is_empty(), "2->0 back edge cut");
    }
}
```

Note for the implementer: `testpkg::block(index, instrs, succs)` builds `gvir::BasicBlock` with `..Default::default()` so `preds` arrive empty in these hand-built fixtures; `Program::from_packages` lowering fills `Block.preds` only from the wire — for guard tests below, hand-set preds via the gvir builder by constructing `gvir::BasicBlock { index, instrs, succs, preds }` directly where needed (check `testpkg.rs` and extend its `block` helper with a `preds` argument if simplest — keep the existing 3-arg helper delegating with `preds: vec![]`... **no**: simplest is a new helper `block_p(index, instrs, preds, succs)` in `testpkg.rs`, leaving `block` untouched).

Run: `mise x -- cargo test -p goverify-analysis encode`
Expected: FAIL to compile (functions undefined).

- [ ] **Step 2: implement sorts + back-edge cutting**

```rust
//! Gated-SSA function encoding (phase-4 spec §3): one whole-function
//! SMT encoding — block guards, phi-as-ite, back edges cut (loops are
//! analyzed as their first iteration; reachability stays
//! under-approximate, so Sat always witnesses a genuine execution).
//! Pure: `Program` in, declarations + assertions out. Anything not
//! modeled havocs — degrade, never die.

use std::collections::BTreeMap;

use goverify_ir::{
    ConstVal, FuncId, Function, Op, Program, TypeId, TypeKind, TypeTable, ValueId, ValueKind,
};
use goverify_solver::{
    BvBinOp, BvCmpOp, CtorDecl, DatatypeDecl, Logic, Query, Sort, Term, ptr_datatype, ptr_nil,
    ptr_sort,
};

/// Slices/strings as length-carrying opaque values: contents havoc,
/// bounds reasoning uses len/cap only. Invariant 0 <= len <= cap is
/// asserted for every Seq-sorted value the encoder introduces.
pub fn seq_datatype() -> DatatypeDecl {
    DatatypeDecl {
        name: "Seq".into(),
        ctors: vec![CtorDecl {
            name: "seq-val".into(),
            fields: vec![
                ("seq-len".into(), Sort::BitVec(64)),
                ("seq-cap".into(), Sort::BitVec(64)),
            ],
        }],
    }
}

/// (width, signed) for integer basic types; None for anything else
/// (floats/complex are NOT ints — phase-3 final-review I1).
pub fn int_repr(types: &TypeTable, t: TypeId) -> Option<(u32, bool)> {
    match types.kind(t) {
        TypeKind::Named { underlying, .. } => int_repr(types, *underlying),
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

/// TypeId -> SMT sort for the v1 theory surface. None = value havocs
/// (no term). `int`/`uint`/`uintptr` are 64-bit (64-bit targets only in
/// v1 — documented degrade).
pub fn sort_of(types: &TypeTable, t: TypeId) -> Option<Sort> {
    if let Some((w, _)) = int_repr(types, t) {
        return Some(Sort::BitVec(w));
    }
    match types.kind(t) {
        TypeKind::Named { underlying, .. } => sort_of(types, *underlying),
        TypeKind::Basic { name } if name == "bool" => Some(Sort::Bool),
        TypeKind::Basic { name } if name == "string" => Some(seq_datatype().sort()),
        TypeKind::Pointer { .. } => Some(ptr_sort()),
        TypeKind::Slice { .. } => Some(seq_datatype().sort()),
        _ => None,
    }
}

/// DFS edge classification from the entry block: returns `succs` with
/// back edges (target on the current DFS stack) removed. Unreachable
/// blocks keep their edges (harmless: their guards become false).
pub fn cut_back_edges(f: &Function) -> Vec<Vec<u32>> {
    let n = f.blocks.len();
    let mut dag: Vec<Vec<u32>> = f.blocks.iter().map(|b| b.succs.clone()).collect();
    let mut state = vec![0u8; n]; // 0 unvisited, 1 on stack, 2 done
    // Iterative DFS; (block, next-succ-index) frames.
    let mut stack: Vec<(usize, usize)> = Vec::new();
    if n == 0 {
        return dag;
    }
    stack.push((0, 0));
    state[0] = 1;
    let mut cut: Vec<(usize, u32)> = Vec::new();
    while let Some(frame) = stack.last_mut() {
        let (b, i) = (frame.0, frame.1);
        let succs = &f.blocks[b].succs;
        if i < succs.len() {
            let s = succs[i] as usize;
            frame.1 += 1;
            if s >= n {
                continue; // raw wire id out of range: ignore edge (degrade)
            }
            match state[s] {
                0 => {
                    state[s] = 1;
                    stack.push((s, 0));
                }
                1 => cut.push((b, s as u32)), // back edge
                _ => {}
            }
        } else {
            state[b] = 2;
            stack.pop();
        }
    }
    for (b, s) in cut {
        dag[b].retain(|&x| x != s);
    }
    dag
}
```

Run: `mise x -- cargo test -p goverify-analysis encode`
Expected: the three implemented-function tests PASS (guard tests arrive next step).

- [ ] **Step 3: failing guard tests, then `encode_func` skeleton with guards**

Add tests (using the `block_p` helper with explicit preds):

```rust
    fn one_func_program(f: goverify_extract::gvir::Function) -> (Program, FuncId) {
        let p = Program::from_packages(vec![pkg("t", vec![f])]);
        let id = p.lookup_func("t.F").unwrap();
        (p, id)
    }

    #[test]
    fn straight_line_guards_entry_true_then_propagate() {
        // 0 -Jump-> 1. g0 = true, g1 = g0.
        let f = func(
            "t.F",
            vec![
                block_p(0, vec![instr("Jump")], vec![], vec![1]),
                block_p(1, vec![instr("Return")], vec![0], vec![]),
            ],
        );
        let (p, id) = one_func_program(f);
        let enc = encode_func(&p, id).unwrap();
        assert_eq!(enc.guards.len(), 2);
        // Entry guard asserted true; g1 defined from g0's edge. Pin via
        // the canonical query text — the stable, printable artifact.
        let q = enc.reach_query(1, vec![]);
        let text = q.canonical_text();
        assert!(text.contains("(assert (= g0 true))"), "entry guard:\n{text}");
        assert!(text.contains("(assert (= g1 g0))"), "jump edge:\n{text}");
        assert!(text.contains("(assert g1)"), "reach conjunct:\n{text}");
    }

    #[test]
    fn branch_guards_split_on_condition() {
        // Block 0 ends in Branch(cond v1: bool param p0); succs [1, 2].
        // g1 = (and g0 p0), g2 = (and g0 (not p0)).
        let mut br = instr("If");
        br.operands = vec![1];
        let f = {
            let mut f = func(
                "t.F",
                vec![
                    block_p(0, vec![br], vec![], vec![1, 2]),
                    block_p(1, vec![instr("Return")], vec![0], vec![]),
                    block_p(2, vec![instr("Return")], vec![0], vec![]),
                ],
            );
            f.params = vec![goverify_extract::gvir::Param {
                id: 1,
                name: "c".into(),
                r#type: 3, // bool in the fixture type table
            }];
            f
        };
        // (fixture needs the bool type interned — reuse types_program's
        // type list in `pkg`'s package or add types to the builder)
        let (p, id) = one_func_program(f);
        let enc = encode_func(&p, id).unwrap();
        let text = enc.reach_query(2, vec![]).canonical_text();
        assert!(text.contains("(= g1 (and g0 p0))"), "then edge:\n{text}");
        assert!(text.contains("(= g2 (and g0 (not p0)))"), "else edge:\n{text}");
    }

    #[test]
    fn merge_block_guard_is_or_of_incoming() {
        // diamond: 0 -> {1,2} -> 3. g3 = (or <edge from 1> <edge from 2>).
        // Edge guards from Jump blocks pass through: g3 = (or g1 g2).
        // ... build as in branch test with block 3 preds [1, 2] ...
    }
```

(The merge test builds the diamond the same way; assert the canonical text contains `(= g3 (or g1 g2))`.)

Then implement:

```rust
/// Term-count budget: a function whose encoding exceeds this many
/// assertions is skipped with a diagnostic (spec §3 size cap).
const ASSERT_CAP: usize = 50_000;

pub struct EncodedFunc {
    pub datatypes: Vec<DatatypeDecl>,
    pub consts: Vec<(String, Sort)>,
    pub asserts: Vec<Term>,
    pub guards: Vec<Term>,
    pub values: BTreeMap<ValueId, Term>,
    pub dag_succs: Vec<Vec<u32>>,
}

impl EncodedFunc {
    pub fn value(&self, v: ValueId) -> Option<&Term> {
        self.values.get(&v)
    }

    /// One canonical obligation query: everything the encoding asserts,
    /// plus reach(block), plus the caller's extra conjuncts (violation,
    /// preconditions). Consts are sorted for determinism.
    pub fn reach_query(&self, block: usize, extra: Vec<Term>) -> Query {
        let mut asserts = self.asserts.clone();
        if let Some(g) = self.guards.get(block) {
            asserts.push(g.clone());
        }
        asserts.extend(extra);
        let mut consts = self.consts.clone();
        consts.sort();
        Query {
            logic: Logic::All,
            datatypes: self.datatypes.clone(),
            consts,
            asserts,
        }
    }
}

fn guard_name(b: usize) -> String {
    format!("g{b}")
}

fn value_name(f: &Function, v: ValueId) -> String {
    // Params use the iface convention p<i> so requires clauses and
    // call-site instantiation share the encoder's terms directly.
    if let Some(i) = f.params.iter().position(|&pv| pv == v) {
        return crate::summary::iface_var_name(&crate::summary::IfaceVar::Param(i as u32));
    }
    format!("v{}", v.0)
}

pub fn encode_func(p: &Program, f: FuncId) -> Result<EncodedFunc, String> {
    let func = p
        .func(f)
        .ok_or_else(|| format!("{}: no body to encode", p.func_name(f)))?;
    let n = func.blocks.len();
    let dag_succs = cut_back_edges(func);
    let mut enc = EncodedFunc {
        datatypes: vec![ptr_datatype(), seq_datatype()],
        consts: Vec::new(),
        asserts: Vec::new(),
        guards: (0..n)
            .map(|b| Term::var(&guard_name(b), Sort::Bool))
            .collect(),
        values: BTreeMap::new(),
        dag_succs,
    };
    for b in 0..n {
        enc.consts.push((guard_name(b), Sort::Bool));
    }
    // Params first: their terms exist before any op reads them.
    for (i, &pv) in func.params.iter().enumerate() {
        let _ = i;
        declare_value(p, func, pv, &mut enc);
    }
    // Pass 1: declare/const-define every value an instruction reads or
    // writes (Task 4 fills op semantics; this pass makes havoc sound).
    // Pass 2 (below): guard structure.
    encode_guards(func, &mut enc)?;
    if enc.asserts.len() > ASSERT_CAP {
        return Err(format!(
            "{}: encoding exceeds {ASSERT_CAP} assertions; skipped",
            p.func_name(f)
        ));
    }
    Ok(enc)
}

/// Introduce `v`'s term if its type is modelable: declare the const and,
/// for Seq-sorted values, assert 0 <= len <= cap. Constants get defining
/// equalities. Idempotent.
fn declare_value(p: &Program, func: &Function, v: ValueId, enc: &mut EncodedFunc) {
    if enc.values.contains_key(&v) {
        return;
    }
    let info = func.value(v);
    let Some(sort) = sort_of(p.types(), info.ty) else {
        return;
    };
    let name = value_name(func, v);
    let t = Term::var(&name, sort.clone());
    enc.consts.push((name, sort.clone()));
    if sort == seq_datatype().sort() {
        seq_invariant(&t, enc);
    }
    if let ValueKind::Const(c) = &info.kind
        && let Some(lit) = const_term(p, info.ty, c)
        && let Ok(eq) = Term::eq(t.clone(), lit)
    {
        enc.asserts.push(eq);
    }
    enc.values.insert(v, t);
}

/// 0 <= seq-len(t) <= seq-cap(t) (unsigned): every Seq value the
/// encoder introduces carries the Go invariant, so cap-based slice
/// bounds can never be tighter than len-based ones.
fn seq_invariant(t: &Term, enc: &mut EncodedFunc) {
    let dt = seq_datatype();
    let (Ok(len), Ok(cap)) = (
        Term::dt_get(&dt, "seq-val", "seq-len", t.clone()),
        Term::dt_get(&dt, "seq-val", "seq-cap", t.clone()),
    ) else {
        return;
    };
    if let Ok(inv) = Term::bv_cmp(BvCmpOp::Ule, len, cap) {
        enc.asserts.push(inv);
    }
}

fn const_term(p: &Program, ty: TypeId, c: &ConstVal) -> Option<Term> {
    match c {
        ConstVal::Bool(b) => Some(Term::bool_lit(*b)),
        ConstVal::Int(i) => {
            let (w, _) = int_repr(p.types(), ty)?;
            // two's-complement truncation into the width
            let masked = (*i as u128) & (u128::MAX >> (128 - w));
            Some(Term::bv_lit(w, masked))
        }
        ConstVal::Nil => match sort_of(p.types(), ty)? {
            Sort::Datatype(n) if n == "Ptr" => Some(ptr_nil()),
            s if s == seq_datatype().sort() => seq_lit(0, 0),
            _ => None,
        },
        ConstVal::Str(bytes) => seq_lit(bytes.len() as u64, bytes.len() as u64),
        _ => None, // BigInt/Float/Complex/Opaque havoc
    }
}

fn seq_lit(len: u64, cap: u64) -> Option<Term> {
    Term::dt_ctor(
        &seq_datatype(),
        "seq-val",
        vec![
            Term::bv_lit(64, len as u128),
            Term::bv_lit(64, cap as u128),
        ],
    )
    .ok()
}

/// Guard structure over the cut DAG: g0 = true; for every other block,
/// g_b = OR of incoming edge guards; a Branch edge conjoins the branch
/// condition (positional: succs[0] = cond, succs[1] = ¬cond). A block
/// whose every in-edge was cut is unreachable in the DAG: g_b = false.
fn encode_guards(func: &Function, enc: &mut EncodedFunc) -> Result<(), String> {
    let n = func.blocks.len();
    // edge_guard[(from, to)] considering only DAG edges
    let mut incoming: Vec<Vec<Term>> = vec![Vec::new(); n];
    for (b, block) in func.blocks.iter().enumerate() {
        let gb = enc.guards[b].clone();
        let cond = block.instrs.last().and_then(|i| match &i.op {
            Op::Branch { cond } => enc.values.get(cond).cloned().or_else(|| {
                // Unmodelable condition: havoc it as a fresh bool so both
                // branches stay possible (missing info = nondeterminism).
                let name = format!("hc{b}");
                enc.consts.push((name.clone(), Sort::Bool));
                Some(Term::var(&name, Sort::Bool))
            }),
            _ => None,
        });
        for (pos, &s) in enc.dag_succs[b].iter().enumerate() {
            let s = s as usize;
            if s >= n {
                continue;
            }
            // Positional index within the ORIGINAL succs decides the
            // branch polarity (dag_succs preserves order).
            let orig_pos = func.blocks[b].succs.iter().position(|&x| x as usize == s);
            let edge = match (&cond, orig_pos) {
                (Some(c), Some(0)) => Term::and(vec![gb.clone(), c.clone()]),
                (Some(c), Some(1)) => Term::not(c.clone())
                    .and_then(|nc| Term::and(vec![gb.clone(), nc])),
                _ => Ok(gb.clone()),
            }
            .map_err(|e| format!("edge guard: {e}"))?;
            let _ = pos;
            incoming[s].push(edge);
        }
    }
    for b in 0..n {
        let gb = enc.guards[b].clone();
        let rhs = if b == 0 {
            Term::bool_lit(true)
        } else if incoming[b].is_empty() {
            Term::bool_lit(false)
        } else if incoming[b].len() == 1 {
            incoming[b][0].clone()
        } else {
            Term::or(incoming[b].clone()).map_err(|e| format!("guard or: {e}"))?
        };
        enc.asserts
            .push(Term::eq(gb, rhs).map_err(|e| format!("guard eq: {e}"))?);
    }
    Ok(())
}
```

Wire into `crates/goverify-analysis/src/lib.rs`:

```rust
pub mod encode;
```

(plus re-exports if the crate's style re-exports at root — match existing style: `pub use encode::{EncodedFunc, encode_func};`)

**Implementation notes for this step (real gotchas):**
- `iface_var_name`/`IfaceVar` visibility: they live in `summary.rs` and are `pub` — the `crate::summary::` path works.
- `Term::dt_get` — check its exact signature in `term.rs:309` (it takes the datatype, ctor name, field name, arg — adjust the two call sites to the real parameter order).
- A duplicated block in `succs` (both branch targets equal) hits `position()` returning 0 for both DAG edges — acceptable: `g ∧ c` OR'd with nothing; if this shape appears, both-edges-same means the branch is irrelevant; do not special-case.
- `hc<b>` havoc'd-condition consts: one per block max (last instr only), deterministic name.

Run: `mise x -- cargo test -p goverify-analysis encode`
Expected: PASS.

- [ ] **Step 4: commit**

```bash
git add crates/goverify-analysis/src/encode.rs crates/goverify-analysis/src/lib.rs crates/goverify-analysis/src/testpkg.rs
git commit --no-gpg-sign -m "analysis: encoder part 1 — sorts, consts, back-edge cut, block guards"
```

---

### Task 4: Encoder part 2 — op semantics and phi

Fill in value definitions for the pure ops; everything else havocs (declared, unconstrained). After this task `encode_func` is semantically complete.

**Files:**
- Modify: `crates/goverify-analysis/src/encode.rs`

**Interfaces:**
- Consumes: Task 3's `EncodedFunc`, `declare_value`, `int_repr`.
- Produces: `encode_func` now asserts defs for Assign/BinOp/UnOp/Phi/MakeSlice and declares (havoc) every other written value. `values` map covers every modelable value in the function.

- [ ] **Step 1: failing tests for op encodings**

```rust
    #[test]
    fn binop_add_encodes_bvadd() {
        // v2 = p0 + 1 (int). Expect (= v2 (bvadd p0 <lit 1>)).
        // Build: one block, BinOp instr kind "BinOp" with sem BinOpSem
        // op "+", register 2, operands [1, aux-const 3] — mirror the
        // operand conventions in lower.rs (read its BinOp arm to build
        // the fixture exactly as the lowerer expects).
        // Assert canonical text contains "(= v2 (bvadd p0" and the
        // 64-bit literal rendering of 1 (copy exact literal syntax from
        // printer.rs's bv rendering, e.g. "#x0000000000000001").
    }

    #[test]
    fn signed_and_unsigned_div_pick_sdiv_udiv() { /* int -> bvsdiv; uint -> bvudiv */ }

    #[test]
    fn comparison_binops_encode_bool_dsts() { /* Lt on int -> (= v2 (bvslt p0 p1)); on uint -> bvult */ }

    #[test]
    fn phi_is_ite_over_incoming_edge_guards() {
        // diamond 0->{1,2}->3, phi at 3 with edges [c1, c2], preds [1,2]:
        // (= v9 (ite e1 c1 c2)) where e1 is the 1->3 edge guard (= g1
        // since 1 ends in Jump). With two incoming edges the last edge is
        // the ite's else branch.
    }

    #[test]
    fn phi_with_cut_back_edge_keeps_preheader_value_only() {
        // loop header 1 with preds [0, 2], 2->1 is a back edge:
        // phi(dst v9, edges [vinit, vloop]) => (= v9 p_init_term) — no ite.
    }

    #[test]
    fn makeslice_defines_len_and_cap() {
        // Make{Slice, args [len, cap]} => (= v5 (seq-val vlen vcap)).
    }

    #[test]
    fn unmodeled_ops_havoc_but_declare() {
        // A Call dst of type *T gets a Ptr-sorted const with NO defining
        // equality; encode_func must not error.
    }

    #[test]
    fn eq_neq_work_across_sorts() {
        // BinOp Eq on pointers => (= v3 (= p0 p1)); Neq => (= v3 (not (= p0 p1))).
    }
```

(Write these fully — each builds its fixture via `testpkg`/gvir builders exactly like Task 3's tests; the exact `Sem` payloads for BinOp/Phi/Make come from reading the corresponding arm in `lower.rs` — `"BinOp"` uses `Sem::Binop(BinOpSem { op: "+" })` etc. Copy the operand layout the lowerer consumes.)

Run: `mise x -- cargo test -p goverify-analysis encode`
Expected: new tests FAIL.

- [ ] **Step 2: implement op encoding**

Extend `encode_func`: after params, walk blocks in index order; for every instr, `declare_value` each operand and dst, then assert a defining equality where the op is modeled:

```rust
fn encode_ops(p: &Program, func: &Function, enc: &mut EncodedFunc) {
    for (bi, block) in func.blocks.iter().enumerate() {
        for ins in &block.instrs {
            // declare operands + dst (order: reads then write)
            for v in op_values(&ins.op) {
                declare_value(p, func, v, enc);
            }
            let Some(def) = op_def(p, func, bi, &ins.op, enc) else {
                continue;
            };
            enc.asserts.push(def);
        }
    }
}

/// The defining equality for a modeled op, if every needed term exists.
fn op_def(
    p: &Program,
    func: &Function,
    block: usize,
    op: &Op,
    enc: &mut EncodedFunc,
) -> Option<Term> {
    let t = |v: &ValueId| enc.values.get(v).cloned();
    match op {
        Op::Assign { dst, src } => Term::eq(t(dst)?, t(src)?).ok(),
        Op::BinOp { dst, kind, lhs, rhs } => {
            let d = t(dst)?;
            let (l, r) = (t(lhs)?, t(rhs)?);
            let rhs_term = binop_term(p, func, *kind, lhs, l, r)?;
            Term::eq(d, rhs_term).ok()
        }
        Op::UnOp { dst, kind, operand } => {
            let d = t(dst)?;
            let o = t(operand)?;
            let rhs = match kind {
                goverify_ir::UnOpKind::Not => Term::not(o).ok()?,
                goverify_ir::UnOpKind::Neg => {
                    // -x = 0 - x at the operand's width
                    let (w, _) = int_repr(p.types(), func.value(*operand).ty)?;
                    Term::bv_bin(BvBinOp::Sub, Term::bv_lit(w, 0), o).ok()?
                }
                goverify_ir::UnOpKind::BitNot => {
                    let (w, _) = int_repr(p.types(), func.value(*operand).ty)?;
                    // ^x = x xor all-ones
                    let ones = (u128::MAX) >> (128 - w);
                    Term::bv_bin(BvBinOp::Xor, o, Term::bv_lit(w, ones)).ok()?
                }
            };
            Term::eq(d, rhs).ok()
        }
        Op::Phi { dst, edges } => {
            let d = t(dst)?;
            let preds = &func.blocks[block].preds;
            // Keep only edges whose pred edge survives in the DAG.
            let mut kept: Vec<(Term, Term)> = Vec::new(); // (edge guard, value)
            for (i, ev) in edges.iter().enumerate() {
                let pr = *preds.get(i)? as usize;
                if pr >= func.blocks.len() {
                    return None;
                }
                if !enc.dag_succs[pr].contains(&(block as u32)) {
                    continue; // back edge: cut
                }
                kept.push((edge_guard_term(func, enc, pr, block)?, t(ev)?));
            }
            let (last_g, last_v) = kept.pop()?;
            let _ = last_g; // final else branch needs no test
            let mut acc = last_v;
            for (g, v) in kept.into_iter().rev() {
                acc = Term::ite(g, v, acc).ok()?;
            }
            Term::eq(d, acc).ok()
        }
        Op::Make { dst, kind: goverify_ir::MakeKind::Slice, args } => {
            let d = t(dst)?;
            let len = args.first().and_then(t)?;
            let cap = args.get(1).and_then(t).unwrap_or_else(|| len.clone());
            let ctor = Term::dt_ctor(&seq_datatype(), "seq-val", vec![len, cap]).ok()?;
            Term::eq(d, ctor).ok()
        }
        _ => None, // Load/Store/Call/Convert/... havoc (declared, unconstrained)
    }
}
```

`binop_term` maps `BinOpKind` with signedness from the **operand** type (`int_repr(p.types(), func.value(*lhs).ty)`):
- Add/Sub/Mul/And/Or/Xor/Shl → the corresponding `BvBinOp` (Shr → `Ashr` signed / `Lshr` unsigned; AndNot → `And` with `Xor`-all-ones of rhs, same trick as BitNot);
- Div → Sdiv/Udiv, Rem → Srem/Urem;
- Eq/Neq → `Term::eq` / `not(eq)` — works for every sort incl. Ptr/Seq/Bool;
- Lt/Leq/Gt/Geq → `bv_cmp` Slt/Sle (signed) or Ult/Ule (unsigned); Gt/Geq swap the operand order (`x > y` ⇒ `y < x`);
- anything else (string concat `+`, float ops — operands without `int_repr` and not Eq/Neq) → None (havoc).

`edge_guard_term(func, enc, pred, block)`: recompute the same edge-guard expression `encode_guards` built (factor the per-edge construction out of `encode_guards` into a shared helper so phi and guards CANNOT diverge — single source of truth). The refactor: `fn edge_guard(func, enc, from: usize, to: usize) -> Option<Term>` used by both.

**Note on havoc'd Branch conditions**: `encode_guards` creates `hc<b>` consts on demand; `edge_guard` must reuse the SAME const (look it up in `enc.consts` before pushing a new one) — otherwise phi and guard disagree. Simplest: precompute all `hc<b>` in a first pass over blocks, before `encode_guards` and `encode_ops`.

`op_values(op) -> Vec<ValueId>` — a match listing every operand/dst per variant (mechanical; every `Op` variant, dsts included, `Option<ValueId>` dsts flattened).

Call `encode_ops` from `encode_func` between param declaration and `encode_guards` — **wait**: guards need Branch conds declared, ops need edge guards for phi. Order: (1) params, (2) declare pass (all values incl. Branch conds + `hc` pre-pass), (3) `encode_guards`, (4) `encode_ops` (defining equalities only — split declaration out of `encode_ops` into the declare pass).

Run: `mise x -- cargo test -p goverify-analysis encode`
Expected: PASS.

- [ ] **Step 3: determinism unit test + commit**

```rust
    #[test]
    fn encoding_is_deterministic() {
        let p = goverify_ir::testutil::load_corpus("ops");
        for f in p.func_ids() {
            if p.func(f).is_none() {
                continue;
            }
            let (Ok(a), Ok(b)) = (encode_func(&p, f), encode_func(&p, f)) else {
                continue;
            };
            for bi in 0..a.guards.len() {
                assert_eq!(
                    a.reach_query(bi, vec![]).canonical_text(),
                    b.reach_query(bi, vec![]).canonical_text(),
                    "{} block {bi}",
                    p.func_name(f)
                );
            }
        }
    }
```

(This needs `goverify-ir`'s `testutil` — already a dependency direction used by `engine_corpus.rs`; the test lives in the module, and `testutil::load_corpus` requires the dev-dependency on `goverify-extract` which `goverify-analysis` already has via testpkg.)

Run: `mise x -- cargo test -p goverify-analysis`
Expected: PASS.

```bash
git add crates/goverify-analysis/src/encode.rs
git commit --no-gpg-sign -m "analysis: encoder part 2 — op semantics, phi-as-ite over edge guards, seq values"
```

---

### Task 5: Checker plumbing — trait, engine, Finding.tag, want harness

The breaking-change task: `Checker::infer_requires` gains `summary_of` (requires propagation), `analyze_full` gains a backend role (two timeout tiers), `Finding` gains `tag`, checkers run in name-sorted order, and the corpus want-comment harness lands. NilTracer is mechanically adapted (real rewrite is Task 6).

**Files:**
- Modify: `crates/goverify-analysis/src/checker.rs`, `engine.rs`, `summary.rs`
- Modify: `crates/goverify-checkers/src/nil.rs` (signature only)
- Modify: `crates/goverify-cli/src/main.rs` (mk_backend role closure)
- Modify: `crates/goverify-ir/src/testutil.rs` (want parser)
- Modify: `crates/goverify-analysis/tests/engine_corpus.rs`, `crates/goverify-checkers/tests/nil_corpus.rs`, `crates/goverify-solver/tests/differential.rs` if they construct backends/checkers (compiler-guided)

**Interfaces:**
- Produces (frozen for Tasks 6–12):

```rust
// checker.rs
pub trait Checker: Sync {
    fn name(&self) -> &'static str;
    fn infer_requires(
        &self,
        p: &Program,
        f: FuncId,
        summary_of: &dyn Fn(FuncId) -> Summary,
        discharge: &mut dyn FnMut(&Query) -> SatResult,
    ) -> Vec<Clause>;
    fn obligations(
        &self,
        p: &Program,
        f: FuncId,
        summary_of: &dyn Fn(FuncId) -> Summary,
    ) -> Vec<Obligation>;
}
pub struct Finding {
    pub checker: String,
    pub tag: String,
    pub func: String,
    pub pos: Option<Pos>,
    pub message: String,
    pub trace: Vec<TraceStep>,   // filled by Task 10; empty until then
}
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct TraceStep { pub block: u32, pub pos: Option<Pos> }

// engine.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendRole { Infer, Findings }
pub fn analyze_full(p, cfg, checkers, mk_backend: &(dyn Fn(BackendRole) -> Box<dyn TextSolver> + Sync)) -> Analysis;

// summary.rs
pub struct BoundClause {
    pub tag: String,
    pub bound: Option<Term>,      // clause with p<i> := args substituted (NOT negated)
    pub violation: Option<Term>,  // ¬bound
}

// goverify-ir testutil
pub fn wants(module: &str) -> Vec<(String, u32, String)>; // (file name, 1-based line, tag)
```

- [ ] **Step 1: trait + BoundClause.bound + Finding.tag/trace, compiler-guided fixups**

Make the signature/struct changes above. Mechanical fallout (let the compiler enumerate):
- `engine.rs analyze_function`: pass `summary_of` through to `infer_requires`.
- `engine.rs` findings pass: `checker.obligations(p, f, &summary_of)` unchanged shape; `Finding` construction adds `tag: ob.tag.clone(), trace: Vec::new()`.
- `engine.rs analyze_full`: sort checkers by name once at entry (deterministic requires order — retires carried T12):

```rust
    let mut checkers: Vec<&dyn Checker> = checkers.to_vec();
    checkers.sort_by_key(|c| c.name());
    let checkers = &checkers[..];
```

- `engine.rs`: `analyze()` passes `&|_role| Box::new(StubSolver)`; findings pass calls `mk_backend(BackendRole::Findings)`; fixpoint workers call `mk_backend(BackendRole::Infer)`.
- `summary.rs bind_violation` → return both bound and violation:

```rust
fn bind(f: &Formula, arg_terms: &[Option<Term>]) -> Option<(Term, Term)> {
    let mut map = BTreeMap::new();
    for (name, _sort) in f.term.free_vars() {
        let idx: u32 = name.strip_prefix('p')?.parse().ok()?;
        let t = arg_terms.get(idx as usize)?.clone()?;
        map.insert(name, t);
    }
    let bound = f.term.substitute(&map).ok()?;
    let violation = Term::not(bound.clone()).ok()?;
    Some((bound, violation))
}
```

with `instantiate_requires` mapping `Some((b, v))` to `BoundClause { tag, bound: Some(b), violation: Some(v) }` and `None` to both-None. Existing summary tests: update field access (`.violation` still there).
- `nil.rs NilTracer`: add the `summary_of` parameter (unused `_summary_of` in `infer_requires`; `obligations` already takes it).
- Fake/test checkers in `engine.rs` tests: add the parameter.
- `main.rs run_findings`: closures become role-aware —

```rust
    let mk: Box<dyn Fn(goverify_analysis::BackendRole) -> Box<dyn goverify_solver::TextSolver> + Sync> =
        match cmd {
            Some(c) => Box::new(move |_role| Box::new(goverify_solver::SmtLib2Process::new(&c, limits))),
            None => Box::new(move |_role| Box::new(goverify_solver::Z3Native::new(limits))),
        };
```

(`debug findings` keeps one timeout for both roles; `check` in Task 11 differentiates.)
- `engine_corpus.rs` / `nil_corpus.rs`: closures gain `|_role|`.

Run: `mise x -- cargo test --workspace --all-features`
Expected: PASS (pure plumbing, no semantic change — findings identical).

- [ ] **Step 2: want-comment parser in testutil, with test**

Append to `crates/goverify-ir/src/testutil.rs`:

```rust
/// Machine-checked corpus expectations (phase-4 spec §6): `// want: tag`
/// (comma-separated for several on one line) attached to the line it
/// annotates. Returns (file name, 1-based line, tag) sorted.
pub fn wants(module: &str) -> Vec<(String, u32, String)> {
    let dir = repo_root().join("testdata/corpus").join(module);
    let mut out = Vec::new();
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("corpus dir {}: {e}", dir.display()))
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "go"))
        .collect();
    files.sort();
    for path in files {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let text = std::fs::read_to_string(&path).unwrap();
        for (i, line) in text.lines().enumerate() {
            let Some(rest) = line.split("// want:").nth(1) else {
                continue;
            };
            for tag in rest.split(',') {
                let tag = tag.trim();
                if !tag.is_empty() {
                    out.push((name.clone(), (i + 1) as u32, tag.to_string()));
                }
            }
        }
    }
    out.sort();
    out
}
```

Unit test beside it (testutil has none yet — add a `#[cfg(test)] mod tests` in testutil.rs using a corpus module that has want comments; defer the assertion to Task 6 when nil corpus carries them, so here just test the parser on a temp dir? `wants` hardcodes repo_root — make the core parser testable:

```rust
pub fn wants(module: &str) -> Vec<(String, u32, String)> {
    wants_in(&repo_root().join("testdata/corpus").join(module))
}
pub fn wants_in(dir: &std::path::Path) -> Vec<(String, u32, String)> { /* body above */ }
```

test:

```rust
    #[test]
    fn wants_parses_tags_lines_and_multi() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join("a.go"),
            "package a\nfunc f() {} // want: nil-deref\n_ = x // want: bounds, div-zero\n",
        )
        .unwrap();
        assert_eq!(
            wants_in(d.path()),
            vec![
                ("a.go".into(), 2, "nil-deref".into()),
                ("a.go".into(), 3, "bounds".into()),
                ("a.go".into(), 3, "div-zero".into()),
            ]
        );
    }
```

(`goverify-ir` already has `tempfile` as a dev-dependency from phase 2's Task 7 promotion — verify in its Cargo.toml; it's a runtime dep there, fine.)

Run: `mise x -- cargo test -p goverify-ir testutil`
Expected: PASS.

- [ ] **Step 3: full gate + commit**

Run: `mise run lint && mise run test`
Expected: PASS.

```bash
git add crates/goverify-analysis crates/goverify-checkers crates/goverify-cli crates/goverify-ir
git commit --no-gpg-sign -m "analysis: checker trait gains summary_of + backend roles; Finding.tag/trace; want-comment parser"
```

---

### Task 6: Nil checker rewrite — path-sensitive `infer_requires`

Replace the entry-block heuristic: every pointer deref site in every block, sat-gated by reachability; plus requires **propagation** — a call site whose instantiated callee clause is violable and expressible over the caller's params becomes the caller's own requires.

**Files:**
- Modify: `crates/goverify-checkers/src/nil.rs`
- Modify: `testdata/corpus/nil/nil.go` (want comments + new cases)
- Modify: `crates/goverify-checkers/tests/nil_corpus.rs` (want harness)
- Delete: `testdata/goldens/nil.findings.txt` usage moves to Task 11's check golden (keep the file until Task 11 rewires; here keep the golden assertion updated if findings text changes)

**Interfaces:**
- Consumes: `encode::{encode_func, EncodedFunc}`, `instantiate_requires` (with `.bound`), `iface_var_name`.
- Produces: `NilChecker` (renamed from `NilTracer`; `goverify-checkers/src/lib.rs` re-exports `NilChecker`; CLI + tests updated). Tag: `"nil-deref"`. Helper reused by Task 7/8:

```rust
/// All (block index, instr index, address ValueId, pos) deref sites.
fn deref_sites(func: &Function) -> Vec<(usize, usize, ValueId, Option<Pos>)>;
/// True iff every free var of `t` is a p<i> param name.
pub(crate) fn params_only(t: &Term) -> bool;
```

- [ ] **Step 1: failing unit tests**

Rewrite `nil.rs`'s test module (keep the gvir builders; they already build blocks/branches). New/changed tests:

```rust
    #[test]
    fn unguarded_param_deref_infers_nonnil_requires() { /* keep, now any block: move deref to block 1 unconditionally jumped to — still infers */ }

    #[test]
    fn guarded_deref_infers_nothing() {
        // if p == nil { return }; deref in the else block. reach(deref)
        // implies p != nil, so reach ∧ nil is UNSAT: no clause. This is
        // the entry-block-panic FP class fix — the phase-3 heuristic
        // could not see this; now the guard is a path condition.
        // Fixture: block 0 BinOp Eq(p, nil-const) -> Branch; succs
        // [ret, deref-block]. Requires must be empty WITH the real Z3
        // discharge.
    }

    #[test]
    fn conditionally_reachable_deref_still_infers() {
        // if c { deref(p) } — reach ∧ nil is SAT (c free): clause emitted.
        // (Conditional-deref requires are deliberate — spec §4.)
    }

    #[test]
    fn requires_propagate_through_call_sites() {
        // t.F(p) { deref p }  t.G(q) { t.F(q) } — G must inherit
        // requires ¬nil(q) via instantiate_requires + summary_of.
        // Drive infer_requires(G) with summary_of returning F's inferred
        // summary; assert one clause whose formula's free vars == {"p0"}.
    }

    #[test]
    fn propagation_stops_at_unexpressible_args() {
        // t.H() { t.F(load-result) } — arg term is a havoc'd v<id>:
        // free vars ⊄ params ⇒ no clause on H.
    }

    #[test]
    fn unknown_verdict_infers_nothing() { /* keep as-is */ }
```

Run: `mise x -- cargo test -p goverify-checkers`
Expected: FAIL (new behaviors absent).

- [ ] **Step 2: implement**

```rust
//! NilChecker (phase-4 spec §4): path-sensitive nil-safety on the gated
//! SSA encoding. Requires-inference covers every deref site whose nil
//! path is reachable (Sat-gated); requires propagate bottom-up through
//! call sites when the instantiated callee clause stays expressible
//! over this function's params. Obligations (call-site + manifest-local)
//! are raised in `obligations` under the function's own preconditions.

use goverify_analysis::encode::{EncodedFunc, encode_func};
use goverify_analysis::{
    Checker, Clause, Formula, Obligation, Summary, instantiate_requires,
};
use goverify_ir::{Callee, FuncId, Function, Op, Pos, Program, TypeKind, ValueId};
use goverify_solver::{Query, SatResult, Term, ptr_is_nil};

pub struct NilChecker;

fn deref_sites(p: &Program, func: &Function) -> Vec<(usize, usize, ValueId, Option<Pos>)> {
    let mut out = Vec::new();
    for (bi, b) in func.blocks.iter().enumerate() {
        for (ii, ins) in b.instrs.iter().enumerate() {
            let subject = match &ins.op {
                Op::Load { addr, .. } | Op::Store { addr, .. } => *addr,
                Op::FieldAddr { base, .. } | Op::Field { base, .. } => *base,
                _ => continue,
            };
            if !matches!(
                p.types().kind(func.value(subject).ty),
                TypeKind::Pointer { .. }
            ) {
                continue;
            }
            out.push((bi, ii, subject, ins.pos.clone()));
        }
    }
    out
}

pub(crate) fn params_only(t: &Term) -> bool {
    t.free_vars().keys().all(|n| {
        n.strip_prefix('p')
            .is_some_and(|rest| rest.parse::<u32>().is_ok())
    })
}

/// Push a clause unless an equal one is present (fixpoint-friendly dedup).
fn push_clause(out: &mut Vec<Clause>, c: Clause) {
    if !out.contains(&c) {
        out.push(c);
    }
}

impl Checker for NilChecker {
    fn name(&self) -> &'static str {
        "nil"
    }

    fn infer_requires(
        &self,
        p: &Program,
        f: FuncId,
        summary_of: &dyn Fn(FuncId) -> Summary,
        discharge: &mut dyn FnMut(&Query) -> SatResult,
    ) -> Vec<Clause> {
        let Some(func) = p.func(f) else { return Vec::new() };
        let Ok(enc) = encode_func(p, f) else { return Vec::new() };
        let mut out = Vec::new();
        // Own deref sites.
        for (bi, _ii, subject, _pos) in deref_sites(p, func) {
            let Some(subj) = enc.value(subject).cloned() else { continue };
            let Ok(is_nil) = ptr_is_nil(subj.clone()) else { continue };
            if !params_only(&subj) {
                continue; // not expressible as a precondition
            }
            if discharge(&enc.reach_query(bi, vec![is_nil.clone()])) != SatResult::Sat {
                continue; // guarded (unsat) or unknown: stay silent
            }
            let Ok(nonnil) = Term::not(is_nil) else { continue };
            push_clause(&mut out, Clause {
                tag: "nil-deref".into(),
                formula: Formula { term: nonnil },
            });
        }
        // Propagated: violable callee requires expressible over params.
        for (bi, ins) in func.blocks.iter().enumerate().flat_map(|(bi, b)| {
            b.instrs.iter().map(move |i| (bi, i))
        }) {
            let Op::Call { callee: Callee::Static(c), args, .. } = &ins.op else {
                continue;
            };
            let arg_terms: Vec<Option<Term>> =
                args.iter().map(|a| enc.value(*a).cloned()).collect();
            for bc in instantiate_requires(&summary_of(*c), &arg_terms) {
                if bc.tag != "nil-deref" {
                    continue; // this checker only owns its own tag
                }
                let (Some(bound), Some(violation)) = (bc.bound, bc.violation) else {
                    continue;
                };
                if !params_only(&bound) {
                    continue;
                }
                if discharge(&enc.reach_query(bi, vec![violation])) != SatResult::Sat {
                    continue;
                }
                push_clause(&mut out, Clause {
                    tag: "nil-deref".into(),
                    formula: Formula { term: bound },
                });
            }
        }
        out
    }

    // `obligations` keeps its Task-5-adapted phase-3 body in this task
    // (const-nil call args only) — Task 7 replaces it wholesale. Do not
    // extend it here; Task 6's tests exercise `infer_requires` only.
}
```

Rename: `pub struct NilChecker`, `crates/goverify-checkers/src/lib.rs` exports `pub use nil::NilChecker;` (drop `NilTracer`), update `main.rs` and `nil_corpus.rs` references.

Run: `mise x -- cargo test -p goverify-checkers`
Expected: PASS.

- [ ] **Step 3: expand the nil corpus with machine want-comments; switch harness**

Rewrite `testdata/corpus/nil/nil.go`:

```go
// Package nilcorpus exercises the phase-4 nil checker: guards as path
// conditions, requires propagation, manifest-local nils, loops.
package nilcorpus

type T struct{ X int }

// deref unconditionally dereferences p: requires p != nil.
func deref(p *T) int { return p.X }

// guarded checks first — no requires, no finding.
func guarded(p *T) int {
	if p == nil {
		return 0
	}
	return p.X
}

// wrapper passes its own param through: inherits deref's requires,
// reports nothing itself.
func wrapper(p *T) int { return deref(p) }

// Bad passes a constant nil to deref.
func Bad() int { return deref(nil) } // want: nil-deref

// BadTwoHops trips wrapper's INHERITED requires.
func BadTwoHops() int { return wrapper(nil) } // want: nil-deref

// LocalNil dereferences a manifest local nil.
func LocalNil() int {
	var p *T
	return p.X // want: nil-deref
}

// Good passes nil only to the guarded function.
func Good() int { return guarded(nil) }

// LoopGuarded derefs only after a loop that assigns p — the back-edge
// cut keeps the first-iteration path; p is non-nil on every kept path.
func LoopGuarded(ts []*T) int {
	total := 0
	for _, t := range ts {
		if t == nil {
			continue
		}
		total += t.X
	}
	return total
}
```

**Caveat for the implementer:** `LocalNil`'s SSA shape — go/ssa may constant-fold `var p *T; return p.X` into a direct nil deref or a `Const nil` load subject. Verify with `mise x -- cargo run -p goverify-cli -- debug ir --func LocalNil` from `testdata/corpus/nil/` what the lowered ops look like, and adjust the corpus case (e.g. route the nil through a branch) until it exercises the manifest-local path Task 7 implements. If Task 7 hasn't landed yet, mark it `// want: nil-deref` and let the corpus test go RED here, GREEN after Task 7 — note it in the task report. Alternative: land this corpus change WITH Task 7 if the intermediate red is unacceptable — but prefer red-then-green across the two tasks (it proves the want harness catches missing findings).

Rewrite `crates/goverify-checkers/tests/nil_corpus.rs`'s golden test as the want-harness test (keep the determinism test as-is, modulo `NilChecker`):

```rust
#[test]
fn nil_corpus_findings_match_want_comments() {
    let p = goverify_ir::testutil::load_corpus("nil");
    let cfg = EngineConfig { opts: Options::default(), cache_dir: None, emit_smt: None };
    let checkers: Vec<&dyn goverify_analysis::Checker> = vec![&NilChecker];
    let a = analyze_full(&p, &cfg, &checkers, &|_role| Box::new(Z3Native::new(limits())));
    let got: std::collections::BTreeSet<(String, u32, String)> = a
        .findings
        .iter()
        .filter(|f| f.func.contains("example.com/nil"))
        .filter_map(|f| {
            let pos = f.pos.as_ref()?;
            Some((pos.file.clone(), pos.line, f.tag.clone()))
        })
        .collect();
    let want: std::collections::BTreeSet<(String, u32, String)> =
        goverify_ir::testutil::wants("nil").into_iter().collect();
    assert_eq!(got, want, "findings vs want comments");
}
```

Delete `testdata/goldens/nil.findings.txt` (its role is replaced by the want harness now and the check-render golden in Task 11).

Run: `mise x -- cargo test -p goverify-checkers --test nil_corpus`
Expected: PASS for every case Task 6 owns (see LocalNil caveat).

- [ ] **Step 4: full gate + commit**

Run: `mise run lint && mise run test`

```bash
git add crates/goverify-checkers testdata/corpus/nil testdata/goldens crates/goverify-cli
git commit --no-gpg-sign -m "checkers: path-sensitive NilChecker requires inference with propagation; nil corpus on want harness"
```

---

### Task 7: Nil checker — obligations under preconditions

Call-site obligations with real symbolic args, and manifest-local obligations, both discharged under the function's own inferred preconditions. This mechanizes the spec's who-reports-what rule.

**Files:**
- Modify: `crates/goverify-checkers/src/nil.rs`

**Interfaces:**
- Consumes: Task 6's `deref_sites`, `params_only`, `EncodedFunc`; `summary_of(f)` for own preconditions.
- Produces: `NilChecker::obligations` final form. Pattern (`own_preconditions` helper) reused by bounds in Task 8:

```rust
/// The function's own requires clauses as query conjuncts: their free
/// p<i> vars are exactly the encoder's param const names, so they can
/// be conjoined directly.
fn own_preconditions(own: &Summary) -> Vec<Term>;
```

- [ ] **Step 1: failing tests**

```rust
    #[test]
    fn const_nil_arg_produces_obligation_other_args_dont() { /* keep phase-3 test, adapt to new signature */ }

    #[test]
    fn wrapper_does_not_self_report() {
        // t.G(q) { t.F(q) } with F requiring ¬nil(p0) and G's own summary
        // (from summary_of) carrying the propagated ¬nil(p0):
        // obligation query = own-preconditions ∧ reach ∧ is-nil(q-term)
        // must be UNSAT ⇒ zero findings when discharged with Z3.
        // Assert: obligations() returns the obligation but its query is
        // Unsat under z3_discharge (the engine's Sat gate keeps it silent)
        // — OR NilChecker skips emitting it; either is acceptable, pin
        // the end state: no Sat verdict.
    }

    #[test]
    fn local_manifest_nil_deref_obligates() {
        // deref of a Const-nil-typed value inside the function (not via
        // call): one obligation, Sat under Z3.
    }

    #[test]
    fn havoc_subject_deref_stays_silent() {
        // deref of a Load result (havoc'd fresh var): free vars ⊄ params
        // and not ground ⇒ NO local obligation (FP storm guard).
    }

    #[test]
    fn symbolic_arg_instantiation_binds_caller_terms() {
        // t.H(a *T, c bool) { p := phi(a, nil based on c); t.F(p) }:
        // violation is-nil(ite(...)) is Sat ⇒ obligation fires with the
        // caller's own precondition set empty. (Phase 3 could only see
        // literal nil args; this pins the symbolic upgrade.)
    }
```

Run: `mise x -- cargo test -p goverify-checkers nil`
Expected: FAIL.

- [ ] **Step 2: implement `obligations`**

```rust
fn own_preconditions(own: &Summary) -> Vec<Term> {
    own.requires.iter().map(|c| c.formula.term.clone()).collect()
}

    fn obligations(
        &self,
        p: &Program,
        f: FuncId,
        summary_of: &dyn Fn(FuncId) -> Summary,
    ) -> Vec<Obligation> {
        let Some(func) = p.func(f) else { return Vec::new() };
        let Ok(enc) = encode_func(p, f) else { return Vec::new() };
        let pre = own_preconditions(&summary_of(f));
        let mut out = Vec::new();

        // Local manifest sites: subject term ground (const nil reached
        // through modeled ops) or params-only (then preconditions decide).
        for (bi, _ii, subject, pos) in deref_sites(p, func) {
            let Some(subj) = enc.value(subject).cloned() else { continue };
            let expressible = subj.free_vars().is_empty() || params_only(&subj);
            if !expressible {
                continue; // havoc'd heap value: silent (spec §4)
            }
            let Ok(is_nil) = ptr_is_nil(subj) else { continue };
            let mut extra = pre.clone();
            extra.push(is_nil);
            out.push(Obligation {
                tag: "nil-deref".into(),
                message: format!("nil dereference in {}", p.func_name(f)),
                pos,
                query: enc.reach_query(bi, extra),
            });
        }

        // Call sites: instantiated callee requires under own preconditions.
        for (bi, b) in func.blocks.iter().enumerate() {
            for ins in &b.instrs {
                let Op::Call { callee: Callee::Static(c), args, .. } = &ins.op else {
                    continue;
                };
                let arg_terms: Vec<Option<Term>> =
                    args.iter().map(|a| enc.value(*a).cloned()).collect();
                for bc in instantiate_requires(&summary_of(*c), &arg_terms) {
                    if bc.tag != "nil-deref" {
                        continue;
                    }
                    let Some(v) = bc.violation else { continue };
                    let mut extra = pre.clone();
                    extra.push(v);
                    out.push(Obligation {
                        tag: bc.tag.clone(),
                        message: format!(
                            "nil passed to {} (violates its {} requirement)",
                            p.func_name(*c),
                            bc.tag
                        ),
                        pos: ins.pos.clone(),
                        query: enc.reach_query(bi, extra),
                    });
                }
            }
        }
        out
    }
```

Note: duplicate suppression between the two arms is inherent — a param deref site is `params_only` ⇒ its own precondition `¬nil(p)` conjoined with `nil(p)` is Unsat ⇒ engine's Sat gate drops it. That's the design's rule working, not an accident; the `wrapper_does_not_self_report` test pins it.

Run: `mise x -- cargo test -p goverify-checkers`
Expected: PASS, including the corpus want test (LocalNil/BadTwoHops now green).

- [ ] **Step 3: full gate + commit**

Run: `mise run lint && mise run test`

```bash
git add crates/goverify-checkers
git commit --no-gpg-sign -m "checkers: NilChecker obligations — symbolic call-site instantiation + manifest locals under own preconditions"
```

---

### Task 8: Bounds checker — index and slice obligations

New `BoundsChecker` sharing the encoder and the same requires/obligations split. This task: Index/IndexAddr/Slice sites. Div/narrowing are Task 9.

**Files:**
- Create: `crates/goverify-checkers/src/bounds.rs`
- Modify: `crates/goverify-checkers/src/lib.rs`
- Create: `testdata/corpus/bounds/go.mod`, `testdata/corpus/bounds/bounds.go`
- Create: `crates/goverify-checkers/tests/bounds_corpus.rs`
- Modify: `mise.toml` (corpus task gains the new test)

**Interfaces:**
- Consumes: `encode::{encode_func, seq_datatype, int_repr}`, `params_only`, `own_preconditions` (move both helpers to a small `crates/goverify-checkers/src/shared.rs` — `pub(crate)`), `instantiate_requires`.
- Produces: `pub struct BoundsChecker;` name `"bounds"`, tags `"bounds"` (this task), `"div-zero"`/`"overflow"` (Task 9). Site helper:

```rust
/// (block, subject-base, index/low/high/max terms, pos) per site kind.
enum BoundsSite { Index { base: ValueId, index: ValueId }, Slice { base: ValueId, low: Option<ValueId>, high: Option<ValueId>, max: Option<ValueId> } }
```

- [ ] **Step 1: failing unit tests**

In `bounds.rs` tests (gvir builders as in nil.rs — extract the shared builder fns into `crates/goverify-checkers/src/testfix.rs` `#[cfg(test)]` module rather than duplicating a third time; nil.rs adopts it too):

```rust
    #[test]
    fn index_on_slice_param_infers_bounds_requires() {
        // t.F(s []int, i int) { _ = s[i] } ⇒ requires
        // 0 <= i < seq-len(s): formula free vars {p0, p1}, tag "bounds".
    }

    #[test]
    fn constant_index_under_len_guard_infers_nothing() {
        // if len(s) > 2 { _ = s[2] } — reach implies len > 2: UNSAT.
        // NOTE: len(s) lowers as a Call to builtin "len" — its dst
        // havocs. To keep this case meaningful the encoder must model
        // builtin len: extend Task 4's op_def with
        //   Op::Call { dst: Some(d), callee: Callee::Builtin(name), args }
        //   if name == "len" && arg is Seq-sorted =>
        //     (= v<d> (seq-len arg))  [width note: len result is int=BV64 ✓]
        // Add that arm HERE with its own unit test (it lives in
        // encode.rs; this checker test is its integration proof).
    }

    #[test]
    fn array_index_uses_static_len() {
        // var a [3]int; _ = a[i] with i param ⇒ requires 0 <= i < 3
        // (array len from TypeKind::Array{len}; base term not needed).
    }

    #[test]
    fn slice_expr_checks_low_le_high_le_cap() {
        // s[l:h] ⇒ violation (or (bvslt l 0) (bvslt h l) (bvsgt h cap(s))).
        // Since bv_cmp has no Sgt: encode h > cap as (bvslt cap h).
    }

    #[test]
    fn manifest_out_of_bounds_obligates() {
        // s := make([]int, 3); _ = s[5] ⇒ local obligation, Sat.
    }
```

Run: `mise x -- cargo test -p goverify-checkers bounds`
Expected: FAIL/compile error.

- [ ] **Step 2: implement — including the `len` builtin encoder arm**

encode.rs addition (with unit test in encode.rs's module):

```rust
        Op::Call { dst: Some(d), callee: goverify_ir::Callee::Builtin(name), args }
            if name == "len" =>
        {
            let dt = t(d)?;
            let arg = args.first().and_then(|a| enc.values.get(a).cloned())?;
            if arg.sort() != &seq_datatype().sort() {
                return None;
            }
            let len = Term::dt_get(&seq_datatype(), "seq-val", "seq-len", arg).ok()?;
            Term::eq(dt, len).ok()
        }
```

bounds.rs core — obligation/requires share one violation builder:

```rust
/// index violation: i < 0 ∨ i >= len  (signed: Go index is int)
fn index_violation(idx: Term, len: Term) -> Option<Term> {
    let neg = Term::bv_cmp(BvCmpOp::Slt, idx.clone(), Term::bv_lit(64, 0)).ok()?;
    let over = Term::bv_cmp(BvCmpOp::Sle, len, idx).ok()?;
    Term::or(vec![neg, over]).ok()
}

/// len term for an index/slice base: Seq values via seq-len accessor,
/// arrays via their static type length; None = unmodelable (silent).
fn len_of(p: &Program, func: &Function, enc: &EncodedFunc, base: ValueId) -> Option<Term> {
    match p.types().kind(func.value(base).ty) {
        TypeKind::Array { len, .. } => Some(Term::bv_lit(64, *len as u128)),
        _ => {
            let b = enc.value(base)?.clone();
            if b.sort() != &seq_datatype().sort() {
                return None;
            }
            Term::dt_get(&seq_datatype(), "seq-val", "seq-len", b).ok()
        }
    }
}
```

`infer_requires`: per Index/IndexAddr site with idx/len terms `params_only` (idx term AND len term jointly — build the violation, check `params_only(&violation)`), Sat-gate `reach ∧ violation`, emit `Clause { tag: "bounds", formula: ¬violation }`. Plus the same propagation arm as nil (tag filter `"bounds"`); factor the propagation loop into `shared.rs`:

```rust
pub(crate) fn propagate_requires(
    p: &Program, func: &Function, enc: &EncodedFunc, tag: &str,
    summary_of: &dyn Fn(FuncId) -> Summary,
    discharge: &mut dyn FnMut(&Query) -> SatResult,
    out: &mut Vec<Clause>,
)
```

(nil.rs adopts it — dedup the Task 6 code.)

`obligations`: local sites where terms are ground-or-params-only, query = own preconditions ∧ reach ∧ violation; call-site arm identical to nil's with tag `"bounds"` — move THAT loop into `shared.rs` too, parameterized by tag and message prefix:

```rust
pub(crate) fn call_site_obligations(
    p: &Program, func: &Function, enc: &EncodedFunc, tag: &str,
    pre: &[Term], summary_of: &dyn Fn(FuncId) -> Summary,
) -> Vec<Obligation>
```

Slice sites: violation = `low < 0 ∨ high < low ∨ cap < high` with cap = `seq-cap` accessor (arrays: static len as cap), missing low/high default to `bv_lit(64,0)` / len term respectively (Go defaults), `max` ignored beyond `cap < max` when present.

Run: `mise x -- cargo test -p goverify-checkers`
Expected: PASS.

- [ ] **Step 3: bounds corpus + want harness + mise wiring**

`testdata/corpus/bounds/go.mod`:

```
module example.com/bounds

go 1.26
```

(copy the exact `go` directive from `testdata/corpus/ops/go.mod`).

`testdata/corpus/bounds/bounds.go`:

```go
// Package boundscorpus exercises the phase-4 bounds checker: index and
// slice obligations, guards as path conditions, requires propagation.
package boundscorpus

// get requires 0 <= i < len(s).
func get(s []int, i int) int { return s[i] }

// safe guards before indexing — no requires, no finding.
func safe(s []int, i int) int {
	if i < 0 || i >= len(s) {
		return 0
	}
	return s[i]
}

// BadIndex indexes a 3-element slice at 5.
func BadIndex() int {
	s := make([]int, 3)
	return s[5] // want: bounds
}

// BadCall violates get's inferred requires.
func BadCall() int {
	s := make([]int, 3)
	return get(s, 7) // want: bounds
}

// GoodCall respects it.
func GoodCall() int {
	s := make([]int, 3)
	return get(s, 2)
}

// BadSlice slices past capacity.
func BadSlice() []int {
	s := make([]int, 2, 4)
	return s[1:5] // want: bounds
}
```

`crates/goverify-checkers/tests/bounds_corpus.rs` — same shape as `nil_corpus.rs` (want harness + determinism run), module `"bounds"`, checker `BoundsChecker`.

`mise.toml` corpus task, extend the checker line:

```toml
  "cargo test -p goverify-checkers --test nil_corpus --test bounds_corpus",
```

Run: `mise run corpus`
Expected: PASS.

- [ ] **Step 4: full gate + commit**

```bash
git add crates/goverify-checkers testdata/corpus/bounds mise.toml crates/goverify-analysis/src/encode.rs
git commit --no-gpg-sign -m "checkers: BoundsChecker index/slice obligations + requires; bounds corpus; len builtin encoding"
```

---

### Task 9: Bounds checker — division and narrowing conversions

**Files:**
- Modify: `crates/goverify-checkers/src/bounds.rs`
- Modify: `testdata/corpus/bounds/bounds.go`
- Modify: `crates/goverify-analysis/src/encode.rs` (nothing new needed — Convert dst already havocs; div/rem values already encode)

**Interfaces:**
- Consumes: `int_repr` (width+sign), existing site scan.
- Produces: tags `"div-zero"` and `"overflow"` live.

- [ ] **Step 1: failing tests**

```rust
    #[test]
    fn div_by_param_infers_nonzero_requires() {
        // t.F(a, b int) { _ = a / b } ⇒ requires b != 0, tag "div-zero".
    }

    #[test]
    fn guarded_div_infers_nothing() { /* if b != 0 { a / b } ⇒ unsat */ }

    #[test]
    fn signed_div_minint_overflow_obligation() {
        // int8 path: a / b violated when a == -128 ∧ b == -1, tag
        // "overflow". Use int8 params so the literals are small.
    }

    #[test]
    fn unsigned_div_has_no_minint_case() { /* uint: only div-zero emitted */ }

    #[test]
    fn narrowing_convert_infers_range_requires() {
        // t.F(x int) int8 { return int8(x) } ⇒ requires -128 <= x <= 127
        // (tag "overflow"): violation (or (bvslt x -128) (bvslt 127 x))
        // with 64-bit literals (sign-extended two's complement for -128).
    }

    #[test]
    fn unsigned_narrowing_uses_unsigned_bound() {
        // uint16(u) for u uint32 ⇒ violation (bvult 0xFFFF u) — wait,
        // that's u > 0xFFFF ⇒ encode as Ult(lit, u).
    }

    #[test]
    fn widening_and_same_width_convert_infers_nothing() { /* int32(x) for x int8 */ }

    #[test]
    fn sign_change_same_width_is_overflow_site() {
        // uint64(x) for x int: violated when x < 0.
    }
```

Run: expect FAIL.

- [ ] **Step 2: implement**

Div/Rem sites — scan `Op::BinOp { kind: Div | Rem, lhs, rhs, .. }`:
- `"div-zero"` violation: `rhs == 0` at the operand width (`Term::eq(r, bv_lit(w, 0))`).
- `"overflow"` (signed Div only): `lhs == MIN(w) ∧ rhs == all-ones(w)` (`MIN(w)` = `1 << (w-1)` as the bit pattern; `-1` = all ones).

Narrowing Convert sites — `Op::Convert { dst, src }` with `int_repr` for both sides `(ws, ss)` → `(wd, sd)`:
- skip unless both are ints and (wd < ws, or wd == ws && sd != ss);
- violation (src term `x` at width `ws`):
  - signed→signed narrow: `x < MIN_d` ∨ `MAX_d < x` (Slt both ways, literals sign-extended to `ws`);
  - unsigned→unsigned narrow: `MAX_d <u x` (Ult(lit, x));
  - signed→unsigned: `x < 0` ∨ (`wd < ws` ? `MAX_d <s x` : nothing more);
  - unsigned→signed: `MAX_d <u x` where MAX_d = `2^(wd-1) - 1` zero-extended.

Literal helper (all literals constructed at width `ws`):

```rust
fn lit_sext(ws: u32, value: i128) -> Term {
    Term::bv_lit(ws, (value as u128) & (u128::MAX >> (128 - ws)))
}
```

Requires arm and obligations arm identical in shape to Task 8 (params-only gate → requires clause `¬violation`; ground/params-only → local obligation under preconditions; propagation via `shared::propagate_requires` with the two new tags — note `propagate_requires`/`call_site_obligations` take the tag as a parameter, call once per tag).

- [ ] **Step 3: corpus additions**

Append to `bounds.go`:

```go
// div requires b != 0.
func div(a, b int) int { return a / b }

// BadDiv divides by a constant zero path.
func BadDiv(a int) int {
	b := 0
	return a / b // want: div-zero
}

// GoodDiv guards.
func GoodDiv(a, b int) int {
	if b == 0 {
		return 0
	}
	return a / b
}

// BadNarrow truncates 300 into an int8.
func BadNarrow() int8 {
	x := 300
	return int8(x) // want: overflow
}

// GoodNarrow stays in range.
func GoodNarrow() int8 {
	x := 100
	return int8(x)
}
```

(**Caveat**: go/ssa constant-folds aggressively — `int8(300)` on a literal is a compile error in Go and `x := 300; int8(x)` may fold to a constant. Verify the lowered IR contains a real Convert; if folded, route `x` through an identity function the inliner can't see, e.g. `func opaque(v int) int { return v }` — adjust until `debug ir` shows the Convert. Same verification for `BadDiv`'s `b`.)

Run: `mise run corpus`
Expected: PASS with the new want lines.

- [ ] **Step 4: full gate + commit**

```bash
git add crates/goverify-checkers testdata/corpus/bounds
git commit --no-gpg-sign -m "checkers: BoundsChecker div-zero/minint-overflow/narrowing obligations; corpus cases"
```

---

### Task 10: Trace reconstruction from sat models

Walk the model's guard assignments into a block path; attach `TraceStep`s to findings in the engine.

**Files:**
- Modify: `crates/goverify-analysis/src/encode.rs` (model parsing + path walk)
- Modify: `crates/goverify-analysis/src/engine.rs` (attach traces on Sat)

**Interfaces:**
- Consumes: `QueryOutcome.model: Option<String>` (both backend formats), `EncodedFunc.dag_succs`, `Finding.trace` (Task 5).
- Produces:

```rust
/// Best-effort model reading for DISPLAY ONLY (verdicts never depend on
/// it): "gN -> true" lines (Z3Native / Z3_model_to_string) and
/// "(define-fun gN () Bool true)" s-exprs (get-model backends).
pub fn guard_values(model_text: &str) -> BTreeMap<String, bool>;
/// Param/variable bindings as display strings ("p0" -> "(ptr-nil)").
pub fn model_bindings(model_text: &str) -> BTreeMap<String, String>;
/// Entry-to-somewhere path: from block 0, repeatedly take the first
/// DAG successor whose guard is true in the model (unassigned = false);
/// stops when no successor qualifies. Deterministic; empty on any
/// irregularity (missing g0, malformed model).
pub fn violating_path(func: &Function, dag_succs: &[Vec<u32>], guards: &BTreeMap<String, bool>) -> Vec<u32>;
```

- [ ] **Step 1: failing unit tests**

```rust
    #[test]
    fn guard_values_parses_z3native_arrow_format() {
        let m = "g0 -> true\ng1 -> false\np0 -> (ptr-addr #x0000000000000001)\n";
        let g = guard_values(m);
        assert_eq!(g.get("g0"), Some(&true));
        assert_eq!(g.get("g1"), Some(&false));
        assert_eq!(g.get("p0"), None, "non-bool ignored");
    }

    #[test]
    fn guard_values_parses_define_fun_format() {
        let m = "((define-fun g0 () Bool true)\n (define-fun g2 () Bool false))";
        let g = guard_values(m);
        assert_eq!(g.get("g0"), Some(&true));
        assert_eq!(g.get("g2"), Some(&false));
    }

    #[test]
    fn guard_values_tolerates_garbage() {
        assert!(guard_values("((((").is_empty());
        assert!(guard_values("").is_empty());
    }

    #[test]
    fn violating_path_walks_true_guards() {
        // diamond 0->{1,2}->3; guards g0,g1,g3 true, g2 false:
        // path = [0, 1, 3].
    }

    #[test]
    fn violating_path_stops_at_dont_care() {
        // g0 true, successors' guards unassigned: path = [0].
    }

    #[test]
    fn model_bindings_extracts_display_strings() {
        let m = "p0 -> (ptr-nil)\nv3 -> #x0000000000000005\n";
        let b = model_bindings(m);
        assert_eq!(b.get("p0").map(String::as_str), Some("(ptr-nil)"));
    }
```

Run: expect FAIL.

- [ ] **Step 2: implement**

`guard_values`: try line format first (`name -> value` split), fall back to `goverify_solver::parse_sexpr` on the whole text, walking `define-fun` triples. Both wrapped so any parse error yields the empty map (this is display plumbing — it must never panic or influence verdicts). `model_bindings`: same two formats, values kept as trimmed source text, `String::from_utf8_lossy`-style tolerance. Sanitization happens at render time (Task 11), not here.

`violating_path`:

```rust
pub fn violating_path(
    func: &Function,
    dag_succs: &[Vec<u32>],
    guards: &BTreeMap<String, bool>,
) -> Vec<u32> {
    if guards.get("g0") != Some(&true) {
        return Vec::new();
    }
    let mut path = vec![0u32];
    let mut cur = 0usize;
    loop {
        let Some(next) = dag_succs.get(cur).and_then(|ss| {
            ss.iter()
                .find(|&&s| guards.get(&format!("g{s}")) == Some(&true))
        }) else {
            break;
        };
        let next = *next as usize;
        if next >= func.blocks.len() || path.len() > func.blocks.len() {
            break; // safety: DAG walk can't exceed block count
        }
        path.push(next as u32);
        cur = next;
    }
    path
}
```

Engine findings pass — after the Sat gate:

```rust
                        if outcome.result == SatResult::Sat {
                            let trace = outcome
                                .model
                                .as_deref()
                                .and_then(|m| trace_for(p, f, m))
                                .unwrap_or_default();
                            per_func.push(Finding {
                                checker: checker.name().to_string(),
                                tag: ob.tag.clone(),
                                func: p.func_name(f).to_string(),
                                pos: ob.pos,
                                message: ob.message,
                                trace,
                            });
                        }
```

with:

```rust
/// Reconstruct the violating path for a Sat finding: re-encode (cheap,
/// pure, deterministic), read guard values, walk. Any failure = no
/// trace (finding still reported).
fn trace_for(p: &Program, f: FuncId, model: &str) -> Option<Vec<crate::checker::TraceStep>> {
    let func = p.func(f)?;
    let enc = crate::encode::encode_func(p, f).ok()?;
    let guards = crate::encode::guard_values(model);
    let path = crate::encode::violating_path(func, &enc.dag_succs, &guards);
    if path.is_empty() {
        return None;
    }
    Some(
        path.into_iter()
            .map(|b| crate::checker::TraceStep {
                block: b,
                pos: func.blocks[b as usize]
                    .instrs
                    .iter()
                    .find_map(|i| i.pos.clone()),
            })
            .collect(),
    )
}
```

Integration test in `engine.rs` tests: an `AlwaysSat`-style backend returning a canned model string (`"g0 -> true\n"`) must produce a finding whose trace is `[TraceStep { block: 0, .. }]`; the existing `AlwaysSat` (model: None) keeps producing traceless findings — assert both.

Run: `mise x -- cargo test -p goverify-analysis`
Expected: PASS.

- [ ] **Step 3: full gate + commit**

```bash
git add crates/goverify-analysis
git commit --no-gpg-sign -m "analysis: violating-path trace reconstruction from sat-model guard values"
```

---

### Task 11: `goverify check` — renderer, exit codes, goldens

**Files:**
- Create: `crates/goverify-cli/src/render.rs`
- Modify: `crates/goverify-cli/src/main.rs`
- Modify: `docs/threat-model.md` (model text now rendered — sanitization bullet)
- Create: `testdata/goldens/nil.check.txt`, `testdata/goldens/bounds.check.txt`
- Modify: `crates/goverify-cli/tests/debug_integration.rs` (check integration + goldens)
- Modify: `mise.toml` corpus task (cli test already listed)

**Interfaces:**
- Consumes: `Analysis.findings` (tag/trace), `goverify_analysis::encode::model_bindings` — **no**: renderer consumes only `Finding`; model values arrive via trace? `Finding` has no model bindings field — **decision**: `Finding` gains `pub model: Vec<(String, String)>` (sorted param bindings, display-only) in this task, populated in the engine next to `trace` via `model_bindings` filtered to `p<i>` names. Compiler-guided fixups (Finding constructions in tests add `model: Vec::new()`).
- Produces:

```rust
// render.rs
/// Human terminal rendering (spec §5): span, caret, trace, model
/// values, fired-clause message. `source_root` resolves Pos.file
/// (relative paths from the extractor); unreadable/missing source
/// degrades to the header line without a snippet.
pub fn render_findings(findings: &[Finding], source_root: &Path) -> String;
/// Strip ANSI/control chars from solver-derived text before terminal
/// output (threat-model: model text is untrusted-ish display input).
fn sanitize(s: &str) -> String;
```

- CLI: `goverify check [patterns] [--gvir-dir] [--emit-smt] [--solver-cmd] [--solver-timeout-ms 100] [--obligation-timeout-ms 250] [--cache-dir]`; exit 0 clean / 1 findings / 2 error.

- [ ] **Step 1: failing renderer unit tests**

`render.rs` tests build `Finding`s by hand plus a temp source tree:

```rust
    #[test]
    fn renders_span_caret_trace_and_model() {
        // finding at file "m.go" line 3 col 9, source written to a temp
        // root; trace [block0@line2, block1@line3]; model [("p0","(ptr-nil)")].
        // Expected exact output (pin the whole string):
        // m.go:3:9: nil-deref: nil passed to t.F (violates its nil-deref requirement) [t.Bad]
        //     3 |     return deref(nil)
        //       |         ^
        //     path: m.go:2 -> m.go:3
        //     with: p0 = (ptr-nil)
    }

    #[test]
    fn missing_source_degrades_to_header() { /* no snippet lines, still the header */ }

    #[test]
    fn sanitize_strips_control_sequences() {
        assert_eq!(sanitize("a\x1b[31mred\x07b"), "a[31mredb");
        // keep \t? no: replace every C0 control except nothing — traces
        // are single-line; strip chars < 0x20 plus 0x7f.
    }

    #[test]
    fn findings_render_in_order_with_blank_line_between() { /* two findings */ }
```

Freeze the exact format in the first test — the goldens below inherit it.

Run: expect FAIL.

- [ ] **Step 2: implement renderer + check subcommand**

`render.rs` (~120 lines): per finding — header `pos: tag: message [func]` (pos format as `dump_findings`), then the source line with a `^` caret at the column (tab-expanded as-is, column is byte-based — good enough for v1), then `path:` line joining trace positions (skip trace steps without pos; omit line when empty), then `with:` line for model bindings (omit when empty). All solver-derived strings (`message` is analyzer-built — safe; model values + any source echo) pass through `sanitize`.

`main.rs`:

```rust
    /// Analyze packages and report findings (spec §10).
    Check(CheckArgs),
```

```rust
#[derive(clap::Args)]
struct CheckArgs {
    /// Directory of pre-extracted .gvir files (omit to extract).
    #[arg(long)]
    gvir_dir: Option<PathBuf>,
    /// Go package patterns for extraction (ignored with --gvir-dir).
    #[arg(default_value = "./...")]
    patterns: Vec<String>,
    #[arg(long)]
    emit_smt: Option<PathBuf>,
    #[arg(long)]
    solver_cmd: Option<String>,
    /// Per-query timeout for requires-inference queries (ms).
    #[arg(long, default_value_t = 100)]
    solver_timeout_ms: u32,
    /// Per-query timeout for obligation (findings) queries (ms) —
    /// function-sized formulas get more room (spec §8).
    #[arg(long, default_value_t = 250)]
    obligation_timeout_ms: u32,
    #[arg(long)]
    cache_dir: Option<PathBuf>,
}
```

`run()` becomes `fn run() -> Result<ExitCode, Box<dyn std::error::Error>>` with `main` mapping `Err` to 2 as today; `run_check`:

```rust
fn run_check(ca: CheckArgs) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let dargs = DebugArgs { gvir_dir: ca.gvir_dir.clone(), func: None, patterns: ca.patterns.clone() };
    let program = load_program(&dargs)?;
    for d in program.diagnostics() {
        eprintln!("goverify: {d}");
    }
    let infer = goverify_solver::SolverLimits { timeout_ms: ca.solver_timeout_ms, ..Default::default() };
    let oblig = goverify_solver::SolverLimits { timeout_ms: ca.obligation_timeout_ms, ..Default::default() };
    let cmd = ca.solver_cmd.clone();
    let mk: Box<dyn Fn(goverify_analysis::BackendRole) -> Box<dyn goverify_solver::TextSolver> + Sync> =
        Box::new(move |role| {
            let lim = match role {
                goverify_analysis::BackendRole::Infer => infer,
                goverify_analysis::BackendRole::Findings => oblig,
            };
            match &cmd {
                Some(c) => Box::new(goverify_solver::SmtLib2Process::new(c, lim)),
                None => Box::new(goverify_solver::Z3Native::new(lim)),
            }
        });
    let cfg = goverify_analysis::EngineConfig {
        opts: goverify_analysis::Options::default(),
        cache_dir: ca.cache_dir.clone(),
        emit_smt: ca.emit_smt.clone(),
    };
    let checkers: Vec<&dyn goverify_analysis::Checker> =
        vec![&goverify_checkers::NilChecker, &goverify_checkers::BoundsChecker];
    let a = goverify_analysis::analyze_full(&program, &cfg, &checkers, &*mk);
    for d in &a.diagnostics {
        eprintln!("goverify: {d}");
    }
    print!("{}", render::render_findings(&a.findings, Path::new(".")));
    Ok(if a.findings.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}
```

`docs/threat-model.md`: update the phase-3 conditional bullet about model text — it IS rendered now; state the sanitization contract (control chars stripped; model text never parsed for verdicts; renderer treats it as untrusted display input).

Run: `mise x -- cargo test -p goverify-cli`
Expected: renderer unit tests PASS.

- [ ] **Step 3: check integration test + goldens**

Append to `crates/goverify-cli/tests/debug_integration.rs` (follow its existing exec-the-binary pattern — read the file first and reuse its helper for invoking the built binary with `GOVERIFY_EXTRACTOR_DIR`):

```rust
#[test]
fn check_reports_findings_with_exit_1_and_matches_golden() {
    // run: goverify check --gvir-dir <extracted nil corpus> from the
    // corpus dir (so source snippets resolve). Assert exit code 1,
    // stdout matches testdata/goldens/nil.check.txt via check_golden.
}

#[test]
fn check_clean_module_exits_0() {
    // hello corpus (no findings): exit 0, empty stdout.
}

#[test]
fn check_bounds_corpus_matches_golden() { /* bounds.check.txt */ }
```

Generate the two goldens by running the binary once the tests are written (`check_golden` follows the repo's blessing convention — read `testutil::check_golden` for the env var it uses, e.g. `UPDATE_GOLDENS=1`), then eyeball them: spans point into corpus files, traces list plausible line sequences, `with:` lines show `p0 = (ptr-nil)` style bindings. Commit goldens.

Run: `mise run corpus && mise x -- cargo test -p goverify-cli`
Expected: PASS.

- [ ] **Step 4: full gate + commit**

```bash
git add crates/goverify-cli testdata/goldens docs/threat-model.md
git commit --no-gpg-sign -m "cli: goverify check — rendered spans/traces/model values, exit codes 0/1/2"
```

---

### Task 12: Properties, fuzz target, differential extension

**Files:**
- Create: `crates/goverify-analysis/tests/encode_props.rs`
- Create: `fuzz/fuzz_targets/ir_encode.rs`; modify `fuzz/Cargo.toml`
- Modify: `.github/workflows/nightly.yml`, `mise.toml` (fuzz task)
- Create: `crates/goverify-checkers/tests/differential_findings.rs`

**Interfaces:**
- Consumes: everything landed; `goverify-ir/tests/props.rs`'s generator approach (read it; reimplement a bounded generator locally — test code is not importable across crates).

- [ ] **Step 1: encoder property tests**

`encode_props.rs` — proptest generator for small arbitrary programs: up to 4 blocks with arbitrary succs/preds (preds CONSISTENT with succs — generate the CFG as an edge set first, derive both), up to 6 instrs per block drawn from {BinOp(+,-,/ with random int widths), UnOp, Phi (edge count = preds count), If/Jump terminators, Call (havoc), Load/Store on pointer params}, params of random modelable types. Properties:

```rust
proptest! {
    #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

    #[test]
    fn encode_never_panics_and_is_deterministic(prog in arb_program()) {
        let p = Program::from_packages(vec![prog]);
        for f in p.func_ids() {
            if p.func(f).is_none() { continue; }
            let a = encode_func(&p, f);
            let b = encode_func(&p, f);
            match (a, b) {
                (Ok(a), Ok(b)) => {
                    for bi in 0..a.guards.len() {
                        prop_assert_eq!(
                            a.reach_query(bi, vec![]).canonical_text(),
                            b.reach_query(bi, vec![]).canonical_text()
                        );
                    }
                }
                (Err(a), Err(b)) => prop_assert_eq!(a, b),
                _ => prop_assert!(false, "determinism: Ok/Err disagree"),
            }
        }
    }

    #[test]
    fn queries_are_well_formed_for_z3(prog in arb_program()) {
        // Every reach query must be parseable/solvable: drive through
        // Z3Native and require SOME verdict (never a crash). Cap: first
        // 4 blocks per function.
    }
}
```

Run: `mise x -- cargo test -p goverify-analysis --test encode_props`
Expected: PASS (fix encoder bugs it finds — treat failures as RED evidence, minimize, fix, keep the seed as a regression `#[test]`).

- [ ] **Step 2: fuzz target + CI wiring (phase-3 I3 lesson: wire it NOW)**

`fuzz/fuzz_targets/ir_encode.rs` (mirror `ir_lower.rs`'s decode preamble — read it and extend the tail):

```rust
#![no_main]
// bytes -> gvir decode -> lower -> encode: reject, never panic.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(pkg) = goverify_extract::load_package(data) else { return };
    let p = goverify_ir::Program::from_packages(vec![pkg]);
    for f in p.func_ids() {
        if p.func(f).is_none() {
            continue;
        }
        let _ = goverify_analysis::encode::encode_func(&p, f);
    }
});
```

(Match the real decode entry point used by `gvir_decode.rs`/`ir_lower.rs` — read those targets and copy the exact preamble; add `goverify-analysis` to `fuzz/Cargo.toml` dependencies + the `[[bin]]` stanza; refresh `fuzz/Cargo.lock` in its own commit like phase 3 did.)

`mise.toml` fuzz task: add `"cargo +nightly fuzz run ir_encode -- -max_total_time=60"`.
`.github/workflows/nightly.yml`: add the `ir_encode` 900s line after `ir_lower`; recount the budget comment and raise `timeout-minutes` accordingly (4 × 15 min runs + cold build ⇒ 90).

Run: `mise x -- cargo +nightly fuzz run ir_encode -- -max_total_time=30 -seed_inputs=fuzz/corpus/ir_lower 2>/dev/null || mise x -- cargo +nightly fuzz run ir_encode -- -max_total_time=30`
Expected: zero crashes in the smoke window (check whether an `ir_lower` seed corpus dir exists first; if not, plain run).

- [ ] **Step 3: differential findings test**

`crates/goverify-checkers/tests/differential_findings.rs`:

```rust
//! Backend agreement at the findings level (spec §6): Z3Native and
//! SmtLib2Process(z3) must produce byte-identical findings dumps over
//! the checker corpus — guards the canonical printer across the new
//! obligation query shapes.

use goverify_analysis::{BackendRole, EngineConfig, Options, analyze_full, dump_findings};
use goverify_checkers::{BoundsChecker, NilChecker};
use goverify_solver::{SmtLib2Process, SolverLimits, Z3Native};

fn limits() -> SolverLimits {
    SolverLimits { timeout_ms: 5_000, mem_mb: 1024 }
}

fn findings(module: &str, process: bool) -> String {
    let p = goverify_ir::testutil::load_corpus(module);
    let cfg = EngineConfig { opts: Options::default(), cache_dir: None, emit_smt: None };
    let checkers: Vec<&dyn goverify_analysis::Checker> = vec![&NilChecker, &BoundsChecker];
    let a = analyze_full(&p, &cfg, &checkers, &|_role: BackendRole| {
        if process {
            Box::new(SmtLib2Process::new("z3", limits()))
        } else {
            Box::new(Z3Native::new(limits()))
        }
    });
    dump_findings(&a, Some(&format!("example.com/{module}")))
}

#[test]
fn backends_agree_on_nil_corpus() {
    assert_eq!(findings("nil", false), findings("nil", true));
}

#[test]
fn backends_agree_on_bounds_corpus() {
    assert_eq!(findings("bounds", false), findings("bounds", true));
}
```

(Traces may differ between backends — model TEXT differs. `dump_findings` doesn't print traces, so this compares verdicts+positions+messages only: exactly what must agree. Note this in a comment.)

Run: `mise x -- cargo test -p goverify-checkers --test differential_findings`
Expected: PASS (needs the pinned z3 CLI on PATH — it's in mise.toml; run under `mise x --`).

- [ ] **Step 4: full gate + commits**

```bash
git add crates/goverify-analysis/tests/encode_props.rs crates/goverify-checkers/tests/differential_findings.rs mise.toml .github/workflows/nightly.yml fuzz/fuzz_targets/ir_encode.rs fuzz/Cargo.toml
git commit --no-gpg-sign -m "test: encoder props, findings-level differential, ir_encode fuzz target with nightly wiring"
git add fuzz/Cargo.lock
git commit --no-gpg-sign -m "fuzz: refresh Cargo.lock for ir_encode target"
```

---

### Task 13: bbolt shakeout harness

**Files:**
- Create: `scripts/shakeout.sh`
- Modify: `mise.toml` (task), `.gitignore` (`.goverify/shakeout/`)
- Create: `docs/shakeout-phase4-bbolt.md` (report skeleton)

**Interfaces:**
- Consumes: the `check` command.
- Produces: `mise run shakeout` — not in blocking CI; the committed report is filled by the end-of-phase triage (a human/controller step, not this task).

- [ ] **Step 1: script + task**

`scripts/shakeout.sh`:

```bash
#!/usr/bin/env bash
# Phase-4 shakeout (spec §7): run goverify check over a pinned bbolt
# checkout. Manual/nightly only — network clone on first run.
set -euo pipefail
PIN="${GOVERIFY_SHAKEOUT_REF:-v1.4.0}"
DIR=".goverify/shakeout/bbolt"
if [ ! -d "$DIR/.git" ]; then
  git clone --quiet https://github.com/etcd-io/bbolt "$DIR"
fi
git -C "$DIR" fetch --quiet --tags
git -C "$DIR" checkout --quiet "$PIN"
cargo build --release -p goverify-cli
BIN="$(pwd)/target/release/goverify"
export GOVERIFY_EXTRACTOR_DIR="$(pwd)/extractor"
cd "$DIR"
# Exit 1 (findings) is the expected outcome; only 2 (analyzer error) fails.
set +e
"$BIN" check ./... --cache-dir "$(pwd)/../cache"
code=$?
set -e
if [ "$code" -eq 2 ]; then
  echo "shakeout: analyzer error" >&2
  exit 2
fi
echo "shakeout: exit $code (0 clean / 1 findings)" >&2
```

`chmod +x scripts/shakeout.sh`. mise task:

```toml
[tasks.shakeout]
description = "phase-4 shakeout: goverify check over pinned bbolt (manual; clones on first run)"
run = "scripts/shakeout.sh"
```

`.gitignore`: add `.goverify/`  (check whether it's already ignored — phase 1 may have added it for `extract`'s default out dir; if present, skip).

`docs/shakeout-phase4-bbolt.md`:

```markdown
# Phase-4 shakeout: etcd-io/bbolt @ v1.4.0

Status: PENDING — run `mise run shakeout`, triage every finding below,
then fill in the tables. Exit criteria (spec §7): all findings triaged,
FP rate recorded, every fixed FP lands a corpus case, dispatch-precision
observations recorded for phase-5 planning.

## Run parameters
- goverify commit: <sha>
- bbolt ref: v1.4.0
- timeouts: infer 100 ms / obligation 250 ms (defaults)

## Findings triage
| # | pos | tag | verdict (TP/FP/unclear) | note / corpus case |
|---|-----|-----|-------------------------|--------------------|

## Totals
- findings: N; TP: N; FP: N (rate: N%); unclear: N
- wall clock (cold / warm cache):

## Dispatch-precision + phase-5 observations
- (carried T10 watch item, §16 dynamic dispatch, timeout-bound FNs)
```

- [ ] **Step 2: smoke-test locally (no network needed for the dry part)**

Run: `bash -n scripts/shakeout.sh` (syntax) and `mise tasks | grep shakeout`.
Expected: task listed. (A full run needs network; do not gate the commit on it — the end-of-phase triage runs it for real.)

```bash
git add scripts/shakeout.sh mise.toml .gitignore docs/shakeout-phase4-bbolt.md
git commit --no-gpg-sign -m "shakeout: pinned-bbolt check harness + report skeleton (manual task)"
```

---

### Task 14: Documentation

**Files:**
- Modify: `ARCHITECTURE.md` (encode module, checker split, trace flow)
- Modify: `README.md` (check quickstart, exit codes, want-comment convention)
- Modify: `AGENTS.md` only if task lists changed (corpus task did — verify the wording still just says "run `mise tasks`"; if it enumerates, add `shakeout`)

- [ ] **Step 1: write the deltas**

ARCHITECTURE.md — add to the `goverify-analysis` section: the `encode` module (gated SSA, back-edge cut = first-iteration under-approximation, guard/phi/seq conventions, ASSERT_CAP degrade) and the checker contract (requires inference in the fixpoint with propagation; obligations under own preconditions; Sat-gated findings; trace reconstruction display-only). Add `goverify-checkers`: NilChecker + BoundsChecker, tag vocabulary.

README.md — quickstart gains:

```markdown
## Checking a module

    goverify check ./...

Exit codes: 0 clean, 1 findings, 2 analyzer error. Findings render as
labeled source spans with the violating path and the callee requirement
that fired. `--solver-timeout-ms`/`--obligation-timeout-ms` tune the
per-query budgets (timeouts suppress reports, never invent them).
```

plus a sentence in the testing section: corpus expectations are `// want: <tag>` comments (`nil-deref`, `bounds`, `div-zero`, `overflow`).

- [ ] **Step 2: full gate + commit**

Run: `mise run lint && mise run test`

```bash
git add ARCHITECTURE.md README.md AGENTS.md
git commit --no-gpg-sign -m "docs: phase-4 architecture, check quickstart, want-comment convention"
```

---

## Self-Review (performed while writing)

1. **Spec coverage:** encoding §3 → Tasks 3–4 (+ preds prerequisite Task 2); checkers §4 → Tasks 6–9 (propagation, preconditions rule, checker-name-sorted requires in Task 5); check + traces §5 → Tasks 10–11; corpus harness §6 → Tasks 5 (parser), 6, 8, 9; shakeout §7 → Task 13; degrade/budgets §8 → ASSERT_CAP (Task 3), role-split timeouts (Tasks 5, 11), catch_unwind already present; carried items §9 → Task 1 (batch), Task 5 (T12 ordering), Task 6 (FP-class test), Task 13 (T10 watch); testing §10 → Tasks 12 (props/fuzz/differential), 11 (goldens), corpus tasks wired in 8.
2. **Known intentional deviations:** none at plan time; `LocalNil`/`BadNarrow` corpus shapes flagged as verify-at-implementation (go/ssa folding).
3. **Type consistency check:** `encode_func(p, f) -> Result<EncodedFunc, String>`; `reach_query(block: usize, extra: Vec<Term>)`; `Checker::infer_requires(p, f, summary_of, discharge)`; `mk_backend(BackendRole)`; `BoundClause { tag, bound, violation }`; `Finding { checker, tag, func, pos, message, trace, model }` — note `model` is introduced in Task 11; Tasks 5–10 construct `Finding` without it until then (Task 11's compiler-guided fixups add it). `wants`/`wants_in` in testutil. Names consistent across tasks.
