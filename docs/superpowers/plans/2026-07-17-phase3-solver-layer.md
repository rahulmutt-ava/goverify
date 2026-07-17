# goverify Phase 3: Solver Layer — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the phase-2 solver stub with the real solver layer — typed term language, canonical SMT-LIB2 printer (the single Term→SMT lowering), `Z3Native` (static bundled z3-sys) and `SmtLib2Process` backends, content-addressed query cache — prove the pipeline end-to-end with a minimal nil-deref "tracer" checker, and make `Effects` location-precise (final-review I2).

**Architecture:** `goverify-solver` is rebuilt around a sort-carrying immutable term AST whose **only** lowering to SMT-LIB2 is the canonical printer; both backends consume those exact bytes, so the cached/emitted artifact is byte-identical to what was solved. `goverify-cache` gets the generic blake3 content-addressed store plus the query-cache layer. `goverify-analysis` swaps `PlaceholderFormula` for real term formulas, gains an obligation-discharge pass, and its `Effects` become `Loc → op-set` maps (access-path roots, bounded depth, explicit `Unknown` bucket). `goverify-checkers` is born with the nil tracer (the phase-4 nil checker's embryo).

**Tech Stack:** Rust (z3-sys static-link, prost, rayon, blake3, proptest dev-dep), Go corpus fixtures, mise tasks, cargo-fuzz, pinned z3 CLI for differential testing.

**Spec:** `docs/superpowers/specs/2026-07-17-phase3-solver-layer-design.md` (parent: `2026-07-16-goverify-design.md` §8–§9, §13).

## Global Constraints

- **Determinism is the root invariant** (parent spec §3, §9): identical source bytes ⇒ byte-identical `.gvir`, debug dumps, **and canonical SMT-LIB2 artifacts**. No timestamps, no absolute paths, no map-iteration order reaching output. Sort before emitting.
- **Single-lowering rule (phase-3 spec §4):** the canonical printer is the ONLY Term→SMT-LIB2 lowering. `Z3Native` must consume the printer's bytes (via `Z3_parse_smtlib2_string`), never build Z3 ASTs from terms directly. A second lowering is a plan violation, not a style choice.
- **Bug-finder semantics (parent §8):** `Unknown` (incl. timeout, solver crash, parse failure, CString failure — *any* abnormality) ⇒ no report. Timeouts must never create false positives. Havoc summaries have no requires.
- **Degrade, never die (parent §11):** the s-expr reader and the cache-record decoder parse bytes the analyzer didn't just produce — they reject with an error/miss, never panic (fuzz target).
- New **runtime** deps this phase: `z3-sys` (with `static-link-z3`) only; `tempfile` gains one new dependent crate (`goverify-solver`, for the process backend's query file). New tools in `mise.toml`: pinned `z3` CLI (differential backend). Sanctioned by phase-3 spec §2. `Cargo.lock` is committed.
- The only Go code lives in `extractor/` (corpus fixtures under `testdata/` are test data, not analyzer code).
- Run everything via `mise x -- <cmd>` or `mise run <task>` from the repo root (plain `cargo`/`go` may not be on PATH). Blocking CI = `mise run lint` + `mise run test` + `secrets` + `audit`.
- Rust edition 2024; `cargo fmt` + `clippy -D warnings` must pass per task; `mise run lint` before every commit.
- Goldens: byte-exact via `goverify_ir::testutil::check_golden`; regenerate with `UPDATE_GOLDENS=1` and hand-review the diff before committing.
- The first `cargo build` after adding z3-sys compiles Z3 from source (~15–30 min, one-time; cached in `target/` + CI cache). Do not "fix" this by switching to dynamic linking — that fallback is a user decision, not an implementer one.

---

## File Structure

```
crates/goverify-solver/
├── Cargo.toml                       # z3-sys, tempfile, goverify-cache      (Tasks 2,6,7,8)
└── src/
    ├── lib.rs                       # re-exports, Solver trait, StubSolver  (Tasks 2–8)
    ├── sort.rs                      # Sort, DatatypeDecl, CtorDecl          (Task 2)
    ├── term.rs                      # Term, smart ctors, substitute         (Task 2)
    ├── printer.rs                   # Logic, Query, canonical_text()        (Task 3)
    ├── reader.rs                    # SExpr, parse_query, parse_response    (Task 4)
    ├── z3native.rs                  # Z3Native (z3-sys, unsafe)             (Task 6)
    ├── process.rs                   # SmtLib2Process                       (Task 7)
    └── discharge.rs                 # TextSolver, discharge_query, adapter  (Task 8)
crates/goverify-solver/tests/differential.rs                              # (Task 7)
crates/goverify-cache/src/
    ├── lib.rs                       # re-exports                            (Task 5)
    ├── store.rs                     # content-addressed Store               (Task 5)
    └── query.rs                     # QueryCache, query_key, CachedOutcome  (Task 5)
crates/goverify-analysis/src/
    ├── summary.rs                   # Formula replaces PlaceholderFormula   (Task 9)
    ├── effects.rs                   # Loc/Root, map-shaped Effects, rebase  (Task 10)
    ├── checker.rs                   # Checker trait, Obligation, Finding    (Task 11)
    └── engine.rs                    # analyze_full, findings pass           (Tasks 9–12)
crates/goverify-checkers/src/
    ├── lib.rs                       # re-exports                            (Task 11)
    └── nil.rs                       # NilTracer                             (Task 11)
crates/goverify-checkers/tests/nil_corpus.rs                              # (Task 12)
crates/goverify-cli/src/main.rs      # debug findings + solver/cache flags   (Task 12)
testdata/corpus/nil/{go.mod,nil.go}  # tracer corpus module                  (Task 12)
testdata/goldens/nil.findings.txt    # findings golden                       (Task 12)
testdata/goldens/conc.summary.txt    # REGENERATED (loc-shaped effects)      (Task 10)
fuzz/fuzz_targets/smt_reader.rs      # reader never panics                   (Task 4)
mise.toml                            # z3 pin, corpus/fuzz task updates      (Tasks 4,7,12)
.github/workflows/ci.yml             # checks timeout bump (z3 cold build)   (Task 6)
.github/workflows/nightly.yml        # smt_reader fuzz, differential sweep   (Tasks 4,7)
ARCHITECTURE.md · README.md · docs/threat-model.md                        # (Task 13)
```

**Key cross-task interfaces** (defined once, used everywhere — later tasks must match these exactly):

```rust
// goverify-solver ---------------------------------------------------------
pub enum Sort { Bool, BitVec(u32), Array(Box<Sort>, Box<Sort>), Datatype(String) }
pub struct CtorDecl { pub name: String, pub fields: Vec<(String, Sort)> }
pub struct DatatypeDecl { pub name: String, pub ctors: Vec<CtorDecl> }
pub struct SortError(pub String);
pub struct Term { /* private node + Sort */ }
impl Term {                       // all constructors sort-check; Err = SortError
    pub fn sort(&self) -> &Sort;
    pub fn bool_lit(b: bool) -> Term;
    pub fn bv_lit(width: u32, value: u128) -> Term;
    pub fn var(name: &str, sort: Sort) -> Term;
    pub fn not(t: Term) -> Result<Term, SortError>;
    pub fn and(ts: Vec<Term>) -> Result<Term, SortError>;
    pub fn or(ts: Vec<Term>) -> Result<Term, SortError>;
    pub fn implies(a: Term, b: Term) -> Result<Term, SortError>;
    pub fn eq(a: Term, b: Term) -> Result<Term, SortError>;
    pub fn ite(c: Term, t: Term, e: Term) -> Result<Term, SortError>;
    pub fn bv_bin(op: BvBinOp, a: Term, b: Term) -> Result<Term, SortError>;
    pub fn bv_cmp(op: BvCmpOp, a: Term, b: Term) -> Result<Term, SortError>;
    pub fn select(arr: Term, idx: Term) -> Result<Term, SortError>;
    pub fn store(arr: Term, idx: Term, val: Term) -> Result<Term, SortError>;
    pub fn dt_ctor(dt: &DatatypeDecl, ctor: &str, args: Vec<Term>) -> Result<Term, SortError>;
    pub fn dt_is(dt: &DatatypeDecl, ctor: &str, arg: Term) -> Result<Term, SortError>;
    pub fn dt_get(dt: &DatatypeDecl, ctor: &str, field: &str, arg: Term) -> Result<Term, SortError>;
    pub fn substitute(&self, map: &BTreeMap<String, Term>) -> Result<Term, SortError>;
    pub fn free_vars(&self) -> BTreeMap<String, Sort>;
}
pub enum BvBinOp { Add, Sub, Mul, Udiv, Sdiv, Urem, Srem, And, Or, Xor, Shl, Lshr, Ashr }
pub enum BvCmpOp { Ult, Ule, Slt, Sle }
// The one datatype v1 needs (phase-3 spec §3): pointers.
pub fn ptr_datatype() -> DatatypeDecl;               // Ptr = ptr-nil | (ptr-addr (ptr-addr-val (_ BitVec 64)))
pub fn ptr_sort() -> Sort;                           // Sort::Datatype("Ptr")
pub fn ptr_nil() -> Term;                            // dt_ctor(Ptr, "ptr-nil", [])
pub fn ptr_is_nil(t: Term) -> Result<Term, SortError>;

pub enum Logic { QfBv, QfAbv, All }                  // "QF_BV" | "QF_ABV" | "ALL"
pub struct Query { pub logic: Logic, pub datatypes: Vec<DatatypeDecl>,
                   pub consts: Vec<(String, Sort)>, pub asserts: Vec<Term> }
impl Query {
    pub fn canonical_text(&self) -> String;          // THE canonical artifact; ends "(check-sat)\n"
    pub fn for_asserts(logic: Logic, asserts: Vec<Term>) -> Query;  // auto-declares free vars + Ptr if used
}

pub enum SatResult { Sat, Unsat, Unknown }           // unchanged from phase 2
pub struct QueryOutcome { pub result: SatResult, pub model: Option<String> }
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SolverLimits { pub timeout_ms: u32, pub mem_mb: u32 }  // Default: 100, 1024
pub trait TextSolver: Send {
    fn identity(&self) -> String;                    // feeds the query-cache key
    fn limits(&self) -> SolverLimits;
    fn solve_text(&mut self, canonical: &str) -> QueryOutcome;
}
pub struct Z3Native { /* ctx, solver, limits, identity */ }      // TextSolver
pub struct SmtLib2Process { /* cmd, limits, identity */ }        // TextSolver
impl Z3Native { pub fn new(limits: SolverLimits) -> Z3Native; }
impl SmtLib2Process { pub fn new(cmd: &str, limits: SolverLimits) -> SmtLib2Process; }

pub fn discharge_query(q: &Query, backend: &mut dyn TextSolver,
                       cache: Option<&QueryCache>, emit_dir: Option<&Path>) -> QueryOutcome;

// Old Solver trait survives (phase-3 spec §2) with typed innards:
pub enum Decl { Const(String, Sort), Datatype(DatatypeDecl) }
pub struct Model(pub String);
pub trait Solver { fn declare(&mut self, decl: Decl); fn assert(&mut self, term: Term);
                   fn check_sat_assuming(&mut self, assumptions: &[Term]) -> SatResult;
                   fn model(&self) -> Option<Model>; fn push(&mut self); fn pop(&mut self); }
pub struct TermSolver { /* backend + frames */ }     // impl Solver over any TextSolver
impl TermSolver { pub fn new(backend: Box<dyn TextSolver>, logic: Logic) -> TermSolver; }
pub struct StubSolver;                               // impl Solver AND TextSolver; always Unknown

// reader (test/fuzz/response surface)
pub enum SExpr { Atom(String), List(Vec<SExpr>) }
pub fn parse_sexpr(input: &str) -> Result<(SExpr, usize), ReadError>;  // (expr, bytes consumed)
pub fn parse_response(first_line: &str) -> SatResult;                  // anything unrecognized => Unknown
pub fn parse_query(text: &str) -> Result<Query, ReadError>;            // round-trip/fuzz surface
pub struct ReadError(pub String);

// goverify-cache ----------------------------------------------------------
pub struct Store { /* root */ }
impl Store {
    pub fn open(root: PathBuf) -> Store;                               // creates root 0700, best-effort
    pub fn get(&self, layer: &str, key: &[u8; 32]) -> Option<Vec<u8>>; // any error => None (miss)
    pub fn put(&self, layer: &str, key: &[u8; 32], value: &[u8]) -> std::io::Result<()>;
}
pub struct QueryKeyParts<'a> { pub canonical: &'a str, pub solver_identity: &'a str,
                               pub timeout_ms: u32, pub mem_mb: u32 }
pub fn query_key(parts: &QueryKeyParts) -> [u8; 32];                   // blake3, length-prefixed fields
pub enum CachedOutcome { Sat { model: Option<String> }, Unsat, Unknown }
pub struct QueryCache { /* store */ }
impl QueryCache {
    pub fn open(root: PathBuf) -> QueryCache;
    pub fn get(&self, key: &[u8; 32]) -> Option<CachedOutcome>;        // corrupt entry => None
    pub fn put(&self, key: &[u8; 32], v: &CachedOutcome) -> std::io::Result<()>;
}

// goverify-analysis -------------------------------------------------------
pub struct Formula { pub term: goverify_solver::Term }   // free vars only p<i>/r<i>
pub struct Clause { pub tag: String, pub formula: Formula }
pub enum IfaceVar { Param(u32), Result(u32) }
pub fn iface_var_name(v: &IfaceVar) -> String;           // Param(0)->"p0", Result(1)->"r1"
pub struct BoundClause { pub tag: String, pub violation: Option<goverify_solver::Term> }
pub fn instantiate_requires(callee: &Summary, arg_terms: &[Option<goverify_solver::Term>])
    -> Vec<BoundClause>;                                  // violation = ¬formula[p_i := arg_terms[i]]

pub enum Root { Param(u32), Global(String), Alloc(u32), Unknown }     // Ord
pub struct Loc { pub root: Root, pub path: Vec<u32> }                 // Ord; path = field indices
pub const MAX_LOC_DEPTH: usize = 2;
pub enum LockOp { Lock, Unlock, RLock, RUnlock, DeferredUnlock, DeferredRUnlock }
pub struct Effects { pub spawns: Spawns,
                     pub chan_ops: BTreeMap<Loc, BTreeSet<ChanOp>>,
                     pub lock_ops: BTreeMap<Loc, BTreeSet<LockOp>> }
pub fn resolve_loc(f: &Function, v: ValueId) -> Loc;                  // def-chain walk, depth-capped
pub fn rebase(callee: &Effects, caller_arg_loc: &dyn Fn(u32) -> Loc) -> Effects;
pub fn collect(p: &Program, id: FuncId, effects_of: &dyn Fn(FuncId) -> Effects) -> Effects;

pub struct Obligation { pub tag: String, pub message: String,
                        pub pos: Option<goverify_ir::Pos>, pub query: goverify_solver::Query }
pub struct Finding { pub checker: String, pub func: String,
                     pub pos: Option<goverify_ir::Pos>, pub message: String }
pub trait Checker: Sync {
    fn name(&self) -> &'static str;
    /// Called inside the fixpoint: requires clauses become part of the
    /// summary. `discharge` is the engine-owned solve path (cache/emit
    /// plumbed); Sat gates emission — Unknown must not manufacture requires.
    fn infer_requires(&self, p: &Program, f: FuncId,
                      discharge: &mut dyn FnMut(&goverify_solver::Query) -> SatResult)
        -> Vec<Clause>;
    /// Called once after the fixpoint, with final summaries: call-site obligations.
    fn obligations(&self, p: &Program, f: FuncId,
                   summary_of: &dyn Fn(FuncId) -> Summary) -> Vec<Obligation>;
}
pub struct EngineConfig { pub opts: Options, pub limits: goverify_solver::SolverLimits,
                          pub cache_dir: Option<PathBuf>, pub emit_smt: Option<PathBuf> }
pub fn analyze_full(p: &Program, cfg: &EngineConfig, checkers: &[&dyn Checker],
                    mk_backend: &(dyn Fn() -> Box<dyn goverify_solver::TextSolver> + Sync))
    -> Analysis;                                          // Analysis gains findings: Vec<Finding>
pub fn dump_findings(a: &Analysis, filter: Option<&str>) -> String;

// goverify-checkers -------------------------------------------------------
pub struct NilTracer;                                     // impl goverify_analysis::Checker
```

---

### Task 1: Phase-2 fast-follow batch

Eight minor items deferred by the phase-2 final review (progress.md). Two are handled elsewhere by design: **T11** (Solver doc comments) is subsumed by Task 2's rewrite of the crate; **T12** (`IfaceVar::Result → None` test) is folded into Task 9 where `instantiate_requires` changes shape — both MUST be noted in this task's report so the triage trail stays intact.

**Files:**
- Modify: `extractor/emit_test.go` (T2: universe-scope Named test)
- Modify: `crates/goverify-ir/src/program.rs` (T5: tighten load_dir diag assert)
- Modify: `crates/goverify-ir/src/func.rs`, `crates/goverify-ir/src/lower.rs` (T6: doc comments)
- Create: `crates/goverify-ir/tests/lock_kind.rs` (T7: RWMutex mapping tests)
- Modify: `crates/goverify-cli/src/main.rs`, `crates/goverify-cli/tests/debug_integration.rs` (T15: --func warn)
- Modify: `testdata/corpus/ops/ops.go`, `testdata/goldens/ops.ir.txt` (T17a: plain recv)
- Modify: `crates/goverify-ir/tests/props.rs` (T17b: Extract/TypeAssert kinds)

**Interfaces:** none new — pure test/doc/diagnostic hardening. No behavior change except one new `eprintln!` warning in the CLI.

- [ ] **Step 1: T2 — universe-scope Named type test (Go side)**

`extractor/emit.go:134` sets `pb.Name = t.Obj().Name()` for universe-scope named types (the branch only `error` reaches in practice); it has no test. The `conc` corpus already uses `error` (`Close() error`). Append to `extractor/emit_test.go`:

```go
// TestUniverseScopeNamed pins emit.go's universe-scope Named branch:
// `error` has no package, so its emitted name must come from
// t.Obj().Name() directly and its kind must be NAMED.
func TestUniverseScopeNamed(t *testing.T) {
	pkgs := extractCorpus(t, "../testdata/corpus/conc", false)
	p := pkgs["example.com/conc"]
	var errT *gvirpb.Type
	for _, ty := range p.Types {
		if ty.Repr == "error" {
			errT = ty
		}
	}
	if errT == nil {
		t.Fatal("no `error` type interned (conc uses `Close() error`)")
	}
	if errT.Kind != gvirpb.TypeKind_TYPE_KIND_NAMED || errT.Name != "error" {
		t.Errorf("error type: kind=%v name=%q, want NAMED/\"error\"", errT.Kind, errT.Name)
	}
}
```

If `extractCorpus`'s package key differs (check the map key convention used by `TestStructuredTypes`), match it. If `error` is interned as `TYPE_KIND_INTERFACE` rather than `NAMED` (check the actual emitted kind first with a quick debug print), assert the kind actually emitted and say so in a comment — the point of the test is pinning the `Name` field, not relitigating the kind taxonomy.

- [ ] **Step 2: Run the Go test**

Run: `cd extractor && mise x -- go test -run TestUniverseScopeNamed ./...`
Expected: PASS (adjust per the note above if the kind assertion fires; re-run).

- [ ] **Step 3: T5 — tighten `load_dir` diagnostic assert**

In `crates/goverify-ir/src/program.rs`, test `load_dir_skips_malformed_and_ignores_non_gvir`: replace the weak assert pair

```rust
        let diags = p.diagnostics();
        assert!(
            !diags.is_empty(),
            "should have a diagnostic for the malformed file"
        );
```

with

```rust
        let diags = p.diagnostics();
        assert_eq!(
            diags.len(),
            1,
            "exactly one diagnostic (malformed.gvir); readme.txt must be \
             extension-filtered, not diagnosed: {diags:?}"
        );
```

- [ ] **Step 4: T6 — doc comments on `Block.succs` and Function-aux FuncId ordering**

`crates/goverify-ir/src/func.rs` — on the `succs` field of `Block`:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub instrs: Vec<Instr>,
    /// Successor block indices as raw wire values — NOT validated against
    /// `Function::blocks.len()`. Any consumer that indexes `blocks` with
    /// these must bounds-check (fuzzed input can carry arbitrary ids); see
    /// `effects::cyclic_blocks` for the filtering pattern.
    pub succs: Vec<u32>,
}
```

`crates/goverify-ir/src/lower.rs` — above the `"Function" => ValueKind::FuncRef(self.intern_func(&a.repr))` arm (line ~70):

```rust
                // NB: interning here assigns FuncIds in aux-traversal
                // order for functions first seen via a Function-kind aux
                // value (e.g. closures referenced before their bodies are
                // lowered). That order is deterministic (aux tables are
                // sorted in the .gvir) but is NOT the alphabetical order
                // pass 1 gives bodies — consumers must not assume FuncId
                // order == name order.
```

- [ ] **Step 5: T7 — `lock_kind` mapping tests**

Create `crates/goverify-ir/tests/lock_kind.rs`:

```rust
//! Pins the sync-intrinsic name table (final-review deferred T7): the
//! RWMutex branches were previously untested.

use goverify_ir::{LockKind, lock_kind};

#[test]
fn all_sync_lock_methods_map() {
    for (name, want) in [
        ("(*sync.Mutex).Lock", LockKind::Lock),
        ("(*sync.Mutex).Unlock", LockKind::Unlock),
        ("(*sync.RWMutex).Lock", LockKind::Lock),
        ("(*sync.RWMutex).Unlock", LockKind::Unlock),
        ("(*sync.RWMutex).RLock", LockKind::RLock),
        ("(*sync.RWMutex).RUnlock", LockKind::RUnlock),
    ] {
        assert_eq!(lock_kind(name), Some(want), "lock_kind({name})");
    }
}

#[test]
fn non_lock_names_do_not_map() {
    for name in ["fmt.Println", "(*sync.WaitGroup).Wait", "", "Lock"] {
        assert_eq!(lock_kind(name), None, "lock_kind({name})");
    }
}
```

- [ ] **Step 6: T15 — warn when `--func` is ignored**

`crates/goverify-cli/src/main.rs`, in `run_debug` right after the `let (kind, args) = match what {...}` binding:

```rust
    // --func filters per-function output; callgraph/sccs dumps are
    // whole-program (final-review deferred T15) — warn instead of
    // silently ignoring the flag.
    if args.func.is_some() && matches!(kind, "callgraph" | "sccs") {
        eprintln!("goverify: --func has no effect on `debug {kind}`; ignoring");
    }
```

Append to `crates/goverify-cli/tests/debug_integration.rs` (match the file's existing `Command::new(env!("CARGO_BIN_EXE_goverify"))` pattern and its `--gvir-dir` fixture setup — reuse whatever helper the existing tests use to get a gvir dir):

```rust
#[test]
fn func_flag_on_callgraph_warns() {
    // Reuse the same extracted gvir dir the other tests use.
    let out = Command::new(env!("CARGO_BIN_EXE_goverify"))
        .args(["debug", "callgraph", "--gvir-dir"])
        .arg(gvir_dir()) // existing helper in this file; adapt name if it differs
        .args(["--func", "anything"])
        .output()
        .expect("run goverify");
    assert!(out.status.success(), "debug callgraph must still succeed");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--func has no effect"),
        "expected ignore-warning on stderr, got: {stderr}"
    );
}
```

- [ ] **Step 7: T17a — plain channel-receive in the ops corpus**

Append to `testdata/corpus/ops/ops.go`:

```go
func RecvPlain(ch chan int) int { return <-ch } // unop recv (plain, no comma-ok)
```

Regenerate the golden: `UPDATE_GOLDENS=1 mise x -- cargo test -p goverify-ir --test lower_golden --test lower_corpus`
Then hand-review `git diff testdata/goldens/ops.ir.txt` — the only change must be the new `RecvPlain` function's block.

- [ ] **Step 8: T17b — Extract/TypeAssert in the property kind pool**

`crates/goverify-ir/tests/props.rs`, in `arb_instruction`'s `kinds` vector, after `"MakeInterface"`:

```rust
        "MakeInterface",
        "Extract",
        "TypeAssert",
        "TotallyUnknownKind",
```

(The `arb_sem` pool already generates `ExtractSem`/`TypeAssertSem` payloads; adding the kinds lets the matched — not just mismatched — pairs exercise `lower.rs`'s real-content paths crate-wide.)

- [ ] **Step 9: Full verification**

Run: `mise run lint && mise run test`
Expected: all green (the props change may shrink/regrow case timing slightly; no failures).

- [ ] **Step 10: Commit**

```bash
git add -A
git commit -m "chore: phase-2 fast-follow batch (final-review deferred items T2,T5,T6,T7,T15,T17)"
```

Report note (verbatim requirement): "T11 subsumed by Task 2 solver rewrite; T12 folded into Task 9 formula swap."

### Task 2: Term language (sorts, terms, smart constructors)

**Files:**
- Create: `crates/goverify-solver/src/sort.rs`
- Create: `crates/goverify-solver/src/term.rs`
- Modify: `crates/goverify-solver/src/lib.rs` (module decls + re-exports; keep the existing `Solver` trait/`SatResult`/`StubSolver` compiling — `Decl`/`Term`-string versions stay temporarily until Task 8 swaps them)

**Interfaces:**
- Consumes: nothing from other tasks.
- Produces: everything under "goverify-solver" in the interface block through `ptr_is_nil` (Sort, CtorDecl, DatatypeDecl, SortError, Term + all constructors, BvBinOp, BvCmpOp, ptr_* helpers). Tasks 3, 4, 8, 9, 11 build on these exact signatures.

**Design notes for the implementer:**
- `Term` is `{ node: Box<Node>? }` — no: use `struct Term { node: Node, sort: Sort }` with `Node` holding `Box<Term>`/`Vec<Term>` children. Plain immutable AST, `#[derive(Debug, Clone, PartialEq, Eq)]` on everything. No interning, no `Rc` (spec §3: YAGNI until profiling says otherwise).
- **Naming is load-bearing**: variable names go verbatim into canonical SMT-LIB2 text, which is hashed. Callers (Tasks 9/11) only ever use `p<i>`, `r<i>`, and `Ptr` — but `Term::var` itself must reject names that aren't a bare SMT-LIB2 simple symbol (`[a-zA-Z_~!@$%^&*+=<>.?/-][a-zA-Z0-9_~!@$%^&*+=<>.?/-]*`) to keep the printer quoting-free: return-by-construction is simpler than escaping. Enforce with a `fn valid_symbol(s: &str) -> bool` check; `Term::var` panics on invalid names? NO — nothing panics: make `Term::var(name, sort) -> Term` keep its infallible signature but debug_assert + replace invalid names with `"invalid_symbol"` in release? Also no (silent wrongness). Resolution: `Term::var` returns `Term` and REQUIRES a valid symbol, checked with `assert!` — this is analyzer-internal construction (our code, not foreign bytes), so a violated invariant is a bug worth failing loudly on in tests; the engine's per-function `catch_unwind` (phase 2) already converts any escape to a diagnostic + havoc, never a crash of the run.

- [ ] **Step 1: Write `sort.rs`**

```rust
//! SMT sorts (phase-3 spec §3): quantifier-free, four theories.

/// A sort. `Datatype` names a declared algebraic datatype (v1 ships only
/// `Ptr`, but the type is general).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Sort {
    Bool,
    BitVec(u32),
    Array(Box<Sort>, Box<Sort>),
    Datatype(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CtorDecl {
    pub name: String,
    /// (accessor name, field sort) pairs.
    pub fields: Vec<(String, Sort)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatatypeDecl {
    pub name: String,
    pub ctors: Vec<CtorDecl>,
}

impl DatatypeDecl {
    pub fn sort(&self) -> Sort {
        Sort::Datatype(self.name.clone())
    }

    pub fn ctor(&self, name: &str) -> Option<&CtorDecl> {
        self.ctors.iter().find(|c| c.name == name)
    }
}

/// Ill-sorted construction. Analyzer-internal: callers degrade to
/// "no obligation" (never a finding) on Err.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SortError(pub String);

impl std::fmt::Display for SortError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "sort error: {}", self.0)
    }
}

impl std::error::Error for SortError {}

/// The one datatype v1 needs: pointers as nil | 64-bit address.
pub fn ptr_datatype() -> DatatypeDecl {
    DatatypeDecl {
        name: "Ptr".into(),
        ctors: vec![
            CtorDecl { name: "ptr-nil".into(), fields: vec![] },
            CtorDecl {
                name: "ptr-addr".into(),
                fields: vec![("ptr-addr-val".into(), Sort::BitVec(64))],
            },
        ],
    }
}

pub fn ptr_sort() -> Sort {
    Sort::Datatype("Ptr".into())
}
```

- [ ] **Step 2: Write the failing tests for `term.rs`** (in `term.rs`'s `#[cfg(test)] mod tests`)

```rust
#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::sort::{Sort, ptr_datatype, ptr_sort};

    #[test]
    fn constructors_carry_sorts() {
        let x = Term::var("x", Sort::BitVec(32));
        let y = Term::var("y", Sort::BitVec(32));
        let add = Term::bv_bin(BvBinOp::Add, x.clone(), y.clone()).unwrap();
        assert_eq!(add.sort(), &Sort::BitVec(32), "bv_bin result sort");
        let lt = Term::bv_cmp(BvCmpOp::Ult, x, y).unwrap();
        assert_eq!(lt.sort(), &Sort::Bool, "bv_cmp result sort");
    }

    #[test]
    fn ill_sorted_construction_is_rejected() {
        let b = Term::bool_lit(true);
        let bv = Term::bv_lit(8, 7);
        assert!(Term::bv_bin(BvBinOp::Add, b.clone(), bv.clone()).is_err(), "bool + bv8");
        assert!(Term::and(vec![bv.clone()]).is_err(), "and over non-bool");
        assert!(Term::ite(bv.clone(), b.clone(), b.clone()).is_err(), "non-bool cond");
        assert!(Term::eq(b, bv).is_err(), "eq across sorts");
        let w32 = Term::bv_lit(32, 1);
        let w8 = Term::bv_lit(8, 1);
        assert!(Term::bv_bin(BvBinOp::Add, w32, w8).is_err(), "width mismatch");
    }

    #[test]
    fn bv_lit_value_must_fit_width() {
        assert!(std::panic::catch_unwind(|| Term::bv_lit(4, 16)).is_err(),
            "bv_lit(4, 16): 16 needs 5 bits — internal misuse, assert fires");
        let _ = Term::bv_lit(4, 15); // fits
    }

    #[test]
    fn ptr_datatype_helpers() {
        let dt = ptr_datatype();
        let p = Term::var("p0", ptr_sort());
        let is_nil = ptr_is_nil(p.clone()).unwrap();
        assert_eq!(is_nil.sort(), &Sort::Bool);
        let addr = Term::dt_get(&dt, "ptr-addr", "ptr-addr-val", p.clone()).unwrap();
        assert_eq!(addr.sort(), &Sort::BitVec(64));
        assert!(Term::dt_ctor(&dt, "no-such-ctor", vec![]).is_err());
        assert!(Term::dt_is(&dt, "ptr-nil", Term::bool_lit(true)).is_err(),
            "tester on non-Ptr arg");
    }

    #[test]
    fn substitute_replaces_free_vars_sort_checked() {
        let p = Term::var("p0", ptr_sort());
        let f = Term::not(ptr_is_nil(p).unwrap()).unwrap();
        let mut m = BTreeMap::new();
        m.insert("p0".to_string(), ptr_nil());
        let g = f.substitute(&m).unwrap();
        assert!(g.free_vars().is_empty(), "p0 fully substituted");
        // sort-mismatched substitution is rejected
        let mut bad = BTreeMap::new();
        bad.insert("p0".to_string(), Term::bool_lit(true));
        assert!(f.substitute(&bad).is_err());
    }

    #[test]
    fn free_vars_collects_names_and_sorts() {
        let x = Term::var("x", Sort::Bool);
        let p = Term::var("p0", ptr_sort());
        let t = Term::and(vec![x, ptr_is_nil(p).unwrap()]).unwrap();
        let fv = t.free_vars();
        assert_eq!(fv.get("x"), Some(&Sort::Bool));
        assert_eq!(fv.get("p0"), Some(&ptr_sort()));
        assert_eq!(fv.len(), 2);
    }
}
```

- [ ] **Step 3: Run to verify failure**

Run: `mise x -- cargo test -p goverify-solver`
Expected: FAIL to compile (module/types don't exist yet).

- [ ] **Step 4: Implement `term.rs`**

```rust
//! Typed first-order terms (phase-3 spec §3). Immutable AST; every Term
//! carries its Sort; ill-sorted construction is unrepresentable through
//! the public API (constructors return Err). The ONLY lowering to
//! SMT-LIB2 is printer.rs (single-lowering rule, spec §4).

use std::collections::BTreeMap;

use crate::sort::{CtorDecl, DatatypeDecl, Sort, SortError, ptr_datatype};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BvBinOp {
    Add, Sub, Mul, Udiv, Sdiv, Urem, Srem, And, Or, Xor, Shl, Lshr, Ashr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BvCmpOp {
    Ult, Ule, Slt, Sle,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Node {
    BoolLit(bool),
    BvLit { width: u32, value: u128 },
    Var(String),
    Not(Box<Term>),
    And(Vec<Term>),
    Or(Vec<Term>),
    Implies(Box<Term>, Box<Term>),
    Eq(Box<Term>, Box<Term>),
    Ite(Box<Term>, Box<Term>, Box<Term>),
    BvBin { op: BvBinOp, lhs: Box<Term>, rhs: Box<Term> },
    BvCmp { op: BvCmpOp, lhs: Box<Term>, rhs: Box<Term> },
    Select(Box<Term>, Box<Term>),
    Store(Box<Term>, Box<Term>, Box<Term>),
    /// Constructor application; dt/ctor names resolved at build time.
    DtCtor { dt: String, ctor: String, args: Vec<Term> },
    /// `(_ is <ctor>)` tester.
    DtIs { ctor: String, arg: Box<Term> },
    /// Field accessor.
    DtGet { field: String, arg: Box<Term> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Term {
    pub(crate) node: Node,
    sort: Sort,
}

fn err(msg: impl Into<String>) -> SortError {
    SortError(msg.into())
}

/// Bare SMT-LIB2 simple symbol — keeps the printer quoting-free.
fn valid_symbol(s: &str) -> bool {
    const EXTRA: &[char] = &['~', '!', '@', '$', '%', '^', '&', '*', '_', '-', '+', '=', '<', '>', '.', '?', '/'];
    let ok = |c: char| c.is_ascii_alphanumeric() || EXTRA.contains(&c);
    !s.is_empty() && !s.starts_with(|c: char| c.is_ascii_digit()) && s.chars().all(ok)
}

impl Term {
    pub fn sort(&self) -> &Sort {
        &self.sort
    }

    pub fn bool_lit(b: bool) -> Term {
        Term { node: Node::BoolLit(b), sort: Sort::Bool }
    }

    /// `value` must fit in `width` bits (analyzer-internal invariant).
    pub fn bv_lit(width: u32, value: u128) -> Term {
        assert!(width >= 1 && width <= 128, "bv_lit width {width}");
        assert!(
            width == 128 || value < (1u128 << width),
            "bv_lit: {value} does not fit in {width} bits"
        );
        Term { node: Node::BvLit { width, value }, sort: Sort::BitVec(width) }
    }

    /// `name` must be a bare SMT-LIB2 symbol (analyzer-internal invariant;
    /// callers only ever pass p<i>/r<i>-shaped names).
    pub fn var(name: &str, sort: Sort) -> Term {
        assert!(valid_symbol(name), "invalid SMT symbol: {name:?}");
        Term { node: Node::Var(name.to_string()), sort }
    }

    pub fn not(t: Term) -> Result<Term, SortError> {
        if t.sort != Sort::Bool {
            return Err(err(format!("not: expected Bool, got {:?}", t.sort)));
        }
        Ok(Term { node: Node::Not(Box::new(t)), sort: Sort::Bool })
    }

    pub fn and(ts: Vec<Term>) -> Result<Term, SortError> {
        Self::nary("and", ts, Node::And)
    }

    pub fn or(ts: Vec<Term>) -> Result<Term, SortError> {
        Self::nary("or", ts, Node::Or)
    }

    fn nary(what: &str, ts: Vec<Term>, mk: fn(Vec<Term>) -> Node) -> Result<Term, SortError> {
        if ts.is_empty() {
            return Err(err(format!("{what}: empty operand list")));
        }
        if let Some(t) = ts.iter().find(|t| t.sort != Sort::Bool) {
            return Err(err(format!("{what}: expected Bool, got {:?}", t.sort)));
        }
        Ok(Term { node: mk(ts), sort: Sort::Bool })
    }

    pub fn implies(a: Term, b: Term) -> Result<Term, SortError> {
        if a.sort != Sort::Bool || b.sort != Sort::Bool {
            return Err(err("implies: both operands must be Bool"));
        }
        Ok(Term { node: Node::Implies(Box::new(a), Box::new(b)), sort: Sort::Bool })
    }

    pub fn eq(a: Term, b: Term) -> Result<Term, SortError> {
        if a.sort != b.sort {
            return Err(err(format!("eq: {:?} vs {:?}", a.sort, b.sort)));
        }
        Ok(Term { node: Node::Eq(Box::new(a), Box::new(b)), sort: Sort::Bool })
    }

    pub fn ite(c: Term, t: Term, e: Term) -> Result<Term, SortError> {
        if c.sort != Sort::Bool {
            return Err(err("ite: condition must be Bool"));
        }
        if t.sort != e.sort {
            return Err(err(format!("ite: branch sorts {:?} vs {:?}", t.sort, e.sort)));
        }
        let sort = t.sort.clone();
        Ok(Term { node: Node::Ite(Box::new(c), Box::new(t), Box::new(e)), sort })
    }

    pub fn bv_bin(op: BvBinOp, lhs: Term, rhs: Term) -> Result<Term, SortError> {
        let sort = Self::same_bv(&lhs, &rhs)?;
        Ok(Term { node: Node::BvBin { op, lhs: Box::new(lhs), rhs: Box::new(rhs) }, sort })
    }

    pub fn bv_cmp(op: BvCmpOp, lhs: Term, rhs: Term) -> Result<Term, SortError> {
        Self::same_bv(&lhs, &rhs)?;
        Ok(Term {
            node: Node::BvCmp { op, lhs: Box::new(lhs), rhs: Box::new(rhs) },
            sort: Sort::Bool,
        })
    }

    fn same_bv(lhs: &Term, rhs: &Term) -> Result<Sort, SortError> {
        match (&lhs.sort, &rhs.sort) {
            (Sort::BitVec(a), Sort::BitVec(b)) if a == b => Ok(lhs.sort.clone()),
            _ => Err(err(format!("bitvec op: {:?} vs {:?}", lhs.sort, rhs.sort))),
        }
    }

    pub fn select(arr: Term, idx: Term) -> Result<Term, SortError> {
        let Sort::Array(k, v) = arr.sort.clone() else {
            return Err(err(format!("select: expected Array, got {:?}", arr.sort)));
        };
        if *k != idx.sort {
            return Err(err(format!("select: index {:?} vs key {:?}", idx.sort, k)));
        }
        Ok(Term { node: Node::Select(Box::new(arr), Box::new(idx)), sort: *v })
    }

    pub fn store(arr: Term, idx: Term, val: Term) -> Result<Term, SortError> {
        let Sort::Array(k, v) = arr.sort.clone() else {
            return Err(err(format!("store: expected Array, got {:?}", arr.sort)));
        };
        if *k != idx.sort || *v != val.sort {
            return Err(err("store: index/value sort mismatch"));
        }
        let sort = arr.sort.clone();
        Ok(Term {
            node: Node::Store(Box::new(arr), Box::new(idx), Box::new(val)),
            sort,
        })
    }

    fn resolve_ctor<'a>(dt: &'a DatatypeDecl, ctor: &str) -> Result<&'a CtorDecl, SortError> {
        dt.ctor(ctor)
            .ok_or_else(|| err(format!("datatype {}: no constructor {ctor}", dt.name)))
    }

    pub fn dt_ctor(dt: &DatatypeDecl, ctor: &str, args: Vec<Term>) -> Result<Term, SortError> {
        let c = Self::resolve_ctor(dt, ctor)?;
        if c.fields.len() != args.len() {
            return Err(err(format!("{ctor}: arity {} vs {}", c.fields.len(), args.len())));
        }
        for ((fname, fsort), a) in c.fields.iter().zip(&args) {
            if fsort != &a.sort {
                return Err(err(format!("{ctor}.{fname}: {:?} vs {:?}", fsort, a.sort)));
            }
        }
        Ok(Term {
            node: Node::DtCtor { dt: dt.name.clone(), ctor: ctor.to_string(), args },
            sort: dt.sort(),
        })
    }

    pub fn dt_is(dt: &DatatypeDecl, ctor: &str, arg: Term) -> Result<Term, SortError> {
        Self::resolve_ctor(dt, ctor)?;
        if arg.sort != dt.sort() {
            return Err(err(format!("(_ is {ctor}): arg is {:?}, want {:?}", arg.sort, dt.sort())));
        }
        Ok(Term { node: Node::DtIs { ctor: ctor.to_string(), arg: Box::new(arg) }, sort: Sort::Bool })
    }

    pub fn dt_get(dt: &DatatypeDecl, ctor: &str, field: &str, arg: Term) -> Result<Term, SortError> {
        let c = Self::resolve_ctor(dt, ctor)?;
        let Some((_, fsort)) = c.fields.iter().find(|(n, _)| n == field) else {
            return Err(err(format!("{ctor}: no field {field}")));
        };
        if arg.sort != dt.sort() {
            return Err(err(format!("{field}: arg is {:?}, want {:?}", arg.sort, dt.sort())));
        }
        let sort = fsort.clone();
        Ok(Term { node: Node::DtGet { field: field.to_string(), arg: Box::new(arg) }, sort })
    }

    /// Capture-free substitution of free variables by name. Sort-checked:
    /// a replacement whose sort differs from the variable's is an error.
    pub fn substitute(&self, map: &BTreeMap<String, Term>) -> Result<Term, SortError> {
        if let Node::Var(name) = &self.node {
            return match map.get(name) {
                Some(r) if r.sort == self.sort => Ok(r.clone()),
                Some(r) => Err(err(format!(
                    "substitute {name}: {:?} vs {:?}", r.sort, self.sort
                ))),
                None => Ok(self.clone()),
            };
        }
        let mut t = self.clone();
        t.node = match t.node {
            n @ (Node::BoolLit(_) | Node::BvLit { .. } | Node::Var(_)) => n,
            Node::Not(a) => Node::Not(Box::new(a.substitute(map)?)),
            Node::And(ts) => Node::And(Self::subst_all(ts, map)?),
            Node::Or(ts) => Node::Or(Self::subst_all(ts, map)?),
            Node::Implies(a, b) => {
                Node::Implies(Box::new(a.substitute(map)?), Box::new(b.substitute(map)?))
            }
            Node::Eq(a, b) => Node::Eq(Box::new(a.substitute(map)?), Box::new(b.substitute(map)?)),
            Node::Ite(c, a, b) => Node::Ite(
                Box::new(c.substitute(map)?),
                Box::new(a.substitute(map)?),
                Box::new(b.substitute(map)?),
            ),
            Node::BvBin { op, lhs, rhs } => Node::BvBin {
                op,
                lhs: Box::new(lhs.substitute(map)?),
                rhs: Box::new(rhs.substitute(map)?),
            },
            Node::BvCmp { op, lhs, rhs } => Node::BvCmp {
                op,
                lhs: Box::new(lhs.substitute(map)?),
                rhs: Box::new(rhs.substitute(map)?),
            },
            Node::Select(a, i) => {
                Node::Select(Box::new(a.substitute(map)?), Box::new(i.substitute(map)?))
            }
            Node::Store(a, i, v) => Node::Store(
                Box::new(a.substitute(map)?),
                Box::new(i.substitute(map)?),
                Box::new(v.substitute(map)?),
            ),
            Node::DtCtor { dt, ctor, args } => {
                Node::DtCtor { dt, ctor, args: Self::subst_all(args, map)? }
            }
            Node::DtIs { ctor, arg } => Node::DtIs { ctor, arg: Box::new(arg.substitute(map)?) },
            Node::DtGet { field, arg } => {
                Node::DtGet { field, arg: Box::new(arg.substitute(map)?) }
            }
        };
        Ok(t)
    }

    fn subst_all(ts: Vec<Term>, map: &BTreeMap<String, Term>) -> Result<Vec<Term>, SortError> {
        ts.into_iter().map(|t| t.substitute(map)).collect()
    }

    /// Free variables (there are no binders in the QF language, so "free"
    /// = "all"). Sorted by name — feeds the printer's declaration order.
    pub fn free_vars(&self) -> BTreeMap<String, Sort> {
        let mut out = BTreeMap::new();
        self.collect_vars(&mut out);
        out
    }

    fn collect_vars(&self, out: &mut BTreeMap<String, Sort>) {
        match &self.node {
            Node::Var(name) => {
                out.insert(name.clone(), self.sort.clone());
            }
            Node::BoolLit(_) | Node::BvLit { .. } => {}
            Node::Not(a) | Node::DtIs { arg: a, .. } | Node::DtGet { arg: a, .. } => {
                a.collect_vars(out);
            }
            Node::And(ts) | Node::Or(ts) | Node::DtCtor { args: ts, .. } => {
                for t in ts {
                    t.collect_vars(out);
                }
            }
            Node::Implies(a, b) | Node::Eq(a, b) => {
                a.collect_vars(out);
                b.collect_vars(out);
            }
            Node::BvBin { lhs, rhs, .. } | Node::BvCmp { lhs, rhs, .. } => {
                lhs.collect_vars(out);
                rhs.collect_vars(out);
            }
            Node::Select(a, b) => {
                a.collect_vars(out);
                b.collect_vars(out);
            }
            Node::Ite(a, b, c) | Node::Store(a, b, c) => {
                a.collect_vars(out);
                b.collect_vars(out);
                c.collect_vars(out);
            }
        }
    }

    /// True if any subterm has Datatype sort or is a Dt* node — tells
    /// `Query::for_asserts` whether the Ptr declaration is needed.
    pub(crate) fn uses_datatype(&self) -> bool {
        if matches!(self.sort, Sort::Datatype(_))
            || matches!(self.node, Node::DtCtor { .. } | Node::DtIs { .. } | Node::DtGet { .. })
        {
            return true;
        }
        match &self.node {
            Node::BoolLit(_) | Node::BvLit { .. } | Node::Var(_) => false,
            Node::Not(a) | Node::DtIs { arg: a, .. } | Node::DtGet { arg: a, .. } => {
                a.uses_datatype()
            }
            Node::And(ts) | Node::Or(ts) | Node::DtCtor { args: ts, .. } => {
                ts.iter().any(Term::uses_datatype)
            }
            Node::Implies(a, b) | Node::Eq(a, b) => a.uses_datatype() || b.uses_datatype(),
            Node::BvBin { lhs, rhs, .. } | Node::BvCmp { lhs, rhs, .. } => {
                lhs.uses_datatype() || rhs.uses_datatype()
            }
            Node::Select(a, b) => a.uses_datatype() || b.uses_datatype(),
            Node::Ite(a, b, c) | Node::Store(a, b, c) => {
                a.uses_datatype() || b.uses_datatype() || c.uses_datatype()
            }
        }
    }
}

pub fn ptr_nil() -> Term {
    Term::dt_ctor(&ptr_datatype(), "ptr-nil", vec![])
        .expect("ptr-nil is a valid nullary constructor")
}

pub fn ptr_is_nil(t: Term) -> Result<Term, SortError> {
    Term::dt_is(&ptr_datatype(), "ptr-nil", t)
}
```

- [ ] **Step 5: Wire `lib.rs`**

Add to `crates/goverify-solver/src/lib.rs` (keeping the phase-2 `Solver` trait, string `Decl`/`Term`… — rename the phase-2 string types to avoid a clash NOW: the old opaque `Decl(String)`/`Term(String)`/`Model` and the `Solver` trait + `StubSolver` are REPLACED wholesale in Task 8; until then, keep them compiling by renaming nothing and instead putting the new AST types in their own modules, re-exported WITHOUT glob):

```rust
mod sort;
mod term;

pub use sort::{CtorDecl, DatatypeDecl, Sort, SortError, ptr_datatype, ptr_sort};
pub use term::{BvBinOp, BvCmpOp, ptr_is_nil, ptr_nil};
```

`term::Term` would clash with the phase-2 `Term(String)`. Until Task 8 deletes the old one, export the new type as `pub use term::Term as AstTerm;` — **Task 8 removes the alias and the old types together**; Tasks 3–7 refer to `crate::term::Term` internally and are unaffected.

- [ ] **Step 6: Run tests**

Run: `mise x -- cargo test -p goverify-solver`
Expected: PASS (all 6 new tests + the phase-2 stub test still green).

- [ ] **Step 7: Lint + commit**

```bash
mise run lint
git add -A
git commit -m "solver: typed term language — sorts, sort-checked constructors, substitution"
```

### Task 3: Canonical SMT-LIB2 printer

**Files:**
- Create: `crates/goverify-solver/src/printer.rs`
- Modify: `crates/goverify-solver/src/lib.rs` (add `mod printer; pub use printer::{Logic, Query};`)

**Interfaces:**
- Consumes: `Term` (`crate::term::Term`), `Sort`, `DatatypeDecl` from Task 2, including `Term::free_vars()` and `Term::uses_datatype()`.
- Produces: `Logic`, `Query { logic, datatypes, consts, asserts }`, `Query::canonical_text() -> String`, `Query::for_asserts(logic, asserts) -> Query`. Tasks 4 (parse), 6/7 (consume bytes), 8 (hash bytes) depend on the exact output format below.

**Canonical format (normative — tests pin every rule):**
1. `(set-logic <L>)\n` where `<L>` ∈ `QF_BV | QF_ABV | ALL`.
2. One `(declare-datatypes ((N 0)) (((ctor (acc sort)...)...)))\n` per datatype, sorted by datatype name.
3. One `(declare-const <name> <sort>)\n` per const, sorted by name.
4. One `(assert <term>)\n` per assert, **in the order given** (obligation construction order is deterministic upstream).
5. `(check-sat)\n` — always last; nothing after it.
6. Terms print fully parenthesized, single spaces, no line breaks inside a term. Bitvector literals print as `(_ bv<value> <width>)`. Testers print as `((_ is <ctor>) <arg>)`. No other whitespace anywhere.

- [ ] **Step 1: Write the failing golden test** (in `printer.rs`'s test module)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::sort::{Sort, ptr_datatype, ptr_sort};
    use crate::term::{BvCmpOp, Term, ptr_is_nil};

    /// The full canonical format, pinned byte-for-byte. If this golden
    /// ever changes, every query-cache entry in the world is invalidated —
    /// that is the point of pinning it.
    #[test]
    fn canonical_text_golden() {
        let p = Term::var("p0", ptr_sort());
        let q = Query::for_asserts(Logic::All, vec![ptr_is_nil(p).unwrap()]);
        assert_eq!(
            q.canonical_text(),
            "(set-logic ALL)\n\
             (declare-datatypes ((Ptr 0)) (((ptr-nil) (ptr-addr (ptr-addr-val (_ BitVec 64))))))\n\
             (declare-const p0 Ptr)\n\
             (assert ((_ is ptr-nil) p0))\n\
             (check-sat)\n"
        );
    }

    #[test]
    fn bv_and_bool_query_golden() {
        let x = Term::var("x", Sort::BitVec(8));
        let five = Term::bv_lit(8, 5);
        let cmp = Term::bv_cmp(BvCmpOp::Ult, x, five).unwrap();
        let q = Query::for_asserts(Logic::QfBv, vec![cmp]);
        assert_eq!(
            q.canonical_text(),
            "(set-logic QF_BV)\n\
             (declare-const x (_ BitVec 8))\n\
             (assert (bvult x (_ bv5 8)))\n\
             (check-sat)\n"
        );
    }

    #[test]
    fn decls_are_sorted_regardless_of_construction_order() {
        let b = Term::var("bbb", Sort::Bool);
        let a = Term::var("aaa", Sort::Bool);
        let q = Query::for_asserts(Logic::QfBv, vec![Term::and(vec![b, a]).unwrap()]);
        let text = q.canonical_text();
        let ai = text.find("declare-const aaa").unwrap();
        let bi = text.find("declare-const bbb").unwrap();
        assert!(ai < bi, "consts sorted by name:\n{text}");
    }

    #[test]
    fn array_sort_prints_smtlib2() {
        let arr = Term::var(
            "m",
            Sort::Array(Box::new(Sort::BitVec(64)), Box::new(Sort::Bool)),
        );
        let idx = Term::bv_lit(64, 0);
        let q = Query::for_asserts(
            Logic::QfAbv,
            vec![Term::select(arr, idx).unwrap()],
        );
        assert!(
            q.canonical_text()
                .contains("(declare-const m (Array (_ BitVec 64) Bool))"),
            "{}",
            q.canonical_text()
        );
    }

    #[test]
    fn printing_is_deterministic() {
        let mk = || {
            let p = Term::var("p0", ptr_sort());
            Query::for_asserts(Logic::All, vec![ptr_is_nil(p).unwrap()])
        };
        assert_eq!(mk().canonical_text(), mk().canonical_text());
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `mise x -- cargo test -p goverify-solver printer`
Expected: FAIL to compile (`Query` missing).

- [ ] **Step 3: Implement `printer.rs`**

```rust
//! Canonical SMT-LIB2 printer (phase-3 spec §4): the ONLY Term→SMT-LIB2
//! lowering in the codebase. Both backends consume these exact bytes and
//! blake3(bytes) is the query-cache identity, so any format change
//! invalidates every cache in the world — the golden test pins it.

use std::fmt::Write;

use crate::sort::{DatatypeDecl, Sort, ptr_datatype};
use crate::term::{BvBinOp, BvCmpOp, Node, Term};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Logic {
    QfBv,
    QfAbv,
    All,
}

impl Logic {
    fn as_str(self) -> &'static str {
        match self {
            Logic::QfBv => "QF_BV",
            Logic::QfAbv => "QF_ABV",
            Logic::All => "ALL",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    pub logic: Logic,
    pub datatypes: Vec<DatatypeDecl>,
    pub consts: Vec<(String, Sort)>,
    pub asserts: Vec<Term>,
}

impl Query {
    /// Build a query from bare assertions: free variables become
    /// declarations automatically; the Ptr datatype is declared iff any
    /// assert mentions a datatype (v1 has only Ptr).
    pub fn for_asserts(logic: Logic, asserts: Vec<Term>) -> Query {
        let mut consts = std::collections::BTreeMap::new();
        let mut needs_ptr = false;
        for a in &asserts {
            consts.append(&mut a.free_vars());
            needs_ptr = needs_ptr || a.uses_datatype();
        }
        Query {
            logic,
            datatypes: if needs_ptr { vec![ptr_datatype()] } else { vec![] },
            consts: consts.into_iter().collect(),
            asserts,
        }
    }

    pub fn canonical_text(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "(set-logic {})", self.logic.as_str());
        let mut dts = self.datatypes.clone();
        dts.sort_by(|a, b| a.name.cmp(&b.name));
        dts.dedup_by(|a, b| a.name == b.name);
        for dt in &dts {
            let _ = write!(out, "(declare-datatypes (({} 0)) ((", dt.name);
            for (i, c) in dt.ctors.iter().enumerate() {
                if i > 0 {
                    out.push(' ');
                }
                let _ = write!(out, "({}", c.name);
                for (fname, fsort) in &c.fields {
                    let _ = write!(out, " ({fname} {})", sort_str(fsort));
                }
                out.push(')');
            }
            out.push_str(")))\n");
        }
        let mut consts = self.consts.clone();
        consts.sort();
        consts.dedup();
        for (name, sort) in &consts {
            let _ = writeln!(out, "(declare-const {name} {})", sort_str(sort));
        }
        for a in &self.asserts {
            let mut t = String::new();
            term_str(a, &mut t);
            let _ = writeln!(out, "(assert {t})");
        }
        out.push_str("(check-sat)\n");
        out
    }
}

fn sort_str(s: &Sort) -> String {
    match s {
        Sort::Bool => "Bool".to_string(),
        Sort::BitVec(w) => format!("(_ BitVec {w})"),
        Sort::Array(k, v) => format!("(Array {} {})", sort_str(k), sort_str(v)),
        Sort::Datatype(n) => n.clone(),
    }
}

fn bv_bin_str(op: BvBinOp) -> &'static str {
    match op {
        BvBinOp::Add => "bvadd",
        BvBinOp::Sub => "bvsub",
        BvBinOp::Mul => "bvmul",
        BvBinOp::Udiv => "bvudiv",
        BvBinOp::Sdiv => "bvsdiv",
        BvBinOp::Urem => "bvurem",
        BvBinOp::Srem => "bvsrem",
        BvBinOp::And => "bvand",
        BvBinOp::Or => "bvor",
        BvBinOp::Xor => "bvxor",
        BvBinOp::Shl => "bvshl",
        BvBinOp::Lshr => "bvlshr",
        BvBinOp::Ashr => "bvashr",
    }
}

fn bv_cmp_str(op: BvCmpOp) -> &'static str {
    match op {
        BvCmpOp::Ult => "bvult",
        BvCmpOp::Ule => "bvule",
        BvCmpOp::Slt => "bvslt",
        BvCmpOp::Sle => "bvsle",
    }
}

fn app(out: &mut String, head: &str, args: &[&Term]) {
    out.push('(');
    out.push_str(head);
    for a in args {
        out.push(' ');
        term_str(a, out);
    }
    out.push(')');
}

fn term_str(t: &Term, out: &mut String) {
    match &t.node {
        Node::BoolLit(true) => out.push_str("true"),
        Node::BoolLit(false) => out.push_str("false"),
        Node::BvLit { width, value } => {
            let _ = write!(out, "(_ bv{value} {width})");
        }
        Node::Var(n) => out.push_str(n),
        Node::Not(a) => app(out, "not", &[a]),
        Node::And(ts) => app(out, "and", &ts.iter().collect::<Vec<_>>()),
        Node::Or(ts) => app(out, "or", &ts.iter().collect::<Vec<_>>()),
        Node::Implies(a, b) => app(out, "=>", &[a, b]),
        Node::Eq(a, b) => app(out, "=", &[a, b]),
        Node::Ite(c, a, b) => app(out, "ite", &[c, a, b]),
        Node::BvBin { op, lhs, rhs } => app(out, bv_bin_str(*op), &[lhs, rhs]),
        Node::BvCmp { op, lhs, rhs } => app(out, bv_cmp_str(*op), &[lhs, rhs]),
        Node::Select(a, i) => app(out, "select", &[a, i]),
        Node::Store(a, i, v) => app(out, "store", &[a, i, v]),
        Node::DtCtor { ctor, args, .. } => {
            if args.is_empty() {
                out.push_str(ctor);
            } else {
                app(out, ctor, &args.iter().collect::<Vec<_>>());
            }
        }
        Node::DtIs { ctor, arg } => {
            let head = format!("(_ is {ctor})");
            app(out, &head, &[arg]);
        }
        Node::DtGet { field, arg } => app(out, field, &[arg]),
    }
}
```

Note: `Node` and `Term::node` must be `pub(crate)` (they already are per Task 2); `and`/`or` with a single element print as `(and x)` — Z3 accepts it, and canonical-form simplicity beats special-casing.

- [ ] **Step 4: Run tests**

Run: `mise x -- cargo test -p goverify-solver printer`
Expected: PASS (5 tests).

- [ ] **Step 5: Lint + commit**

```bash
mise run lint
git add -A
git commit -m "solver: canonical SMT-LIB2 printer — the single Term lowering, format pinned by goldens"
```

### Task 4: S-expression/term reader, round-trip properties, fuzz target

**Files:**
- Create: `crates/goverify-solver/src/reader.rs`
- Modify: `crates/goverify-solver/src/lib.rs` (`mod reader; pub use reader::{ReadError, SExpr, parse_query, parse_response, parse_sexpr};`)
- Create: `fuzz/fuzz_targets/smt_reader.rs`
- Modify: `fuzz/Cargo.toml` (new `[[bin]]` target), `mise.toml` (fuzz task), `.github/workflows/nightly.yml` (fuzz step)
- Modify: `crates/goverify-solver/Cargo.toml` (add `proptest` dev-dep: `proptest.workspace = true`)

**Interfaces:**
- Consumes: `Query`, `Logic` (Task 3); `Term`, `Sort`, `DatatypeDecl` (Task 2).
- Produces: `parse_sexpr(input) -> Result<(SExpr, usize), ReadError>`, `parse_response(first_line) -> SatResult`, `parse_query(text) -> Result<Query, ReadError>`, `ReadError(pub String)`. Task 7 uses `parse_response` + `parse_sexpr` (model slurping); the round-trip property here guards Task 3's printer.

**Design notes:**
- This parses bytes the analyzer didn't write (solver stdout; fuzz input): **reject, never panic**. Recursion is the panic vector — parse iteratively or depth-cap at 64; input length cap 1 MiB (`ReadError` beyond).
- `parse_query` only needs to understand what `canonical_text()` emits (set-logic/declare-datatypes/declare-const/assert/check-sat and the term grammar of Task 3). Anything else ⇒ `ReadError`. It exists for the round-trip property and the fuzz target — it is NOT a general SMT-LIB2 parser; say so in the module doc.
- Term parsing needs sorts for variables: build a symbol table from the `declare-const`s and datatype decls first, then parse assert terms bottom-up, reconstructing via the Task-2 smart constructors (which re-sort-check for free — a parse that constructs an ill-sorted term is itself a `ReadError`).

- [ ] **Step 1: Write failing unit tests** (in `reader.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::SatResult;
    use crate::printer::{Logic, Query};
    use crate::sort::ptr_sort;
    use crate::term::{Term, ptr_is_nil};

    #[test]
    fn response_lines() {
        assert_eq!(parse_response("sat"), SatResult::Sat);
        assert_eq!(parse_response("unsat"), SatResult::Unsat);
        assert_eq!(parse_response("unknown"), SatResult::Unknown);
        assert_eq!(parse_response("timeout"), SatResult::Unknown);
        assert_eq!(parse_response(""), SatResult::Unknown);
        assert_eq!(parse_response("(error \"boom\")"), SatResult::Unknown);
    }

    #[test]
    fn sexpr_basic() {
        let (e, n) = parse_sexpr("(a (b c) d)").unwrap();
        assert_eq!(n, 11);
        let SExpr::List(items) = e else { panic!("want list") };
        assert_eq!(items.len(), 3);
    }

    #[test]
    fn sexpr_rejects_garbage_without_panicking() {
        for bad in ["", "(", ")", "(a", "((((((", "(a))"] {
            assert!(parse_sexpr(bad).is_err(), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn sexpr_depth_cap() {
        let deep = format!("{}{}{}", "(".repeat(100), "x", ")".repeat(100));
        assert!(parse_sexpr(&deep).is_err(), "depth > 64 must be rejected");
    }

    #[test]
    fn query_round_trips() {
        let p = Term::var("p0", ptr_sort());
        let q = Query::for_asserts(Logic::All, vec![ptr_is_nil(p).unwrap()]);
        let text = q.canonical_text();
        let parsed = parse_query(&text).expect("canonical text must parse");
        assert_eq!(parsed.canonical_text(), text, "print∘parse must be a fixpoint");
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `mise x -- cargo test -p goverify-solver reader`
Expected: FAIL to compile.

- [ ] **Step 3: Implement `reader.rs`**

```rust
//! S-expression reader for solver responses + the canonical-query parser
//! used by the round-trip property suite and the fuzz target. Parses
//! bytes the analyzer didn't write: rejects, never panics (parent §11,
//! §12.4). NOT a general SMT-LIB2 parser — it understands exactly the
//! subset printer.rs emits, plus solver response lines.

use std::collections::BTreeMap;

use crate::SatResult;
use crate::printer::{Logic, Query};
use crate::sort::{CtorDecl, DatatypeDecl, Sort};
use crate::term::{BvBinOp, BvCmpOp, Term};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadError(pub String);

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "smt read error: {}", self.0)
    }
}

impl std::error::Error for ReadError {}

fn err(m: impl Into<String>) -> ReadError {
    ReadError(m.into())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SExpr {
    Atom(String),
    List(Vec<SExpr>),
}

const MAX_INPUT: usize = 1 << 20; // 1 MiB
const MAX_DEPTH: usize = 64;

/// Parse one s-expression from the front of `input`; returns it plus the
/// number of bytes consumed. Iterative (explicit stack), depth-capped.
pub fn parse_sexpr(input: &str) -> Result<(SExpr, usize), ReadError> {
    if input.len() > MAX_INPUT {
        return Err(err("input too large"));
    }
    let b = input.as_bytes();
    let mut i = 0usize;
    let mut stack: Vec<Vec<SExpr>> = Vec::new();
    loop {
        while i < b.len() && (b[i] as char).is_whitespace() {
            i += 1;
        }
        if i >= b.len() {
            return Err(err("unexpected end of input"));
        }
        match b[i] {
            b'(' => {
                if stack.len() >= MAX_DEPTH {
                    return Err(err("nesting too deep"));
                }
                stack.push(Vec::new());
                i += 1;
            }
            b')' => {
                let done = stack.pop().ok_or_else(|| err("unbalanced ')'"))?;
                i += 1;
                let e = SExpr::List(done);
                match stack.last_mut() {
                    Some(parent) => parent.push(e),
                    None => return Ok((e, i)),
                }
            }
            b'"' => {
                // quoted string atom (models contain them); keep quotes.
                let start = i;
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    i += 1;
                }
                if i >= b.len() {
                    return Err(err("unterminated string"));
                }
                i += 1;
                let e = SExpr::Atom(input[start..i].to_string());
                match stack.last_mut() {
                    Some(parent) => parent.push(e),
                    None => return Ok((e, i)),
                }
            }
            _ => {
                let start = i;
                while i < b.len() && !(b[i] as char).is_whitespace() && b[i] != b'(' && b[i] != b')'
                {
                    i += 1;
                }
                let e = SExpr::Atom(input[start..i].to_string());
                match stack.last_mut() {
                    Some(parent) => parent.push(e),
                    None => return Ok((e, i)),
                }
            }
        }
    }
}

/// First response line → SatResult. ANYTHING unrecognized is Unknown
/// (bug-finder semantics: garbage output must never become a report).
pub fn parse_response(first_line: &str) -> SatResult {
    match first_line.trim() {
        "sat" => SatResult::Sat,
        "unsat" => SatResult::Unsat,
        _ => SatResult::Unknown,
    }
}

/// Parse text in the exact canonical shape printer.rs emits.
pub fn parse_query(text: &str) -> Result<Query, ReadError> {
    if text.len() > MAX_INPUT {
        return Err(err("input too large"));
    }
    let mut logic = None;
    let mut datatypes: Vec<DatatypeDecl> = Vec::new();
    let mut consts: Vec<(String, Sort)> = Vec::new();
    let mut asserts: Vec<Term> = Vec::new();
    let mut saw_check_sat = false;
    let mut rest = text;
    while !rest.trim().is_empty() {
        if saw_check_sat {
            return Err(err("content after (check-sat)"));
        }
        let (e, n) = parse_sexpr(rest)?;
        rest = &rest[n..];
        let SExpr::List(items) = &e else {
            return Err(err("top level must be command lists"));
        };
        match items.first() {
            Some(SExpr::Atom(a)) if a == "set-logic" => {
                let [_, SExpr::Atom(l)] = items.as_slice() else {
                    return Err(err("malformed set-logic"));
                };
                logic = Some(match l.as_str() {
                    "QF_BV" => Logic::QfBv,
                    "QF_ABV" => Logic::QfAbv,
                    "ALL" => Logic::All,
                    other => return Err(err(format!("unknown logic {other}"))),
                });
            }
            Some(SExpr::Atom(a)) if a == "declare-datatypes" => {
                datatypes.push(parse_datatype(items)?);
            }
            Some(SExpr::Atom(a)) if a == "declare-const" => {
                let [_, SExpr::Atom(name), sort] = items.as_slice() else {
                    return Err(err("malformed declare-const"));
                };
                consts.push((name.clone(), parse_sort(sort)?));
            }
            Some(SExpr::Atom(a)) if a == "assert" => {
                let [_, body] = items.as_slice() else {
                    return Err(err("malformed assert"));
                };
                let env: BTreeMap<String, Sort> = consts.iter().cloned().collect();
                asserts.push(parse_term(body, &env, &datatypes)?);
            }
            Some(SExpr::Atom(a)) if a == "check-sat" => {
                saw_check_sat = true;
            }
            _ => return Err(err("unknown command")),
        }
    }
    if !saw_check_sat {
        return Err(err("missing (check-sat)"));
    }
    Ok(Query {
        logic: logic.ok_or_else(|| err("missing (set-logic)"))?,
        datatypes,
        consts,
        asserts,
    })
}

fn parse_sort(e: &SExpr) -> Result<Sort, ReadError> {
    match e {
        SExpr::Atom(a) if a == "Bool" => Ok(Sort::Bool),
        SExpr::Atom(a) => Ok(Sort::Datatype(a.clone())),
        SExpr::List(items) => match items.as_slice() {
            [SExpr::Atom(u), SExpr::Atom(bv), SExpr::Atom(w)]
                if u == "_" && bv == "BitVec" =>
            {
                Ok(Sort::BitVec(w.parse().map_err(|_| err("bad width"))?))
            }
            [SExpr::Atom(arr), k, v] if arr == "Array" => Ok(Sort::Array(
                Box::new(parse_sort(k)?),
                Box::new(parse_sort(v)?),
            )),
            _ => Err(err("unknown sort")),
        },
    }
}

fn parse_datatype(items: &[SExpr]) -> Result<DatatypeDecl, ReadError> {
    // ((N 0)) (((ctor (acc sort)...) ...))
    let [_, SExpr::List(names), SExpr::List(bodies)] = items else {
        return Err(err("malformed declare-datatypes"));
    };
    let [SExpr::List(nv)] = names.as_slice() else {
        return Err(err("expect one datatype"));
    };
    let [SExpr::Atom(name), SExpr::Atom(zero)] = nv.as_slice() else {
        return Err(err("expect (Name 0)"));
    };
    if zero != "0" {
        return Err(err("parametric datatypes unsupported"));
    }
    let [SExpr::List(ctors)] = bodies.as_slice() else {
        return Err(err("expect one ctor list"));
    };
    let mut out = Vec::new();
    for c in ctors {
        let SExpr::List(cv) = c else { return Err(err("ctor must be a list")) };
        let Some((SExpr::Atom(cname), fields)) = cv.split_first() else {
            return Err(err("empty ctor"));
        };
        let mut fs = Vec::new();
        for f in fields {
            let SExpr::List(fv) = f else { return Err(err("field must be a list")) };
            let [SExpr::Atom(fname), fsort] = fv.as_slice() else {
                return Err(err("malformed field"));
            };
            fs.push((fname.clone(), parse_sort(fsort)?));
        }
        out.push(CtorDecl { name: cname.clone(), fields: fs });
    }
    Ok(DatatypeDecl { name: name.clone(), ctors: out })
}

fn bv_bin_of(s: &str) -> Option<BvBinOp> {
    Some(match s {
        "bvadd" => BvBinOp::Add, "bvsub" => BvBinOp::Sub, "bvmul" => BvBinOp::Mul,
        "bvudiv" => BvBinOp::Udiv, "bvsdiv" => BvBinOp::Sdiv, "bvurem" => BvBinOp::Urem,
        "bvsrem" => BvBinOp::Srem, "bvand" => BvBinOp::And, "bvor" => BvBinOp::Or,
        "bvxor" => BvBinOp::Xor, "bvshl" => BvBinOp::Shl, "bvlshr" => BvBinOp::Lshr,
        "bvashr" => BvBinOp::Ashr,
        _ => return None,
    })
}

fn bv_cmp_of(s: &str) -> Option<BvCmpOp> {
    Some(match s {
        "bvult" => BvCmpOp::Ult, "bvule" => BvCmpOp::Ule,
        "bvslt" => BvCmpOp::Slt, "bvsle" => BvCmpOp::Sle,
        _ => return None,
    })
}

fn parse_term(
    e: &SExpr,
    env: &BTreeMap<String, Sort>,
    dts: &[DatatypeDecl],
) -> Result<Term, ReadError> {
    let sub = |e: &SExpr| parse_term(e, env, dts);
    let ill = |se: crate::sort::SortError| err(format!("ill-sorted: {se}"));
    match e {
        SExpr::Atom(a) if a == "true" => Ok(Term::bool_lit(true)),
        SExpr::Atom(a) if a == "false" => Ok(Term::bool_lit(false)),
        SExpr::Atom(a) => {
            if let Some(sort) = env.get(a) {
                return Ok(Term::var(a, sort.clone()));
            }
            // nullary datatype constructor?
            for dt in dts {
                if dt.ctor(a).is_some() {
                    return Term::dt_ctor(dt, a, vec![]).map_err(ill);
                }
            }
            Err(err(format!("unknown atom {a}")))
        }
        SExpr::List(items) => match items.as_slice() {
            [SExpr::Atom(u), SExpr::Atom(bv), SExpr::Atom(w)] if u == "_" && bv.starts_with("bv") => {
                let value: u128 = bv[2..].parse().map_err(|_| err("bad bv literal"))?;
                let width: u32 = w.parse().map_err(|_| err("bad bv width"))?;
                if width == 0 || width > 128 || (width < 128 && value >= (1u128 << width)) {
                    return Err(err("bv literal out of range"));
                }
                Ok(Term::bv_lit(width, value))
            }
            [SExpr::List(tester), arg] => {
                // ((_ is ctor) arg)
                let [SExpr::Atom(u), SExpr::Atom(is), SExpr::Atom(ctor)] = tester.as_slice()
                else {
                    return Err(err("unknown applied form"));
                };
                if u != "_" || is != "is" {
                    return Err(err("unknown applied form"));
                }
                let dt = dts
                    .iter()
                    .find(|d| d.ctor(ctor).is_some())
                    .ok_or_else(|| err(format!("tester for unknown ctor {ctor}")))?;
                Term::dt_is(dt, ctor, sub(arg)?).map_err(ill)
            }
            [SExpr::Atom(head), rest @ ..] => {
                let args: Vec<Term> = rest.iter().map(sub).collect::<Result<_, _>>()?;
                let one = |args: &[Term]| args[0].clone();
                match (head.as_str(), args.len()) {
                    ("not", 1) => Term::not(one(&args)).map_err(ill),
                    ("and", n) if n >= 1 => Term::and(args).map_err(ill),
                    ("or", n) if n >= 1 => Term::or(args).map_err(ill),
                    ("=>", 2) => Term::implies(args[0].clone(), args[1].clone()).map_err(ill),
                    ("=", 2) => Term::eq(args[0].clone(), args[1].clone()).map_err(ill),
                    ("ite", 3) => {
                        Term::ite(args[0].clone(), args[1].clone(), args[2].clone()).map_err(ill)
                    }
                    ("select", 2) => Term::select(args[0].clone(), args[1].clone()).map_err(ill),
                    ("store", 3) => {
                        Term::store(args[0].clone(), args[1].clone(), args[2].clone()).map_err(ill)
                    }
                    (h, 2) if bv_bin_of(h).is_some() => {
                        Term::bv_bin(bv_bin_of(h).unwrap(), args[0].clone(), args[1].clone())
                            .map_err(ill)
                    }
                    (h, 2) if bv_cmp_of(h).is_some() => {
                        Term::bv_cmp(bv_cmp_of(h).unwrap(), args[0].clone(), args[1].clone())
                            .map_err(ill)
                    }
                    (h, _) => {
                        // ctor application or accessor
                        for dt in dts {
                            if dt.ctor(h).is_some() {
                                return Term::dt_ctor(dt, h, args).map_err(ill);
                            }
                            for c in &dt.ctors {
                                if c.fields.iter().any(|(f, _)| f == h) && args.len() == 1 {
                                    return Term::dt_get(dt, &c.name, h, one(&args)).map_err(ill);
                                }
                            }
                        }
                        Err(err(format!("unknown head {h}")))
                    }
                }
            }
            _ => Err(err("unknown term shape")),
        },
    }
}
```

- [ ] **Step 4: Run tests, then add the round-trip property**

Run: `mise x -- cargo test -p goverify-solver reader` — expected PASS. Then append a proptest module to `reader.rs`:

```rust
#[cfg(test)]
mod props {
    use proptest::prelude::*;

    use super::*;
    use crate::printer::{Logic, Query};
    use crate::sort::{Sort, ptr_sort};
    use crate::term::{BvBinOp, BvCmpOp, Term, ptr_is_nil, ptr_nil};

    /// Small random term over a fixed variable pool — every theory shows
    /// up: bool, bitvec, arrays, the Ptr datatype.
    fn arb_term() -> impl Strategy<Value = Term> {
        let leaf = prop_oneof![
            any::<bool>().prop_map(Term::bool_lit),
            (0u128..256).prop_map(|v| Term::bv_lit(8, v)),
            Just(Term::var("x", Sort::BitVec(8))),
            Just(Term::var("b", Sort::Bool)),
            Just(Term::var("p0", ptr_sort())),
            Just(ptr_nil()),
            Just(Term::var(
                "m",
                Sort::Array(Box::new(Sort::BitVec(8)), Box::new(Sort::Bool)),
            )),
        ];
        leaf.prop_recursive(4, 32, 3, |inner| {
            prop_oneof![
                (inner.clone(), inner.clone()).prop_filter_map("eq", |(a, b)| Term::eq(a, b).ok()),
                inner.clone().prop_filter_map("not", |a| Term::not(a).ok()),
                prop::collection::vec(inner.clone(), 1..3)
                    .prop_filter_map("and", |ts| Term::and(ts).ok()),
                (inner.clone(), inner.clone())
                    .prop_filter_map("bvadd", |(a, b)| Term::bv_bin(BvBinOp::Add, a, b).ok()),
                (inner.clone(), inner.clone())
                    .prop_filter_map("bvult", |(a, b)| Term::bv_cmp(BvCmpOp::Ult, a, b).ok()),
                inner.clone().prop_filter_map("isnil", |a| ptr_is_nil(a).ok()),
                (inner.clone(), inner)
                    .prop_filter_map("select", |(a, i)| Term::select(a, i).ok()),
            ]
        })
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 128, ..ProptestConfig::default() })]

        /// print → parse → print is a fixpoint (phase-3 spec §12): this,
        /// not the differential harness, is the canonical printer's guard.
        #[test]
        fn print_parse_print_fixpoint(t in arb_term()) {
            // Only Bool terms can be asserted; wrap non-bool t in an eq.
            let a = if t.sort() == &Sort::Bool { t.clone() }
                    else { Term::eq(t.clone(), t).unwrap() };
            let q = Query::for_asserts(Logic::All, vec![a]);
            let text = q.canonical_text();
            let parsed = parse_query(&text).expect("canonical text must parse");
            prop_assert_eq!(parsed.canonical_text(), text);
        }

        /// The reader never panics on arbitrary input (cheap in-process
        /// complement to the fuzz target).
        #[test]
        fn reader_never_panics(s in "\\PC{0,256}") {
            let _ = parse_sexpr(&s);
            let _ = parse_query(&s);
            let _ = parse_response(&s);
        }
    }
}
```

Add `proptest.workspace = true` under `[dev-dependencies]` in `crates/goverify-solver/Cargo.toml`.

Run: `mise x -- cargo test -p goverify-solver`
Expected: PASS.

- [ ] **Step 5: Fuzz target**

Create `fuzz/fuzz_targets/smt_reader.rs` (mirror the structure of the existing `fuzz/fuzz_targets/gvir_decode.rs` for the boilerplate):

```rust
//! The reader parses solver output — bytes the analyzer didn't write.
//! It must reject, never panic (parent spec §12.4).
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = goverify_solver::parse_sexpr(s);
        let _ = goverify_solver::parse_query(s);
        let _ = goverify_solver::parse_response(s);
    }
});
```

In `fuzz/Cargo.toml`: add `goverify-solver = { path = "../crates/goverify-solver" }` to `[dependencies]` and a `[[bin]]` block copying the existing targets' shape (`name = "smt_reader"`, `path = "fuzz_targets/smt_reader.rs"`, `test = false`, `doc = false`).

In `mise.toml`, extend the fuzz task:

```toml
[tasks.fuzz]
description = "fuzz smoke run (nightly tier; needs rustup nightly)"
run = [
  "cargo +nightly fuzz run gvir_decode -- -max_total_time=60",
  "cargo +nightly fuzz run ir_lower -- -max_total_time=60",
  "cargo +nightly fuzz run smt_reader -- -max_total_time=60",
]
dir = "{{cwd}}"
```

In `.github/workflows/nightly.yml`, after the `ir_lower` step (phase-2 final-review lesson: nightly wiring is part of THIS task's file list, not an afterthought):

```yaml
      - run: cargo +nightly fuzz run smt_reader -- -max_total_time=900
```

and bump the fuzz job's `timeout-minutes: 45` → `60` (three 900 s runs + harness builds), updating the comment above it to say three runs.

- [ ] **Step 6: Fuzz smoke + verification**

Run: `mise x -- cargo +nightly fuzz run smt_reader -- -max_total_time=30`
Expected: exits clean, no crashes. (If `+nightly` is unavailable in the sandbox, note it in the report — CI covers it — but attempt it first.)

Run: `mise run lint && mise run test`
Expected: green.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "solver: s-expr/query reader (reject-never-panic), print∘parse round-trip property, smt_reader fuzz target"
```

### Task 5: goverify-cache — content-addressed store + query cache

**Files:**
- Create: `crates/goverify-cache/src/store.rs`
- Create: `crates/goverify-cache/src/query.rs`
- Modify: `crates/goverify-cache/src/lib.rs` (replace the stub)
- Modify: `crates/goverify-cache/Cargo.toml` (deps: `blake3.workspace = true`; dev-deps: `tempfile.workspace = true`)
- Modify: root `Cargo.toml` `[workspace.dependencies]`: add `goverify-cache = { path = "crates/goverify-cache" }`

**Interfaces:**
- Consumes: nothing from other tasks (`canonical` arrives as `&str`).
- Produces: `Store::{open,get,put}`, `QueryKeyParts`, `query_key`, `CachedOutcome`, `QueryCache::{open,get,put}` exactly as in the interface block. Task 8 (`discharge_query`) and Task 12 (CLI `--cache-dir`) consume them.

**Design notes:**
- Layout: `<root>/<layer>/<hh>/<hex>` (`hh` = first two hex chars of the 64-char lowercase blake3 hex). Query layer name: `"query"`.
- `put` = write to `<root>/<layer>/tmp-<hex>-<pid>` then `fs::rename` (atomic on POSIX, same filesystem by construction). Concurrent same-key writers are benign: same key ⇒ same deterministic value (spec §7); last rename wins with identical bytes. Take a `std::fs::File::lock` (Rust ≥1.89, we pin 1.97.1) exclusive lock on `<root>/<layer>.lock` for the duration of `put` — cheap, satisfies the spec's advisory-locking requirement for concurrent runs, and serializes directory creation.
- `get`: ANY failure (missing, unreadable, decode error) is `None` — corrupt-entry-as-miss (parent §11).
- **Key derivation is length-prefixed** (phase-1 deferred lesson: concatenation without length prefixes lets `("ab","c")` collide with `("a","bc")`): hash `u64-LE(len(field)) ‖ field` for each field in a fixed order.
- The cache record is bytes the analyzer wrote *in a previous run* — decode defensively, never panic: version byte `1`, result byte (`0`=Unsat, `1`=Sat, `2`=Unknown), then for Sat: model-present byte (`0|1`) + `u32-LE` model length + model bytes. Anything else ⇒ `None`.

- [ ] **Step 1: Write failing tests** (`store.rs` and `query.rs` test modules)

```rust
// store.rs tests
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_then_get_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path().to_path_buf());
        let key = [7u8; 32];
        s.put("query", &key, b"hello").unwrap();
        assert_eq!(s.get("query", &key), Some(b"hello".to_vec()));
    }

    #[test]
    fn missing_key_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path().to_path_buf());
        assert_eq!(s.get("query", &[0u8; 32]), None);
    }

    #[test]
    fn layers_are_disjoint() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path().to_path_buf());
        let key = [1u8; 32];
        s.put("query", &key, b"q").unwrap();
        assert_eq!(s.get("summary", &key), None);
    }

    #[test]
    fn concurrent_puts_same_key_are_safe() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path().to_path_buf());
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    let s2 = Store::open(dir.path().to_path_buf());
                    s2.put("query", &[9u8; 32], b"same-bytes").unwrap();
                });
            }
        });
        assert_eq!(s.get("query", &[9u8; 32]), Some(b"same-bytes".to_vec()));
    }
}

// query.rs tests
#[cfg(test)]
mod tests {
    use super::*;

    fn parts<'a>(c: &'a str, id: &'a str) -> QueryKeyParts<'a> {
        QueryKeyParts { canonical: c, solver_identity: id, timeout_ms: 100, mem_mb: 1024 }
    }

    #[test]
    fn key_is_stable_and_field_sensitive() {
        let k1 = query_key(&parts("(check-sat)\n", "z3-4.12"));
        assert_eq!(k1, query_key(&parts("(check-sat)\n", "z3-4.12")), "stable");
        assert_ne!(k1, query_key(&parts("(check-sat)\n", "z3-4.13")), "identity in key");
        assert_ne!(
            k1,
            query_key(&QueryKeyParts { timeout_ms: 200, ..parts("(check-sat)\n", "z3-4.12") }),
            "limits in key"
        );
    }

    #[test]
    fn key_fields_are_length_prefixed() {
        // ("ab","c…") vs ("a","bc…") must not collide.
        assert_ne!(query_key(&parts("ab", "c")), query_key(&parts("a", "bc")));
    }

    #[test]
    fn outcome_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let c = QueryCache::open(dir.path().to_path_buf());
        let key = [3u8; 32];
        for v in [
            CachedOutcome::Unsat,
            CachedOutcome::Unknown,
            CachedOutcome::Sat { model: None },
            CachedOutcome::Sat { model: Some("((p0 ptr-nil))".into()) },
        ] {
            c.put(&key, &v).unwrap();
            assert_eq!(c.get(&key), Some(v));
        }
    }

    #[test]
    fn corrupt_entry_is_a_miss_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let c = QueryCache::open(dir.path().to_path_buf());
        let key = [4u8; 32];
        c.put(&key, &CachedOutcome::Unsat).unwrap();
        // Truncate / garble the underlying file.
        let hex: String = key.iter().map(|b| format!("{b:02x}")).collect();
        let path = dir.path().join("query").join(&hex[..2]).join(&hex);
        for bytes in [&b""[..], &b"\xff\xff\xff"[..], &[1, 1][..]] {
            std::fs::write(&path, bytes).unwrap();
            assert_eq!(c.get(&key), None, "bytes {bytes:?} must be a miss");
        }
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `mise x -- cargo test -p goverify-cache`
Expected: FAIL to compile.

- [ ] **Step 3: Implement**

`crates/goverify-cache/src/lib.rs`:

```rust
//! Content-addressed cache (parent spec §9; phase-3 spec §7). Phase 3
//! ships the generic store + the query layer; extraction/summary layers
//! land in phase 5 on the same Store.

mod query;
mod store;

pub use query::{CachedOutcome, QueryCache, QueryKeyParts, query_key};
pub use store::Store;
```

`crates/goverify-cache/src/store.rs`:

```rust
//! Filesystem content-addressed store: `<root>/<layer>/<hh>/<hex>`.
//! Writes are tempfile+rename (atomic); reads treat every failure as a
//! miss; an advisory exclusive lock per layer serializes writers from
//! concurrent runs (parent spec §9).

use std::fs;
use std::io;
use std::path::PathBuf;

#[derive(Debug)]
pub struct Store {
    root: PathBuf,
}

fn hex(key: &[u8; 32]) -> String {
    key.iter().map(|b| format!("{b:02x}")).collect()
}

impl Store {
    /// Never fails: directory creation is deferred to `put` (a read-only
    /// consumer of a nonexistent cache just misses).
    pub fn open(root: PathBuf) -> Store {
        Store { root }
    }

    fn entry_path(&self, layer: &str, key: &[u8; 32]) -> PathBuf {
        let h = hex(key);
        self.root.join(layer).join(&h[..2]).join(h)
    }

    /// Any failure — missing, unreadable, permission — is a miss.
    pub fn get(&self, layer: &str, key: &[u8; 32]) -> Option<Vec<u8>> {
        fs::read(self.entry_path(layer, key)).ok()
    }

    pub fn put(&self, layer: &str, key: &[u8; 32], value: &[u8]) -> io::Result<()> {
        let layer_dir = self.root.join(layer);
        let dest = self.entry_path(layer, key);
        fs::create_dir_all(dest.parent().expect("entry path has parent"))?;
        // Advisory lock (spec §7): serializes concurrent runs' writes.
        let lock_path = self.root.join(format!("{layer}.lock"));
        let lock = fs::File::create(&lock_path)?;
        lock.lock()?;
        let tmp = layer_dir.join(format!("tmp-{}-{}", &hex(key)[..8], std::process::id()));
        fs::write(&tmp, value)?;
        let renamed = fs::rename(&tmp, &dest);
        let _ = lock.unlock();
        renamed
    }
}
```

`crates/goverify-cache/src/query.rs`:

```rust
//! Query-cache layer (parent spec §9.3): key = blake3 over the canonical
//! SMT-LIB2 text ⊕ solver identity ⊕ limits (length-prefixed fields);
//! value = SatResult + model text for Sat. Unknown IS cached — it is
//! deterministic per solver build, which is exactly why identity and
//! limits are in the key.

use std::path::PathBuf;

use crate::store::Store;

pub struct QueryKeyParts<'a> {
    pub canonical: &'a str,
    pub solver_identity: &'a str,
    pub timeout_ms: u32,
    pub mem_mb: u32,
}

/// Length-prefixed field hashing: `("ab","c")` must never collide with
/// `("a","bc")` (phase-1 final-review deferred lesson).
pub fn query_key(parts: &QueryKeyParts) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    for field in [parts.canonical.as_bytes(), parts.solver_identity.as_bytes()] {
        h.update(&(field.len() as u64).to_le_bytes());
        h.update(field);
    }
    h.update(&parts.timeout_ms.to_le_bytes());
    h.update(&parts.mem_mb.to_le_bytes());
    *h.finalize().as_bytes()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CachedOutcome {
    Sat { model: Option<String> },
    Unsat,
    Unknown,
}

const VERSION: u8 = 1;
const LAYER: &str = "query";

fn encode(v: &CachedOutcome) -> Vec<u8> {
    let mut out = vec![VERSION];
    match v {
        CachedOutcome::Unsat => out.push(0),
        CachedOutcome::Sat { model } => {
            out.push(1);
            match model {
                None => out.push(0),
                Some(m) => {
                    out.push(1);
                    out.extend((m.len() as u32).to_le_bytes());
                    out.extend(m.as_bytes());
                }
            }
        }
        CachedOutcome::Unknown => out.push(2),
    }
    out
}

/// Bytes from disk: decode defensively, None on any mismatch (corrupt
/// entry = miss, parent §11). Trailing garbage is also a miss.
fn decode(b: &[u8]) -> Option<CachedOutcome> {
    match b {
        [VERSION, 0] => Some(CachedOutcome::Unsat),
        [VERSION, 2] => Some(CachedOutcome::Unknown),
        [VERSION, 1, 0] => Some(CachedOutcome::Sat { model: None }),
        [VERSION, 1, 1, rest @ ..] => {
            let (len, rest) = rest.split_first_chunk::<4>()?;
            let len = u32::from_le_bytes(*len) as usize;
            if rest.len() != len {
                return None;
            }
            Some(CachedOutcome::Sat {
                model: Some(String::from_utf8(rest.to_vec()).ok()?),
            })
        }
        _ => None,
    }
}

pub struct QueryCache {
    store: Store,
}

impl QueryCache {
    pub fn open(root: PathBuf) -> QueryCache {
        QueryCache { store: Store::open(root) }
    }

    pub fn get(&self, key: &[u8; 32]) -> Option<CachedOutcome> {
        decode(&self.store.get(LAYER, key)?)
    }

    pub fn put(&self, key: &[u8; 32], v: &CachedOutcome) -> std::io::Result<()> {
        self.store.put(LAYER, key, &encode(v))
    }
}
```

`crates/goverify-cache/Cargo.toml` (replace stub deps):

```toml
[package]
name = "goverify-cache"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
blake3.workspace = true

[dev-dependencies]
tempfile.workspace = true
```

- [ ] **Step 4: Run tests**

Run: `mise x -- cargo test -p goverify-cache`
Expected: PASS (9 tests).

- [ ] **Step 5: Lint + commit**

```bash
mise run lint
git add -A
git commit -m "cache: content-addressed store (atomic writes, advisory lock, corrupt=miss) + query layer"
```

### Task 6: Z3Native backend (static bundled z3-sys)

**Files:**
- Modify: root `Cargo.toml` (`[workspace.dependencies]`: `z3-sys = { version = "0.8", features = ["static-link-z3"] }`, `goverify-solver` path entry already exists)
- Modify: `crates/goverify-solver/Cargo.toml` (add `z3-sys.workspace = true`)
- Create: `crates/goverify-solver/src/z3native.rs`
- Modify: `crates/goverify-solver/src/lib.rs` (`mod z3native;` + re-exports; define `QueryOutcome`, `SolverLimits`, `TextSolver` here)
- Modify: `.github/workflows/ci.yml` (checks job `timeout-minutes: 10` → `30`, with comment)

**Interfaces:**
- Consumes: `SatResult` (existing), canonical text bytes (Task 3 produces them, but this task's tests may hand-write SMT-LIB2 strings).
- Produces: `SolverLimits { timeout_ms: u32, mem_mb: u32 }` (Default `{ timeout_ms: 100, mem_mb: 1024 }`), `QueryOutcome { result: SatResult, model: Option<String> }`, `trait TextSolver: Send { fn identity(&self) -> String; fn limits(&self) -> SolverLimits; fn solve_text(&mut self, canonical: &str) -> QueryOutcome; }`, `Z3Native::new(limits) -> Z3Native` implementing `TextSolver`. Tasks 7, 8, 11, 12 depend on these exact shapes.

**Design notes (read before writing unsafe code):**
- `z3-sys` pin: `0.8` with `static-link-z3` is the crates.io state as of the spec date; if a newer minor exists at execution time, prefer it (same feature name) and record the bump in the commit message. `Cargo.lock` captures the exact version either way. First build compiles Z3 (~15–30 min) — run `mise x -- cargo build -p goverify-solver` once, early, in the background if you like.
- Use `Z3_mk_context` (NOT `_rc`): reference counting is automatic; ASTs die with the context. One `Z3Native` per rayon worker (Task 12); the struct is `Send` but not `Sync` — do NOT implement `Sync`.
- **Set a no-op error handler** (`Z3_set_error_handler`) immediately after context creation. The default handler aborts the process; with a no-op handler installed, errors surface via `Z3_get_error_code` checks. Every abnormal path returns `Unknown` (never a report, never a panic).
- Timeout/memory: solver-level params `timeout` (ms) and `max_memory` (MB) via `Z3_params_set_uint`.
- **Single-lowering rule:** `solve_text` receives the canonical text. Strip the trailing `"(check-sat)\n"` (exact suffix; if absent, treat as internal error → `Unknown`) before `Z3_parse_smtlib2_string` — Z3's parser handles declarations/assertions, while check-sat is ours to run via `Z3_solver_check`.
- Crash/error recovery (parent §11): if `Z3_get_error_code != Z3_OK` after parsing or checking, tear down and rebuild the context/solver (`self.reset()`) so a poisoned context can't leak into the next query, and return `Unknown`.
- Identity: `"z3native:"` + `Z3_get_full_version()` string. This feeds the query-cache key (Task 8).

- [ ] **Step 1: Dependency + CI plumbing**

Root `Cargo.toml` `[workspace.dependencies]`:

```toml
z3-sys = { version = "0.8", features = ["static-link-z3"] }
```

`crates/goverify-solver/Cargo.toml` `[dependencies]`: add `z3-sys.workspace = true`.

`.github/workflows/ci.yml`, checks job:

```yaml
    # z3-sys static-link compiles Z3 from source on a cold cache (~20 min);
    # warm runs stay well under the spec's 10-minute budget via rust-cache.
    timeout-minutes: 30
```

Run `mise x -- cargo build -p goverify-solver` now (the slow one-time Z3 build; subsequent builds are incremental).

- [ ] **Step 2: Write failing tests** (in `z3native.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::SatResult;

    fn solver() -> Z3Native {
        Z3Native::new(SolverLimits::default())
    }

    #[test]
    fn trivial_sat_with_model() {
        let out = solver().solve_text(
            "(set-logic QF_BV)\n\
             (declare-const x (_ BitVec 8))\n\
             (assert (bvult x (_ bv5 8)))\n\
             (check-sat)\n",
        );
        assert_eq!(out.result, SatResult::Sat);
        assert!(out.model.is_some(), "sat must carry a model");
    }

    #[test]
    fn trivial_unsat() {
        let out = solver().solve_text(
            "(set-logic QF_BV)\n\
             (declare-const b Bool)\n\
             (assert (and b (not b)))\n\
             (check-sat)\n",
        );
        assert_eq!(out.result, SatResult::Unsat);
        assert!(out.model.is_none(), "unsat carries no model");
    }

    #[test]
    fn ptr_datatype_queries_work() {
        let out = solver().solve_text(
            "(set-logic ALL)\n\
             (declare-datatypes ((Ptr 0)) (((ptr-nil) (ptr-addr (ptr-addr-val (_ BitVec 64))))))\n\
             (declare-const p0 Ptr)\n\
             (assert ((_ is ptr-nil) p0))\n\
             (check-sat)\n",
        );
        assert_eq!(out.result, SatResult::Sat, "unconstrained ptr can be nil");
    }

    #[test]
    fn malformed_input_is_unknown_not_panic() {
        for bad in ["", "garbage", "(assert (undeclared))\n(check-sat)\n", "(check-sat)\n"] {
            let out = solver().solve_text(bad);
            assert_eq!(out.result, SatResult::Unknown, "{bad:?} => Unknown");
        }
    }

    #[test]
    fn solver_survives_a_bad_query() {
        let mut s = solver();
        assert_eq!(s.solve_text("garbage").result, SatResult::Unknown);
        // Context must have been rebuilt: a good query still works.
        let out = s.solve_text(
            "(set-logic QF_BV)\n(declare-const b Bool)\n(assert b)\n(check-sat)\n",
        );
        assert_eq!(out.result, SatResult::Sat);
    }

    #[test]
    fn identity_names_z3_and_version() {
        let id = solver().identity();
        assert!(id.starts_with("z3native:"), "{id}");
        assert!(id.len() > "z3native:".len(), "{id}");
    }

    #[test]
    fn queries_are_independent_across_calls() {
        let mut s = solver();
        let unsat = "(set-logic QF_BV)\n(declare-const b Bool)\n\
                     (assert (and b (not b)))\n(check-sat)\n";
        let sat = "(set-logic QF_BV)\n(declare-const b Bool)\n(assert b)\n(check-sat)\n";
        assert_eq!(s.solve_text(unsat).result, SatResult::Unsat);
        assert_eq!(s.solve_text(sat).result, SatResult::Sat, "no state bleed (push/pop)");
    }
}
```

- [ ] **Step 3: Run to verify failure**

Run: `mise x -- cargo test -p goverify-solver z3native`
Expected: FAIL to compile.

- [ ] **Step 4: Implement** — `lib.rs` additions first:

```rust
mod z3native;

pub use z3native::Z3Native;

/// Per-query resource caps (parent spec §8: default 100 ms).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SolverLimits {
    pub timeout_ms: u32,
    pub mem_mb: u32,
}

impl Default for SolverLimits {
    fn default() -> Self {
        SolverLimits { timeout_ms: 100, mem_mb: 1024 }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryOutcome {
    pub result: SatResult,
    /// Model text, Sat only. Display-only — never parsed for decisions.
    pub model: Option<String>,
}

/// A backend that consumes canonical SMT-LIB2 bytes (single-lowering
/// rule, phase-3 spec §4). `identity()` feeds the query-cache key.
pub trait TextSolver: Send {
    fn identity(&self) -> String;
    fn limits(&self) -> SolverLimits;
    fn solve_text(&mut self, canonical: &str) -> QueryOutcome;
}
```

`crates/goverify-solver/src/z3native.rs`:

```rust
//! Z3 via the C API, statically linked (parent spec §13). Consumes
//! canonical SMT-LIB2 text — Z3_parse_smtlib2_string, never AST-building
//! from terms (single-lowering rule). All abnormal paths => Unknown.

use std::ffi::{CStr, CString};
use std::ptr;

use z3_sys::*;

use crate::{QueryOutcome, SatResult, SolverLimits, TextSolver};

pub struct Z3Native {
    ctx: Z3_context,
    solver: Z3_solver,
    limits: SolverLimits,
    identity: String,
}

// One instance per rayon worker; moved, never shared (`Send`, not Sync).
unsafe impl Send for Z3Native {}

/// Replaces Z3's default abort-on-error handler; errors are checked via
/// Z3_get_error_code after each fallible call.
extern "C" fn quiet_error_handler(_ctx: Z3_context, _e: Z3_error_code) {}

fn make_ctx_solver(limits: SolverLimits) -> (Z3_context, Z3_solver) {
    unsafe {
        let cfg = Z3_mk_config();
        let ctx = Z3_mk_context(cfg);
        Z3_del_config(cfg);
        Z3_set_error_handler(ctx, Some(quiet_error_handler));
        let solver = Z3_mk_solver(ctx);
        Z3_solver_inc_ref(ctx, solver);
        let params = Z3_mk_params(ctx);
        Z3_params_inc_ref(ctx, params);
        let timeout = CString::new("timeout").expect("static");
        Z3_params_set_uint(
            ctx,
            params,
            Z3_mk_string_symbol(ctx, timeout.as_ptr()),
            limits.timeout_ms,
        );
        let max_memory = CString::new("max_memory").expect("static");
        Z3_params_set_uint(
            ctx,
            params,
            Z3_mk_string_symbol(ctx, max_memory.as_ptr()),
            limits.mem_mb,
        );
        Z3_solver_set_params(ctx, solver, params);
        Z3_params_dec_ref(ctx, params);
        (ctx, solver)
    }
}

impl Z3Native {
    pub fn new(limits: SolverLimits) -> Z3Native {
        let (ctx, solver) = make_ctx_solver(limits);
        let identity = unsafe {
            let v = Z3_get_full_version();
            format!("z3native:{}", CStr::from_ptr(v).to_string_lossy())
        };
        Z3Native { ctx, solver, limits, identity }
    }

    /// Tear down and rebuild after any Z3 error: a poisoned context must
    /// not leak into the next query (parent §11 worker-restart semantics).
    fn reset(&mut self) {
        unsafe {
            Z3_solver_dec_ref(self.ctx, self.solver);
            Z3_del_context(self.ctx);
        }
        let (ctx, solver) = make_ctx_solver(self.limits);
        self.ctx = ctx;
        self.solver = solver;
    }

    fn ok(&self) -> bool {
        unsafe { Z3_get_error_code(self.ctx) == ErrorCode::OK }
    }
}

impl Drop for Z3Native {
    fn drop(&mut self) {
        unsafe {
            Z3_solver_dec_ref(self.ctx, self.solver);
            Z3_del_context(self.ctx);
        }
    }
}

const UNKNOWN: QueryOutcome = QueryOutcome { result: SatResult::Unknown, model: None };

impl TextSolver for Z3Native {
    fn identity(&self) -> String {
        self.identity.clone()
    }

    fn limits(&self) -> SolverLimits {
        self.limits
    }

    fn solve_text(&mut self, canonical: &str) -> QueryOutcome {
        // The canonical artifact always ends "(check-sat)\n"; Z3's parser
        // only handles declarations/assertions — check-sat is ours.
        let Some(body) = canonical.strip_suffix("(check-sat)\n") else {
            return UNKNOWN;
        };
        let Ok(cbody) = CString::new(body) else {
            return UNKNOWN;
        };
        unsafe {
            Z3_solver_push(self.ctx, self.solver);
            let vec = Z3_parse_smtlib2_string(
                self.ctx,
                cbody.as_ptr(),
                0,
                ptr::null(),
                ptr::null(),
                0,
                ptr::null(),
                ptr::null(),
            );
            if !self.ok() {
                self.reset();
                return UNKNOWN;
            }
            for i in 0..Z3_ast_vector_size(self.ctx, vec) {
                Z3_solver_assert(self.ctx, self.solver, Z3_ast_vector_get(self.ctx, vec, i));
            }
            if !self.ok() {
                self.reset();
                return UNKNOWN;
            }
            let r = Z3_solver_check(self.ctx, self.solver);
            let outcome = match r {
                Z3_L_TRUE => {
                    let model = Z3_solver_get_model(self.ctx, self.solver);
                    let text = if model.is_null() || !self.ok() {
                        None
                    } else {
                        Z3_model_inc_ref(self.ctx, model);
                        let s = CStr::from_ptr(Z3_model_to_string(self.ctx, model))
                            .to_string_lossy()
                            .into_owned();
                        Z3_model_dec_ref(self.ctx, model);
                        Some(s)
                    };
                    QueryOutcome { result: SatResult::Sat, model: text }
                }
                Z3_L_FALSE => QueryOutcome { result: SatResult::Unsat, model: None },
                _ => UNKNOWN,
            };
            if !self.ok() {
                self.reset();
                return UNKNOWN;
            }
            Z3_solver_pop(self.ctx, self.solver, 1);
            outcome
        }
    }
}
```

API-name check: z3-sys 0.8's error-code type is `ErrorCode::OK` and lbool constants are `Z3_L_TRUE`/`Z3_L_FALSE` — if the pinned version's names differ (e.g. `Z3_error_code_Z3_OK`), adapt to what `cargo doc -p z3-sys` shows; the structure above is what matters.

- [ ] **Step 5: Run tests**

Run: `mise x -- cargo test -p goverify-solver z3native`
Expected: PASS (7 tests). These are REAL Z3 runs — first execution pays the build.

- [ ] **Step 6: Lint + commit**

```bash
mise run lint
git add -A
git commit -m "solver: Z3Native backend — static bundled z3-sys, canonical-text consumption, error-reset, limits"
```

### Task 7: SmtLib2Process backend + differential harness

**Files:**
- Create: `crates/goverify-solver/src/process.rs`
- Modify: `crates/goverify-solver/src/lib.rs` (`mod process; pub use process::SmtLib2Process;`)
- Modify: `crates/goverify-solver/Cargo.toml` (add `tempfile.workspace = true` to `[dependencies]` — runtime dep, sanctioned by phase-3 spec §1/global constraints)
- Create: `crates/goverify-solver/tests/differential.rs`
- Modify: `mise.toml` (pin z3 CLI), `.github/workflows/nightly.yml` (differential sweep step)

**Interfaces:**
- Consumes: `TextSolver`, `QueryOutcome`, `SolverLimits` (Task 6); `parse_response`, `parse_sexpr` (Task 4); `Query`/`Term` builders (Tasks 2–3) in the harness.
- Produces: `SmtLib2Process::new(cmd: &str, limits: SolverLimits) -> SmtLib2Process` implementing `TextSolver`. Task 12's `--solver-cmd` flag constructs it.

**Design notes:**
- One-shot per query: write canonical text + `"(get-model)\n"` appended to a temp file, run `<cmd> <file>`, read stdout. First line → `parse_response`; if `sat`, slurp one s-expr from the remainder as the model text (best-effort: failure ⇒ `model: None`, result stays Sat). z3 prints `(error "model is not available")` after unsat's get-model — the first-line parse already decided, so that noise is ignored by construction.
- Timeout: `child.try_wait()` poll loop with 10 ms sleeps until `timeout_ms + 250` grace elapses, then `child.kill()` ⇒ `Unknown`. (Also pass the solver its own soft limit when the command is z3: do NOT special-case — the grace-window kill is the universal mechanism; solver-side limits arrive via the file's own content in a later phase if ever needed.)
- Identity: run `<cmd> --version` once in `new()`, take the first line: `"process:<cmd-basename>:<version-line>"`; on failure `"process:<cmd-basename>:unknown-version"`. (A version-blind identity would poison the query cache across solver upgrades — parent spec §9.3.)
- mise pin: match the bundled Z3 as closely as the release tags allow. `z3-sys 0.8` bundles Z3 4.12.x, so pin the CLI at `4.12.1` (or the nearest tag mise's backend resolves; verify with `mise x -- z3 --version` and record the actual pin in the commit message).

- [ ] **Step 1: mise z3 pin**

Add to `mise.toml` `[tools]`:

```toml
"ubi:Z3Prover/z3" = { version = "4.12.1", exe = "z3" }
```

Run: `mise install && mise x -- z3 --version`
Expected: `Z3 version 4.12.1 - 64 bit`. If the ubi backend can't resolve the asset naming for this tag, try `4.13.4`; whichever resolves, note it — the differential harness only compares sat/unsat verdicts, so an exact bundled-version match is preferred but not required.

- [ ] **Step 2: Write failing tests** (in `process.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SatResult, SolverLimits};

    fn z3() -> SmtLib2Process {
        SmtLib2Process::new("z3", SolverLimits::default())
    }

    #[test]
    fn sat_unsat_and_model() {
        let sat = z3().solve_text(
            "(set-logic QF_BV)\n(declare-const b Bool)\n(assert b)\n(check-sat)\n",
        );
        assert_eq!(sat.result, SatResult::Sat);
        assert!(sat.model.is_some());
        let unsat = z3().solve_text(
            "(set-logic QF_BV)\n(declare-const b Bool)\n(assert (and b (not b)))\n(check-sat)\n",
        );
        assert_eq!(unsat.result, SatResult::Unsat);
        assert!(unsat.model.is_none());
    }

    #[test]
    fn garbage_and_missing_binary_are_unknown() {
        assert_eq!(z3().solve_text("garbage").result, SatResult::Unknown);
        let mut missing = SmtLib2Process::new("goverify-no-such-solver", SolverLimits::default());
        assert_eq!(missing.solve_text("(check-sat)\n").result, SatResult::Unknown);
    }

    #[test]
    fn identity_includes_version() {
        let id = z3().identity();
        assert!(id.starts_with("process:z3:"), "{id}");
        assert!(id.contains("4.1"), "version line captured: {id}");
    }
}
```

- [ ] **Step 3: Implement `process.rs`**

```rust
//! Portable text backend (parent spec §8): pipes canonical SMT-LIB2 to
//! any solver binary. Used by --solver-cmd and the differential harness.
//! Every failure — spawn, timeout, garbage output — is Unknown.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::reader::{parse_response, parse_sexpr};
use crate::{QueryOutcome, SatResult, SolverLimits, TextSolver};

pub struct SmtLib2Process {
    cmd: String,
    limits: SolverLimits,
    identity: String,
}

impl SmtLib2Process {
    pub fn new(cmd: &str, limits: SolverLimits) -> SmtLib2Process {
        let base = Path::new(cmd)
            .file_name()
            .map_or_else(|| cmd.to_string(), |b| b.to_string_lossy().into_owned());
        let version = Command::new(cmd)
            .arg("--version")
            .output()
            .ok()
            .and_then(|o| {
                String::from_utf8(o.stdout)
                    .ok()?
                    .lines()
                    .next()
                    .map(str::to_string)
            })
            .unwrap_or_else(|| "unknown-version".into());
        SmtLib2Process {
            cmd: cmd.to_string(),
            limits,
            identity: format!("process:{base}:{version}"),
        }
    }

    fn run(&self, canonical: &str) -> Option<String> {
        let mut file = tempfile::Builder::new()
            .prefix("goverify-query-")
            .suffix(".smt2")
            .tempfile()
            .ok()?;
        file.write_all(canonical.as_bytes()).ok()?;
        file.write_all(b"(get-model)\n").ok()?;
        file.flush().ok()?;
        let mut child = Command::new(&self.cmd)
            .arg(file.path())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let deadline =
            Instant::now() + Duration::from_millis(u64::from(self.limits.timeout_ms) + 250);
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
            }
        }
        let mut out = String::new();
        std::io::Read::read_to_string(&mut child.stdout.take()?, &mut out).ok()?;
        Some(out)
    }
}

impl TextSolver for SmtLib2Process {
    fn identity(&self) -> String {
        self.identity.clone()
    }

    fn limits(&self) -> SolverLimits {
        self.limits
    }

    fn solve_text(&mut self, canonical: &str) -> QueryOutcome {
        let Some(out) = self.run(canonical) else {
            return QueryOutcome { result: SatResult::Unknown, model: None };
        };
        let mut lines = out.splitn(2, '\n');
        let result = parse_response(lines.next().unwrap_or(""));
        let model = if result == SatResult::Sat {
            lines
                .next()
                .and_then(|rest| parse_sexpr(rest).ok())
                .map(|(sexpr, _)| format!("{sexpr:?}"))
        } else {
            None
        };
        QueryOutcome { result, model }
    }
}
```

Note on the model: `format!("{sexpr:?}")` is a placeholder-quality rendering — replace it with the raw consumed slice instead: capture `rest[..n]` from `parse_sexpr`'s consumed-byte count and store that trimmed string. Model text is display-only; both backends' models are never compared or parsed for decisions.

- [ ] **Step 4: Run tests**

Run: `mise x -- cargo test -p goverify-solver process`
Expected: PASS (needs the mise z3 on PATH — run via `mise x`).

- [ ] **Step 5: Differential harness** — create `crates/goverify-solver/tests/differential.rs`:

```rust
//! Differential solver tests (parent spec §12.8): Z3Native and
//! SmtLib2Process(z3) must agree on verdicts. Blocking tier runs the
//! fixed set below; the nightly sweep adds proptest-generated queries
//! (`--ignored`).

use goverify_solver::{
    BvCmpOp, Logic, Query, SatResult, SmtLib2Process, SolverLimits, Sort, TextSolver, Z3Native,
    ptr_is_nil, ptr_nil, ptr_sort,
};
// NOTE: `Term` is exported as `AstTerm` until Task 8 removes the alias —
// if this task lands before Task 8, import `AstTerm as Term`.
use goverify_solver::AstTerm as Term;

fn fixed_queries() -> Vec<(&'static str, Query)> {
    let x = || Term::var("x", Sort::BitVec(8));
    let b = || Term::var("b", Sort::Bool);
    let p = || Term::var("p0", ptr_sort());
    vec![
        ("bool-sat", Query::for_asserts(Logic::QfBv, vec![b()])),
        (
            "bool-unsat",
            Query::for_asserts(
                Logic::QfBv,
                vec![Term::and(vec![b(), Term::not(b()).unwrap()]).unwrap()],
            ),
        ),
        (
            "bv-sat",
            Query::for_asserts(
                Logic::QfBv,
                vec![Term::bv_cmp(BvCmpOp::Ult, x(), Term::bv_lit(8, 5)).unwrap()],
            ),
        ),
        (
            "bv-unsat",
            Query::for_asserts(
                Logic::QfBv,
                vec![Term::bv_cmp(BvCmpOp::Ult, x(), Term::bv_lit(8, 0)).unwrap()],
            ),
        ),
        (
            "ptr-nil-sat",
            Query::for_asserts(Logic::All, vec![ptr_is_nil(p()).unwrap()]),
        ),
        (
            "ptr-nil-unsat",
            Query::for_asserts(
                Logic::All,
                vec![
                    Term::eq(p(), ptr_nil()).unwrap(),
                    Term::not(ptr_is_nil(p()).unwrap()).unwrap(),
                ],
            ),
        ),
        (
            "array-sat",
            Query::for_asserts(Logic::QfAbv, vec![{
                let m = Term::var(
                    "m",
                    Sort::Array(Box::new(Sort::BitVec(8)), Box::new(Sort::Bool)),
                );
                Term::select(m, Term::bv_lit(8, 3)).unwrap()
            }]),
        ),
    ]
}

/// Generous limits: differential queries are trivial; a CI-box hiccup
/// must not turn a verdict into Unknown and mask a real divergence.
fn limits() -> SolverLimits {
    SolverLimits { timeout_ms: 5_000, mem_mb: 1024 }
}

#[test]
fn backends_agree_on_fixed_queries() {
    let mut native = Z3Native::new(limits());
    let mut process = SmtLib2Process::new("z3", limits());
    for (name, q) in fixed_queries() {
        let text = q.canonical_text();
        let a = native.solve_text(&text);
        let b = process.solve_text(&text);
        assert_ne!(a.result, SatResult::Unknown, "{name}: native Unknown on a trivial query");
        assert_eq!(a.result, b.result, "{name}: backends disagree");
    }
}

/// Nightly sweep: random queries from the reader's term generator shape.
/// Ignored in the blocking tier (parent spec §12: full differential
/// sweep is nightly).
#[test]
#[ignore = "nightly differential sweep"]
fn backends_agree_on_random_queries() {
    use proptest::prelude::*;
    use proptest::strategy::ValueTree;
    use proptest::test_runner::TestRunner;

    let mut runner = TestRunner::deterministic(); // fixed seed: reproducible
    let mut native = Z3Native::new(limits());
    let mut process = SmtLib2Process::new("z3", limits());
    for i in 0..256 {
        let t = goverify_solver::testgen::arb_bool_term()
            .new_tree(&mut runner)
            .expect("gen")
            .current();
        let q = Query::for_asserts(Logic::All, vec![t]);
        let text = q.canonical_text();
        let a = native.solve_text(&text).result;
        let b = process.solve_text(&text).result;
        assert_eq!(a, b, "case {i} diverged:\n{text}");
    }
}
```

This needs the term generator shared: move `arb_term` from Task 4's `reader.rs` props into a `#[doc(hidden)] pub mod testgen` in `goverify-solver` (feature-gate-free; add `proptest` as a **regular** dependency? NO — keep it dev-only: instead put `testgen` behind `#[cfg(any(test, feature = "testgen"))]`, add a `testgen = ["dep:proptest"]` feature, move `proptest` to `[dependencies]` as `optional = true`, and have the differential test enable it via `[dev-dependencies] goverify-solver = { path = "..", features = ["testgen"] }`... that self-dependency trick is NOT allowed in the same crate). Resolution — simplest correct structure: `testgen` is a `pub mod` compiled only under the `testgen` cargo feature (`proptest` optional dep), and `crates/goverify-solver/Cargo.toml` gets:

```toml
[features]
testgen = ["dep:proptest"]

[dependencies]
proptest = { workspace = true, optional = true }
```

Integration tests within the same crate DO see feature-enabled items when the feature is on; enable it for tests by adding `required-features = ["testgen"]` to a `[[test]] name = "differential"` block. The `mise run test` workspace invocation must then include `--all-features` — update `mise.toml`'s test task to `cargo test --workspace --all-features`. `arb_bool_term()` wraps Task 4's `arb_term()` with the bool-coercion (`eq(t,t)` when non-bool). Export both from `testgen`; `reader.rs`'s props import from it too (single source).

- [ ] **Step 6: Nightly sweep wiring**

`.github/workflows/nightly.yml`, new job after `fuzz`:

```yaml
  differential:
    runs-on: ubuntu-latest
    # Cold z3-sys build dominates (~20 min) + 256 dual-solver queries.
    timeout-minutes: 45
    steps:
      - uses: actions/checkout@34e114876b0b11c390a56381ad16ebd13914f8d5 # v4
      - uses: jdx/mise-action@c37c93293d6b742fc901e1406b8f764f6fb19dac # v2
      - uses: Swatinem/rust-cache@e18b497796c12c097a38f9edb9d0641fb99eee32 # v2
      - run: mise x -- cargo test -p goverify-solver --all-features --test differential -- --ignored
```

- [ ] **Step 7: Verification + commit**

Run: `mise run lint && mise run test` (blocking differential included), then once: `mise x -- cargo test -p goverify-solver --all-features --test differential -- --ignored` (the sweep; ~minutes).
Expected: all green.

```bash
git add -A
git commit -m "solver: SmtLib2Process backend, pinned z3 CLI, differential harness (blocking set + nightly sweep)"
```

### Task 8: discharge_query, Solver-trait swap, StubSolver rebuild

**Files:**
- Create: `crates/goverify-solver/src/discharge.rs`
- Modify: `crates/goverify-solver/src/lib.rs` (DELETE the phase-2 opaque `Decl(String)`/`Term(String)`/`Model`/`StubSolver`/`Solver`; re-export the new versions; remove the `AstTerm` alias — `Term` is now THE term)
- Modify: `crates/goverify-solver/Cargo.toml` (add `goverify-cache.workspace = true`, `blake3.workspace = true`)
- Modify: `crates/goverify-analysis/src/engine.rs` (imports only: `StubSolver` still exists with the same name; `discharge()`'s `solver.check_sat_assuming(&[])` call still compiles against the new trait — verify, don't rewrite; Task 9 rewrites the clause types)

**Interfaces:**
- Consumes: `Query`/`canonical_text` (Task 3), `TextSolver`/`QueryOutcome`/`SolverLimits` (Task 6), `QueryCache`/`query_key`/`CachedOutcome` (Task 5).
- Produces: `discharge_query(q, backend, cache, emit_dir) -> QueryOutcome`; new `Decl { Const(String, Sort), Datatype(DatatypeDecl) }`, `Model(pub String)`, `trait Solver` (same five methods, typed), `TermSolver::new(backend, logic)`, `StubSolver` implementing BOTH `Solver` and `TextSolver`. Tasks 11–12 call `discharge_query`; the engine keeps compiling throughout.

- [ ] **Step 1: Write failing tests** (in `discharge.rs`)

```rust
#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;
    use crate::printer::{Logic, Query};
    use crate::term::Term;
    use crate::{QueryOutcome, SatResult, SolverLimits, TextSolver};

    /// Scripted backend: answers Sat, counts calls.
    struct Counting(&'static AtomicU32);

    impl TextSolver for Counting {
        fn identity(&self) -> String {
            "counting:1".into()
        }
        fn limits(&self) -> SolverLimits {
            SolverLimits::default()
        }
        fn solve_text(&mut self, _c: &str) -> QueryOutcome {
            self.0.fetch_add(1, Ordering::SeqCst);
            QueryOutcome { result: SatResult::Sat, model: Some("(model)".into()) }
        }
    }

    fn q(b: bool) -> Query {
        Query::for_asserts(Logic::QfBv, vec![Term::bool_lit(b)])
    }

    #[test]
    fn cache_hit_skips_backend() {
        static CALLS: AtomicU32 = AtomicU32::new(0);
        let dir = tempfile::tempdir().unwrap();
        let cache = goverify_cache::QueryCache::open(dir.path().to_path_buf());
        let mut backend = Counting(&CALLS);
        let a = discharge_query(&q(true), &mut backend, Some(&cache), None);
        let b = discharge_query(&q(true), &mut backend, Some(&cache), None);
        assert_eq!(CALLS.load(Ordering::SeqCst), 1, "second call must be a cache hit");
        assert_eq!(a, b, "hit reproduces outcome incl. model");
        assert_eq!(a.result, SatResult::Sat);
    }

    #[test]
    fn distinct_queries_distinct_entries() {
        static CALLS: AtomicU32 = AtomicU32::new(0);
        let dir = tempfile::tempdir().unwrap();
        let cache = goverify_cache::QueryCache::open(dir.path().to_path_buf());
        let mut backend = Counting(&CALLS);
        discharge_query(&q(true), &mut backend, Some(&cache), None);
        discharge_query(&q(false), &mut backend, Some(&cache), None);
        assert_eq!(CALLS.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn emit_smt_writes_canonical_bytes() {
        static CALLS: AtomicU32 = AtomicU32::new(0);
        let dir = tempfile::tempdir().unwrap();
        let mut backend = Counting(&CALLS);
        let query = q(true);
        discharge_query(&query, &mut backend, None, Some(dir.path()));
        let files: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(files.len(), 1);
        let content = std::fs::read_to_string(files[0].as_ref().unwrap().path()).unwrap();
        assert_eq!(content, query.canonical_text(), "artifact == solved bytes");
    }

    #[test]
    fn no_cache_no_emit_still_solves() {
        static CALLS: AtomicU32 = AtomicU32::new(0);
        let out = discharge_query(&q(true), &mut Counting(&CALLS), None, None);
        assert_eq!(out.result, SatResult::Sat);
    }
}
```

Add `tempfile` to solver's dev-deps if not already there (it's a runtime dep since Task 7 — dev use is covered).

- [ ] **Step 2: Run to verify failure**

Run: `mise x -- cargo test -p goverify-solver discharge`
Expected: FAIL to compile.

- [ ] **Step 3: Implement `discharge.rs`**

```rust
//! The single solver-layer entry point (phase-3 spec §8): renders the
//! canonical text exactly once, keys the query cache with it, and on a
//! miss drives the backend with those same bytes.

use std::path::Path;

use goverify_cache::{CachedOutcome, QueryCache, QueryKeyParts, query_key};

use crate::printer::Query;
use crate::{QueryOutcome, SatResult, TextSolver};

pub fn discharge_query(
    q: &Query,
    backend: &mut dyn TextSolver,
    cache: Option<&QueryCache>,
    emit_dir: Option<&Path>,
) -> QueryOutcome {
    let text = q.canonical_text();
    let limits = backend.limits();
    if let Some(dir) = emit_dir {
        // Deterministic filename = content hash; best-effort (diagnostic
        // surface only, never affects verdicts).
        let name = format!("{}.smt2", blake3::hash(text.as_bytes()).to_hex());
        let _ = std::fs::create_dir_all(dir);
        let _ = std::fs::write(dir.join(name), &text);
    }
    let key = cache.map(|_| {
        query_key(&QueryKeyParts {
            canonical: &text,
            solver_identity: &backend.identity(),
            timeout_ms: limits.timeout_ms,
            mem_mb: limits.mem_mb,
        })
    });
    if let (Some(c), Some(k)) = (cache, key.as_ref())
        && let Some(hit) = c.get(k)
    {
        return match hit {
            CachedOutcome::Sat { model } => QueryOutcome { result: SatResult::Sat, model },
            CachedOutcome::Unsat => QueryOutcome { result: SatResult::Unsat, model: None },
            CachedOutcome::Unknown => QueryOutcome { result: SatResult::Unknown, model: None },
        };
    }
    let out = backend.solve_text(&text);
    if let (Some(c), Some(k)) = (cache, key.as_ref()) {
        let v = match &out {
            QueryOutcome { result: SatResult::Sat, model } => {
                CachedOutcome::Sat { model: model.clone() }
            }
            QueryOutcome { result: SatResult::Unsat, .. } => CachedOutcome::Unsat,
            QueryOutcome { result: SatResult::Unknown, .. } => CachedOutcome::Unknown,
        };
        let _ = c.put(k, &v); // cache write failure degrades to slower, never wrong
    }
    out
}
```

- [ ] **Step 4: Swap the `Solver` trait innards + rebuild StubSolver** (in `lib.rs`)

Delete the phase-2 `pub struct Decl(pub String);`, `pub struct Term(pub String);`, `pub struct Model;`, old `Solver` trait + `StubSolver` + their test. Replace with:

```rust
pub use term::Term; // the alias AstTerm is gone; Term is the typed AST

/// A declaration for the incremental Solver interface (parent spec §8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decl {
    Const(String, Sort),
    Datatype(DatatypeDecl),
}

/// A satisfying model: the solver's textual rendering, display-only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Model(pub String);

pub trait Solver {
    fn declare(&mut self, decl: Decl);
    fn assert(&mut self, term: Term);
    fn check_sat_assuming(&mut self, assumptions: &[Term]) -> SatResult;
    fn model(&self) -> Option<Model>;
    fn push(&mut self);
    fn pop(&mut self);
}

/// Answers Unknown to everything (⇒ no report). Implements both solver
/// interfaces so tests and the engine default need no backend.
pub struct StubSolver;

impl Solver for StubSolver {
    fn declare(&mut self, _decl: Decl) {}
    fn assert(&mut self, _term: Term) {}
    fn check_sat_assuming(&mut self, _assumptions: &[Term]) -> SatResult {
        SatResult::Unknown
    }
    fn model(&self) -> Option<Model> {
        None
    }
    fn push(&mut self) {}
    fn pop(&mut self) {}
}

impl TextSolver for StubSolver {
    fn identity(&self) -> String {
        "stub".into()
    }
    fn limits(&self) -> SolverLimits {
        SolverLimits::default()
    }
    fn solve_text(&mut self, _canonical: &str) -> QueryOutcome {
        QueryOutcome { result: SatResult::Unknown, model: None }
    }
}

/// Adapter: the incremental Solver interface over any TextSolver. Each
/// check renders ONE canonical query from the accumulated frames + the
/// assumptions (single-lowering rule holds: rendering is Query's).
pub struct TermSolver {
    backend: Box<dyn TextSolver>,
    logic: Logic,
    decls: Vec<Decl>,
    asserts: Vec<Term>,
    frames: Vec<(usize, usize)>,
    last_model: Option<Model>,
}

impl TermSolver {
    pub fn new(backend: Box<dyn TextSolver>, logic: Logic) -> TermSolver {
        TermSolver {
            backend,
            logic,
            decls: Vec::new(),
            asserts: Vec::new(),
            frames: Vec::new(),
            last_model: None,
        }
    }

    fn to_query(&self, assumptions: &[Term]) -> Query {
        let mut datatypes = Vec::new();
        let mut consts = Vec::new();
        for d in &self.decls {
            match d {
                Decl::Const(n, s) => consts.push((n.clone(), s.clone())),
                Decl::Datatype(dt) => datatypes.push(dt.clone()),
            }
        }
        let mut asserts = self.asserts.clone();
        asserts.extend_from_slice(assumptions);
        Query { logic: self.logic, datatypes, consts, asserts }
    }
}

impl Solver for TermSolver {
    fn declare(&mut self, decl: Decl) {
        self.decls.push(decl);
    }
    fn assert(&mut self, term: Term) {
        self.asserts.push(term);
    }
    fn check_sat_assuming(&mut self, assumptions: &[Term]) -> SatResult {
        let out = self.backend.solve_text(&self.to_query(assumptions).canonical_text());
        self.last_model = match (&out.result, out.model) {
            (SatResult::Sat, Some(m)) => Some(Model(m)),
            _ => None,
        };
        out.result
    }
    fn model(&self) -> Option<Model> {
        self.last_model.clone()
    }
    fn push(&mut self) {
        self.frames.push((self.decls.len(), self.asserts.len()));
    }
    fn pop(&mut self) {
        if let Some((d, a)) = self.frames.pop() {
            self.decls.truncate(d);
            self.asserts.truncate(a);
        }
    }
}
```

Add a `TermSolver` unit test in `lib.rs`'s test module: push/assert `bool_lit(false)`/check (backend `Z3Native` if cheap, else a scripted `TextSolver`) → `Unsat`; pop; check with no asserts → `Sat`; plus the retained `stub_solver_always_answers_unknown` test updated to the typed API:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    struct Scripted(Vec<SatResult>);
    impl TextSolver for Scripted {
        fn identity(&self) -> String { "scripted".into() }
        fn limits(&self) -> SolverLimits { SolverLimits::default() }
        fn solve_text(&mut self, _c: &str) -> QueryOutcome {
            QueryOutcome { result: self.0.remove(0), model: None }
        }
    }

    #[test]
    fn stub_solver_always_answers_unknown() {
        let mut s = StubSolver;
        s.declare(Decl::Const("x".into(), Sort::Bool));
        s.push();
        s.assert(Term::var("x", Sort::Bool));
        assert_eq!(s.check_sat_assuming(&[]), SatResult::Unknown);
        assert!(s.model().is_none(), "Unknown must never expose a model");
        s.pop();
    }

    #[test]
    fn term_solver_frames_pop_asserts() {
        let mut s = TermSolver::new(Box::new(Scripted(vec![SatResult::Unsat, SatResult::Sat])), Logic::QfBv);
        s.push();
        s.assert(Term::bool_lit(false));
        assert_eq!(s.check_sat_assuming(&[]), SatResult::Unsat);
        s.pop();
        assert_eq!(s.check_sat_assuming(&[]), SatResult::Sat, "popped assert must be gone");
    }
}
```

- [ ] **Step 5: Fix the engine's imports**

`goverify-analysis/src/engine.rs` still does `use goverify_solver::{SatResult, Solver, StubSolver};` and calls `solver.check_sat_assuming(&[])` — all three names survive with identical semantics, so the crate should compile untouched. Also update the differential test's `use goverify_solver::AstTerm as Term;` → `use goverify_solver::Term;`. Run the workspace build to prove it: `mise x -- cargo build --workspace`.

- [ ] **Step 6: Full verification + commit**

Run: `mise run lint && mise run test`
Expected: green (workspace-wide — the engine, CLI, and corpus tests all still pass against the typed solver crate).

```bash
git add -A
git commit -m "solver: discharge_query (render-once + cache + emit), typed Solver trait, TermSolver adapter"
```

### Task 9: Formula swap — real terms in summaries

**Files:**
- Modify: `crates/goverify-analysis/src/summary.rs` (replace `PlaceholderFormula`)
- Modify: `crates/goverify-analysis/src/engine.rs` (`discharge` + call site adapt)
- Modify: `crates/goverify-analysis/src/lib.rs` (re-export updates)

**Interfaces:**
- Consumes: `Term`, `Sort`, `SortError`, `ptr_sort` (Tasks 2/8).
- Produces: `Formula { term }`, `Clause { tag, formula }`, `IfaceVar`, `iface_var_name`, `BoundClause { tag, violation: Option<Term> }`, `instantiate_requires(callee, arg_terms: &[Option<Term>]) -> Vec<BoundClause>`. Task 11's tracer builds `Clause`s and consumes `BoundClause.violation`; includes the folded fast-follow item **T12** (Result→None… now Result→no-violation).

- [ ] **Step 1: Rewrite `summary.rs`'s formula half** (tests below drive it; `Summary`, `Provenance`, `havoc()` are unchanged)

```rust
//! Function summaries (parent spec §5), phase-3 form: clause formulas are
//! real terms over the function's symbolic interface. Free variables use
//! the fixed naming p<i> (params) / r<i> (results) — `iface_var_name` is
//! the single source of that convention.

use std::collections::BTreeMap;

use goverify_solver::Term;

use crate::effects::Effects;

/// A variable of the function's symbolic interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IfaceVar {
    Param(u32),
    Result(u32),
}

/// THE naming convention for interface variables in formulas. Checkers
/// (Task 11) must build vars with exactly these names.
pub fn iface_var_name(v: &IfaceVar) -> String {
    match v {
        IfaceVar::Param(i) => format!("p{i}"),
        IfaceVar::Result(i) => format!("r{i}"),
    }
}

/// A clause formula: a Bool-sorted term whose free variables are all
/// p<i>/r<i>-named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Formula {
    pub term: Term,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Clause {
    /// Which checker/fact this clause states, e.g. "nil-deref".
    pub tag: String,
    pub formula: Formula,
}
```

(`Summary` keeps `requires: Vec<Clause>, ensures: Vec<Clause>, effects, provenance` — only `Clause`'s shape changed.)

```rust
/// A callee requires-clause instantiated at a call site: `violation` is
/// ¬formula with p<i> := arg_terms[i]. None = some needed variable had no
/// caller term (unknown arg, Result var, sort mismatch, arity overflow) —
/// callers MUST treat None as "cannot evaluate; do not report".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundClause {
    pub tag: String,
    pub violation: Option<Term>,
}

pub fn instantiate_requires(callee: &Summary, arg_terms: &[Option<Term>]) -> Vec<BoundClause> {
    callee
        .requires
        .iter()
        .map(|c| BoundClause {
            tag: c.tag.clone(),
            violation: bind_violation(&c.formula, arg_terms),
        })
        .collect()
}

fn bind_violation(f: &Formula, arg_terms: &[Option<Term>]) -> Option<Term> {
    let mut map = BTreeMap::new();
    for (name, _sort) in f.term.free_vars() {
        // Only p<i> vars can be bound at a call site; anything else
        // (r<i>, or a checker bug) makes the clause unevaluable here.
        let idx: u32 = name.strip_prefix('p')?.parse().ok()?;
        let t = arg_terms.get(idx as usize)?.clone()?;
        map.insert(name, t);
    }
    let bound = f.term.substitute(&map).ok()?;
    Term::not(bound).ok()
}
```

- [ ] **Step 2: Rewrite the tests** (replacing the phase-2 `instantiate_*` tests; the havoc test stays):

```rust
#[cfg(test)]
mod tests {
    use goverify_solver::{Term, ptr_is_nil, ptr_nil, ptr_sort};

    use super::*;

    fn nonnil_clause(param: u32) -> Clause {
        let v = IfaceVar::Param(param);
        let p = Term::var(&iface_var_name(&v), ptr_sort());
        Clause {
            tag: "nil-deref".into(),
            formula: Formula { term: Term::not(ptr_is_nil(p).unwrap()).unwrap() },
        }
    }

    fn callee_with(requires: Vec<Clause>) -> Summary {
        Summary { requires, ..Summary::default() }
    }

    #[test]
    fn nil_arg_binds_to_violation_term() {
        let callee = callee_with(vec![nonnil_clause(0)]);
        let bound = instantiate_requires(&callee, &[Some(ptr_nil())]);
        assert_eq!(bound.len(), 1);
        let v = bound[0].violation.as_ref().expect("bindable");
        // violation = ¬¬(is-nil nil): no free vars left.
        assert!(v.free_vars().is_empty(), "fully ground violation");
    }

    #[test]
    fn unknown_arg_means_no_violation() {
        let callee = callee_with(vec![nonnil_clause(0)]);
        assert_eq!(
            instantiate_requires(&callee, &[None])[0].violation,
            None,
            "unknown arg: cannot evaluate; do not report"
        );
    }

    #[test]
    fn out_of_range_param_means_no_violation() {
        let callee = callee_with(vec![nonnil_clause(5)]);
        assert_eq!(instantiate_requires(&callee, &[])[0].violation, None);
    }

    /// Folded fast-follow T12: a Result-var clause can never be bound at
    /// a call site — violation must be None, not a bogus term.
    #[test]
    fn result_var_clause_means_no_violation() {
        let r = Term::var(&iface_var_name(&IfaceVar::Result(0)), ptr_sort());
        let callee = callee_with(vec![Clause {
            tag: "t".into(),
            formula: Formula { term: ptr_is_nil(r).unwrap() },
        }]);
        assert_eq!(
            instantiate_requires(&callee, &[Some(ptr_nil())])[0].violation,
            None
        );
    }

    #[test]
    fn sort_mismatched_arg_means_no_violation() {
        let callee = callee_with(vec![nonnil_clause(0)]);
        assert_eq!(
            instantiate_requires(&callee, &[Some(Term::bool_lit(true))])[0].violation,
            None,
            "substitute() sort check must degrade, not report"
        );
    }

    #[test]
    fn havoc_summary_has_no_requires() {
        // Missing info must never create false positives (parent spec §11).
        let h = Summary::havoc();
        assert!(h.requires.is_empty());
        assert_eq!(h.provenance, Provenance::Havoc);
        assert_eq!(h.effects, crate::effects::Effects::top());
    }
}
```

- [ ] **Step 3: Adapt `engine.rs`**

The phase-2 discharge path (`instantiate_requires(&callee_summary, args)` with `&[ValueId]`, `discharge(&obligations, solver)`) no longer type-checks. Replace `discharge` and the call site minimally — Task 12 replaces this whole block with the real findings pass; here it only needs to compile and preserve "stub ⇒ zero findings":

```rust
/// Discharge instantiated requires-clauses. Bug-finder semantics (parent
/// spec §8): only Sat reports; Unsat and Unknown (incl. timeout) are
/// silent. Unbindable clauses (violation: None) are silent by
/// construction.
pub fn discharge(obligations: &[BoundClause], solver: &mut dyn Solver) -> Vec<Finding> {
    obligations
        .iter()
        .filter_map(|o| {
            let v = o.violation.clone()?;
            solver.push();
            solver.assert(v);
            let r = solver.check_sat_assuming(&[]);
            solver.pop();
            (r == SatResult::Sat).then(|| Finding { tag: o.tag.clone() })
        })
        .collect()
}
```

and at the phase-2 call site in `analyze_function`, args no longer map to terms (that's the tracer's job, Task 11) — pass all-unknown:

```rust
                    let callee_summary = summary_of(*callee_id);
                    let arg_terms: Vec<Option<goverify_solver::Term>> =
                        args.iter().map(|_| None).collect();
                    let obligations = instantiate_requires(&callee_summary, &arg_terms);
                    let _findings = discharge(&obligations, solver);
```

Update `engine.rs`'s `discharge_with_stub_solver_reports_nothing` test to build a `BoundClause` with a real violation term (`Term::bool_lit(true)` is fine) and assert Stub ⇒ empty.

- [ ] **Step 4: Verification + commit**

Run: `mise run lint && mise run test`
Expected: green — engine corpus/golden output is UNCHANGED (requires/ensures counts stay 0 everywhere; only types changed).

```bash
git add -A
git commit -m "analysis: real term formulas replace PlaceholderFormula; instantiation binds violations (incl. folded T12 test)"
```

### Task 10: Effects redesign — location-precise lattice (final-review I2)

**Files:**
- Modify: `crates/goverify-analysis/src/effects.rs` (Loc/Root, map-shaped Effects, resolve_loc, rebase, DeferredUnlock, new collect)
- Modify: `crates/goverify-analysis/src/engine.rs` (collect call site: pass `effects_of` closure; `render_effects` for the new shape)
- Modify: `crates/goverify-analysis/src/lib.rs` (re-export `Loc`, `Root`)
- Modify: `testdata/goldens/conc.summary.txt` (REGENERATE)
- Modify: `crates/goverify-analysis/tests/engine_corpus.rs` (loc-aware assertions)

**Interfaces:**
- Consumes: `Function::value()`, `ValueKind`, `Op`, `lock_kind` (goverify-ir, unchanged).
- Produces: `Root { Param(u32), Global(String), Alloc(u32), Unknown }`, `Loc { root, path: Vec<u32> }`, `MAX_LOC_DEPTH: usize = 2`, `LockOp` extended with `DeferredUnlock | DeferredRUnlock`, `Effects { spawns, chan_ops: BTreeMap<Loc, BTreeSet<ChanOp>>, lock_ops: BTreeMap<Loc, BTreeSet<LockOp>> }`, `resolve_loc(f, v) -> Loc`, `rebase(callee_effects, caller_arg_loc) -> Effects`, `collect(p, id, effects_of: &dyn Fn(FuncId) -> Effects) -> Effects`. Phase-7 checkers are the eventual consumer; Task 12's dumps render it.

**Semantics (normative):**
- `resolve_loc` walks the value's def chain: `FieldAddr{base, field}` prepends `field` to the path and recurses on `base`; a `ValueKind::Param` value roots at `Param(position in f.params)`; `ValueKind::Global(name)` roots at `Global(name)`; an `Op::Alloc{dst}` def roots at `Alloc(dst.0)`; **everything else** (loads, phis, calls, opaque, missing defs) is `Loc::unknown()`. Path deeper than `MAX_LOC_DEPTH` ⇒ `Loc::unknown()` (spec §9: overflow degrades the root).
- `Effects::top()` = `spawns: Unbounded` + `Unknown → all ChanOps` + `Unknown → all LockOps` (the finite top).
- `join` = pointwise `BTreeSet` union under `BTreeMap` key merge; `spawns` max. Still commutative/idempotent/monotone — property-tested.
- `rebase(callee, caller_arg_loc)`: `Param(i)`-rooted entries re-root through `caller_arg_loc(i)` with paths concatenated caller-first (depth-capped ⇒ Unknown); `Global` passes through; callee-local `Alloc` and `Unknown` become `Unknown` (a callee's local lock has no caller-visible identity — phase 7 refines if needed).
- `collect` records own ops at `resolve_loc` of the op's subject (`mu` for locks, `chan` for send/recv/close/make-chan dst, per-arm chans for select), joins **static** callees via `rebase(effects_of(callee), resolve caller arg i)`, and joins invoke/dynamic/builtin **call-graph** callees via `rebase` with `|_| Loc::unknown()` — same set-level result as phase 2, now with identity where it exists. The `Go`/`Defer` intrinsic-callee handling (final-review C2 fix) is preserved: `defer mu.Unlock()` records `DeferredUnlock` (`defer (*sync.RWMutex).RUnlock()` ⇒ `DeferredRUnlock`) at the receiver argument's Loc; `go`/`defer close(ch)` records `Close` at the chan argument's Loc; a *plain* deferred non-lock static callee still contributes via the call-graph edge exactly as before.
- **Call-graph double-count guard:** static callees reached via `Op::Call` are ALSO edges in `graph.callees(f)`. Phase 2 joined per-edge; the new `collect` must NOT join the same callee twice with different arg mappings. Resolution: `collect` iterates **instructions** (Call/Go/Defer with `Callee::Static`) for arg-precise rebasing, and separately joins `effects_of(c)` under the all-Unknown mapping for every call-graph callee `c` that has NO static instruction site in `f` (invoke/dynamic-resolved edges). Compute the static-site set first, then diff.

- [ ] **Step 1: Rewrite `effects.rs` types + failing tests first.** Key test set (replaces/extends the phase-2 tests — the C2 regression trio is UPDATED, not deleted):

```rust
#[cfg(test)]
mod tests {
    use goverify_ir::Program;

    use super::*;
    use crate::testpkg::{
        block, call, defer_call, defer_close, func, go_call, go_close, instr, pkg,
    };

    // ... keep the existing builders/usage patterns; new assertions below.

    #[test]
    fn defer_lock_method_records_deferred_unlock() {
        let p = Program::from_packages(vec![pkg(
            "t",
            vec![func(
                "t.F",
                vec![block(
                    0,
                    vec![defer_call("(*sync.Mutex).Unlock"), instr("Return")],
                    vec![],
                )],
            )],
        )]);
        let e = collect(&p, p.lookup_func("t.F").unwrap(), &|_| Effects::empty());
        let ops: BTreeSet<LockOp> = e.lock_ops.values().flatten().copied().collect();
        assert!(
            ops.contains(&LockOp::DeferredUnlock),
            "defer mu.Unlock() must be DeferredUnlock, not Unlock: {e:?}"
        );
        assert!(!ops.contains(&LockOp::Unlock), "not double-recorded as plain Unlock");
    }

    #[test]
    fn lock_on_param_gets_param_rooted_loc() {
        use goverify_extract::gvir;
        use goverify_extract::gvir::instruction::Sem;

        use crate::testpkg::func_with_params;

        // t.F(p) { p.Lock() } — a Call with static_callee "(*sync.Mutex).Lock"
        // and operand 1 (the param value id) lowers to Op::Lock{mu: param}.
        let mut lock = instr("Call");
        lock.operands = vec![1];
        lock.sem = Some(Sem::Call(gvir::CallSem {
            static_callee: "(*sync.Mutex).Lock".into(),
            ..Default::default()
        }));
        let p = Program::from_packages(vec![pkg(
            "t",
            vec![func_with_params(
                "t.F",
                vec![gvir::Param { id: 1, name: "p".into(), r#type: 0 }],
                vec![block(0, vec![lock, instr("Return")], vec![])],
            )],
        )]);
        let e = collect(&p, p.lookup_func("t.F").unwrap(), &|_| Effects::empty());
        let want = Loc { root: Root::Param(0), path: vec![] };
        assert!(
            e.lock_ops.get(&want).is_some_and(|ops| ops.contains(&LockOp::Lock)),
            "Lock must be recorded at the param-rooted Loc: {e:?}"
        );
    }

    #[test]
    fn effects_top_is_unknown_bucket() {
        let t = Effects::top();
        assert_eq!(t.lock_ops.len(), 1);
        assert!(t.lock_ops.contains_key(&Loc::unknown()));
        assert_eq!(
            t.lock_ops[&Loc::unknown()].len(),
            6,
            "all six LockOps incl. deferred variants"
        );
    }

    #[test]
    fn rebase_reroots_param_entries_through_caller_args() {
        let mut callee = Effects::empty();
        callee
            .lock_ops
            .entry(Loc { root: Root::Param(0), path: vec![1] })
            .or_default()
            .insert(LockOp::Lock);
        let rebased = rebase(&callee, &|i| {
            assert_eq!(i, 0);
            Loc { root: Root::Param(3), path: vec![2] }
        });
        let want = Loc { root: Root::Param(3), path: vec![2, 1] };
        assert!(rebased.lock_ops.contains_key(&want), "{rebased:?}");
    }

    #[test]
    fn rebase_depth_overflow_degrades_to_unknown() {
        let mut callee = Effects::empty();
        callee
            .lock_ops
            .entry(Loc { root: Root::Param(0), path: vec![1, 2] })
            .or_default()
            .insert(LockOp::Lock);
        let rebased = rebase(&callee, &|_| Loc { root: Root::Param(0), path: vec![3] });
        assert!(
            rebased.lock_ops.contains_key(&Loc::unknown()),
            "3-deep path exceeds MAX_LOC_DEPTH=2: {rebased:?}"
        );
    }

    #[test]
    fn rebase_callee_alloc_becomes_unknown() {
        let mut callee = Effects::empty();
        callee
            .lock_ops
            .entry(Loc { root: Root::Alloc(7), path: vec![] })
            .or_default()
            .insert(LockOp::Lock);
        let rebased = rebase(&callee, &|_| Loc::unknown());
        assert_eq!(rebased.lock_ops.keys().collect::<Vec<_>>(), vec![&Loc::unknown()]);
    }
}

#[cfg(test)]
mod props {
    use proptest::prelude::*;

    use super::*;

    fn arb_loc() -> impl Strategy<Value = Loc> {
        (
            prop_oneof![
                (0u32..4).prop_map(Root::Param),
                Just(Root::Global("t.G".into())),
                (0u32..8).prop_map(Root::Alloc),
                Just(Root::Unknown),
            ],
            prop::collection::vec(0u32..4, 0..=2),
        )
            .prop_map(|(root, path)| Loc { root, path })
    }

    fn arb_effects() -> impl Strategy<Value = Effects> {
        (
            prop::collection::btree_map(
                arb_loc(),
                prop::collection::btree_set(
                    prop::sample::select(vec![
                        LockOp::Lock, LockOp::Unlock, LockOp::RLock,
                        LockOp::RUnlock, LockOp::DeferredUnlock, LockOp::DeferredRUnlock,
                    ]),
                    1..3,
                ),
                0..3,
            ),
            prop::sample::select(vec![Spawns::None, Spawns::Bounded, Spawns::Unbounded]),
        )
            .prop_map(|(lock_ops, spawns)| Effects { spawns, chan_ops: BTreeMap::new(), lock_ops })
    }

    proptest! {
        #[test]
        fn join_is_commutative_idempotent_monotone(a in arb_effects(), b in arb_effects()) {
            let mut ab = a.clone(); ab.join(&b);
            let mut ba = b.clone(); ba.join(&a);
            prop_assert_eq!(&ab, &ba, "commutative");
            let mut aa = a.clone(); aa.join(&a);
            prop_assert_eq!(&aa, &a, "idempotent");
            let mut a_top = a.clone(); a_top.join(&Effects::top());
            prop_assert_eq!(&a_top, &{ let mut t = Effects::top(); t.join(&a); t }, "top absorbs symmetrically");
        }
    }
}
```

If `func_with_params`'s actual signature differs (check `testpkg.rs` — phase 2's prepass tests are the reference usage), adapt the fixture to it; the assertion is the fixed part.

- [ ] **Step 2: Implement the new `effects.rs`** — types:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LockOp {
    Lock,
    Unlock,
    RLock,
    RUnlock,
    /// `defer mu.Unlock()` — runs at function exit, not program point
    /// (phase-2 Task 7 watch item; phase-C balance analysis needs the
    /// distinction natively).
    DeferredUnlock,
    DeferredRUnlock,
}

/// Where an effect happens (phase-3 spec §9): an access-path root plus a
/// bounded field path. `Unknown` is the finite top bucket — there is no
/// global collapse anymore.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Root {
    Param(u32),
    Global(String),
    /// Alloc-site identity: the allocating instruction's dst ValueId —
    /// only meaningful within its own function; rebase maps it to Unknown.
    Alloc(u32),
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Loc {
    pub root: Root,
    pub path: Vec<u32>,
}

pub const MAX_LOC_DEPTH: usize = 2;

impl Loc {
    pub fn unknown() -> Loc {
        Loc { root: Root::Unknown, path: Vec::new() }
    }

    fn capped(self) -> Loc {
        if self.path.len() > MAX_LOC_DEPTH || self.root == Root::Unknown {
            Loc::unknown()
        } else {
            self
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Effects {
    pub spawns: Spawns,
    pub chan_ops: BTreeMap<Loc, BTreeSet<ChanOp>>,
    pub lock_ops: BTreeMap<Loc, BTreeSet<LockOp>>,
}
```

`top()` builds the two `Unknown → full set` maps (6 LockOps, 5 ChanOps). `join` merges maps pointwise. `resolve_loc(f: &Function, v: ValueId) -> Loc`: build a def-map (`ValueId → &Op` over all instrs) once per call — expose a `pub(crate) struct DefMap` if profiling ever hurts, but per-call rebuilding is fine at phase-3 scale; walk: `Op::FieldAddr { base, field } => { path.insert(0, field); cur = base }`, terminate on Param (index = position in `f.params`, else Unknown), Global kind, Alloc def, anything else Unknown; iteration bounded by `MAX_LOC_DEPTH + 1` hops then Unknown. `rebase` per the semantics block. `collect` per the semantics block including the double-count guard; deferred lock mapping:

```rust
fn deferred(op: LockOp) -> LockOp {
    match op {
        LockOp::Unlock => LockOp::DeferredUnlock,
        LockOp::RUnlock => LockOp::DeferredRUnlock,
        other => other, // defer mu.Lock() is weird but keep it a plain Lock
    }
}
```

- [ ] **Step 3: Adapt `engine.rs`**

The collect call site becomes:

```rust
        let effects = effects::collect(p, f, &|c| summary_of(c).effects);
```

(the `callee_effects: Vec<Effects>` / `refs` plumbing disappears). `render_effects` becomes loc-keyed — deterministic because `BTreeMap`:

```rust
fn render_loc(l: &Loc) -> String {
    let mut s = match &l.root {
        Root::Param(i) => format!("p{i}"),
        Root::Global(g) => format!("g:{g}"),
        Root::Alloc(v) => format!("alloc:{v}"),
        Root::Unknown => "?".to_string(),
    };
    for f in &l.path {
        s.push_str(&format!(".f{f}"));
    }
    s
}

fn render_loc_map<T: std::fmt::Debug>(m: &BTreeMap<Loc, BTreeSet<T>>) -> String {
    m.iter()
        .map(|(loc, ops)| {
            let ops: Vec<String> = ops.iter().map(|o| format!("{o:?}")).collect();
            format!("{}:[{}]", render_loc(loc), ops.join(","))
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn render_effects(e: &Effects) -> String {
    format!(
        "{{spawns:{:?} chan:{{{}}} locks:{{{}}}}}",
        e.spawns,
        render_loc_map(&e.chan_ops),
        render_loc_map(&e.lock_ops)
    )
}
```

- [ ] **Step 4: Corpus + goldens**

`engine_corpus.rs`: `conc_corpus_effects_are_sane` now flattens ops across locs:

```rust
    let ops: std::collections::BTreeSet<_> =
        e.lock_ops.values().flatten().copied().collect();
    assert!(
        ops.contains(&goverify_analysis::LockOp::Lock)
            && ops.contains(&goverify_analysis::LockOp::DeferredUnlock),
        "Close locks and defer-unlocks: {e:?}"
    );
```

(the conc fixture's `Close` uses `defer f.mu.Unlock()` — the C2-era plain `Unlock` assertion becomes `DeferredUnlock`; verify against the fixture source and adjust if conc unlocks non-deferred too.)

Regenerate: `UPDATE_GOLDENS=1 mise x -- cargo test -p goverify-analysis --test engine_corpus`
Hand-review `git diff testdata/goldens/conc.summary.txt`: every `locks:[...]`/`chan:[...]` gains loc prefixes; nothing else moves. The stdlib-sensitivity comment at `engine_corpus.rs:56` stays valid — keep the filter.

- [ ] **Step 5: Full verification + commit**

Run: `mise run lint && mise run test && mise run corpus`
Expected: green with the regenerated golden.

```bash
git add -A
git commit -m "analysis: location-precise effects (final-review I2) — access-path Locs, Unknown bucket, rebase, DeferredUnlock"
```

### Task 11: Checker trait + NilTracer

**Files:**
- Create: `crates/goverify-analysis/src/checker.rs`
- Modify: `crates/goverify-analysis/src/lib.rs` (`mod checker;` + re-exports `Checker, Obligation, Finding`)
- Modify: `crates/goverify-analysis/src/engine.rs` (move `Finding` into `checker.rs`: `Finding { checker, func, pos, message }` replaces the phase-2 `Finding { tag }`; DELETE `pub fn discharge` + its tests and the per-call-site discharge block inside `analyze_function` — Task 12's findings pass via `discharge_query` supersedes both, and keeping a dead tag-shaped API around would force a second rewrite)
- Create: `crates/goverify-checkers/src/nil.rs`
- Modify: `crates/goverify-checkers/src/lib.rs`, `crates/goverify-checkers/Cargo.toml`
- Modify: root `Cargo.toml` `[workspace.dependencies]` (`goverify-checkers = { path = "crates/goverify-checkers" }`)

**Interfaces:**
- Consumes: `Clause`/`Formula`/`iface_var_name`/`instantiate_requires`/`BoundClause` (Task 9); `Query`/`Logic`/`Term`/`ptr_*` (Tasks 2–3); `Program`/`Function`/`Op`/`ValueKind`/`ConstVal`/`TypeKind` (goverify-ir).
- Produces: `trait Checker { name, infer_requires(p, f) -> Vec<Clause>, obligations(p, f, summary_of) -> Vec<Obligation> }`, `Obligation { tag, message, pos, query }`, `Finding { checker, func, pos, message }`, `NilTracer`. Task 12 threads them through `analyze_full` and the CLI.

**Tracer semantics (deliberately minimal — phase-4 grows it, spec §8):**
- `infer_requires`: scan ONLY the entry block (`blocks[0]`) — a deref there is unconditional by construction. A deref = `Op::Load { addr }`, `Op::Store { addr }`, `Op::FieldAddr { base }`, or `Op::Field { base }` whose subject value's `ValueKind` is `Param` and whose type's `TypeKind` is `Pointer`. For each such param (deduped, ascending index): emit `Clause { tag: "nil-deref", formula: ¬(is-nil p<i>) }` — but FIRST confirm the nil path is satisfiable by asking the solver (`Query::for_asserts(Logic::All, vec![is-nil p<i>])` through the `discharge` closure below): `Sat` ⇒ emit, anything else ⇒ skip (Unknown must not manufacture requires any more than findings; and this keeps the SMT path exercised on every corpus run, which is the tracer's whole point).
  - This means `infer_requires` needs a solver. Give the trait method a discharge closure parameter rather than a solver: `infer_requires(&self, p, f, discharge: &mut dyn FnMut(&Query) -> SatResult) -> Vec<Clause>` — the engine owns backend/cache/emit plumbing (Task 12); tests pass a closure over `Z3Native` or a constant.
- `obligations`: for every `Op::Call { callee: Static(c), args }` in any block: `instantiate_requires(summary_of(c), arg_terms)` where `arg_terms[i] = Some(ptr_nil())` iff the arg's `ValueKind` is `Const(ConstVal::Nil)`, else `None` (constant-nil only — zero-FP stance; propagation to caller params is phase 4). Each `BoundClause` with `violation: Some(v)` becomes `Obligation { tag, message: format!("nil passed to {} (requires {})", p.func_name(c), tag), pos: ins.pos.clone(), query: Query::for_asserts(Logic::All, vec![v]) }`.
- Trait signature update in the interface block accordingly:

```rust
pub trait Checker: Sync {
    fn name(&self) -> &'static str;
    fn infer_requires(&self, p: &Program, f: FuncId,
                      discharge: &mut dyn FnMut(&Query) -> SatResult) -> Vec<Clause>;
    fn obligations(&self, p: &Program, f: FuncId,
                   summary_of: &dyn Fn(FuncId) -> Summary) -> Vec<Obligation>;
}
```

- [ ] **Step 1: `checker.rs`** (types + trait exactly as above, plus):

```rust
/// A reported violation (parent spec §10 rendering arrives in phase 4;
/// debug output in Task 12).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Finding {
    pub checker: String,
    pub func: String,
    pub pos: Option<goverify_ir::Pos>,
    pub message: String,
}
```

`Pos` needs `PartialOrd, Ord` derives added in `goverify-ir/src/func.rs` (mechanical; file/line/col ordering is exactly what deterministic output wants).

- [ ] **Step 2: Failing tests for `NilTracer`** (`crates/goverify-checkers/src/nil.rs` test module). `goverify-analysis`'s `testpkg` is crate-private and its `goverify-extract` dependency situation makes lifting it messy — REPLICATE the few builders locally instead (they're small, and `goverify-extract` is already in this crate's dev-deps):

```rust
#[cfg(test)]
mod tests {
    use goverify_analysis::{Checker, Clause, Summary};
    use goverify_extract::gvir;
    use goverify_extract::gvir::instruction::Sem;
    use goverify_ir::Program;
    use goverify_solver::{Query, SatResult, SolverLimits, Z3Native, discharge_query};

    use super::*;

    // -- local gvir builders (mirrors goverify-analysis's testpkg) --------

    fn instr(kind: &str) -> gvir::Instruction {
        gvir::Instruction { kind: kind.into(), ..Default::default() }
    }

    fn block(index: u32, instrs: Vec<gvir::Instruction>, succs: Vec<u32>) -> gvir::BasicBlock {
        gvir::BasicBlock { index, instrs, succs }
    }

    fn ty(id: u32, repr: &str, kind: gvir::TypeKind, name: &str, elem: u32) -> gvir::Type {
        gvir::Type {
            id,
            repr: repr.into(),
            kind: kind as i32,
            name: name.into(),
            elem,
            ..Default::default()
        }
    }

    /// Package "t" with the struct-pointer type pair (1 = T, 2 = *T)
    /// and the given functions.
    fn pkg_with_ptr_types(functions: Vec<gvir::Function>) -> Program {
        let package = gvir::Package {
            import_path: "t".into(),
            functions,
            types: vec![
                ty(1, "T", gvir::TypeKind::Struct, "", 0),
                ty(2, "*T", gvir::TypeKind::Pointer, "", 1),
            ],
            ..Default::default()
        };
        Program::from_packages(vec![package])
    }

    /// t.F(p *T): FieldAddr p.X in the block layout given.
    fn deref_func(blocks: Vec<gvir::BasicBlock>) -> gvir::Function {
        gvir::Function {
            id: "t.F".into(),
            params: vec![gvir::Param { id: 1, name: "p".into(), r#type: 2 }],
            blocks,
            ..Default::default()
        }
    }

    fn field_addr_on_param() -> gvir::Instruction {
        let mut fa = instr("FieldAddr");
        fa.register = 2;
        fa.operands = vec![1];
        fa.sem = Some(Sem::Field(gvir::FieldSem { index: 0, name: "X".into() }));
        fa
    }

    /// t.F with the deref unconditionally in the entry block.
    fn deref_program() -> Program {
        pkg_with_ptr_types(vec![deref_func(vec![block(
            0,
            vec![field_addr_on_param(), instr("Return")],
            vec![],
        )])])
    }

    fn z3_discharge() -> impl FnMut(&Query) -> SatResult {
        let mut solver = Z3Native::new(SolverLimits { timeout_ms: 5_000, mem_mb: 1024 });
        move |q| discharge_query(q, &mut solver, None, None).result
    }

    #[test]
    fn unguarded_param_deref_infers_nonnil_requires() {
        let p = deref_program();
        let f = p.lookup_func("t.F").unwrap();
        let reqs = NilTracer.infer_requires(&p, f, &mut z3_discharge());
        assert_eq!(reqs.len(), 1, "one deref'd pointer param: {reqs:?}");
        assert_eq!(reqs[0].tag, "nil-deref");
    }

    #[test]
    fn unknown_verdict_infers_nothing() {
        let p = deref_program();
        let f = p.lookup_func("t.F").unwrap();
        let mut always_unknown = |_q: &Query| SatResult::Unknown;
        assert!(
            NilTracer.infer_requires(&p, f, &mut always_unknown).is_empty(),
            "Unknown must not manufacture requires (parent spec §8)"
        );
    }

    #[test]
    fn guarded_deref_in_later_block_infers_nothing() {
        // Entry block only branches; the deref lives in block 1 — the
        // entry-block-only tracer must stay silent.
        let p = pkg_with_ptr_types(vec![deref_func(vec![
            block(0, vec![{ let mut b = instr("If"); b.operands = vec![1]; b }], vec![1, 2]),
            block(1, vec![field_addr_on_param(), instr("Return")], vec![]),
            block(2, vec![instr("Return")], vec![]),
        ])]);
        let f = p.lookup_func("t.F").unwrap();
        assert!(
            NilTracer.infer_requires(&p, f, &mut z3_discharge()).is_empty(),
            "non-entry deref must infer nothing in phase 3"
        );
    }

    #[test]
    fn const_nil_arg_produces_obligation_other_args_dont() {
        // t.Caller(q *T) { t.F(nil); t.F(q) } — aux value 3 is const nil.
        let mut call_nil = instr("Call");
        call_nil.operands = vec![3];
        call_nil.sem = Some(Sem::Call(gvir::CallSem {
            static_callee: "t.F".into(),
            ..Default::default()
        }));
        let mut call_param = instr("Call");
        call_param.operands = vec![1];
        call_param.sem = Some(Sem::Call(gvir::CallSem {
            static_callee: "t.F".into(),
            ..Default::default()
        }));
        let caller = gvir::Function {
            id: "t.Caller".into(),
            params: vec![gvir::Param { id: 1, name: "q".into(), r#type: 2 }],
            aux: vec![gvir::AuxValue {
                id: 3,
                kind: "Const".into(),
                repr: "nil".into(),
                r#type: 2,
                value: Some(gvir::ConstValue {
                    kind: Some(gvir::const_value::Kind::NilValue(true)),
                }),
                ..Default::default()
            }],
            blocks: vec![block(0, vec![call_nil, call_param, instr("Return")], vec![])],
            ..Default::default()
        };
        // (If ConstValue's nil encoding differs, mirror however
        // testpkg/lower tests in goverify-analysis/-ir encode `nil`
        // consts — grep for ConstVal::Nil usages there.)
        let p = pkg_with_ptr_types(vec![
            deref_func(vec![block(0, vec![field_addr_on_param(), instr("Return")], vec![])]),
            caller,
        ]);
        let callee_id = p.lookup_func("t.F").unwrap();
        let caller_id = p.lookup_func("t.Caller").unwrap();
        // Give t.F the requires the tracer itself would infer.
        let requires: Vec<Clause> = NilTracer.infer_requires(&p, callee_id, &mut z3_discharge());
        assert!(!requires.is_empty(), "precondition of this test");
        let summary_of = |f: goverify_ir::FuncId| {
            let mut s = Summary::default();
            if f == callee_id {
                s.requires = requires.clone();
            }
            s
        };
        let obs = NilTracer.obligations(&p, caller_id, &summary_of);
        assert_eq!(obs.len(), 1, "only the const-nil call obligates: {obs:?}");
        assert_eq!(obs[0].tag, "nil-deref");
        let verdict = z3_discharge()(&obs[0].query);
        assert_eq!(verdict, SatResult::Sat, "nil-into-nonnil violation is satisfiable");
    }
}
```

- [ ] **Step 3: Implement `nil.rs`**

```rust
//! NilTracer (phase-3 spec §8): the nil checker's embryo. Deliberately
//! minimal — entry-block unconditional derefs + constant-nil call args —
//! it exists to prove extractor→IR→engine→cache→Z3→finding end to end.
//! Phase 4 replaces the heuristics with real path-sensitive analysis
//! behind the same Checker trait.

use goverify_analysis::{
    Checker, Clause, Formula, IfaceVar, Obligation, Summary, iface_var_name, instantiate_requires,
};
use goverify_ir::{ConstVal, FuncId, Op, Program, TypeKind, ValueId, ValueKind};
use goverify_solver::{Logic, Query, SatResult, Term, ptr_nil, ptr_sort};

pub struct NilTracer;

/// Pointer-typed param derefs in the ENTRY block only: unconditional by
/// construction, no path condition needed.
fn entry_block_deref_params(p: &Program, f: FuncId) -> Vec<u32> {
    let Some(func) = p.func(f) else { return Vec::new() };
    let Some(entry) = func.blocks.first() else { return Vec::new() };
    let mut out = Vec::new();
    for ins in &entry.instrs {
        let subject = match &ins.op {
            Op::Load { addr, .. } | Op::Store { addr, .. } => *addr,
            Op::FieldAddr { base, .. } | Op::Field { base, .. } => *base,
            _ => continue,
        };
        let info = func.value(subject);
        if !matches!(info.kind, ValueKind::Param) {
            continue;
        }
        if !matches!(p.types().kind(info.ty), TypeKind::Pointer { .. }) {
            continue;
        }
        if let Some(idx) = func.params.iter().position(|&pv| pv == subject) {
            let idx = idx as u32;
            if !out.contains(&idx) {
                out.push(idx);
            }
        }
    }
    out.sort_unstable();
    out
}

impl Checker for NilTracer {
    fn name(&self) -> &'static str {
        "nil"
    }

    fn infer_requires(
        &self,
        p: &Program,
        f: FuncId,
        discharge: &mut dyn FnMut(&Query) -> SatResult,
    ) -> Vec<Clause> {
        let mut out = Vec::new();
        for idx in entry_block_deref_params(p, f) {
            let name = iface_var_name(&IfaceVar::Param(idx));
            let pvar = Term::var(&name, ptr_sort());
            let Ok(is_nil) = goverify_solver::ptr_is_nil(pvar.clone()) else { continue };
            // Sat = the nil path exists => the deref needs a precondition.
            // Anything else (incl. Unknown) => stay silent (parent §8).
            if discharge(&Query::for_asserts(Logic::All, vec![is_nil.clone()])) != SatResult::Sat {
                continue;
            }
            let Ok(nonnil) = Term::not(is_nil) else { continue };
            out.push(Clause { tag: "nil-deref".into(), formula: Formula { term: nonnil } });
        }
        out
    }

    fn obligations(
        &self,
        p: &Program,
        f: FuncId,
        summary_of: &dyn Fn(FuncId) -> Summary,
    ) -> Vec<Obligation> {
        let Some(func) = p.func(f) else { return Vec::new() };
        let mut out = Vec::new();
        for b in &func.blocks {
            for ins in &b.instrs {
                let Op::Call { callee: goverify_ir::Callee::Static(c), args, .. } = &ins.op
                else {
                    continue;
                };
                let arg_terms: Vec<Option<Term>> = args
                    .iter()
                    .map(|&a| {
                        matches!(
                            func.value(a).kind,
                            ValueKind::Const(ConstVal::Nil)
                        )
                        .then(ptr_nil)
                    })
                    .collect();
                for bc in instantiate_requires(&summary_of(*c), &arg_terms) {
                    let Some(v) = bc.violation else { continue };
                    out.push(Obligation {
                        tag: bc.tag.clone(),
                        message: format!(
                            "nil passed to {} (violates its {} requirement)",
                            p.func_name(*c),
                            bc.tag
                        ),
                        pos: ins.pos.clone(),
                        query: Query::for_asserts(Logic::All, vec![v]),
                    });
                }
            }
        }
        out
    }
}
```

`crates/goverify-checkers/Cargo.toml`:

```toml
[package]
name = "goverify-checkers"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
goverify-analysis.workspace = true
goverify-ir.workspace = true
goverify-solver.workspace = true

[dev-dependencies]
goverify-extract.workspace = true
tempfile.workspace = true
```

(`tempfile` serves Task 12's determinism test in this crate's `tests/`.)

(add `goverify-analysis` to `[workspace.dependencies]` if not present — it is, from phase 2.)

- [ ] **Step 4: Run tests**

Run: `mise x -- cargo test -p goverify-checkers`
Expected: PASS (4 tests, real Z3 behind two of them).

- [ ] **Step 5: Lint + commit**

```bash
mise run lint
git add -A
git commit -m "checkers: Checker trait + NilTracer — entry-block requires inference, const-nil call obligations"
```

### Task 12: Engine wiring, nil corpus, CLI `debug findings`, determinism

**Files:**
- Modify: `crates/goverify-analysis/src/engine.rs` (`EngineConfig`, `analyze_full`, findings pass, `dump_findings`)
- Modify: `crates/goverify-analysis/src/lib.rs` (re-exports)
- Modify: `crates/goverify-cli/src/main.rs` (Findings subcommand + flags), `crates/goverify-cli/Cargo.toml` (add `goverify-checkers.workspace = true`, `goverify-solver.workspace = true`)
- Create: `testdata/corpus/nil/go.mod`, `testdata/corpus/nil/nil.go`
- Create: `testdata/goldens/nil.findings.txt`
- Create: `crates/goverify-checkers/tests/nil_corpus.rs`
- Modify: `crates/goverify-cli/tests/debug_integration.rs`, `mise.toml` (corpus task)

**Interfaces:**
- Consumes: everything — `Checker`/`NilTracer` (Task 11), `discharge_query`/`TextSolver`/`Z3Native`/`SmtLib2Process`/`StubSolver` (Tasks 6–8), `QueryCache` (Task 5), new effects `collect` (Task 10).
- Produces: `EngineConfig { opts, limits, cache_dir: Option<PathBuf>, emit_smt: Option<PathBuf> }`, `analyze_full(p, cfg, checkers, mk_backend) -> Analysis` with `Analysis.findings: Vec<Finding>`, `dump_findings(a) -> String`, CLI `goverify debug findings`. This is the end-to-end tracer milestone.

**Engine design:**
- `analyze(p, opts)` keeps its exact signature (every existing caller/test survives) and becomes `analyze_full(p, &EngineConfig{opts, ..default}, &[], &|| Box::new(StubSolver))` — no checkers, no findings, same output as phase 2.
- Inside the fixpoint (in `analyze_function`): after effects, run `checker.infer_requires(p, f, &mut discharge_closure)` for each checker; the union (checker-name order, then clause order) becomes `summary.requires`. The discharge closure wraps `discharge_query(q, backend, cache, emit_dir)` over the wave-worker's backend instance (`mk_backend` is called once per SCC task, exactly like `mk_solver` today).
- **Findings pass** (new, after all waves): single `for f in p.func_ids()` loop (deterministic order), one fresh backend, `checkers → obligations → discharge_query → Sat ⇒ Finding { checker: name, func: p.func_name(f), pos, message }`; sort per-function findings by `(pos, message)` before appending (Pos now derives Ord, Task 11). Sequential is CORRECT here, not lazy: determinism first; parallelize in phase 5 with the same slot pattern as summaries if profiling asks.
- Requires-inference caveat for recursive SCCs: `infer_requires` is intraprocedural (no summary reads), so re-running it per fixpoint round is wasteful but harmless and — critically — convergent (same input ⇒ same clauses; summary equality check still terminates). Fine for phase 3; note it in a comment.
- The per-call-site `discharge(instantiate_requires(...all-None...))` block and `pub fn discharge` itself were already deleted in Task 11 — the findings pass calls `discharge_query` directly on each `Obligation.query`.
- `dump_findings(a: &Analysis, filter: Option<&str>) -> String`: one line per finding, already-sorted assembly order: `"<file>:<line>:<col>: <checker>: <message> [<func>]"`, `pos: None` renders as `"-:-:-"`; `filter` is a substring match on `func` (same convention as `dump_summaries`). The filter exists because whole-DAG extraction includes stdlib — an unfiltered corpus golden would flake on Go toolchain bumps (the phase-2 conc.summary lesson). Trailing newline iff non-empty (same convention as `render_lines`).

**CLI:** add to `DebugWhat`:

```rust
    /// Run the analysis + checkers and print findings (phase-3 tracer).
    Findings(FindingsArgs),
```

```rust
#[derive(clap::Args)]
struct FindingsArgs {
    #[command(flatten)]
    common: DebugArgs,
    /// Dump every canonical SMT-LIB2 query to this directory.
    #[arg(long)]
    emit_smt: Option<PathBuf>,
    /// Solve via an external SMT-LIB2 binary instead of built-in Z3.
    #[arg(long)]
    solver_cmd: Option<String>,
    /// Per-query timeout in milliseconds.
    #[arg(long, default_value_t = 100)]
    solver_timeout_ms: u32,
    /// Query-cache directory (omit to run uncached).
    #[arg(long)]
    cache_dir: Option<PathBuf>,
}
```

Handler (in `run_debug`, new arm — restructure the `(kind, args)` match to accommodate the different args type; keep the existing five arms untouched):

```rust
        DebugWhat::Findings(fa) => {
            let program = load_program(&fa.common)?; // extract the shared gvir-dir/extract logic into fn load_program(&DebugArgs) -> Result<Program, ...>
            for d in program.diagnostics() {
                eprintln!("goverify: {d}");
            }
            let limits = goverify_solver::SolverLimits {
                timeout_ms: fa.solver_timeout_ms,
                ..Default::default()
            };
            let cfg = goverify_analysis::EngineConfig {
                opts: goverify_analysis::Options::default(),
                limits,
                cache_dir: fa.cache_dir.clone(),
                emit_smt: fa.emit_smt.clone(),
            };
            let cmd = fa.solver_cmd.clone();
            let mk: Box<dyn Fn() -> Box<dyn goverify_solver::TextSolver> + Sync> =
                match cmd {
                    Some(c) => Box::new(move || {
                        Box::new(goverify_solver::SmtLib2Process::new(&c, limits))
                    }),
                    None => Box::new(move || Box::new(goverify_solver::Z3Native::new(limits))),
                };
            let checkers: Vec<&dyn goverify_analysis::Checker> =
                vec![&goverify_checkers::NilTracer];
            let a = goverify_analysis::analyze_full(&program, &cfg, &checkers, &*mk);
            for d in &a.diagnostics {
                eprintln!("goverify: {d}");
            }
            print!("{}", goverify_analysis::dump_findings(&a, None));
            Ok(())
        }
```

(`--func` intentionally ABSENT from findings — filtering findings is a `check`-UX concern, phase 4; the flattened `DebugArgs` provides gvir-dir/patterns only. If clap's flatten drags `--func` along, add the same "no effect" warning as Task 1's callgraph/sccs arm.)

**Corpus module** `testdata/corpus/nil/nil.go` (go.mod: `module example.com/nil` + the same `go` directive as `testdata/corpus/ops/go.mod`):

```go
// Package nilcorpus exercises the phase-3 nil tracer end to end:
// one inferred requires, one violated call site, one guarded function.
package nilcorpus

type T struct{ X int }

// deref unconditionally dereferences p in its entry block: the tracer
// must infer `requires p != nil`.
func deref(p *T) int { return p.X }

// guarded checks first — deref happens in a non-entry block; the
// entry-block tracer must infer nothing. // want: no finding
func guarded(p *T) int {
	if p == nil {
		return 0
	}
	return p.X
}

// Bad passes a constant nil to deref. // want: nil finding here
func Bad() int { return deref(nil) }

// Good passes nil only to the guarded function. // want: no finding
func Good() int { return guarded(nil) }
```

**Corpus test** `crates/goverify-checkers/tests/nil_corpus.rs`:

```rust
//! End-to-end tracer corpus (phase-3 spec §8, §12): source → extractor →
//! IR → engine → Z3 → finding, plus artifact determinism.

use goverify_analysis::{EngineConfig, Options, analyze_full, dump_findings};
use goverify_checkers::NilTracer;
use goverify_solver::{SolverLimits, Z3Native};

fn limits() -> SolverLimits {
    // Corpus queries are trivial; generous timeout so slow CI can't turn
    // a Sat into Unknown and flake the golden.
    SolverLimits { timeout_ms: 5_000, mem_mb: 1024 }
}

fn run(emit: Option<std::path::PathBuf>) -> String {
    let p = goverify_ir::testutil::load_corpus("nil");
    let cfg = EngineConfig {
        opts: Options::default(),
        limits: limits(),
        cache_dir: None,
        emit_smt: emit,
    };
    let checkers: Vec<&dyn goverify_analysis::Checker> = vec![&NilTracer];
    let a = analyze_full(&p, &cfg, &checkers, &|| Box::new(Z3Native::new(limits())));
    // Filter to the corpus module: whole-DAG extraction includes stdlib,
    // and stdlib-derived findings would churn with Go toolchain bumps.
    dump_findings(&a, Some("example.com/nil"))
}

#[test]
fn nil_corpus_findings_match_golden() {
    let text = run(None);
    assert_eq!(text.lines().count(), 1, "exactly one finding:\n{text}");
    assert!(text.contains("nil.go"), "position points into nil.go:\n{text}");
    assert!(text.contains("Bad"), "finding is in Bad:\n{text}");
    assert!(!text.contains("Good"), "no finding in Good:\n{text}");
    goverify_ir::testutil::check_golden("nil.findings.txt", &text);
}

#[test]
fn findings_and_smt_artifacts_are_deterministic() {
    let d1 = tempfile::tempdir().unwrap();
    let d2 = tempfile::tempdir().unwrap();
    let t1 = run(Some(d1.path().to_path_buf()));
    let t2 = run(Some(d2.path().to_path_buf()));
    assert_eq!(t1, t2, "findings dump must be byte-identical across runs");
    let list = |d: &std::path::Path| -> Vec<(String, Vec<u8>)> {
        let mut v: Vec<_> = std::fs::read_dir(d)
            .unwrap()
            .map(|e| {
                let p = e.unwrap().path();
                (
                    p.file_name().unwrap().to_string_lossy().into_owned(),
                    std::fs::read(&p).unwrap(),
                )
            })
            .collect();
        v.sort();
        v
    };
    assert_eq!(
        list(d1.path()),
        list(d2.path()),
        "--emit-smt artifacts must be byte-identical (names are content hashes)"
    );
    assert!(!list(d1.path()).is_empty(), "the tracer must actually emit queries");
}
```

Golden bootstrap: `UPDATE_GOLDENS=1 mise x -- cargo test -p goverify-checkers --test nil_corpus` then review `testdata/goldens/nil.findings.txt` by hand — it must be exactly one line pointing into `nil.go` at `Bad`'s call.

**debug_integration.rs** gains one test: `debug findings --gvir-dir <nil-corpus-extraction>` exits 0 and its stdout CONTAINS the Bad finding line (not byte-equality — the CLI prints unfiltered findings, which may legitimately include stdlib-derived ones). Extract the nil corpus the same way the existing tests extract theirs.

**mise.toml** corpus task gains the two new suites:

```toml
[tasks.corpus]
description = "corpus + determinism suite (full extractor pipeline)"
run = [
  "cargo test -p goverify-extract --test extract_integration",
  "cargo test -p goverify-ir --test lower_golden --test lower_corpus --test callgraph_corpus",
  "cargo test -p goverify-analysis --test engine_corpus",
  "cargo test -p goverify-checkers --test nil_corpus",
  "cargo test -p goverify-cli --test debug_integration",
]
```

- [ ] **Step 1: `EngineConfig` + `analyze_full` + findings pass in `engine.rs`** (per the design block; `analyze` delegates; unit tests: `analyze` output unchanged on the testpkg fixtures — existing tests ARE that proof; new unit test: a scripted always-Sat `TextSolver` + a fake checker producing one obligation yields exactly one finding with deterministic order across two runs)
- [ ] **Step 2: Run engine tests** — `mise x -- cargo test -p goverify-analysis` — PASS
- [ ] **Step 3: Corpus module + golden bootstrap + nil_corpus tests** (write `go.mod`/`nil.go`, run with `UPDATE_GOLDENS=1`, hand-review, tighten asserts) — `mise x -- cargo test -p goverify-checkers --test nil_corpus` — PASS
- [ ] **Step 4: CLI subcommand + integration test** — `mise x -- cargo test -p goverify-cli` — PASS
- [ ] **Step 5: mise corpus task update; full verification**

Run: `mise run lint && mise run test && mise run corpus`
Expected: green. This is the phase's end-to-end milestone: a Go source file produced a Z3-confirmed finding through every layer.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "engine+cli: analyze_full with checkers/findings pass, nil tracer corpus end-to-end, debug findings"
```

### Task 13: Docs, threat model, phase close

**Files:**
- Modify: `ARCHITECTURE.md` (goverify-solver/cache/checkers rows: real descriptions — single-lowering rule, query-cache keying, tracer status)
- Modify: `README.md` (quickstart: `goverify debug findings ./...`, `--emit-smt`, the one-time z3-sys build-time warning)
- Modify: `docs/threat-model.md` (two bullets: `--solver-cmd` executes a user-supplied binary — restate parent §14 at the concrete flag; cached model text is rendered to terminals — cache trust boundary per parent §14 applies to it)
- Modify: `AGENTS.md` ONLY IF a workflow changed (the corpus task list did not change shape; `mise run test` now needs `--all-features` — that change lives in mise.toml, AGENTS.md's task-name references stay valid; verify and touch nothing if so)

**Interfaces:** none — documentation of what Tasks 2–12 built. No code.

- [ ] **Step 1: ARCHITECTURE.md** — replace the placeholder/stub rows for the three crates with (adapt to the file's existing table/prose style):
  - `goverify-solver`: typed QF term language (Bool/BV/Array/Ptr-ADT); canonical SMT-LIB2 printer is the ONLY term lowering — cache keys and emitted artifacts are byte-identical to solved queries; `Z3Native` (static bundled) + `SmtLib2Process` both consume printer bytes; differential harness guards agreement.
  - `goverify-cache`: content-addressed store (blake3, atomic rename, advisory lock, corrupt=miss); query layer keyed on canonical text ⊕ solver identity ⊕ limits; extraction/summary layers arrive in phase 5.
  - `goverify-checkers`: `Checker` trait consumers of the engine; currently the nil tracer (entry-block requires + const-nil call sites) — phase 4 grows it into the real nil/bounds checkers.
- [ ] **Step 2: README.md quickstart addition** (after the existing debug examples):

```markdown
### Findings (phase-3 tracer)

    goverify debug findings            # analyze CWD, print nil-tracer findings
    goverify debug findings --emit-smt /tmp/smt   # dump canonical SMT-LIB2 artifacts
    goverify debug findings --solver-cmd z3       # portable backend instead of built-in Z3

The first `cargo build` compiles a statically-linked Z3 (~20 minutes,
one-time, cached afterwards).
```

- [ ] **Step 3: threat-model bullets; verify AGENTS.md needs nothing**
- [ ] **Step 4: Full-suite final verification**

Run: `mise run lint && mise run test && mise run corpus && mise x -- cargo build --workspace`
Then the fuzz smoke: `mise run fuzz` (or note nightly-only if the sandbox lacks rustup nightly).
Expected: everything green.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "docs: phase-3 architecture/README/threat-model updates"
```

---

## Execution notes for the controller

- **Branch:** `phase3/solver-layer` off `main` (post-`972abe4`). Same worktree/subagent conventions as phase 2; progress ledger in `.superpowers/sdd/progress.md` under a new `=== PHASE 3 ===` header.
- **Task order is the dependency order.** Strict prerequisites: 2→3→4 (printer needs terms; reader needs printer), 5 independent after 1, 6 needs 3 only for tests (hand-written SMT strings decouple it — 6 can run parallel to 4–5), 7 needs 4+6, 8 needs 3+5+6, 9 needs 8, 10 needs 9 (engine file contention; semantically only needs phase-2 code), 11 needs 9 (+2/3), 12 needs 7+10+11, 13 last.
- **The z3-sys cold build** (~15–30 min) lands in Task 6 Step 1 — start that build early in the task, keep working while it compiles.
- **User-decision items to surface in the final summary:** the actual z3-sys version + bundled Z3 version + mise z3 CLI pin chosen (Task 6/7 discretion windows); the `mise run test` → `--all-features` change (Task 7); ci.yml checks timeout 10→30 (Task 6).





