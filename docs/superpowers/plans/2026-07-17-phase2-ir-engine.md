# goverify Phase 2: IR + Engine Core — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Enrich the `.gvir` schema with structured semantics, lower it to the analyzer-owned IR in Rust, build the whole-DAG call graph + SCC scheduler + pre-pass + in-memory summary fixpoint against a stub solver, and expose all of it through deterministic `goverify debug` dumps.

**Architecture:** The Go extractor stays a faithful serializer of x/tools SSA, now with structured per-instruction/type/const payloads (schema v2). `goverify-ir` lowers packages into a ~31-op IR, interns types/functions globally, and builds the cross-package call graph with Tarjan SCC condensation. `goverify-analysis` schedules SCCs callees-first (rayon wave-parallel), computes real effect summaries and placeholder requires/ensures clauses with a bounded fixpoint (widen after k=3), and classifies functions per pre-pass domain. `goverify-solver` ships the `Solver` trait + `StubSolver` (always `Unknown` ⇒ no report).

**Tech Stack:** Rust (prost, rayon, proptest dev-dep), Go (`x/tools/go/ssa`), protobuf, mise tasks, cargo-fuzz.

**Spec:** `docs/superpowers/specs/2026-07-17-phase2-ir-engine-design.md` (parent: `2026-07-16-goverify-design.md`).

## Global Constraints

- **Determinism is the root invariant** (parent spec §3, §9): identical source bytes ⇒ byte-identical `.gvir` and byte-identical debug dumps. No timestamps, no absolute paths, no map-iteration order reaching output. Sort before emitting.
- The **only** Go code lives in `extractor/`. Everything else is Rust.
- Schema bump is coordinated: `schema_version` in `proto/gvir/v1/gvir.proto` docs + `schemaVersion` (Go, `extractor/emit.go`) + `SCHEMA_VERSION` (Rust, `crates/goverify-extract/src/load.rs`) move together to `"2"`; regen with `mise run proto-gen`; commit `extractor/gvirpb/`.
- New runtime dep this phase: `rayon` only. New dev-dep: `proptest` only. Both sanctioned by parent spec §12–13. `Cargo.lock` is committed.
- **Degrade, never die** (parent §11): lowering is total — unmodeled/unknown instructions become `Op::Havoc` + a counted diagnostic, never an error. All value/type/block id lookups from `.gvir` are bounds-checked (fuzzed input!): out-of-range ⇒ `Havoc` + diagnostic.
- Parsers of bytes the analyzer didn't write must reject, never panic (fuzz targets in `fuzz/`).
- Timeout semantics: `Unknown` from the solver ⇒ no report. `StubSolver` always answers `Unknown`.
- Havoc summaries have **no requires** — missing info must never create false positives.
- Run everything via `mise x -- <cmd>` or `mise run <task>` from the repo root (plain `cargo` may not be on PATH). Blocking CI = `mise run lint` + `mise run test` + `secrets` + `audit`; budget 10 min.
- Rust edition 2024, workspace lints; `cargo fmt` + `clippy -D warnings` must pass per task.

---

## File Structure

```
proto/gvir/v1/gvir.proto                    # schema v2                        (Task 1)
extractor/
├── gvirpb/gvir.pb.go                       # regen, committed                 (Task 1)
├── emit.go                                 # structured types/consts/sems    (Tasks 2–4)
└── emit_test.go                            # Go-side emitter tests           (Tasks 2–4)
crates/goverify-extract/src/load.rs         # SCHEMA_VERSION = "2"             (Task 1)
crates/goverify-ir/
├── src/lib.rs                              # re-exports                       (Task 5)
├── src/types.rs                            # TypeId, TypeKind, TypeTable      (Task 5)
├── src/program.rs                          # FuncId interner, Program         (Task 5)
├── src/func.rs                             # ValueId, Function, Block, Instr  (Task 6)
├── src/op.rs                               # Op enum (~31 ops)                (Task 6)
├── src/lower.rs                            # gvir → IR lowering               (Tasks 6–7)
├── src/dump.rs                             # canonical text dumps             (Task 8)
├── src/callgraph.rs                        # CallGraph + Sccs (Tarjan)        (Tasks 9–10)
└── tests/lower_golden.rs                   # corpus goldens + determinism     (Tasks 8, 16)
crates/goverify-solver/src/lib.rs           # Solver trait + StubSolver        (Task 11)
crates/goverify-analysis/
├── src/lib.rs                              # re-exports                       (Task 12)
├── src/summary.rs                          # Summary/Clause/PlaceholderFormula(Task 12)
├── src/effects.rs                          # Effects, collection              (Task 13)
├── src/prepass.rs                          # value-clean classification       (Task 13)
└── src/engine.rs                           # scheduler, fixpoint, widening    (Task 14)
crates/goverify-cli/src/main.rs             # `goverify debug …`               (Task 15)
crates/goverify-cli/tests/debug_integration.rs                              #  (Task 15)
testdata/corpus/ops/{go.mod,ops.go}         # op-family corpus module          (Task 16)
testdata/goldens/*.txt                      # curated golden dumps             (Tasks 8, 16)
fuzz/fuzz_targets/ir_lower.rs               # decode→lower, never panic        (Task 18)
ARCHITECTURE.md · README.md                 # updated                          (Task 18)
```

**Key cross-task interfaces** (defined once, used everywhere):

```rust
// goverify-ir
pub struct TypeId(pub u32);                       // global, interned by canonical repr
pub struct FuncId(pub u32);                       // global, interned by ssa function id string
pub struct ValueId(pub u32);                      // per-function, same numbering as .gvir
impl TypeTable {
    pub fn kind(&self, id: TypeId) -> &TypeKind;
    pub fn repr(&self, id: TypeId) -> &str;
}
impl Program {
    pub fn from_packages(pkgs: Vec<gvir::Package>) -> Program;      // infallible
    pub fn load_dir(dir: &Path) -> std::io::Result<Program>;        // skips bad files w/ diagnostic
    pub fn func_ids(&self) -> impl Iterator<Item = FuncId> + '_;    // ascending FuncId
    pub fn func(&self, id: FuncId) -> Option<&Function>;            // None = external/bodyless
    pub fn func_name(&self, id: FuncId) -> &str;
    pub fn lookup_func(&self, name: &str) -> Option<FuncId>;
    pub fn types(&self) -> &TypeTable;
    pub fn diagnostics(&self) -> &[String];
}
impl CallGraph {
    pub fn build(p: &Program) -> CallGraph;
    pub fn callees(&self, f: FuncId) -> &[FuncId];                  // sorted, deduped
}
impl Sccs {
    pub fn compute(p: &Program, g: &CallGraph) -> Sccs;
    pub fn schedule(&self) -> &[Vec<FuncId>];                       // callees-first, members sorted
}
pub fn dump_function(p: &Program, f: FuncId) -> String;
pub fn dump_callgraph(p: &Program, g: &CallGraph) -> String;
pub fn dump_sccs(p: &Program, s: &Sccs) -> String;

// goverify-solver
pub enum SatResult { Sat, Unsat, Unknown }
pub trait Solver {
    fn declare(&mut self, decl: Decl);
    fn assert(&mut self, term: Term);
    fn check_sat_assuming(&mut self, assumptions: &[Term]) -> SatResult;
    fn model(&self) -> Option<Model>;
    fn push(&mut self);
    fn pop(&mut self);
}
pub struct StubSolver;                            // always Unknown

// goverify-analysis
pub struct Summary { pub requires: Vec<Clause>, pub ensures: Vec<Clause>,
                     pub effects: Effects, pub provenance: Provenance }
pub enum Provenance { Inferred, Havoc }
pub struct Clause { pub formula: PlaceholderFormula }
pub struct PlaceholderFormula { pub tag: String, pub vars: Vec<IfaceVar> }
pub enum IfaceVar { Param(u32), Result(u32) }
pub struct BoundClause { pub tag: String, pub vars: Vec<Option<ValueId>> }
pub fn instantiate_requires(callee: &Summary, args: &[ValueId]) -> Vec<BoundClause>;
pub struct Effects { pub spawns: Spawns, pub chan_ops: BTreeSet<ChanOp>,
                     pub lock_ops: BTreeSet<LockOp> }
pub enum Spawns { None, Bounded, Unbounded }      // ordered lattice
pub struct Analysis { pub summaries: BTreeMap<FuncId, Summary>,
                      pub prepass: BTreeMap<FuncId, Domains>,
                      pub diagnostics: Vec<String> }
pub struct Domains { pub value_clean: bool, pub concurrency_clean: bool }
pub struct Options { pub widen_after: u32 }       // default 3
pub fn analyze(p: &Program, opts: &Options) -> Analysis;   // = analyze_with_solver(StubSolver)
pub fn analyze_with_solver(p: &Program, opts: &Options,
    mk_solver: &(dyn Fn() -> Box<dyn Solver> + Sync)) -> Analysis;
pub struct Finding;                                        // phase 4 fills this in
pub fn discharge(obligations: &[BoundClause], solver: &mut dyn Solver) -> Vec<Finding>;
// filter = substring match on function ids; None = all
pub fn dump_prepass(p: &Program, a: &Analysis, filter: Option<&str>) -> String;
pub fn dump_summaries(p: &Program, a: &Analysis, filter: Option<&str>) -> String;

// goverify-ir testutil (integration-test helper, #[doc(hidden)])
pub mod testutil {
    pub fn repo_root() -> PathBuf;
    pub fn load_corpus(module: &str) -> Program;   // Sidecar-extract + load_dir
    pub fn check_golden(name: &str, got: &str);    // UPDATE_GOLDENS=1 rewrites
}
```

---

### Task 1: Schema v2 — structured types, consts, and instruction semantics

The proto gains everything at once (one bump); the extractor populates the
new fields across Tasks 2–4. Until then the new fields are empty — that's
fine, schema "2" is unreleased and evolves within this plan.

**Files:**
- Modify: `proto/gvir/v1/gvir.proto`
- Modify: `extractor/emit.go` (only `schemaVersion` constant)
- Modify: `crates/goverify-extract/src/load.rs` (only `SCHEMA_VERSION` + its tests)
- Regenerate: `extractor/gvirpb/gvir.pb.go` (committed)

**Interfaces:**
- Produces: gvir v2 messages consumed by Tasks 2–7: `Type.{kind,elem,key,array_len,chan_dir,fields,params,results,variadic,name}`, `Field`, `ConstValue`, `Instruction.sem` oneof (`BinOpSem`,`UnOpSem`,`CallSem`,`FieldSem`,`TypeAssertSem`,`ExtractSem`,`LookupSem`,`AllocSem`,`SelectSem`), `MethodSet.methods: repeated Method`.

- [ ] **Step 1: Rewrite the schema.** Replace the bodies of `Type`, `AuxValue`, `Instruction`, and `MethodSet` in `proto/gvir/v1/gvir.proto`, and append the new messages/enums. The full new content of the changed section:

```proto
enum TypeKind {
  TYPE_KIND_UNSPECIFIED = 0;
  TYPE_KIND_BASIC = 1;       // name = "int", "string", …
  TYPE_KIND_NAMED = 2;       // name = fully-qualified; elem = underlying type id
  TYPE_KIND_POINTER = 3;     // elem
  TYPE_KIND_SLICE = 4;       // elem
  TYPE_KIND_ARRAY = 5;       // elem, array_len
  TYPE_KIND_MAP = 6;         // key, elem
  TYPE_KIND_CHAN = 7;        // elem, chan_dir
  TYPE_KIND_STRUCT = 8;      // fields
  TYPE_KIND_INTERFACE = 9;
  TYPE_KIND_SIGNATURE = 10;  // params, results, variadic
  TYPE_KIND_TUPLE = 11;      // params
  TYPE_KIND_TYPE_PARAM = 12;
}

// Structured form alongside the display repr (phase-2 spec §2.2). All
// component references are type ids in this package's table; 0 = absent.
message Type {
  uint32 id = 1;         // 1-based
  string repr = 2;       // types.TypeString, display only — loader never parses it
  TypeKind kind = 3;
  uint32 elem = 4;       // pointer/slice/array/chan elem; map value; named underlying
  uint32 key = 5;        // map key
  uint64 array_len = 6;
  uint32 chan_dir = 7;   // types.ChanDir: 0 SendRecv, 1 SendOnly, 2 RecvOnly
  repeated Field fields = 8;    // struct fields, declaration order
  repeated uint32 params = 9;   // signature params / tuple members, in order
  repeated uint32 results = 10; // signature results, in order
  bool variadic = 11;
  string name = 12;      // basic: type name; named: fully-qualified name
}

message Field {
  string name = 1;
  uint32 type = 2;
  bool embedded = 3;
}

// Structured constant (phase-2 spec §2.3).
message ConstValue {
  oneof value {
    bool bool = 1;
    int64 int = 2;         // ints representable in i64 (signed or unsigned ≤ MaxInt64)
    string big_int = 3;    // decimal string when outside i64
    uint64 float_bits = 4; // IEEE-754 f64 bits
    bytes str = 5;
    bool nil = 6;          // always true: the nil / zero-value constant
    string complex = 7;    // constant.ExactString rendering
  }
}
```

`AuxValue` gains one field (keep 1–4 as-is):

```proto
message AuxValue {
  uint32 id = 1;
  string kind = 2;  // "Const" | "Global" | "Function" | "Builtin" | "FreeVar" | "Value"
  string repr = 3;  // canonical ssa String(), display only
  uint32 type = 4;
  ConstValue const = 5;  // set iff kind == "Const"
}
```

`Instruction` gains the `sem` oneof (keep 1–6 as-is) plus the payload messages:

```proto
// Structured payloads for kinds where operands + type are insufficient
// (phase-2 spec §2.1). kind/detail strings remain for debugging only.
message Instruction {
  string kind = 1;
  uint32 register = 2;
  uint32 type = 3;
  repeated uint32 operands = 4;
  Position pos = 5;
  string detail = 6;
  oneof sem {
    BinOpSem binop = 7;
    UnOpSem unop = 8;
    CallSem call = 9;          // Call, Defer, Go
    FieldSem field = 10;       // Field, FieldAddr
    TypeAssertSem type_assert = 11;
    ExtractSem extract = 12;
    LookupSem lookup = 13;
    AllocSem alloc = 14;
    SelectSem select = 15;
  }
}

message BinOpSem { string op = 1; }                  // token string: "+", "<", "&^", …
message UnOpSem  { string op = 1; bool comma_ok = 2; }
message CallSem {
  string static_callee = 1;  // ssa function id; "" when dynamic or invoke
  string method = 2;         // invoke mode: plain method name
  uint32 iface_type = 3;     // invoke mode: interface type id
  bool invoke = 4;
  string builtin = 5;        // builtin name when callee is *ssa.Builtin
  uint32 method_sig = 6;     // invoke mode: signature type id of the method
}
message FieldSem      { uint32 index = 1; string name = 2; }
message TypeAssertSem { uint32 asserted = 1; bool comma_ok = 2; }
message ExtractSem    { uint32 index = 1; }
message LookupSem     { bool comma_ok = 1; }
message AllocSem      { bool heap = 1; }
message SelectSem     { repeated SelectState states = 1; bool blocking = 2; }
message SelectState   { uint32 dir = 1; uint32 chan_operand = 2; uint32 send_operand = 3; }
```

`MethodSet` becomes structured (replaces `repeated string methods`):

```proto
// Method-set entry. func_id is the concrete ssa function implementing the
// method ("" for interface (abstract) methods). sig excludes the receiver,
// so interface and implementer entries intern to the same signature id.
message Method {
  string name = 1;     // plain method name, e.g. "Close"
  uint32 sig = 2;      // signature type id
  string func_id = 3;  // ssa function id of the concrete method; "" if abstract
}

message MethodSet {
  uint32 type = 1;                // type id of the named type T
  repeated Method methods = 2;    // types.NewMethodSet order (by name)
}
```

Also update the header comment's schema notes if they enumerate fields.

- [ ] **Step 2: Regenerate Go bindings**

Run: `mise run proto-gen`
Expected: `extractor/gvirpb/gvir.pb.go` rewritten; `git status` shows it modified.

- [ ] **Step 3: Bump the two version constants.** In `extractor/emit.go`: `schemaVersion = "2"`. In `crates/goverify-extract/src/load.rs`: `SCHEMA_VERSION: &str = "2"` — and update its `rejects_wrong_schema_version` test if it hardcodes `"1"` anywhere (it uses the constant; verify).

- [ ] **Step 4: Fix Go compile errors from the MethodSet change.** `emitMethodSets` in `extractor/emit.go` still appends strings. Minimal fix now (full structured emission is Task 2's concern for types; method `func_id`s land in Task 4 — here just keep today's information content):

```go
pb := &gvirpb.MethodSet{Type: e.typeID(T)}
for i := range ms.Len() {
    obj := ms.At(i).Obj().(*types.Func)
    pb.Methods = append(pb.Methods, &gvirpb.Method{
        Name: obj.Name(),
        Sig:  e.typeID(ms.At(i).Type()),
    })
}
```

- [ ] **Step 5: Build + full test sweep**

Run: `mise run build && mise run test && (cd extractor && go test ./...)`
Expected: PASS everywhere (determinism suite included — new fields are empty, output still canonical).

- [ ] **Step 6: Lint + commit**

Run: `mise run lint`

```bash
git add proto/ extractor/ crates/goverify-extract/
git commit -m "gvir schema v2: structured types, consts, instruction sems (fields emitted in follow-up tasks)"
```

---

### Task 2: Extractor — structured type emission

**Files:**
- Modify: `extractor/emit.go` (`typeID` + new `fillType`)
- Test: `extractor/emit_test.go`

**Interfaces:**
- Consumes: gvir v2 `Type`/`Field` messages (Task 1).
- Produces: every `Type` in every `.gvir` has `kind` set (never `TYPE_KIND_UNSPECIFIED` for types Go's checker can produce) and components filled; consumed by Task 5's `TypeTable`.

Recursive types (`type T struct { next *T }`) require two-phase interning:
reserve the id keyed by repr first, then fill components (which may
recursively intern). Ids stay first-encounter order — deterministic.

- [ ] **Step 1: Write the failing Go test** (append to `extractor/emit_test.go`; follow the file's existing helpers for building a test package — it already extracts fixture modules in-process):

```go
func TestStructuredTypes(t *testing.T) {
	pkgs := extractCorpus(t, "../testdata/corpus/hello", false)
	p := pkgs["example.com/hello"]
	byRepr := map[string]*gvirpb.Type{}
	for _, ty := range p.Types {
		byRepr[ty.Repr] = ty
		if ty.Kind == gvirpb.TypeKind_TYPE_KIND_UNSPECIFIED {
			t.Errorf("type %q has unspecified kind", ty.Repr)
		}
	}
	intT, ok := byRepr["int"]
	if !ok {
		t.Fatal("no int type interned")
	}
	if intT.Kind != gvirpb.TypeKind_TYPE_KIND_BASIC || intT.Name != "int" {
		t.Errorf("int: kind=%v name=%q", intT.Kind, intT.Name)
	}
}

func TestStructuredTypesRecursive(t *testing.T) {
	// withdeps or a dedicated fixture must contain: type node struct{ next *node }
	pkgs := extractCorpus(t, "../testdata/corpus/withdeps", false)
	p := pkgs["example.com/withdeps"]
	var structT *gvirpb.Type
	for _, ty := range p.Types {
		if ty.Kind == gvirpb.TypeKind_TYPE_KIND_STRUCT && len(ty.Fields) == 1 && ty.Fields[0].Name == "next" {
			structT = ty
		}
	}
	if structT == nil {
		t.Fatal("recursive struct not found (add `type node struct{ next *node }` + use to withdeps)")
	}
	ptr := p.Types[structT.Fields[0].Type-1] // ids are 1-based, table sorted by id
	if ptr.Kind != gvirpb.TypeKind_TYPE_KIND_POINTER {
		t.Errorf("next field: want pointer, got %v", ptr.Kind)
	}
}
```

If `testdata/corpus/withdeps` has no recursive struct, add to its source:

```go
type node struct{ next *node }

func chain(n *node) int {
	c := 0
	for n != nil {
		n = n.next
		c++
	}
	return c
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cd extractor && go test -run 'TestStructuredTypes' ./...`
Expected: FAIL — kinds are unspecified.

- [ ] **Step 3: Implement.** In `emit.go`, split `typeID` into reserve + fill:

```go
func (e *emitter) typeID(t types.Type) uint32 {
	repr := types.TypeString(t, func(p *types.Package) string { return p.Path() })
	if id, ok := e.typeIDs[repr]; ok {
		return id
	}
	id := uint32(len(e.typeIDs) + 1)
	e.typeIDs[repr] = id
	pb := &gvirpb.Type{Id: id, Repr: repr}
	e.out.Types = append(e.out.Types, pb) // append BEFORE fill: recursion sees the id
	e.fillType(pb, t)
	return id
}

func (e *emitter) fillType(pb *gvirpb.Type, t types.Type) {
	switch t := t.(type) {
	case *types.Basic:
		pb.Kind, pb.Name = gvirpb.TypeKind_TYPE_KIND_BASIC, t.Name()
	case *types.Named:
		pb.Kind = gvirpb.TypeKind_TYPE_KIND_NAMED
		pb.Name = t.Obj().Pkg().Path() + "." + t.Obj().Name()
		if t.Obj().Pkg() == nil { // error type, builtins
			pb.Name = t.Obj().Name()
		}
		pb.Elem = e.typeID(t.Underlying())
	case *types.Alias:
		e.fillType(pb, types.Unalias(t)) // aliases are transparent
	case *types.Pointer:
		pb.Kind, pb.Elem = gvirpb.TypeKind_TYPE_KIND_POINTER, e.typeID(t.Elem())
	case *types.Slice:
		pb.Kind, pb.Elem = gvirpb.TypeKind_TYPE_KIND_SLICE, e.typeID(t.Elem())
	case *types.Array:
		pb.Kind, pb.Elem = gvirpb.TypeKind_TYPE_KIND_ARRAY, e.typeID(t.Elem())
		pb.ArrayLen = uint64(t.Len())
	case *types.Map:
		pb.Kind = gvirpb.TypeKind_TYPE_KIND_MAP
		pb.Key, pb.Elem = e.typeID(t.Key()), e.typeID(t.Elem())
	case *types.Chan:
		pb.Kind, pb.Elem = gvirpb.TypeKind_TYPE_KIND_CHAN, e.typeID(t.Elem())
		pb.ChanDir = uint32(t.Dir())
	case *types.Struct:
		pb.Kind = gvirpb.TypeKind_TYPE_KIND_STRUCT
		for i := range t.NumFields() {
			f := t.Field(i)
			pb.Fields = append(pb.Fields, &gvirpb.Field{
				Name: f.Name(), Type: e.typeID(f.Type()), Embedded: f.Embedded(),
			})
		}
	case *types.Interface:
		pb.Kind = gvirpb.TypeKind_TYPE_KIND_INTERFACE
	case *types.Signature:
		pb.Kind, pb.Variadic = gvirpb.TypeKind_TYPE_KIND_SIGNATURE, t.Variadic()
		for i := range t.Params().Len() {
			pb.Params = append(pb.Params, e.typeID(t.Params().At(i).Type()))
		}
		for i := range t.Results().Len() {
			pb.Results = append(pb.Results, e.typeID(t.Results().At(i).Type()))
		}
	case *types.Tuple:
		pb.Kind = gvirpb.TypeKind_TYPE_KIND_TUPLE
		for i := range t.Len() {
			pb.Params = append(pb.Params, e.typeID(t.At(i).Type()))
		}
	case *types.TypeParam:
		pb.Kind = gvirpb.TypeKind_TYPE_KIND_TYPE_PARAM
	}
	// anything else stays TYPE_KIND_UNSPECIFIED — the Rust side treats
	// unspecified as opaque/unknown (degrade, never die)
}
```

Note the `*types.Named` nil-package guard must come **before** using `t.Obj().Pkg().Path()` — reorder so the guard is first:

```go
	case *types.Named:
		pb.Kind = gvirpb.TypeKind_TYPE_KIND_NAMED
		if t.Obj().Pkg() != nil {
			pb.Name = t.Obj().Pkg().Path() + "." + t.Obj().Name()
		} else {
			pb.Name = t.Obj().Name() // universe scope: "error"
		}
		pb.Elem = e.typeID(t.Underlying())
```

- [ ] **Step 4: Run Go tests**

Run: `cd extractor && go test ./...`
Expected: PASS (including existing determinism-relevant tests).

- [ ] **Step 5: Full corpus + determinism suite**

Run: `mise run corpus && mise run test`
Expected: PASS — `extraction_is_byte_identical_across_runs` proves recursion order didn't leak nondeterminism.

- [ ] **Step 6: Lint + commit**

Run: `mise run lint`

```bash
git add extractor/ testdata/
git commit -m "extractor: emit structured type kinds and components (gvir v2)"
```

---

### Task 3: Extractor — structured constants and simple instruction sems

**Files:**
- Modify: `extractor/emit.go`
- Test: `extractor/emit_test.go`

**Interfaces:**
- Consumes: `ConstValue`, `BinOpSem`, `UnOpSem`, `FieldSem`, `TypeAssertSem`, `ExtractSem`, `LookupSem`, `AllocSem` (Task 1).
- Produces: every Const `AuxValue` carries `const`; every `BinOp`/`UnOp`/`Field`/`FieldAddr`/`TypeAssert`/`Extract`/`Lookup`/`Alloc` instruction carries its sem payload. Consumed by Tasks 6–7.

- [ ] **Step 1: Write the failing Go test** (append to `extractor/emit_test.go`):

```go
// findInstr returns instructions of the given kind across all functions.
func findInstr(p *gvirpb.Package, kind string) []*gvirpb.Instruction {
	var out []*gvirpb.Instruction
	for _, f := range p.Functions {
		for _, b := range f.Blocks {
			for _, ins := range b.Instrs {
				if ins.Kind == kind {
					out = append(out, ins)
				}
			}
		}
	}
	return out
}

func TestStructuredConstsAndSems(t *testing.T) {
	pkgs := extractCorpus(t, "../testdata/corpus/hello", false)
	p := pkgs["example.com/hello"]

	// hello.Add contains a BinOp; its sem must carry the token.
	binops := findInstr(p, "BinOp")
	if len(binops) == 0 {
		t.Fatal("no BinOp in hello corpus")
	}
	for _, ins := range binops {
		if ins.GetBinop().GetOp() == "" {
			t.Errorf("BinOp without sem.op: %s", ins.Detail)
		}
	}

	// Every Const aux value must carry a structured ConstValue.
	for _, f := range p.Functions {
		for _, a := range f.Aux {
			if a.Kind == "Const" && a.Const == nil {
				t.Errorf("%s: const aux %q lacks ConstValue", f.Id, a.Repr)
			}
		}
	}
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cd extractor && go test -run TestStructuredConstsAndSems ./...`
Expected: FAIL.

- [ ] **Step 3: Implement const emission.** In `emit.go`, extend the `operandID` closure's AuxValue construction (and the FreeVar loop stays as-is): where `auxKind(v)` returns `"Const"`, attach the value. Add:

```go
func constValue(c *ssa.Const) *gvirpb.ConstValue {
	if c.Value == nil { // nil pointer/interface/map/…, or zero value
		return &gvirpb.ConstValue{Value: &gvirpb.ConstValue_Nil{Nil: true}}
	}
	switch c.Value.Kind() {
	case constant.Bool:
		return &gvirpb.ConstValue{Value: &gvirpb.ConstValue_Bool{Bool: constant.BoolVal(c.Value)}}
	case constant.Int:
		if i, exact := constant.Int64Val(c.Value); exact {
			return &gvirpb.ConstValue{Value: &gvirpb.ConstValue_Int{Int: i}}
		}
		return &gvirpb.ConstValue{Value: &gvirpb.ConstValue_BigInt{BigInt: c.Value.ExactString()}}
	case constant.Float:
		f, _ := constant.Float64Val(c.Value)
		return &gvirpb.ConstValue{Value: &gvirpb.ConstValue_FloatBits{FloatBits: math.Float64bits(f)}}
	case constant.String:
		return &gvirpb.ConstValue{Value: &gvirpb.ConstValue_Str{Str: []byte(constant.StringVal(c.Value))}}
	case constant.Complex:
		return &gvirpb.ConstValue{Value: &gvirpb.ConstValue_Complex{Complex: c.Value.ExactString()}}
	}
	return nil // Unknown kind: leave unset; Rust treats as opaque
}
```

(imports: add `go/constant` and `math`.) Wire it in `operandID`:

```go
aux := &gvirpb.AuxValue{Id: id, Kind: auxKind(v), Repr: v.String(), Type: e.typeID(v.Type())}
if c, ok := v.(*ssa.Const); ok {
	aux.Const = constValue(c)
}
f.Aux = append(f.Aux, aux)
```

- [ ] **Step 4: Implement the simple sems.** In `emitFunction`'s pass 2, after building `pi`, add a type switch on the concrete instruction:

```go
switch ins := ins.(type) {
case *ssa.BinOp:
	pi.Sem = &gvirpb.Instruction_Binop{Binop: &gvirpb.BinOpSem{Op: ins.Op.String()}}
case *ssa.UnOp:
	pi.Sem = &gvirpb.Instruction_Unop{Unop: &gvirpb.UnOpSem{Op: ins.Op.String(), CommaOk: ins.CommaOk}}
case *ssa.Field:
	st := ins.X.Type().Underlying().(*types.Struct)
	pi.Sem = &gvirpb.Instruction_Field{Field: &gvirpb.FieldSem{
		Index: uint32(ins.Field), Name: st.Field(ins.Field).Name()}}
case *ssa.FieldAddr:
	st := ins.X.Type().Underlying().(*types.Pointer).Elem().Underlying().(*types.Struct)
	pi.Sem = &gvirpb.Instruction_Field{Field: &gvirpb.FieldSem{
		Index: uint32(ins.Field), Name: st.Field(ins.Field).Name()}}
case *ssa.TypeAssert:
	pi.Sem = &gvirpb.Instruction_TypeAssert{TypeAssert: &gvirpb.TypeAssertSem{
		Asserted: e.typeID(ins.AssertedType), CommaOk: ins.CommaOk}}
case *ssa.Extract:
	pi.Sem = &gvirpb.Instruction_Extract{Extract: &gvirpb.ExtractSem{Index: uint32(ins.Index)}}
case *ssa.Lookup:
	pi.Sem = &gvirpb.Instruction_Lookup{Lookup: &gvirpb.LookupSem{CommaOk: ins.CommaOk}}
case *ssa.Alloc:
	pi.Sem = &gvirpb.Instruction_Alloc{Alloc: &gvirpb.AllocSem{Heap: ins.Heap}}
}
```

`FieldAddr.X` may be a `*types.Named` pointing at a pointer in generic
code; if the two type assertions above can panic on any corpus input,
guard with the comma-ok form and skip the sem (degrade, never die):

```go
case *ssa.FieldAddr:
	if pt, ok := ins.X.Type().Underlying().(*types.Pointer); ok {
		if st, ok := pt.Elem().Underlying().(*types.Struct); ok {
			pi.Sem = &gvirpb.Instruction_Field{Field: &gvirpb.FieldSem{
				Index: uint32(ins.Field), Name: st.Field(ins.Field).Name()}}
		}
	}
```

Use the guarded form for both `Field` and `FieldAddr`.

- [ ] **Step 5: Run tests, full sweep, commit**

Run: `cd extractor && go test ./...` then `mise run corpus && mise run lint`
Expected: PASS.

```bash
git add extractor/
git commit -m "extractor: structured consts + BinOp/UnOp/Field/TypeAssert/Extract/Lookup/Alloc sems"
```

---

### Task 4: Extractor — CallSem, SelectSem, and concrete method ids

**Files:**
- Modify: `extractor/emit.go` (call/select sems; `emitMethodSets` func ids)
- Test: `extractor/emit_test.go`

**Interfaces:**
- Consumes: `CallSem`, `SelectSem`, `Method` (Task 1).
- Produces: every `Call`/`Defer`/`Go` carries `CallSem` (static callee id, or invoke method+iface+sig, or builtin name); every `Select` carries states; every concrete `Method` carries `func_id`. Consumed by Task 7 (lowering) and Task 9 (call graph).

- [ ] **Step 1: Write the failing Go test.** Requires a corpus fixture with an interface call, a goroutine, and a select. Add `testdata/corpus/conc/go.mod`:

```
module example.com/conc

go 1.25.10
```

and `testdata/corpus/conc/conc.go`:

```go
package conc

import "sync"

type Closer interface{ Close() error }

type file struct{ mu sync.Mutex }

func (f *file) Close() error {
	f.mu.Lock()
	defer f.mu.Unlock()
	return nil
}

func CloseAll(cs []Closer) {
	for _, c := range cs {
		_ = c.Close() // invoke-mode call
	}
}

func Fan(a, b chan int) int {
	done := make(chan struct{})
	go func() { close(done) }() // go + closure + builtin close
	select {
	case v := <-a:
		return v
	case b <- 1:
		return 1
	case <-done:
		return 0
	}
}
```

Append to `extractor/emit_test.go`:

```go
func TestCallAndSelectSems(t *testing.T) {
	pkgs := extractCorpus(t, "../testdata/corpus/conc", false)
	p := pkgs["example.com/conc"]

	var invokes, statics, builtins, selects int
	for _, kind := range []string{"Call", "Defer", "Go"} {
		for _, ins := range findInstr(p, kind) {
			c := ins.GetCall()
			if c == nil {
				t.Fatalf("%s without CallSem: %s", kind, ins.Detail)
			}
			switch {
			case c.Invoke:
				invokes++
				if c.Method == "" || c.IfaceType == 0 || c.MethodSig == 0 {
					t.Errorf("invoke sem incomplete: %+v", c)
				}
			case c.Builtin != "":
				builtins++
			case c.StaticCallee != "":
				statics++
			}
		}
	}
	if invokes == 0 || statics == 0 || builtins == 0 {
		t.Errorf("want ≥1 of each call mode, got invoke=%d static=%d builtin=%d",
			invokes, statics, builtins)
	}

	sel := findInstr(p, "Select")
	if len(sel) != 1 || len(sel[0].GetSelect().States) != 3 || !sel[0].GetSelect().Blocking {
		t.Fatalf("want one blocking 3-state Select, got %+v", sel)
	}

	// Concrete method-set entries must carry ssa func ids.
	found := false
	for _, ms := range p.MethodSets {
		for _, m := range ms.Methods {
			if m.Name == "Close" && m.FuncId != "" {
				found = true
			}
		}
	}
	if !found {
		t.Error("no concrete Close method with func_id in method sets")
	}
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cd extractor && go test -run TestCallAndSelectSems ./...`
Expected: FAIL (nil CallSem).

- [ ] **Step 3: Implement CallSem.** In the sem type switch, handle the three call-wrapping instructions via `ssa.CallInstruction` (they share `Common()`):

```go
case ssa.CallInstruction: // *ssa.Call, *ssa.Defer, *ssa.Go
	cc := ins.Common()
	sem := &gvirpb.CallSem{}
	if cc.IsInvoke() {
		sem.Invoke = true
		sem.Method = cc.Method.Name()
		sem.IfaceType = e.typeID(cc.Value.Type())
		sem.MethodSig = e.typeID(cc.Method.Type())
	} else {
		switch v := cc.Value.(type) {
		case *ssa.Builtin:
			sem.Builtin = v.Name()
		case *ssa.Function:
			sem.StaticCallee = v.String()
		case *ssa.MakeClosure:
			sem.StaticCallee = v.Fn.(*ssa.Function).String()
		}
	}
	pi.Sem = &gvirpb.Instruction_Call{Call: sem}
```

Order matters in the type switch: `ssa.CallInstruction` is an interface —
place this case **after** the concrete cases from Task 3 (Go evaluates
cases in order; none of Task 3's kinds implement `CallInstruction`, but
keep the interface case last for clarity). Note `cc.StaticCallee()` also
covers the `MakeClosure` case; using it directly is fine:

```go
if f := cc.StaticCallee(); f != nil {
	sem.StaticCallee = f.String()
}
```

Prefer the `StaticCallee()` form.

- [ ] **Step 4: Implement SelectSem** (same switch):

```go
case *ssa.Select:
	sem := &gvirpb.SelectSem{Blocking: ins.Blocking}
	for _, st := range ins.States {
		s := &gvirpb.SelectState{Dir: uint32(st.Dir), ChanOperand: operandID(st.Chan)}
		if st.Send != nil {
			s.SendOperand = operandID(st.Send)
		}
		sem.States = append(sem.States, s)
	}
	pi.Sem = &gvirpb.Instruction_Select{Select: sem}
```

- [ ] **Step 5: Implement method func ids.** In `emitMethodSets`, resolve each concrete method to its ssa function via the program. `emitMethodSets` receives `sp *ssa.Package`; the program is `sp.Prog`:

```go
for i := range ms.Len() {
	sel := ms.At(i)
	obj := sel.Obj().(*types.Func)
	m := &gvirpb.Method{Name: obj.Name(), Sig: e.typeID(sel.Type())}
	if !types.IsInterface(T) {
		if fn := sp.Prog.MethodValue(sel); fn != nil {
			m.FuncId = fn.String()
		}
	}
	pb.Methods = append(pb.Methods, m)
}
```

**Caveat:** `Prog.MethodValue` builds method wrappers on demand and must be
called before `extract.go` serializes — it already is (emission happens
inside extraction). If `MethodValue` returns wrappers whose `String()`
differs from the declared method's id for promoted/embedded methods,
that's correct: the wrapper *is* the function the call graph should edge to,
and wrappers are part of `ssa.Program`. However, wrapper functions are
emitted per-package by the existing function enumeration only if
`extract.go`'s function-collection pass includes them — check how `fns` is
collected in `extract.go`; if wrappers are absent from `fns`, the func_id
still resolves (Task 9 treats unknown FuncIds as external/havoc), which is
acceptable for phase 2. Do not expand function enumeration in this task.

- [ ] **Step 6: Run tests, sweep, commit**

Run: `cd extractor && go test ./...` then `mise run corpus && mise run test && mise run lint`
Expected: PASS.

```bash
git add extractor/ testdata/corpus/conc/
git commit -m "extractor: CallSem/SelectSem + concrete method func ids (gvir v2 complete)"
```

---

### Task 5: goverify-ir — TypeTable and Program skeleton

**Files:**
- Create: `crates/goverify-ir/src/types.rs`, `crates/goverify-ir/src/program.rs`
- Modify: `crates/goverify-ir/src/lib.rs`, `crates/goverify-ir/Cargo.toml`

**Interfaces:**
- Consumes: `goverify_extract::gvir` messages + `load_package` (existing).
- Produces: `TypeId`, `TypeKind`, `TypeTable`, `FuncId`, `Program::{from_packages, load_dir, func_ids, func, func_name, lookup_func, types, diagnostics}` (see the interfaces block at the top; `Program.func` returns `None` until Task 6 adds lowered bodies — in this task `Program` stores raw functions and name interning only, with `func` stubbed to return `None`).

- [ ] **Step 1: Cargo wiring.** `crates/goverify-ir/Cargo.toml`:

```toml
[package]
name = "goverify-ir"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
goverify-extract = { workspace = true }

[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 2: Write the failing test** (in `types.rs` `#[cfg(test)]`):

```rust
#[test]
fn interns_across_packages_by_repr() {
    use goverify_extract::gvir;
    let mk = |id, repr: &str, kind| gvir::Type {
        id, repr: repr.into(), kind: kind as i32, ..Default::default()
    };
    // Two packages both describe `*int`, with different local ids.
    let pkg_a = vec![
        mk(1, "int", gvir::TypeKind::Basic),
        gvir::Type { id: 2, repr: "*int".into(), kind: gvir::TypeKind::Pointer as i32,
                     elem: 1, ..Default::default() },
    ];
    let pkg_b = vec![
        gvir::Type { id: 1, repr: "*int".into(), kind: gvir::TypeKind::Pointer as i32,
                     elem: 2, ..Default::default() },
        mk(2, "int", gvir::TypeKind::Basic),
    ];
    let mut table = TypeTable::default();
    let map_a = table.import_package(&pkg_a);
    let map_b = table.import_package(&pkg_b);
    assert_eq!(map_a[2], map_b[1], "*int must intern to one global id");
    let TypeKind::Pointer { elem } = *table.kind(map_b[1]) else {
        panic!("expected pointer kind");
    };
    assert_eq!(table.repr(elem), "int");
}

#[test]
fn out_of_range_component_degrades_to_unknown() {
    use goverify_extract::gvir;
    let pkg = vec![gvir::Type { id: 1, repr: "*bad".into(),
        kind: gvir::TypeKind::Pointer as i32, elem: 99, ..Default::default() }];
    let mut table = TypeTable::default();
    let map = table.import_package(&pkg); // must not panic
    let TypeKind::Pointer { elem } = *table.kind(map[1]) else { panic!() };
    assert!(matches!(table.kind(elem), TypeKind::Unknown));
}
```

- [ ] **Step 3: Run to verify failure** — `mise x -- cargo test -p goverify-ir` — FAIL: types don't exist.

- [ ] **Step 4: Implement `types.rs`.**

```rust
//! Global type table: structured Go types interned across packages by
//! canonical repr string. Per-package .gvir type ids are local; importing
//! a package returns the local→global mapping.

use std::collections::HashMap;

use goverify_extract::gvir;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TypeId(pub u32);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldInfo {
    pub name: String,
    pub ty: TypeId,
    pub embedded: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeKind {
    Basic { name: String },
    Named { name: String, underlying: TypeId },
    Pointer { elem: TypeId },
    Slice { elem: TypeId },
    Array { elem: TypeId, len: u64 },
    Map { key: TypeId, value: TypeId },
    Chan { elem: TypeId, dir: u32 },
    Struct { fields: Vec<FieldInfo> },
    Interface,
    Signature { params: Vec<TypeId>, results: Vec<TypeId>, variadic: bool },
    Tuple { elems: Vec<TypeId> },
    TypeParam,
    Unknown,
}

#[derive(Debug, Default)]
pub struct TypeTable {
    by_repr: HashMap<String, TypeId>,
    reprs: Vec<String>,
    kinds: Vec<TypeKind>,
}

impl TypeTable {
    pub fn kind(&self, id: TypeId) -> &TypeKind {
        self.kinds.get(id.0 as usize).unwrap_or(&TypeKind::Unknown)
    }

    pub fn repr(&self, id: TypeId) -> &str {
        self.reprs.get(id.0 as usize).map_or("<unknown>", |s| s)
    }

    /// The shared Unknown type (index 0 is reserved for it).
    pub fn unknown(&mut self) -> TypeId {
        self.intern("<unknown>")
    }

    fn intern(&mut self, repr: &str) -> TypeId {
        if let Some(&id) = self.by_repr.get(repr) {
            return id;
        }
        let id = TypeId(self.reprs.len() as u32);
        self.by_repr.insert(repr.to_string(), id);
        self.reprs.push(repr.to_string());
        self.kinds.push(TypeKind::Unknown);
        id
    }

    /// Import one package's type list; returns local-id → global TypeId.
    /// Index 0 of the returned map is the Unknown type (local id 0 means
    /// "absent" in .gvir). Malformed component references degrade to
    /// Unknown — never panic (fuzzed input).
    pub fn import_package(&mut self, types: &[gvir::Type]) -> Vec<TypeId> {
        let unknown = self.unknown();
        let max_local = types.iter().map(|t| t.id).max().unwrap_or(0) as usize;
        let mut map = vec![unknown; max_local + 1];
        // Pass 1: intern all reprs so cycles resolve.
        for t in types {
            if t.id != 0 {
                map[t.id as usize] = self.intern(&t.repr);
            }
        }
        // Pass 2: translate kinds. First writer for a repr wins; identical
        // sources produce identical structures, so later writers agree.
        for t in types {
            if t.id == 0 {
                continue;
            }
            let gid = map[t.id as usize];
            if !matches!(self.kinds[gid.0 as usize], TypeKind::Unknown) {
                continue; // already populated by an earlier package
            }
            let r = |local: u32| -> TypeId {
                map.get(local as usize).copied().unwrap_or(unknown)
            };
            let kind = match gvir::TypeKind::try_from(t.kind).unwrap_or(gvir::TypeKind::Unspecified) {
                gvir::TypeKind::Basic => TypeKind::Basic { name: t.name.clone() },
                gvir::TypeKind::Named => TypeKind::Named { name: t.name.clone(), underlying: r(t.elem) },
                gvir::TypeKind::Pointer => TypeKind::Pointer { elem: r(t.elem) },
                gvir::TypeKind::Slice => TypeKind::Slice { elem: r(t.elem) },
                gvir::TypeKind::Array => TypeKind::Array { elem: r(t.elem), len: t.array_len },
                gvir::TypeKind::Map => TypeKind::Map { key: r(t.key), value: r(t.elem) },
                gvir::TypeKind::Chan => TypeKind::Chan { elem: r(t.elem), dir: t.chan_dir },
                gvir::TypeKind::Struct => TypeKind::Struct {
                    fields: t.fields.iter().map(|f| FieldInfo {
                        name: f.name.clone(), ty: r(f.r#type), embedded: f.embedded,
                    }).collect(),
                },
                gvir::TypeKind::Interface => TypeKind::Interface,
                gvir::TypeKind::Signature => TypeKind::Signature {
                    params: t.params.iter().map(|&p| r(p)).collect(),
                    results: t.results.iter().map(|&p| r(p)).collect(),
                    variadic: t.variadic,
                },
                gvir::TypeKind::Tuple => TypeKind::Tuple {
                    elems: t.params.iter().map(|&p| r(p)).collect(),
                },
                gvir::TypeKind::TypeParam => TypeKind::TypeParam,
                gvir::TypeKind::Unspecified => TypeKind::Unknown,
            };
            self.kinds[gid.0 as usize] = kind;
        }
        map
    }
}
```

(Adjust the prost enum accessor names to what codegen produces —
`gvir::type_kind` naming may differ; check `target/…/gvir.v1.rs`.)

- [ ] **Step 5: Implement `program.rs`** (raw-package container this task; lowering fills bodies in Task 6):

```rust
//! Whole-DAG program: all loaded packages, functions interned by their
//! stable ssa id string, sorted for determinism.

use std::collections::HashMap;
use std::path::Path;

use goverify_extract::{gvir, load_package};

use crate::func::Function;
use crate::types::TypeTable;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FuncId(pub u32);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MethodInfo {
    pub name: String,
    pub sig: crate::types::TypeId,
    pub func: Option<FuncId>, // None = abstract (interface) method
}

#[derive(Debug, Default)]
pub struct Program {
    types: TypeTable,
    func_names: Vec<String>,           // FuncId → ssa id string
    by_name: HashMap<String, FuncId>,
    funcs: Vec<Option<Function>>,      // FuncId → lowered body (None = external)
    /// Method sets of named types, keyed by the type's global TypeId,
    /// sorted entries. Used by Task 9's invoke resolution.
    pub method_sets: std::collections::BTreeMap<crate::types::TypeId, Vec<MethodInfo>>,
    diagnostics: Vec<String>,
}

impl Program {
    /// Build from decoded packages. Infallible: malformed content degrades
    /// to diagnostics + havoc (fuzz target decodes arbitrary bytes into
    /// packages and calls this).
    pub fn from_packages(mut pkgs: Vec<gvir::Package>) -> Program {
        // Deterministic global order regardless of input order.
        pkgs.sort_by(|a, b| a.import_path.cmp(&b.import_path));
        let mut p = Program::default();
        // Pass 1: intern every function name (sorted per package already;
        // sort globally for FuncId stability).
        let mut names: Vec<&str> = pkgs
            .iter()
            .flat_map(|pkg| pkg.functions.iter().map(|f| f.id.as_str()))
            .collect();
        names.sort_unstable();
        names.dedup();
        for n in names {
            p.intern_func(n);
        }
        // Pass 2: types, method sets, bodies (bodies land in Task 6).
        for pkg in &pkgs {
            let tmap = p.types.import_package(&pkg.types);
            p.import_method_sets(pkg, &tmap);
            // Task 6 inserts: p.lower_package(pkg, &tmap);
        }
        p
    }

    pub fn load_dir(dir: &Path) -> std::io::Result<Program> {
        let mut pkgs = Vec::new();
        let mut diags = Vec::new();
        let mut entries: Vec<_> = std::fs::read_dir(dir)?
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "gvir"))
            .collect();
        entries.sort();
        for path in entries {
            match load_package(&path) {
                Ok(pkg) => pkgs.push(pkg),
                Err(e) => diags.push(format!("skipping {}: {e}", path.display())),
            }
        }
        let mut p = Program::from_packages(pkgs);
        p.diagnostics.splice(0..0, diags);
        Ok(p)
    }

    pub(crate) fn intern_func(&mut self, name: &str) -> FuncId {
        if let Some(&id) = self.by_name.get(name) {
            return id;
        }
        let id = FuncId(self.func_names.len() as u32);
        self.by_name.insert(name.to_string(), id);
        self.func_names.push(name.to_string());
        self.funcs.push(None);
        id
    }

    fn import_method_sets(&mut self, pkg: &gvir::Package, tmap: &[crate::types::TypeId]) {
        for ms in &pkg.method_sets {
            let Some(&ty) = tmap.get(ms.r#type as usize) else { continue };
            let entry = self.method_sets.entry(ty).or_default();
            if !entry.is_empty() {
                continue; // same named type seen from another package
            }
            for m in &ms.methods {
                let func = (!m.func_id.is_empty()).then(|| self.intern_func(&m.func_id));
                let sig = tmap.get(m.sig as usize).copied()
                    .unwrap_or_else(|| self.types.unknown());
                entry.push(MethodInfo { name: m.name.clone(), sig, func });
            }
        }
    }

    pub fn func_ids(&self) -> impl Iterator<Item = FuncId> + '_ {
        (0..self.func_names.len() as u32).map(FuncId)
    }

    pub fn func(&self, id: FuncId) -> Option<&Function> {
        self.funcs.get(id.0 as usize).and_then(Option::as_ref)
    }

    pub fn func_name(&self, id: FuncId) -> &str {
        self.func_names.get(id.0 as usize).map_or("<unknown>", |s| s)
    }

    pub fn lookup_func(&self, name: &str) -> Option<FuncId> {
        self.by_name.get(name).copied()
    }

    pub fn types(&self) -> &TypeTable {
        &self.types
    }

    pub fn diagnostics(&self) -> &[String] {
        &self.diagnostics
    }

    pub(crate) fn push_diagnostic(&mut self, d: String) {
        self.diagnostics.push(d);
    }
}
```

`func.rs` gets a placeholder `pub struct Function;` this task (real content Task 6). `lib.rs`:

```rust
//! Analyzer-owned SSA-style IR + call graph (phase 2).

mod func;
mod program;
mod types;

pub use func::Function;
pub use program::{FuncId, MethodInfo, Program};
pub use types::{FieldInfo, TypeId, TypeKind, TypeTable};
```

- [ ] **Step 6: Add a Program unit test** (in `program.rs`): two minimal `gvir::Package` values with overlapping function names in reversed input order produce identical `FuncId` assignment (assert `lookup_func` results equal across the two `from_packages` calls).

```rust
#[test]
fn func_ids_stable_under_package_order() {
    use goverify_extract::gvir;
    let f = |id: &str| gvir::Function { id: id.into(), ..Default::default() };
    let pkg = |path: &str, fs: Vec<gvir::Function>| gvir::Package {
        import_path: path.into(), functions: fs, ..Default::default()
    };
    let a = || pkg("a", vec![f("a.F"), f("a.G")]);
    let b = || pkg("b", vec![f("b.H")]);
    let p1 = Program::from_packages(vec![a(), b()]);
    let p2 = Program::from_packages(vec![b(), a()]);
    for name in ["a.F", "a.G", "b.H"] {
        assert_eq!(p1.lookup_func(name), p2.lookup_func(name), "{name}");
    }
}
```

- [ ] **Step 7: Run, lint, commit**

Run: `mise x -- cargo test -p goverify-ir && mise run lint`
Expected: PASS.

```bash
git add crates/goverify-ir/ Cargo.lock
git commit -m "ir: global TypeTable + Program skeleton with deterministic interning"
```

---

### Task 6: goverify-ir — op set, value tables, and core lowering

**Files:**
- Create: `crates/goverify-ir/src/op.rs`, `crates/goverify-ir/src/func.rs` (replace placeholder), `crates/goverify-ir/src/lower.rs`
- Modify: `crates/goverify-ir/src/program.rs` (call `lower_package`), `src/lib.rs`

**Interfaces:**
- Consumes: `TypeTable::import_package` map, `Program::intern_func` (Task 5); gvir v2 sems (Tasks 3–4).
- Produces (for Tasks 7–15):

```rust
pub struct ValueId(pub u32);
pub enum ValueKind { Param, FreeVar, Const(ConstVal), Global(String),
                     FuncRef(FuncId), Builtin(String), Instr, Opaque }
pub enum ConstVal { Bool(bool), Int(i64), BigInt(String), Float(u64),
                    Str(Vec<u8>), Nil, Complex(String), Opaque }
pub struct ValueInfo { pub ty: TypeId, pub kind: ValueKind }
pub struct Function { pub id: FuncId, pub sig: TypeId, pub params: Vec<ValueId>,
                      pub values: Vec<ValueInfo>, pub blocks: Vec<Block>,
                      pub pos: Option<Pos> }
impl Function { pub fn value(&self, v: ValueId) -> &ValueInfo; }   // total: OOB → Opaque/Unknown
pub struct Block { pub instrs: Vec<Instr>, pub succs: Vec<u32> }
pub struct Instr { pub op: Op, pub pos: Option<Pos> }
pub struct Pos { pub file: String, pub line: u32, pub col: u32 }
```

and the `Op` enum below.

- [ ] **Step 1: Define `op.rs`** (the ~31-op set from spec §3.2):

```rust
//! The analyzer-owned instruction set (phase-2 spec §3.2). Checkers see
//! only these ops — x/tools SSA quirks stop at lower.rs.

use crate::func::ValueId;
use crate::program::FuncId;
use crate::types::TypeId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum BinOpKind {
    Add, Sub, Mul, Div, Rem, And, Or, Xor, Shl, Shr, AndNot,
    Eq, Neq, Lt, Leq, Gt, Geq,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOpKind { Neg, Not, BitNot }

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MakeKind { Chan, Map, Slice }

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LockKind { Lock, Unlock, RLock, RUnlock }

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Callee {
    Static(FuncId),
    /// Interface method call: resolved by the call graph (Task 9).
    Invoke { iface: TypeId, method: String, sig: TypeId },
    Builtin(String),
    /// Function-value call through `value`.
    Dynamic { value: ValueId },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectArm {
    pub dir: u32, // types.ChanDir: 1 send, 2 recv
    pub chan: ValueId,
    pub send: Option<ValueId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    Assign { dst: ValueId, src: ValueId },
    Alloc { dst: ValueId, heap: bool },
    Load { dst: ValueId, addr: ValueId },
    Store { addr: ValueId, val: ValueId },
    FieldAddr { dst: ValueId, base: ValueId, field: u32 },
    Field { dst: ValueId, base: ValueId, field: u32 },
    IndexAddr { dst: ValueId, base: ValueId, index: ValueId },
    Index { dst: ValueId, base: ValueId, index: ValueId },
    Lookup { dst: ValueId, map: ValueId, key: ValueId, comma_ok: bool },
    Slice { dst: ValueId, base: ValueId, low: Option<ValueId>,
            high: Option<ValueId>, max: Option<ValueId> },
    BinOp { dst: ValueId, kind: BinOpKind, lhs: ValueId, rhs: ValueId },
    UnOp { dst: ValueId, kind: UnOpKind, operand: ValueId },
    Convert { dst: ValueId, src: ValueId },
    Extract { dst: ValueId, tuple: ValueId, index: u32 },
    Phi { dst: ValueId, edges: Vec<ValueId> },
    Call { dst: Option<ValueId>, callee: Callee, args: Vec<ValueId> },
    MakeClosure { dst: ValueId, func: FuncId, bindings: Vec<ValueId> },
    MakeInterface { dst: ValueId, src: ValueId },
    Make { dst: ValueId, kind: MakeKind, args: Vec<ValueId> },
    Send { chan: ValueId, val: ValueId },
    Recv { dst: ValueId, chan: ValueId, comma_ok: bool },
    CloseChan { chan: ValueId },
    Select { dst: ValueId, arms: Vec<SelectArm>, blocking: bool },
    Go { callee: Callee, args: Vec<ValueId> },
    Defer { callee: Callee, args: Vec<ValueId> },
    Return { vals: Vec<ValueId> },
    Jump,
    Branch { cond: ValueId },
    Panic { val: ValueId },
    TypeAssert { dst: ValueId, src: ValueId, asserted: TypeId, comma_ok: bool },
    Lock { kind: LockKind, mu: ValueId },
    /// The explicit "not modeled" op. dst is havoc'd when present.
    Havoc { dst: Option<ValueId> },
}
```

- [ ] **Step 2: Write `func.rs`** with the structs from the Interfaces block; `Function::value` clamps out-of-range ids to a shared `ValueInfo { ty: unknown, kind: Opaque }` static-per-function fallback (store one extra entry or return a `const`-constructed reference via `OnceLock` — simplest: `values.get(v.0 as usize).unwrap_or(&self.opaque)` with an `opaque: ValueInfo` field).

- [ ] **Step 3: Write the failing lowering test** (in `lower.rs` `#[cfg(test)]`; build a tiny `gvir::Function` by hand):

```rust
fn test_pkg(instrs: Vec<gvir::Instruction>) -> gvir::Package {
    gvir::Package {
        import_path: "t".into(),
        types: vec![gvir::Type { id: 1, repr: "int".into(),
            kind: gvir::TypeKind::Basic as i32, name: "int".into(), ..Default::default() }],
        functions: vec![gvir::Function {
            id: "t.F".into(),
            params: vec![gvir::Param { id: 1, name: "x".into(), r#type: 1 },
                         gvir::Param { id: 2, name: "y".into(), r#type: 1 }],
            blocks: vec![gvir::BasicBlock { index: 0, instrs, succs: vec![] }],
            ..Default::default()
        }],
        ..Default::default()
    }
}

#[test]
fn lowers_binop_and_return() {
    let pkg = test_pkg(vec![
        gvir::Instruction { kind: "BinOp".into(), register: 3, r#type: 1,
            operands: vec![1, 2],
            sem: Some(gvir::instruction::Sem::Binop(gvir::BinOpSem { op: "+".into() })),
            ..Default::default() },
        gvir::Instruction { kind: "Return".into(), operands: vec![3], ..Default::default() },
    ]);
    let p = Program::from_packages(vec![pkg]);
    let f = p.func(p.lookup_func("t.F").unwrap()).expect("lowered body");
    let ops: Vec<&Op> = f.blocks[0].instrs.iter().map(|i| &i.op).collect();
    assert!(matches!(ops[0], Op::BinOp { kind: BinOpKind::Add, .. }), "{ops:?}");
    assert!(matches!(ops[1], Op::Return { .. }), "{ops:?}");
}

#[test]
fn unknown_kind_lowers_to_havoc_not_panic() {
    let pkg = test_pkg(vec![gvir::Instruction {
        kind: "FrobnicateV9".into(), register: 3, r#type: 1, ..Default::default() }]);
    let p = Program::from_packages(vec![pkg]);
    let f = p.func(p.lookup_func("t.F").unwrap()).unwrap();
    assert!(matches!(f.blocks[0].instrs[0].op, Op::Havoc { dst: Some(_) }));
    assert!(p.diagnostics().iter().any(|d| d.contains("FrobnicateV9")));
}
```

- [ ] **Step 4: Run to verify failure** — `mise x -- cargo test -p goverify-ir lowers_binop` — FAIL.

- [ ] **Step 5: Implement `lower.rs` core.** Structure: `pub(crate) fn lower_function(prog: &mut ProgramBuilderView…)` — practical shape: lowering needs `&mut Program` for interning (funcs/diagnostics) while reading `pkg`; implement as a method on `Program`:

```rust
impl Program {
    pub(crate) fn lower_package(&mut self, pkg: &gvir::Package, tmap: &[TypeId]) {
        for gf in &pkg.functions {
            if gf.blocks.is_empty() {
                continue; // bodyless: stays None (external)
            }
            let f = self.lower_function(pkg, gf, tmap);
            let id = self.intern_func(&gf.id);
            self.funcs[id.0 as usize] = Some(f);
        }
    }
}
```

`lower_function` builds the value table then the blocks:

```rust
fn resolve_ty(tmap: &[TypeId], unknown: TypeId, local: u32) -> TypeId {
    tmap.get(local as usize).copied().unwrap_or(unknown)
}

impl Program {
    fn lower_function(&mut self, pkg: &gvir::Package, gf: &gvir::Function,
                      tmap: &[TypeId]) -> Function {
        let unknown = self.types.unknown();
        let id = self.intern_func(&gf.id);
        // Value table: index = .gvir value id (1-based; slot 0 = opaque).
        let max_id = value_id_ceiling(gf); // max over params/aux/registers
        let mut values = vec![ValueInfo { ty: unknown, kind: ValueKind::Opaque }; max_id + 1];
        let mut params = Vec::with_capacity(gf.params.len());
        for p in &gf.params {
            if let Some(slot) = values.get_mut(p.id as usize) {
                *slot = ValueInfo { ty: resolve_ty(tmap, unknown, p.r#type),
                                    kind: ValueKind::Param };
                params.push(ValueId(p.id));
            }
        }
        for a in &gf.aux {
            let kind = match a.kind.as_str() {
                "Const" => ValueKind::Const(lower_const(a)),
                "Global" => ValueKind::Global(a.repr.clone()),
                "Function" => ValueKind::FuncRef(self.intern_func(&a.repr)),
                "Builtin" => ValueKind::Builtin(a.repr.clone()),
                "FreeVar" => ValueKind::FreeVar,
                _ => ValueKind::Opaque,
            };
            if let Some(slot) = values.get_mut(a.id as usize) {
                *slot = ValueInfo { ty: resolve_ty(tmap, unknown, a.r#type), kind };
            }
        }
        // register slots get ValueKind::Instr + result type
        for b in &gf.blocks {
            for ins in &b.instrs {
                if ins.register != 0 {
                    if let Some(slot) = values.get_mut(ins.register as usize) {
                        *slot = ValueInfo { ty: resolve_ty(tmap, unknown, ins.r#type),
                                            kind: ValueKind::Instr };
                    }
                }
            }
        }
        let blocks = gf.blocks.iter().map(|b| Block {
            succs: b.succs.clone(),
            instrs: b.instrs.iter()
                .filter_map(|ins| self.lower_instr(gf, ins, tmap))
                .collect(),
        }).collect();
        Function { id, sig: resolve_ty(tmap, unknown, gf.r#type), params, values, blocks,
                   pos: lower_pos(pkg, &gf.pos),
                   opaque: ValueInfo { ty: unknown, kind: ValueKind::Opaque } }
    }
}
```

`lower_const` maps `ConstValue` → `ConstVal` (`None` payload → `Opaque`).
`lower_pos` resolves `Position.file` through `pkg.files` (bounds-checked;
0 or out-of-range → `None`… file unknown but line present: keep
`Pos { file: String::new(), line, col }`).

`lower_instr` (the heart — this task covers the non-call ops; calls/concurrency in Task 7). Helper accessors: `op0/op1/op2` read `ins.operands.get(n)` returning `ValueId(0)` (the opaque slot) when absent — total, never panics. `dst()` = `ins.register != 0` ⇒ `Some(ValueId(register))`.

```rust
fn lower_instr(&mut self, gf: &gvir::Function, ins: &gvir::Instruction,
               tmap: &[TypeId]) -> Option<Instr> {
    use gvir::instruction::Sem;
    let v = |i: usize| ValueId(ins.operands.get(i).copied().unwrap_or(0));
    let vopt = |i: usize| ins.operands.get(i).copied()
        .filter(|&x| x != 0).map(ValueId);
    let dst = (ins.register != 0).then(|| ValueId(ins.register));
    let havoc = |p: &mut Program| { // shared fallback
        p.push_diagnostic(format!("{}: unmodeled instruction kind {:?}", gf.id, ins.kind));
        Some(Op::Havoc { dst })
    };
    let op = match ins.kind.as_str() {
        "Alloc" => Some(Op::Alloc { dst: dst?,
            heap: matches!(&ins.sem, Some(Sem::Alloc(a)) if a.heap) }),
        "BinOp" => match &ins.sem {
            Some(Sem::Binop(b)) => match binop_kind(&b.op) {
                Some(kind) => Some(Op::BinOp { dst: dst?, kind, lhs: v(0), rhs: v(1) }),
                None => havoc(self),
            },
            _ => havoc(self),
        },
        "UnOp" => match &ins.sem {
            Some(Sem::Unop(u)) => match u.op.as_str() {
                "*" => Some(Op::Load { dst: dst?, addr: v(0) }),
                "<-" => Some(Op::Recv { dst: dst?, chan: v(0), comma_ok: u.comma_ok }),
                "-" => Some(Op::UnOp { dst: dst?, kind: UnOpKind::Neg, operand: v(0) }),
                "!" => Some(Op::UnOp { dst: dst?, kind: UnOpKind::Not, operand: v(0) }),
                "^" => Some(Op::UnOp { dst: dst?, kind: UnOpKind::BitNot, operand: v(0) }),
                _ => havoc(self),
            },
            _ => havoc(self),
        },
        "Store" => Some(Op::Store { addr: v(0), val: v(1) }),
        "FieldAddr" | "Field" => {
            let idx = match &ins.sem { Some(Sem::Field(f)) => f.index, _ => 0 };
            if ins.kind == "FieldAddr" {
                Some(Op::FieldAddr { dst: dst?, base: v(0), field: idx })
            } else {
                Some(Op::Field { dst: dst?, base: v(0), field: idx })
            }
        }
        "IndexAddr" => Some(Op::IndexAddr { dst: dst?, base: v(0), index: v(1) }),
        "Index" => Some(Op::Index { dst: dst?, base: v(0), index: v(1) }),
        "Lookup" => Some(Op::Lookup { dst: dst?, map: v(0), key: v(1),
            comma_ok: matches!(&ins.sem, Some(Sem::Lookup(l)) if l.comma_ok) }),
        "Slice" => Some(Op::Slice { dst: dst?, base: v(0),
            low: vopt(1), high: vopt(2), max: vopt(3) }),
        "Convert" | "ChangeInterface" | "SliceToArrayPointer" | "MultiConvert" =>
            Some(Op::Convert { dst: dst?, src: v(0) }),
        "ChangeType" => Some(Op::Assign { dst: dst?, src: v(0) }),
        "Extract" => Some(Op::Extract { dst: dst?, tuple: v(0),
            index: match &ins.sem { Some(Sem::Extract(e)) => e.index, _ => 0 } }),
        "Phi" => Some(Op::Phi { dst: dst?,
            edges: ins.operands.iter().map(|&o| ValueId(o)).collect() }),
        "MakeInterface" => Some(Op::MakeInterface { dst: dst?, src: v(0) }),
        "MakeChan" => Some(Op::Make { dst: dst?, kind: MakeKind::Chan,
            args: vec![v(0)] }),
        "MakeMap" => Some(Op::Make { dst: dst?, kind: MakeKind::Map,
            args: ins.operands.iter().map(|&o| ValueId(o)).collect() }),
        "MakeSlice" => Some(Op::Make { dst: dst?, kind: MakeKind::Slice,
            args: vec![v(0), v(1)] }),
        "MapUpdate" => Some(Op::Store { addr: v(0), val: v(2) }), // map[k]=v as opaque store
        "Return" => Some(Op::Return {
            vals: ins.operands.iter().map(|&o| ValueId(o)).collect() }),
        "Jump" => Some(Op::Jump),
        "If" => Some(Op::Branch { cond: v(0) }),
        "Panic" => Some(Op::Panic { val: v(0) }),
        "TypeAssert" => match &ins.sem {
            Some(Sem::TypeAssert(t)) => Some(Op::TypeAssert { dst: dst?, src: v(0),
                asserted: resolve_ty(tmap, self.types_unknown(), t.asserted),
                comma_ok: t.comma_ok }),
            _ => havoc(self),
        },
        "Send" => Some(Op::Send { chan: v(0), val: v(1) }),
        "Range" | "Next" => Some(Op::Havoc { dst }), // spec §3.3: loop primitives havoc
        "DebugRef" | "RunDefers" => None,            // dropped: no analyzer-visible semantics
        // Call/Defer/Go/Select/MakeClosure: Task 7
        _ => havoc(self),
    };
    // `dst?` above: a value-producing kind arriving without a register is
    // malformed input — treat as havoc-no-dst, not None:
    let op = op.or(Some(Op::Havoc { dst: None }));
    ...
}
```

**Correction to the sketch above** (make the plan's code exact): the `dst?`
early-return conflicts with the `.or(...)` recovery — instead of `Option`
chaining via `?`, write a small macro or match `dst` explicitly per arm:
`let Some(d) = dst else { return fallback_havoc(self, gf, ins); }` at the
top of each value-producing arm, where `fallback_havoc` pushes the
diagnostic and returns `Some(Instr { op: Op::Havoc { dst: None }, pos })`.
The invariant to implement: **every arm returns Some(instr) or None
(dropped), and no input can panic.** `DebugRef`/`RunDefers` are the only
`None`s; they carry no analyzer-visible semantics (defers are recorded at
the `defer` op itself).

`binop_kind` maps the 17 token strings (`"+" "-" "*" "/" "%" "&" "|" "^"
"<<" ">>" "&^" "==" "!=" "<" "<=" ">" ">="`) to `BinOpKind`; unknown → `None`.

Temporarily route `"Call" | "Defer" | "Go" | "Select" | "MakeClosure"` to
`havoc(self)` so this task is green; Task 7 replaces them.

Finally, enable lowering in `from_packages` (Task 5 left the hook commented): `p.lower_package(pkg, &tmap);`

- [ ] **Step 6: Run tests**

Run: `mise x -- cargo test -p goverify-ir`
Expected: PASS.

- [ ] **Step 7: Lint + commit**

```bash
git add crates/goverify-ir/
git commit -m "ir: op set + value tables + core lowering (calls land next)"
```

---

### Task 7: goverify-ir — call, closure, concurrency, and intrinsic lowering

**Files:**
- Modify: `crates/goverify-ir/src/lower.rs`

**Interfaces:**
- Consumes: `CallSem`/`SelectSem` (Task 4), `Callee`, `LockKind` (Task 6).
- Produces: `Op::Call/Go/Defer` with precise `Callee`, `Op::MakeClosure`, `Op::Select`, `Op::CloseChan`, `Op::Lock` — the complete op surface for Tasks 8–14.

- [ ] **Step 1: Write the failing tests** (extend `lower.rs` tests; hand-built packages as in Task 6):

```rust
#[test]
fn lowers_static_call_and_lock_intrinsics() {
    // aux id 3 = Function "(*sync.Mutex).Lock"; call it with operand order
    // [callee, receiver]; also a plain static call to t.G.
    let pkg = gvir::Package {
        import_path: "t".into(),
        types: vec![gvir::Type { id: 1, repr: "*sync.Mutex".into(),
            kind: gvir::TypeKind::Pointer as i32, ..Default::default() }],
        functions: vec![gvir::Function {
            id: "t.F".into(),
            params: vec![gvir::Param { id: 1, name: "mu".into(), r#type: 1 }],
            blocks: vec![gvir::BasicBlock { index: 0, instrs: vec![
                gvir::Instruction { kind: "Call".into(), operands: vec![0, 1],
                    sem: Some(gvir::instruction::Sem::Call(gvir::CallSem {
                        static_callee: "(*sync.Mutex).Lock".into(), ..Default::default() })),
                    ..Default::default() },
                gvir::Instruction { kind: "Call".into(), operands: vec![0],
                    sem: Some(gvir::instruction::Sem::Call(gvir::CallSem {
                        static_callee: "t.G".into(), ..Default::default() })),
                    ..Default::default() },
                gvir::Instruction { kind: "Return".into(), ..Default::default() },
            ], succs: vec![] }],
            ..Default::default()
        }],
        ..Default::default()
    };
    let p = Program::from_packages(vec![pkg]);
    let f = p.func(p.lookup_func("t.F").unwrap()).unwrap();
    let ops: Vec<&Op> = f.blocks[0].instrs.iter().map(|i| &i.op).collect();
    assert!(matches!(ops[0], Op::Lock { kind: LockKind::Lock, .. }), "{ops:?}");
    assert!(matches!(ops[1], Op::Call { callee: Callee::Static(_), .. }), "{ops:?}");
}

#[test]
fn lowers_builtin_close_to_closechan() {
    // Call with sem.builtin = "close", operands [callee, ch]
    // → Op::CloseChan { chan }
    // (build analogous to the test above; assert matches!(op, Op::CloseChan { .. }))
}
```

Write `lowers_builtin_close_to_closechan` fully, following the same
construction pattern with `builtin: "close".into()` in the `CallSem` and a
chan-typed param.

- [ ] **Step 2: Run to verify failure** — the ops come out as `Havoc` (Task 6's temporary routing).

- [ ] **Step 3: Implement.** Replace the temporary arms:

```rust
"Call" | "Defer" | "Go" => {
    let Some(Sem::Call(c)) = &ins.sem else { return fallback_havoc(self, gf, ins); };
    // SSA operand layout: non-invoke: [callee, args…]; invoke: [recv, args…].
    let (callee, args) = if c.invoke {
        (Callee::Invoke {
            iface: resolve_ty(tmap, unknown, c.iface_type),
            method: c.method.clone(),
            sig: resolve_ty(tmap, unknown, c.method_sig),
        }, ins.operands.iter().map(|&o| ValueId(o)).collect::<Vec<_>>())
    } else if !c.builtin.is_empty() {
        (Callee::Builtin(c.builtin.clone()),
         ins.operands.iter().skip(1).map(|&o| ValueId(o)).collect())
    } else if !c.static_callee.is_empty() {
        (Callee::Static(self.intern_func(&c.static_callee)),
         ins.operands.iter().skip(1).map(|&o| ValueId(o)).collect())
    } else {
        (Callee::Dynamic { value: v(0) },
         ins.operands.iter().skip(1).map(|&o| ValueId(o)).collect())
    };
    match ins.kind.as_str() {
        "Go" => Some(Op::Go { callee, args }),
        "Defer" => Some(Op::Defer { callee, args }),
        _ => lower_plain_call(dst, callee, args), // intrinsic rewrites below
    }
}
"MakeClosure" => {
    // operands: [fn, bindings…]; fn is a Function aux value.
    let Some(d) = dst else { return fallback_havoc(self, gf, ins); };
    let ValueKind::FuncRef(func) = self.value_kind_of(gf, ins, 0) else {
        return fallback_havoc(self, gf, ins);
    };
    Some(Op::MakeClosure { dst: d, func,
        bindings: ins.operands.iter().skip(1).map(|&o| ValueId(o)).collect() })
}
"Select" => {
    let Some(Sem::Select(s)) = &ins.sem else { return fallback_havoc(self, gf, ins); };
    let Some(d) = dst else { return fallback_havoc(self, gf, ins); };
    Some(Op::Select { dst: d, blocking: s.blocking,
        arms: s.states.iter().map(|st| SelectArm {
            dir: st.dir, chan: ValueId(st.chan_operand),
            send: (st.send_operand != 0).then(|| ValueId(st.send_operand)),
        }).collect() })
}
```

`lower_plain_call` rewrites intrinsics:

```rust
fn lock_kind(name: &str) -> Option<LockKind> {
    match name {
        "(*sync.Mutex).Lock" | "(*sync.RWMutex).Lock" => Some(LockKind::Lock),
        "(*sync.Mutex).Unlock" | "(*sync.RWMutex).Unlock" => Some(LockKind::Unlock),
        "(*sync.RWMutex).RLock" => Some(LockKind::RLock),
        "(*sync.RWMutex).RUnlock" => Some(LockKind::RUnlock),
        _ => None,
    }
}

fn lower_plain_call(&self, dst: Option<ValueId>, callee: Callee, args: Vec<ValueId>)
    -> Option<Op> {
    if let Callee::Static(f) = callee {
        if let Some(kind) = lock_kind(self.func_name(f)) {
            return Some(Op::Lock { kind, mu: args.first().copied().unwrap_or(ValueId(0)) });
        }
    }
    if let Callee::Builtin(name) = &callee {
        if name == "close" {
            return Some(Op::CloseChan { chan: args.first().copied().unwrap_or(ValueId(0)) });
        }
    }
    Some(Op::Call { dst, callee, args })
}
```

`value_kind_of(gf, ins, operand_index)` looks the operand id up in the
already-built value table (the aux entries were materialized before block
lowering in Task 6's `lower_function` — pass `&values` into `lower_instr`
instead of re-deriving; adjust the signature to
`fn lower_instr(&mut self, gf, ins, tmap, values: &[ValueInfo]) -> Option<Instr>`).

- [ ] **Step 4: Run tests** — `mise x -- cargo test -p goverify-ir` — PASS.

- [ ] **Step 5: Whole-corpus smoke.** First create the shared test helper `crates/goverify-ir/src/testutil.rs` (public but `#[doc(hidden)]`; every later integration test — Tasks 8, 9, 14, 16 — uses it, so it lives in `src`, not copy-pasted `tests/` modules):

```rust
//! Integration-test helpers: extract a corpus module through the real
//! sidecar and load it. Not part of the analyzer API.

use std::path::{Path, PathBuf};

use goverify_extract::Sidecar;

use crate::program::Program;

pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

/// Extract testdata/corpus/<module> (whole DAG) into a kept temp dir and
/// load it. Panics on failure — test-only code.
pub fn load_corpus(module: &str) -> Program {
    let root = repo_root();
    let sc = Sidecar::build(&root.join("extractor"), &root.join("target/extractor-bin"))
        .expect("Sidecar::build");
    let dir = tempfile::tempdir().expect("tempdir").keep();
    sc.extract(&root.join("testdata/corpus").join(module), &["./..."], &dir)
        .expect("extract");
    Program::load_dir(&dir).expect("load_dir")
}
```

Wire-up: `lib.rs` gets `#[doc(hidden)] pub mod testutil;`; `tempfile` moves
from `[dev-dependencies]` to a regular dependency of `goverify-ir`
(test-only helpers in `src` need it; it's already a workspace dep, and the
cost is accepted for phase 2 — note it in the commit message).

Then the integration test `crates/goverify-ir/tests/lower_corpus.rs`:

```rust
//! Lowering totality over the real corpus: extract conc (whole DAG,
//! sync + runtime deps included), lower everything, count havocs.

use goverify_ir::{Program, testutil};

#[test]
fn lowers_conc_corpus_with_full_dag() {
    let p: Program = testutil::load_corpus("conc");
    let close = p.lookup_func("(*example.com/conc.file).Close").expect("Close lowered");
    assert!(p.func(close).is_some(), "Close must have a body");
    // Every function lowered; havoc diagnostics are allowed but bounded.
    let havoc_diags = p.diagnostics().iter().filter(|d| d.contains("unmodeled")).count();
    assert!(havoc_diags < 200, "unexpected havoc explosion: {havoc_diags}");
}
```

Run: `mise x -- cargo test -p goverify-ir --test lower_corpus`
Expected: PASS (this pulls sync/runtime stdlib through the full pipeline — first real whole-DAG exercise).

- [ ] **Step 6: Lint + commit**

```bash
git add crates/goverify-ir/
git commit -m "ir: call/closure/select lowering + lock and close intrinsics"
```

---

### Task 8: goverify-ir — canonical IR dump + first golden

**Files:**
- Create: `crates/goverify-ir/src/dump.rs`, `crates/goverify-ir/tests/lower_golden.rs`, `testdata/goldens/hello.ir.txt`
- Modify: `crates/goverify-ir/src/lib.rs`

**Interfaces:**
- Consumes: `Program`, `Function`, `Op` (Tasks 5–7).
- Produces: `dump_function(p, f) -> String` — canonical text; used by Task 15's CLI, Task 16's determinism suite, and these goldens.

Dump format (fixed, documented in `dump.rs`'s header): one function per
stanza, values printed as `v<N>`, types as their repr in parens only for
params, ops one per line indented two spaces, blocks labeled `b<N>` with
their successor list. Example:

```
func example.com/hello.Add (v1 int, v2 int)
  b0 -> []
    v3 = binop Add v1 v2
    return v3
```

Rules: struct field ops print the resolved field name after `#index` when
the base's type resolves (`field-addr v1 #0 mu`); calls print the callee
(`call t.G(v2)` / `call-invoke io.Closer.Close(v1)` / `call-builtin len(v1)`
/ `call-dyn v4(v2)`); consts print inline where they're defined? No —
consts are values; the value table is printed as a header line per
function listing non-instr values: `  aux v4 = const 1`, `  aux v5 = func t.G`,
sorted by value id. Deterministic by construction (everything iterates
vectors, never maps).

- [ ] **Step 1: Write the failing golden test** (`tests/lower_golden.rs`):

```rust
//! Curated golden dumps (phase-2 spec §9.4). Byte-exact. Regenerate with
//! UPDATE_GOLDENS=1 after intentional lowering/dump changes and review
//! the diff by hand.

use goverify_ir::{dump_function, testutil};

fn dump_module(module: &str, import_path: &str) -> String {
    let p = testutil::load_corpus(module);
    let mut s = String::new();
    for f in p.func_ids() {
        // Golden covers only the module's own package — stdlib dumps
        // would churn with Go toolchain bumps.
        if p.func(f).is_some() && p.func_name(f).contains(import_path) {
            s.push_str(&dump_function(&p, f));
            s.push('\n');
        }
    }
    s
}

#[test]
fn hello_ir_matches_golden() {
    testutil::check_golden("hello.ir.txt", &dump_module("hello", "example.com/hello"));
}
```

And add `check_golden` to `src/testutil.rs` (Task 7 created it):

```rust
/// Byte-exact golden comparison. UPDATE_GOLDENS=1 rewrites the file;
/// always review the diff by hand before committing.
pub fn check_golden(name: &str, got: &str) {
    let path = repo_root().join("testdata/goldens").join(name);
    if std::env::var_os("UPDATE_GOLDENS").is_some() {
        std::fs::write(&path, got).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("missing golden {name} ({e}); run with UPDATE_GOLDENS=1"));
    assert_eq!(want, got, "golden {name} drifted; review + UPDATE_GOLDENS=1 if intended");
}
```

- [ ] **Step 2: Implement `dump.rs`.**

```rust
//! Canonical text renderings of the IR (phase-2 spec §7). These strings
//! are a determinism surface: byte-compared across runs in CI. Iterate
//! vectors only — a HashMap iteration here is a bug.

use std::fmt::Write;

use crate::func::{ValueId, ValueKind};
use crate::op::{Callee, Op};
use crate::program::{FuncId, Program};

pub fn dump_function(p: &Program, id: FuncId) -> String {
    let Some(f) = p.func(id) else {
        return format!("func {} <external>\n", p.func_name(id));
    };
    let mut s = String::new();
    let params = f.params.iter()
        .map(|&v| format!("v{} {}", v.0, p.types().repr(f.value(v).ty)))
        .collect::<Vec<_>>().join(", ");
    let _ = writeln!(s, "func {} ({params})", p.func_name(id));
    for (i, info) in f.values.iter().enumerate() {
        match &info.kind {
            ValueKind::Const(c) => { let _ = writeln!(s, "  aux v{i} = const {c:?}"); }
            ValueKind::Global(g) => { let _ = writeln!(s, "  aux v{i} = global {g}"); }
            ValueKind::FuncRef(fid) => {
                let _ = writeln!(s, "  aux v{i} = func {}", p.func_name(*fid));
            }
            ValueKind::Builtin(b) => { let _ = writeln!(s, "  aux v{i} = builtin {b}"); }
            _ => {}
        }
    }
    for (bi, b) in f.blocks.iter().enumerate() {
        let _ = writeln!(s, "  b{bi} -> {:?}", b.succs);
        for ins in &b.instrs {
            let _ = writeln!(s, "    {}", render_op(p, &ins.op));
        }
    }
    s
}

fn render_callee(p: &Program, c: &Callee) -> String {
    match c {
        Callee::Static(f) => format!("call {}", p.func_name(*f)),
        Callee::Invoke { iface, method, .. } =>
            format!("call-invoke {}.{method}", p.types().repr(*iface)),
        Callee::Builtin(b) => format!("call-builtin {b}"),
        Callee::Dynamic { value } => format!("call-dyn v{}", value.0),
    }
}

fn vlist(vs: &[ValueId]) -> String {
    vs.iter().map(|v| format!("v{}", v.0)).collect::<Vec<_>>().join(" ")
}

fn render_op(p: &Program, op: &Op) -> String {
    match op {
        Op::Assign { dst, src } => format!("v{} = assign v{}", dst.0, src.0),
        Op::Alloc { dst, heap } => format!("v{} = alloc heap={heap}", dst.0),
        Op::Load { dst, addr } => format!("v{} = load v{}", dst.0, addr.0),
        Op::Store { addr, val } => format!("store v{} <- v{}", addr.0, val.0),
        Op::FieldAddr { dst, base, field } =>
            format!("v{} = field-addr v{} #{field}", dst.0, base.0),
        Op::Field { dst, base, field } =>
            format!("v{} = field v{} #{field}", dst.0, base.0),
        Op::IndexAddr { dst, base, index } =>
            format!("v{} = index-addr v{} v{}", dst.0, base.0, index.0),
        Op::Index { dst, base, index } =>
            format!("v{} = index v{} v{}", dst.0, base.0, index.0),
        Op::Lookup { dst, map, key, comma_ok } =>
            format!("v{} = lookup v{} v{} ok={comma_ok}", dst.0, map.0, key.0),
        Op::Slice { dst, base, low, high, max } => format!(
            "v{} = slice v{} [{}:{}:{}]", dst.0, base.0,
            low.map_or(String::new(), |v| format!("v{}", v.0)),
            high.map_or(String::new(), |v| format!("v{}", v.0)),
            max.map_or(String::new(), |v| format!("v{}", v.0))),
        Op::BinOp { dst, kind, lhs, rhs } =>
            format!("v{} = binop {kind:?} v{} v{}", dst.0, lhs.0, rhs.0),
        Op::UnOp { dst, kind, operand } =>
            format!("v{} = unop {kind:?} v{}", dst.0, operand.0),
        Op::Convert { dst, src } => format!("v{} = convert v{}", dst.0, src.0),
        Op::Extract { dst, tuple, index } =>
            format!("v{} = extract v{} #{index}", dst.0, tuple.0),
        Op::Phi { dst, edges } => format!("v{} = phi {}", dst.0, vlist(edges)),
        Op::Call { dst, callee, args } => match dst {
            Some(d) => format!("v{} = {}({})", d.0, render_callee(p, callee), vlist(args)),
            None => format!("{}({})", render_callee(p, callee), vlist(args)),
        },
        Op::MakeClosure { dst, func, bindings } =>
            format!("v{} = make-closure {} [{}]", dst.0, p.func_name(*func), vlist(bindings)),
        Op::MakeInterface { dst, src } => format!("v{} = make-interface v{}", dst.0, src.0),
        Op::Make { dst, kind, args } =>
            format!("v{} = make {kind:?} {}", dst.0, vlist(args)),
        Op::Send { chan, val } => format!("send v{} <- v{}", chan.0, val.0),
        Op::Recv { dst, chan, comma_ok } =>
            format!("v{} = recv v{} ok={comma_ok}", dst.0, chan.0),
        Op::CloseChan { chan } => format!("close v{}", chan.0),
        Op::Select { dst, arms, blocking } => format!(
            "v{} = select blocking={blocking} [{}]", dst.0,
            arms.iter().map(|a| match a.send {
                Some(sv) => format!("send v{} <- v{}", a.chan.0, sv.0),
                None => format!("recv v{}", a.chan.0),
            }).collect::<Vec<_>>().join(", ")),
        Op::Go { callee, args } => format!("go {}({})", render_callee(p, callee), vlist(args)),
        Op::Defer { callee, args } =>
            format!("defer {}({})", render_callee(p, callee), vlist(args)),
        Op::Return { vals } => format!("return {}", vlist(vals)),
        Op::Jump => "jump".to_string(),
        Op::Branch { cond } => format!("branch v{}", cond.0),
        Op::Panic { val } => format!("panic v{}", val.0),
        Op::TypeAssert { dst, src, asserted, comma_ok } => format!(
            "v{} = type-assert v{} {} ok={comma_ok}",
            dst.0, src.0, p.types().repr(*asserted)),
        Op::Lock { kind, mu } => format!("{kind:?} v{}", mu.0).to_lowercase(),
        Op::Havoc { dst } => match dst {
            Some(d) => format!("v{} = havoc", d.0),
            None => "havoc".to_string(),
        },
    }
}
```

(`ConstVal` needs a manual compact `Debug`/`Display` — derive `Debug` is
acceptable for phase 2, it's deterministic.)

- [ ] **Step 3: Generate + review the golden**

Run: `UPDATE_GOLDENS=1 mise x -- cargo test -p goverify-ir --test lower_golden`
Then **read `testdata/goldens/hello.ir.txt` by hand** and verify it renders
`example.com/hello.Add` as expected (binop Add of the two params, return).

- [ ] **Step 4: Run without UPDATE_GOLDENS** — PASS.

- [ ] **Step 5: Lint + commit**

```bash
git add crates/goverify-ir/ testdata/goldens/
git commit -m "ir: canonical dump + hello golden"
```

---

### Task 9: goverify-ir — call graph

**Files:**
- Create: `crates/goverify-ir/src/callgraph.rs`
- Modify: `crates/goverify-ir/src/lib.rs`, `src/dump.rs` (add `dump_callgraph`)

**Interfaces:**
- Consumes: `Program` (`func_ids`, `func`, `method_sets`), `Op::{Call,Go,Defer,MakeClosure}`, `Callee` (Tasks 5–7).
- Produces: `CallGraph::build(p) -> CallGraph`, `CallGraph::callees(f) -> &[FuncId]` (sorted, deduped), `dump_callgraph(p, g) -> String`. Consumed by Tasks 10, 13–15.

Resolution rules (phase-2 spec §4.1):
- `Callee::Static(f)` → edge to `f`.
- `Callee::Invoke { iface, method, sig }` → if `iface` has a method set in
  `p.method_sets` (named interface), edges to every concrete type whose
  method set **includes** all of the interface's `(name, sig)` pairs, at
  the invoked method; else (anonymous iface) fall back to every concrete
  method with matching `(method, sig)` anywhere.
- `Callee::Dynamic { value }` → edges to every address-taken function whose
  signature TypeId equals the callee value's type. Address-taken = appears
  as `ValueKind::FuncRef` in any function's value table where it is *used
  by at least one op that is not the rewritten callee* — after Task 7's
  lowering, static callees are inside `Callee::Static`, not operands, so
  **any** remaining `FuncRef` operand/binding occurrence is address-taken.
  Collect: scan every op's value operands; a `FuncRef` seen there, plus
  every `MakeClosure { func }`, is address-taken.

- [ ] **Step 1: Write the failing test** (`tests/callgraph_corpus.rs`, using `testutil` from Task 7):

```rust
use goverify_ir::{CallGraph, testutil};

#[test]
fn invoke_call_resolves_to_concrete_impl() {
    let p = testutil::load_corpus("conc");
    let g = CallGraph::build(&p);
    let call_all = p.lookup_func("example.com/conc.CloseAll").unwrap();
    let close = p.lookup_func("(*example.com/conc.file).Close").unwrap();
    assert!(
        g.callees(call_all).contains(&close),
        "CloseAll must edge to (*file).Close via implements-approximation; got {:?}",
        g.callees(call_all).iter().map(|&f| p.func_name(f)).collect::<Vec<_>>()
    );
}

#[test]
fn go_closure_edges_to_the_closure_body() {
    let p = testutil::load_corpus("conc");
    let g = CallGraph::build(&p);
    let fan = p.lookup_func("example.com/conc.Fan").unwrap();
    let anon = g.callees(fan).iter()
        .any(|&f| p.func_name(f).starts_with("example.com/conc.Fan$"));
    assert!(anon, "Fan must edge to its goroutine closure Fan$1");
}
```

- [ ] **Step 2: Run to verify failure** — `CallGraph` doesn't exist.

- [ ] **Step 3: Implement `callgraph.rs`.**

```rust
//! Whole-DAG call graph (phase-2 spec §4.1). Static edges are precise;
//! invoke edges use implements-based approximation over method sets;
//! function-value edges use address-taken × signature matching. Extra
//! edges only widen summaries — they never invent findings.

use std::collections::{BTreeSet, HashMap, HashSet};

use crate::func::{ValueId, ValueKind};
use crate::op::{Callee, Op};
use crate::program::{FuncId, MethodInfo, Program};
use crate::types::TypeId;

pub struct CallGraph {
    callees: Vec<Vec<FuncId>>, // indexed by FuncId
}

impl CallGraph {
    pub fn callees(&self, f: FuncId) -> &[FuncId] {
        self.callees.get(f.0 as usize).map_or(&[], Vec::as_slice)
    }

    pub fn build(p: &Program) -> CallGraph {
        // Index 1: (method name, sig) → [(owner type methods, concrete FuncId)]
        // Index 2: address-taken functions grouped by signature TypeId.
        let mut by_name_sig: HashMap<(&str, TypeId), Vec<(&Vec<MethodInfo>, FuncId)>> =
            HashMap::new();
        for methods in p.method_sets.values() {
            if methods.iter().any(|m| m.func.is_none()) {
                continue; // interface set: not a concrete implementer
            }
            for m in methods {
                if let Some(f) = m.func {
                    by_name_sig.entry((m.name.as_str(), m.sig)).or_default()
                        .push((methods, f));
                }
            }
        }
        let mut address_taken: HashMap<TypeId, BTreeSet<FuncId>> = HashMap::new();
        for id in p.func_ids() {
            let Some(f) = p.func(id) else { continue };
            for b in &f.blocks {
                for ins in &b.instrs {
                    let mut mark = |v: ValueId| {
                        if let ValueKind::FuncRef(target) = f.value(v).kind {
                            address_taken.entry(f.value(v).ty).or_default().insert(target);
                        }
                    };
                    match &ins.op {
                        Op::MakeClosure { func, dst, .. } => {
                            address_taken.entry(f.value(*dst).ty).or_default().insert(*func);
                        }
                        op => for v in op_value_operands(op) { mark(v); },
                    }
                }
            }
        }
        let n = p.func_ids().count();
        let mut callees: Vec<BTreeSet<FuncId>> = vec![BTreeSet::new(); n];
        for id in p.func_ids() {
            let Some(f) = p.func(id) else { continue };
            let out = &mut callees[id.0 as usize];
            for b in &f.blocks {
                for ins in &b.instrs {
                    let callee = match &ins.op {
                        Op::Call { callee, .. } | Op::Go { callee, .. }
                        | Op::Defer { callee, .. } => callee,
                        _ => continue,
                    };
                    match callee {
                        Callee::Static(t) => { out.insert(*t); }
                        Callee::Builtin(_) => {}
                        Callee::Invoke { iface, method, sig } => {
                            resolve_invoke(p, &by_name_sig, *iface, method, *sig, out);
                        }
                        Callee::Dynamic { value } => {
                            if let Some(set) = address_taken.get(&f.value(*value).ty) {
                                out.extend(set.iter().copied());
                            }
                        }
                    }
                }
            }
        }
        CallGraph { callees: callees.into_iter().map(|s| s.into_iter().collect()).collect() }
    }
}

fn resolve_invoke(
    p: &Program,
    by_name_sig: &HashMap<(&str, TypeId), Vec<(&Vec<MethodInfo>, FuncId)>>,
    iface: TypeId,
    method: &str,
    sig: TypeId,
    out: &mut BTreeSet<FuncId>,
) {
    let Some(candidates) = by_name_sig.get(&(method, sig)) else { return };
    // Interface's own method set, when known, filters candidates to true
    // implementers (method-set inclusion).
    let iface_ms: Option<&Vec<MethodInfo>> = p.method_sets.get(&iface)
        .filter(|ms| ms.iter().all(|m| m.func.is_none()));
    for (impl_ms, f) in candidates {
        let implements = match iface_ms {
            Some(req) => req.iter().all(|rm| impl_ms.iter()
                .any(|im| im.name == rm.name && im.sig == rm.sig)),
            None => true, // anonymous iface: name+sig fallback
        };
        if implements {
            out.insert(*f);
        }
    }
}

/// Every ValueId an op reads (not defines). Add arms for ALL Op variants;
/// the compiler's exhaustiveness check is the point — a new op can't
/// silently hide function references.
fn op_value_operands(op: &Op) -> Vec<ValueId> { /* exhaustive match */ }
```

Write `op_value_operands` exhaustively (every variant, listing its read
operands; `Havoc`/`Jump` → `vec![]`). ~40 lines of mechanical match arms.

`dump_callgraph` (in `dump.rs`): one line per function with any edges,
sorted by caller name, callees sorted by name:

```rust
pub fn dump_callgraph(p: &Program, g: &CallGraph) -> String {
    let mut lines: Vec<String> = p.func_ids()
        .filter(|&f| !g.callees(f).is_empty())
        .map(|f| {
            let mut names: Vec<&str> = g.callees(f).iter()
                .map(|&c| p.func_name(c)).collect();
            names.sort_unstable();
            format!("{} -> {}", p.func_name(f), names.join(", "))
        })
        .collect();
    lines.sort_unstable();
    lines.join("\n") + "\n"
}
```

- [ ] **Step 4: Run tests** — both corpus tests PASS.

- [ ] **Step 5: Lint + commit**

```bash
git add crates/goverify-ir/
git commit -m "ir: call graph — static, invoke (implements-based), dynamic (address-taken)"
```

---

### Task 10: goverify-ir — Tarjan SCCs and the deterministic schedule

**Files:**
- Create: SCC code appended to `crates/goverify-ir/src/callgraph.rs`
- Modify: `crates/goverify-ir/src/dump.rs` (`dump_sccs`), `src/lib.rs`

**Interfaces:**
- Consumes: `CallGraph` (Task 9).
- Produces: `Sccs::compute(p, g) -> Sccs`; `Sccs::schedule() -> &[Vec<FuncId>]`
  — SCCs in callees-first (reverse-topological) order, members sorted by
  FuncId; `Sccs::scc_of(f) -> usize`; `Sccs::callee_sccs(i) -> &[usize]`
  (deduped, excludes self). Consumed by Tasks 13–15.

Tarjan emits an SCC only after all SCCs reachable from it — with edges
pointing caller→callee that is exactly callees-first analysis order. Seed
the outer loop in ascending FuncId order and the output is deterministic.
Use the **iterative** formulation (explicit stack) — recursion would
overflow on deep stdlib call chains.

- [ ] **Step 1: Write failing unit tests** (in `callgraph.rs` tests; hand-built graphs — add a test-only `CallGraph::from_edges(n: usize, edges: &[(u32, u32)]) -> CallGraph` constructor):

```rust
#[cfg(test)]
pub(crate) fn from_edges(n: usize, edges: &[(u32, u32)]) -> CallGraph {
    let mut callees = vec![std::collections::BTreeSet::new(); n];
    for &(a, b) in edges {
        callees[a as usize].insert(FuncId(b));
    }
    CallGraph { callees: callees.into_iter().map(|s| s.into_iter().collect()).collect() }
}

#[test]
fn schedule_is_callees_first() {
    // 0 -> 1 -> 2, 0 -> 2
    let g = CallGraph::from_edges(3, &[(0, 1), (1, 2), (0, 2)]);
    let sccs = Sccs::compute_from_graph(3, &g);
    let order: Vec<u32> = sccs.schedule().iter().map(|s| s[0].0).collect();
    assert_eq!(order, vec![2, 1, 0]);
}

#[test]
fn mutual_recursion_is_one_scc() {
    // 0 <-> 1, both call 2
    let g = CallGraph::from_edges(3, &[(0, 1), (1, 0), (0, 2), (1, 2)]);
    let sccs = Sccs::compute_from_graph(3, &g);
    assert_eq!(sccs.schedule().len(), 2);
    assert_eq!(sccs.schedule()[0], vec![FuncId(2)]);
    assert_eq!(sccs.schedule()[1], vec![FuncId(0), FuncId(1)]); // sorted members
}
```

(`Sccs::compute(p, g)` delegates to `compute_from_graph(p.func_ids().count(), g)` so unit tests skip Program construction.)

- [ ] **Step 2: Run to verify failure**, then **Step 3: Implement** iterative Tarjan:

```rust
pub struct Sccs {
    schedule: Vec<Vec<FuncId>>,   // callees-first
    scc_of: Vec<usize>,           // FuncId index → position in schedule
    callee_sccs: Vec<Vec<usize>>, // per schedule position, deduped, no self
}

impl Sccs {
    pub fn compute(p: &Program, g: &CallGraph) -> Sccs {
        Self::compute_from_graph(p.func_ids().count(), g)
    }

    pub fn compute_from_graph(n: usize, g: &CallGraph) -> Sccs {
        const UNVISITED: u32 = u32::MAX;
        let mut index = vec![UNVISITED; n];
        let mut lowlink = vec![0u32; n];
        let mut on_stack = vec![false; n];
        let mut stack: Vec<u32> = Vec::new();
        let mut next_index = 0u32;
        let mut schedule: Vec<Vec<FuncId>> = Vec::new();
        let mut scc_of = vec![usize::MAX; n];

        // Iterative Tarjan: frame = (node, next-child-cursor).
        for root in 0..n as u32 {
            if index[root as usize] != UNVISITED {
                continue;
            }
            let mut frames: Vec<(u32, usize)> = vec![(root, 0)];
            while let Some(&mut (node, ref mut cursor)) = frames.last_mut() {
                let ni = node as usize;
                if *cursor == 0 {
                    index[ni] = next_index;
                    lowlink[ni] = next_index;
                    next_index += 1;
                    stack.push(node);
                    on_stack[ni] = true;
                }
                let edges = g.callees(FuncId(node));
                if *cursor < edges.len() {
                    let child = edges[*cursor].0;
                    *cursor += 1;
                    let ci = child as usize;
                    if ci >= n {
                        continue; // defensive: malformed edge
                    }
                    if index[ci] == UNVISITED {
                        frames.push((child, 0));
                    } else if on_stack[ci] {
                        lowlink[ni] = lowlink[ni].min(index[ci]);
                    }
                } else {
                    frames.pop();
                    if let Some(&mut (parent, _)) = frames.last_mut() {
                        let pi = parent as usize;
                        lowlink[pi] = lowlink[pi].min(lowlink[ni]);
                    }
                    if lowlink[ni] == index[ni] {
                        let mut members = Vec::new();
                        loop {
                            let w = stack.pop().unwrap();
                            on_stack[w as usize] = false;
                            members.push(FuncId(w));
                            if w == node {
                                break;
                            }
                        }
                        members.sort_unstable();
                        for m in &members {
                            scc_of[m.0 as usize] = schedule.len();
                        }
                        schedule.push(members);
                    }
                }
            }
        }
        // callee SCC deps per schedule slot.
        let mut callee_sccs: Vec<Vec<usize>> = vec![Vec::new(); schedule.len()];
        for (si, members) in schedule.iter().enumerate() {
            let mut deps: Vec<usize> = members.iter()
                .flat_map(|&m| g.callees(m).iter().map(|&c| scc_of[c.0 as usize]))
                .filter(|&d| d != si && d != usize::MAX)
                .collect();
            deps.sort_unstable();
            deps.dedup();
            callee_sccs[si] = deps;
        }
        Sccs { schedule, scc_of, callee_sccs }
    }

    pub fn schedule(&self) -> &[Vec<FuncId>] { &self.schedule }
    pub fn scc_of(&self, f: FuncId) -> usize { self.scc_of[f.0 as usize] }
    pub fn callee_sccs(&self, i: usize) -> &[usize] { &self.callee_sccs[i] }
}
```

**Note the borrow on `frames.last_mut()`** — the sketch's pattern
(`&mut (node, ref mut cursor)`) needs care; restructure to
`let (node, cursor_val) = { let f = frames.last_mut().unwrap(); … }` if the
borrow checker objects. Behavior, not shape, is the requirement; the tests
define behavior.

`dump_sccs`: one line per SCC in schedule order:
`scc 12 [recursive]: nameA, nameB` (names sorted; `[recursive]` iff >1
member or a self-edge).

- [ ] **Step 4: Add the self-recursion test:**

```rust
#[test]
fn self_recursive_function_is_its_own_scc() {
    let g = CallGraph::from_edges(2, &[(0, 0), (0, 1)]);
    let sccs = Sccs::compute_from_graph(2, &g);
    assert_eq!(sccs.schedule(), &[vec![FuncId(1)], vec![FuncId(0)]]);
    assert_eq!(sccs.callee_sccs(1), &[0]); // self-edge excluded
}
```

- [ ] **Step 5: Run tests, lint, commit**

Run: `mise x -- cargo test -p goverify-ir && mise run lint`

```bash
git add crates/goverify-ir/
git commit -m "ir: iterative Tarjan SCC condensation + callees-first schedule"
```

---

### Task 11: goverify-solver — Solver trait + StubSolver

**Files:**
- Modify: `crates/goverify-solver/src/lib.rs` (replace skeleton)

**Interfaces:**
- Produces: `Decl`, `Term`, `Model`, `SatResult`, `trait Solver`, `StubSolver`. Consumed by Task 14's engine and phase 3's real backends.

- [ ] **Step 1: Write the failing test** (in `lib.rs`):

```rust
#[test]
fn stub_solver_always_answers_unknown() {
    let mut s = StubSolver;
    s.declare(Decl("(declare-const x Bool)".into()));
    s.push();
    s.assert(Term("x".into()));
    assert_eq!(s.check_sat_assuming(&[]), SatResult::Unknown);
    assert!(s.model().is_none(), "Unknown must never expose a model");
    s.pop();
}
```

- [ ] **Step 2: Implement:**

```rust
//! Solver abstraction (parent spec §8). Phase 2 ships the trait and a
//! stub; Z3Native and SmtLib2Process arrive in phase 3. Decl/Term are
//! opaque SMT-LIB2 fragments for now — phase 3 replaces their innards
//! with the typed term language behind the same trait.

/// A declaration (sort, const, or function) in canonical SMT-LIB2 text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decl(pub String);

/// A term in canonical SMT-LIB2 text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Term(pub String);

/// A satisfying model. Opaque in phase 2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Model;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SatResult {
    Sat,
    Unsat,
    /// Includes timeouts. Bug-finder semantics: Unknown ⇒ no report
    /// (parent spec §8) — timeouts must never create false positives.
    Unknown,
}

pub trait Solver {
    fn declare(&mut self, decl: Decl);
    fn assert(&mut self, term: Term);
    fn check_sat_assuming(&mut self, assumptions: &[Term]) -> SatResult;
    fn model(&self) -> Option<Model>;
    fn push(&mut self);
    fn pop(&mut self);
}

/// Answers Unknown to everything: with bug-finder semantics this means
/// "report nothing", which is exactly right while no checkers exist.
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
```

- [ ] **Step 3: Run, lint, commit**

Run: `mise x -- cargo test -p goverify-solver && mise run lint`

```bash
git add crates/goverify-solver/
git commit -m "solver: Solver trait + StubSolver (always Unknown => no report)"
```

---

### Task 12: goverify-analysis — summaries, placeholder formulas, substitution

**Files:**
- Create: `crates/goverify-analysis/src/summary.rs`
- Modify: `crates/goverify-analysis/src/lib.rs`, `crates/goverify-analysis/Cargo.toml`

**Interfaces:**
- Consumes: `goverify_ir::{ValueId, FuncId}`.
- Produces: `Summary`, `Clause`, `PlaceholderFormula`, `IfaceVar`, `BoundClause`, `Provenance`, `instantiate_requires`, `Summary::havoc()`. Consumed by Tasks 13–15; `PlaceholderFormula` is the exact type phase 3 replaces with real terms.

- [ ] **Step 1: Cargo wiring.** `goverify-analysis/Cargo.toml` dependencies: `goverify-ir = { path = "../goverify-ir" }`, `goverify-solver = { path = "../goverify-solver" }` (add both to `[workspace.dependencies]` in the root `Cargo.toml` and use `workspace = true`, matching how `goverify-extract` is wired).

- [ ] **Step 2: Write the failing tests** (in `summary.rs`):

```rust
#[test]
fn instantiate_maps_params_to_args() {
    let callee = Summary {
        requires: vec![Clause { formula: PlaceholderFormula {
            tag: "nonnil".into(),
            vars: vec![IfaceVar::Param(0), IfaceVar::Param(2)],
        }}],
        ..Summary::default()
    };
    let args = [ValueId(7), ValueId(8), ValueId(9)];
    let bound = instantiate_requires(&callee, &args);
    assert_eq!(bound, vec![BoundClause {
        tag: "nonnil".into(),
        vars: vec![Some(ValueId(7)), Some(ValueId(9))],
    }]);
}

#[test]
fn instantiate_out_of_range_param_binds_none() {
    let callee = Summary {
        requires: vec![Clause { formula: PlaceholderFormula {
            tag: "t".into(), vars: vec![IfaceVar::Param(5)],
        }}],
        ..Summary::default()
    };
    assert_eq!(instantiate_requires(&callee, &[])[0].vars, vec![None]);
}

#[test]
fn havoc_summary_has_no_requires() {
    // Missing info must never create false positives (parent spec §11).
    let h = Summary::havoc();
    assert!(h.requires.is_empty());
    assert_eq!(h.provenance, Provenance::Havoc);
    assert_eq!(h.effects, crate::effects::Effects::top());
}
```

(The `Effects::top()` assertion compiles after Task 13; gate it with a
`// Task 13 uncomments` marker or write Task 13's `effects.rs` stub —
simplest: create `effects.rs` in this task with just the type + `top()`/
`empty()`, and let Task 13 fill collection logic. Do that.)

- [ ] **Step 3: Implement `summary.rs`:**

```rust
//! Function summaries (parent spec §5), phase-2 form: clause structure and
//! instantiation are real; formulas are placeholders until phase 3's term
//! language replaces `PlaceholderFormula` behind this same API.

use goverify_ir::ValueId;

use crate::effects::Effects;

/// A variable of the function's symbolic interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IfaceVar {
    Param(u32),
    Result(u32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaceholderFormula {
    /// Which fact this clause states, e.g. "nonnil". Opaque to phase 2.
    pub tag: String,
    pub vars: Vec<IfaceVar>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Clause {
    pub formula: PlaceholderFormula,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provenance {
    Inferred,
    Havoc,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    pub requires: Vec<Clause>,
    pub ensures: Vec<Clause>,
    pub effects: Effects,
    pub provenance: Provenance,
}

impl Default for Summary {
    fn default() -> Self {
        Summary {
            requires: Vec::new(),
            ensures: Vec::new(),
            effects: Effects::empty(),
            provenance: Provenance::Inferred,
        }
    }
}

impl Summary {
    /// The unknown-function summary: no requires (missing info must never
    /// create false positives), top effects (assume the worst).
    pub fn havoc() -> Summary {
        Summary {
            requires: Vec::new(),
            ensures: Vec::new(),
            effects: Effects::top(),
            provenance: Provenance::Havoc,
        }
    }
}

/// A callee clause bound to caller values. None = the interface var had no
/// corresponding caller value (malformed input or Result var) — callers
/// must treat None as "cannot evaluate; do not report".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundClause {
    pub tag: String,
    pub vars: Vec<Option<ValueId>>,
}

pub fn instantiate_requires(callee: &Summary, args: &[ValueId]) -> Vec<BoundClause> {
    callee.requires.iter().map(|c| BoundClause {
        tag: c.formula.tag.clone(),
        vars: c.formula.vars.iter().map(|v| match v {
            IfaceVar::Param(i) => args.get(*i as usize).copied(),
            IfaceVar::Result(_) => None,
        }).collect(),
    }).collect()
}
```

And the `effects.rs` stub (Task 13 adds collection):

```rust
//! Concurrency effects (parent spec §5). Unlike requires/ensures these are
//! NOT placeholders: they are syntactic facts, fully functional in phase 2.

use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ChanOp { Make, Send, Recv, Close, Select }

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LockOp { Lock, Unlock, RLock, RUnlock }

/// Ordered: None < Bounded < Unbounded (join = max).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum Spawns { #[default] None, Bounded, Unbounded }

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Effects {
    pub spawns: Spawns,
    pub chan_ops: BTreeSet<ChanOp>,
    pub lock_ops: BTreeSet<LockOp>,
}

impl Effects {
    pub fn empty() -> Effects {
        Effects::default()
    }

    pub fn top() -> Effects {
        Effects {
            spawns: Spawns::Unbounded,
            chan_ops: [ChanOp::Make, ChanOp::Send, ChanOp::Recv, ChanOp::Close,
                       ChanOp::Select].into(),
            lock_ops: [LockOp::Lock, LockOp::Unlock, LockOp::RLock,
                       LockOp::RUnlock].into(),
        }
    }

    pub fn is_empty(&self) -> bool {
        *self == Effects::empty()
    }

    pub fn join(&mut self, other: &Effects) {
        self.spawns = self.spawns.max(other.spawns);
        self.chan_ops.extend(other.chan_ops.iter().copied());
        self.lock_ops.extend(other.lock_ops.iter().copied());
    }
}
```

`lib.rs`:

```rust
//! Analysis engine: SCC scheduler, pre-pass, summary instantiation,
//! bounded fixpoint (phase 2; parent spec §2).

mod effects;
mod summary;

pub use effects::{ChanOp, Effects, LockOp, Spawns};
pub use summary::{BoundClause, Clause, IfaceVar, PlaceholderFormula, Provenance,
                  Summary, instantiate_requires};
```

- [ ] **Step 4: Run, lint, commit**

Run: `mise x -- cargo test -p goverify-analysis && mise run lint`

```bash
git add crates/goverify-analysis/ Cargo.toml Cargo.lock
git commit -m "analysis: summary types, placeholder formulas, instantiation, effects lattice"
```

---

### Task 13: goverify-analysis — effect collection and the value-clean pre-pass

**Files:**
- Create: `crates/goverify-analysis/src/prepass.rs`, `crates/goverify-analysis/src/testpkg.rs` (test-only builders)
- Modify: `crates/goverify-analysis/src/effects.rs` (collection), `src/lib.rs`, `Cargo.toml` (dev-dep `goverify-extract`)

**Interfaces:**
- Consumes: `Program`, `Function`, `Op`, `Callee`, `TypeKind` (goverify-ir); `Effects` (Task 12).
- Produces:
  - `effects::collect(p: &Program, f: FuncId, callee_effects: &dyn Fn(FuncId) -> Effects) -> Effects` — own ops + joined callee effects; `go`-in-a-CFG-cycle ⇒ `Spawns::Unbounded`.
  - `prepass::value_clean(p: &Program, f: FuncId) -> bool`.
  - `Domains { value_clean, concurrency_clean }` (concurrency_clean = `effects.is_empty()`, computed by Task 14's engine).

- [ ] **Step 1: Add the shared gvir test builder.** Both this task's and
Task 14's tests build tiny packages by hand; define the builder **once** in
`crates/goverify-analysis/src/testpkg.rs` (`#[cfg(test)] mod testpkg;` in
`lib.rs`):

```rust
//! Test-only builders for hand-written gvir packages.

use goverify_extract::gvir;

pub fn call(target: &str) -> gvir::Instruction {
    gvir::Instruction {
        kind: "Call".into(),
        sem: Some(gvir::instruction::Sem::Call(gvir::CallSem {
            static_callee: target.into(),
            ..Default::default()
        })),
        ..Default::default()
    }
}

pub fn go_call(target: &str) -> gvir::Instruction {
    gvir::Instruction { kind: "Go".into(), ..call(target) }
}

pub fn instr(kind: &str) -> gvir::Instruction {
    gvir::Instruction { kind: kind.into(), ..Default::default() }
}

pub fn block(index: u32, instrs: Vec<gvir::Instruction>, succs: Vec<u32>)
    -> gvir::BasicBlock {
    gvir::BasicBlock { index, instrs, succs }
}

pub fn func(id: &str, blocks: Vec<gvir::BasicBlock>) -> gvir::Function {
    gvir::Function { id: id.into(), blocks, ..Default::default() }
}

pub fn pkg(path: &str, functions: Vec<gvir::Function>) -> gvir::Package {
    gvir::Package { import_path: path.into(), functions, ..Default::default() }
}
```

(`goverify-extract` must therefore be a dev-visible dependency of
goverify-analysis — it already is transitively, but add it explicitly to
`[dependencies]` since `src/testpkg.rs` is compiled under `cfg(test)` of
this crate; a `[dev-dependencies]` entry does not cover `src/` test
modules' non-test siblings — `#[cfg(test)]` modules in `src` can use
dev-dependencies, so `[dev-dependencies] goverify-extract` suffices. Use
dev-dependencies.)

- [ ] **Step 2: Write failing tests** (`effects.rs`):

```rust
#[cfg(test)]
mod tests {
    use goverify_ir::Program;

    use super::*;
    use crate::testpkg::{block, call, func, go_call, instr, pkg};

    #[test]
    fn go_in_loop_is_unbounded_spawn() {
        // CFG: b0 -> b1; b1 contains Go and loops to itself; b1 -> b2.
        let p = Program::from_packages(vec![pkg("t", vec![func("t.F", vec![
            block(0, vec![instr("Jump")], vec![1]),
            block(1, vec![go_call("t.G"), instr("Jump")], vec![1, 2]),
            block(2, vec![instr("Return")], vec![]),
        ])])]);
        let e = collect(&p, p.lookup_func("t.F").unwrap(), &[]);
        assert_eq!(e.spawns, Spawns::Unbounded);
    }

    #[test]
    fn straight_line_go_is_bounded() {
        let p = Program::from_packages(vec![pkg("t", vec![func("t.F", vec![
            block(0, vec![go_call("t.G"), instr("Return")], vec![]),
        ])])]);
        let e = collect(&p, p.lookup_func("t.F").unwrap(), &[]);
        assert_eq!(e.spawns, Spawns::Bounded);
    }

    #[test]
    fn callee_effects_join_in() {
        let p = Program::from_packages(vec![pkg("t", vec![func("t.F", vec![
            block(0, vec![call("t.G"), instr("Return")], vec![]),
        ])])]);
        let mut callee = Effects::empty();
        callee.lock_ops.insert(LockOp::Lock);
        let e = collect(&p, p.lookup_func("t.F").unwrap(), &[&callee]);
        assert!(e.lock_ops.contains(&LockOp::Lock));
    }
}
```

- [ ] **Step 3: Run to verify failure**, then **Step 4: implement collection** in `effects.rs`:

```rust
use goverify_ir::{FuncId, Op, Program};

/// Blocks that sit on a CFG cycle: reachable from themselves. O(B²) DFS —
/// fine for phase 2 (functions are small; revisit if profiling says so).
fn cyclic_blocks(f: &goverify_ir::Function) -> Vec<bool> {
    let n = f.blocks.len();
    let mut cyclic = vec![false; n];
    for start in 0..n {
        let mut seen = vec![false; n];
        let mut stack: Vec<usize> = f.blocks[start].succs.iter()
            .map(|&s| s as usize).filter(|&s| s < n).collect();
        while let Some(b) = stack.pop() {
            if b == start {
                cyclic[start] = true;
                break;
            }
            if !seen[b] {
                seen[b] = true;
                stack.extend(f.blocks[b].succs.iter()
                    .map(|&s| s as usize).filter(|&s| s < n));
            }
        }
    }
    cyclic
}

/// Own concurrency ops + join of all call-graph callees' effects.
/// Callee resolution (static, invoke, dynamic) already happened in the
/// call graph, so `callee_effects` is simply the effects of every callee
/// edge — call-site precision is unnecessary for set-union effects.
pub fn collect(p: &Program, id: FuncId, callee_effects: &[&Effects]) -> Effects {
    let Some(f) = p.func(id) else { return Effects::top() };
    let cyclic = cyclic_blocks(f);
    let mut e = Effects::empty();
    for ce in callee_effects {
        e.join(ce);
    }
    for (bi, b) in f.blocks.iter().enumerate() {
        for ins in &b.instrs {
            match &ins.op {
                Op::Make { kind: goverify_ir::MakeKind::Chan, .. } => {
                    e.chan_ops.insert(ChanOp::Make);
                }
                Op::Send { .. } => { e.chan_ops.insert(ChanOp::Send); }
                Op::Recv { .. } => { e.chan_ops.insert(ChanOp::Recv); }
                Op::CloseChan { .. } => { e.chan_ops.insert(ChanOp::Close); }
                Op::Select { .. } => { e.chan_ops.insert(ChanOp::Select); }
                Op::Lock { kind, .. } => {
                    e.lock_ops.insert(match kind {
                        goverify_ir::LockKind::Lock => LockOp::Lock,
                        goverify_ir::LockKind::Unlock => LockOp::Unlock,
                        goverify_ir::LockKind::RLock => LockOp::RLock,
                        goverify_ir::LockKind::RUnlock => LockOp::RUnlock,
                    });
                }
                Op::Go { .. } => {
                    let s = if cyclic[bi] { Spawns::Unbounded } else { Spawns::Bounded };
                    e.spawns = e.spawns.max(s);
                }
                _ => {}
            }
        }
    }
    e
}
```

- [ ] **Step 5: Implement `prepass.rs`:**

```rust
//! Pre-pass, value domain (phase-2 spec §5): a function is value-clean if
//! nothing in it can raise a value obligation — no deref of a
//! non-locally-allocated pointer, no indexing/lookup/slicing, no
//! division/remainder, no narrowing conversion. Syntactic and
//! intraprocedural by design; sound direction: false (=not clean) is
//! always safe, so unknowns are not clean.

use std::collections::HashSet;

use goverify_ir::{BinOpKind, FuncId, Op, Program, TypeKind, ValueId};

fn int_width(name: &str) -> Option<u32> {
    Some(match name {
        "int8" | "uint8" | "byte" => 8,
        "int16" | "uint16" => 16,
        "int32" | "uint32" | "rune" | "float32" => 32,
        "int" | "uint" | "int64" | "uint64" | "uintptr" | "float64" => 64,
        _ => return None,
    })
}

pub fn value_clean(p: &Program, id: FuncId) -> bool {
    let Some(f) = p.func(id) else { return false };
    let allocs: HashSet<ValueId> = f.blocks.iter()
        .flat_map(|b| &b.instrs)
        .filter_map(|i| match i.op {
            Op::Alloc { dst, .. } => Some(dst),
            _ => None,
        })
        .collect();
    let local = |v: ValueId| allocs.contains(&v);
    for b in &f.blocks {
        for ins in &b.instrs {
            let dirty = match &ins.op {
                Op::Load { addr, .. } | Op::Store { addr, .. } => !local(*addr),
                Op::FieldAddr { base, .. } => !local(*base),
                Op::IndexAddr { .. } | Op::Index { .. } | Op::Lookup { .. }
                | Op::Slice { .. } => true,
                Op::BinOp { kind: BinOpKind::Div | BinOpKind::Rem, .. } => true,
                Op::Convert { dst, src } => {
                    let (dt, st) = (p.types().kind(f.value(*dst).ty),
                                    p.types().kind(f.value(*src).ty));
                    match (dt, st) {
                        (TypeKind::Basic { name: d }, TypeKind::Basic { name: s }) => {
                            match (int_width(d), int_width(s)) {
                                (Some(dw), Some(sw)) => dw < sw, // narrowing
                                _ => true,                        // unknown basics: not clean
                            }
                        }
                        _ => true,
                    }
                }
                _ => false,
            };
            if dirty {
                return false;
            }
        }
    }
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Domains {
    pub value_clean: bool,
    pub concurrency_clean: bool,
}
```

With these tests in `prepass.rs` (extend `testpkg` with a
`func_with_params(id, params: Vec<gvir::Param>, blocks)` builder — same
shape as `func` plus the `params` field):

```rust
#[cfg(test)]
mod tests {
    use goverify_extract::gvir;
    use goverify_ir::Program;

    use super::*;
    use crate::testpkg::{block, func_with_params, instr, pkg};

    fn ty(id: u32, repr: &str, kind: gvir::TypeKind, name: &str) -> gvir::Type {
        gvir::Type { id, repr: repr.into(), kind: kind as i32,
                     name: name.into(), ..Default::default() }
    }

    fn param(id: u32, t: u32) -> gvir::Param {
        gvir::Param { id, name: format!("p{id}"), r#type: t }
    }

    fn build(types: Vec<gvir::Type>, params: Vec<gvir::Param>,
             instrs: Vec<gvir::Instruction>) -> Program {
        let mut package = pkg("t", vec![func_with_params(
            "t.F", params, vec![block(0, instrs, vec![])])]);
        package.types = types;
        Program::from_packages(vec![package])
    }

    fn clean(p: &Program) -> bool {
        value_clean(p, p.lookup_func("t.F").unwrap())
    }

    #[test]
    fn param_deref_is_not_clean() {
        let mut load = instr("UnOp");
        load.register = 2;
        load.operands = vec![1];
        load.sem = Some(gvir::instruction::Sem::Unop(
            gvir::UnOpSem { op: "*".into(), ..Default::default() }));
        let mut pointer = ty(2, "*int", gvir::TypeKind::Pointer, "");
        pointer.elem = 1;
        let p = build(
            vec![ty(1, "int", gvir::TypeKind::Basic, "int"), pointer],
            vec![param(1, 2)],
            vec![load, instr("Return")],
        );
        assert!(!clean(&p));
    }

    #[test]
    fn pure_arithmetic_is_clean() {
        let mut add = instr("BinOp");
        add.register = 3;
        add.operands = vec![1, 2];
        add.sem = Some(gvir::instruction::Sem::Binop(gvir::BinOpSem { op: "+".into() }));
        let p = build(
            vec![ty(1, "int", gvir::TypeKind::Basic, "int")],
            vec![param(1, 1), param(2, 1)],
            vec![add, instr("Return")],
        );
        assert!(clean(&p));
    }

    #[test]
    fn division_is_not_clean() {
        let mut div = instr("BinOp");
        div.register = 3;
        div.operands = vec![1, 2];
        div.sem = Some(gvir::instruction::Sem::Binop(gvir::BinOpSem { op: "/".into() }));
        let p = build(
            vec![ty(1, "int", gvir::TypeKind::Basic, "int")],
            vec![param(1, 1), param(2, 1)],
            vec![div, instr("Return")],
        );
        assert!(!clean(&p));
    }

    #[test]
    fn narrowing_convert_is_not_clean() {
        let mut conv = instr("Convert");
        conv.register = 2;
        conv.r#type = 2; // int8 result
        conv.operands = vec![1];
        let p = build(
            vec![ty(1, "int64", gvir::TypeKind::Basic, "int64"),
                 ty(2, "int8", gvir::TypeKind::Basic, "int8")],
            vec![param(1, 1)],
            vec![conv, instr("Return")],
        );
        assert!(!clean(&p));
    }
}
```

- [ ] **Step 6: Run, lint, commit**

Run: `mise x -- cargo test -p goverify-analysis && mise run lint`

```bash
git add crates/goverify-analysis/
git commit -m "analysis: effect collection (cycle-aware spawns) + value-clean pre-pass"
```

---

### Task 14: goverify-analysis — SCC engine: scheduler, fixpoint, widening

**Files:**
- Create: `crates/goverify-analysis/src/engine.rs`
- Modify: `crates/goverify-analysis/src/lib.rs`, `Cargo.toml` (add `rayon`)

**Interfaces:**
- Consumes: `Sccs`, `CallGraph`, `Program` (Tasks 9–10); `Summary`, `effects::collect`, `prepass` (Tasks 12–13); `StubSolver` (Task 11).
- Produces: `Options { widen_after: u32 }` (`Default` = 3), `Analysis { summaries, prepass, diagnostics }`, `analyze(p: &Program, opts: &Options) -> Analysis`, plus `dump_prepass`/`dump_summaries`. Consumed by Task 15's CLI.

Scheduling: **wave-parallel** — group SCCs by longest-path depth over the
condensation DAG (leaves = depth 0), process depths in ascending order,
`rayon par_iter` within a wave. A barrier per wave is mildly pessimistic
vs. true dataflow scheduling; chosen for simplicity, results are
deterministic either way because summaries are pure functions of inputs.
Revisit only if phase-5 profiling says so (record this note as a comment).

- [ ] **Step 1: Add rayon.** Root `Cargo.toml` `[workspace.dependencies]`: `rayon = "1"`. Analysis crate: `rayon = { workspace = true }`.

- [ ] **Step 2: Write failing tests** (`engine.rs`, using `crate::testpkg` from Task 13):

```rust
#[cfg(test)]
mod tests {
    use goverify_ir::Program;

    use super::*;
    use crate::effects::{Effects, LockOp};
    use crate::summary::Provenance;
    use crate::testpkg::{block, call, func, instr, pkg};

    fn straight(id: &str, body: Vec<goverify_extract::gvir::Instruction>)
        -> goverify_extract::gvir::Function {
        let mut instrs = body;
        instrs.push(instr("Return"));
        func(id, vec![block(0, instrs, vec![])])
    }

    #[test]
    fn effects_propagate_bottom_up() {
        let p = Program::from_packages(vec![pkg("t", vec![
            straight("t.Leaf", vec![call("(*sync.Mutex).Lock")]),
            straight("t.Mid", vec![call("t.Leaf")]),
            straight("t.Top", vec![call("t.Mid")]),
        ])]);
        let a = analyze(&p, &Options::default());
        let top = p.lookup_func("t.Top").unwrap();
        assert!(a.summaries[&top].effects.lock_ops.contains(&LockOp::Lock),
            "Lock effect must propagate Leaf→Mid→Top");
        assert!(!a.prepass[&top].concurrency_clean);
    }

    #[test]
    fn external_callee_gets_havoc_summary() {
        // unknown.G is interned via the call but has no body anywhere.
        let p = Program::from_packages(vec![pkg("t", vec![
            straight("t.F", vec![call("unknown.G")]),
        ])]);
        let a = analyze(&p, &Options::default());
        let f = p.lookup_func("t.F").unwrap();
        assert_eq!(a.summaries[&f].effects, Effects::top(),
            "havoc callee effects must flow into the caller");
        let g = p.lookup_func("unknown.G").unwrap();
        assert_eq!(a.summaries[&g].provenance, Provenance::Havoc);
    }

    #[test]
    fn recursive_scc_converges_without_widening() {
        // t.Even <-> t.Odd, no concurrency ops: fixpoint stabilizes at
        // empty effects immediately; provenance stays Inferred.
        let p = Program::from_packages(vec![pkg("t", vec![
            straight("t.Even", vec![call("t.Odd")]),
            straight("t.Odd", vec![call("t.Even")]),
        ])]);
        let a = analyze(&p, &Options::default());
        let even = p.lookup_func("t.Even").unwrap();
        assert_eq!(a.summaries[&even].provenance, Provenance::Inferred);
        assert!(a.summaries[&even].effects.is_empty());
    }

    #[test]
    fn widening_kicks_in_after_k_rounds() {
        // The Lock op makes round 1 change the SCC's summaries (empty →
        // {Lock}); with widen_after = 0 that first change triggers
        // widening, so the whole SCC comes out Havoc instead of iterating
        // to the (reachable) fixpoint.
        let evenodd = || pkg("t", vec![
            straight("t.Even", vec![call("(*sync.Mutex).Lock"), call("t.Odd")]),
            straight("t.Odd", vec![call("t.Even")]),
        ]);
        let a0 = analyze(&Program::from_packages(vec![evenodd()]),
                         &Options { widen_after: 0 });
        let p = Program::from_packages(vec![evenodd()]);
        let even = p.lookup_func("t.Even").unwrap();
        assert_eq!(a0.summaries[&even].provenance, Provenance::Havoc);
        // Sanity: with the default k the same SCC converges Inferred.
        let a3 = analyze(&p, &Options::default());
        assert_eq!(a3.summaries[&even].provenance, Provenance::Inferred);
    }
}
```

- [ ] **Step 3: Implement `engine.rs`:**

```rust
//! Bottom-up SCC engine (phase-2 spec §4.2–4.3): wave-parallel schedule,
//! bounded fixpoint on recursive SCCs, widening to havoc after k rounds,
//! catch_unwind per function.

use std::collections::BTreeMap;
use std::sync::Mutex;

use rayon::prelude::*;

use goverify_ir::{CallGraph, FuncId, Program, Sccs};

use crate::effects;
use crate::prepass::{self, Domains};
use crate::summary::{Provenance, Summary};

#[derive(Debug, Clone)]
pub struct Options {
    pub widen_after: u32,
}

impl Default for Options {
    fn default() -> Self {
        Options { widen_after: 3 }
    }
}

#[derive(Debug)]
pub struct Analysis {
    pub summaries: BTreeMap<FuncId, Summary>,
    pub prepass: BTreeMap<FuncId, Domains>,
    pub diagnostics: Vec<String>,
}

pub fn analyze(p: &Program, opts: &Options) -> Analysis {
    let graph = CallGraph::build(p);
    let sccs = Sccs::compute(p, &graph);
    let n_sccs = sccs.schedule().len();

    // Wave assignment: depth(scc) = 1 + max(depth of callee sccs).
    // callee_sccs always precede in schedule order, so one forward pass.
    let mut depth = vec![0usize; n_sccs];
    for i in 0..n_sccs {
        depth[i] = sccs.callee_sccs(i).iter().map(|&d| depth[d] + 1).max().unwrap_or(0);
    }
    let max_depth = depth.iter().copied().max().unwrap_or(0);
    let mut waves: Vec<Vec<usize>> = vec![Vec::new(); max_depth + 1];
    for (i, &d) in depth.iter().enumerate() {
        waves[d].push(i); // schedule order within wave: deterministic
    }

    // Summaries live in a slot-per-function vec so waves can write in
    // parallel without locking the whole map.
    let n_funcs = p.func_ids().count();
    let slots: Vec<Mutex<Option<Summary>>> = (0..n_funcs).map(|_| Mutex::new(None)).collect();
    let diags: Mutex<Vec<String>> = Mutex::new(Vec::new());

    let summary_of = |f: FuncId| -> Summary {
        slots[f.0 as usize].lock().unwrap().clone().unwrap_or_else(Summary::havoc)
    };

    for wave in &waves {
        wave.par_iter().for_each(|&si| {
            let members = &sccs.schedule()[si];
            let recursive = members.len() > 1
                || members.iter().any(|&m| graph.callees(m).contains(&m));
            let mut current: BTreeMap<FuncId, Summary> = members.iter()
                .map(|&m| (m, Summary::default())) // optimistic start
                .collect();
            let mut rounds = 0u32;
            loop {
                let mut changed = false;
                for &m in members {
                    let new = analyze_function(p, &graph, m, &|f| {
                        current.get(&f).cloned().unwrap_or_else(|| summary_of(f))
                    }, &diags);
                    if current[&m] != new {
                        current.insert(m, new);
                        changed = true;
                    }
                }
                if !recursive || !changed {
                    break;
                }
                if rounds >= /* opts.widen_after */ 0 {
                    // widen: havoc every member, done (deterministic top)
                    for &m in members {
                        current.insert(m, Summary::havoc());
                    }
                    break;
                }
                rounds += 1;
            }
            for (m, s) in current {
                *slots[m.0 as usize].lock().unwrap() = Some(s);
            }
        });
    }
    // …assemble Analysis: summaries from slots (missing/external → havoc),
    // prepass domains: value_clean via prepass::value_clean, concurrency_clean
    // = summary.effects.is_empty().
    let mut summaries = BTreeMap::new();
    let mut pre = BTreeMap::new();
    for f in p.func_ids() {
        let s = slots[f.0 as usize].lock().unwrap().clone()
            .unwrap_or_else(Summary::havoc);
        pre.insert(f, Domains {
            value_clean: prepass::value_clean(p, f),
            concurrency_clean: s.effects.is_empty(),
        });
        summaries.insert(f, s);
    }
    Analysis { summaries, prepass: pre, diagnostics: diags.into_inner().unwrap() }
}

fn analyze_function(
    p: &Program,
    graph: &CallGraph,
    f: FuncId,
    summary_of: &dyn Fn(FuncId) -> Summary,
    diags: &Mutex<Vec<String>>,
) -> Summary {
    let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if p.func(f).is_none() {
            return Summary::havoc(); // external / bodyless
        }
        let callee_effects: Vec<crate::effects::Effects> = graph.callees(f)
            .iter().map(|&c| summary_of(c).effects).collect();
        let refs: Vec<&crate::effects::Effects> = callee_effects.iter().collect();
        Summary {
            effects: effects::collect(p, f, &refs),
            ..Summary::default()
        }
    }));
    match run {
        Ok(s) => s,
        Err(_) => {
            diags.lock().unwrap().push(format!(
                "internal: panic while analyzing {}; havoc summary substituted",
                p.func_name(f)));
            Summary::havoc()
        }
    }
}
```

Fix the sketch's placeholder: the widening comparison is
`if rounds >= opts.widen_after` (thread `opts` in via the closure), and
`Options` must be passed into `analyze`'s parallel closure by reference
(it's `Sync`). Also note `current[&m]` panics on missing key — members are
all pre-inserted, so it can't miss; keep it.

**Determinism requirement (test it):** run `analyze` twice on the `conc`
corpus and assert the two `Analysis::summaries` maps are equal — rayon
ordering must not leak. Add that as an integration test
`crates/goverify-analysis/tests/engine_corpus.rs` (copy `common` helper
pattern from Task 9, or move `load_corpus` into `goverify-ir`'s public
`#[doc(hidden)] pub mod testutil` — choose the testutil module; both
crates' integration tests need it and copy-paste drifts):

```rust
#[test]
fn analysis_is_deterministic_across_runs() {
    let p = goverify_ir::testutil::load_corpus("conc");
    let a1 = analyze(&p, &Options::default());
    let a2 = analyze(&p, &Options::default());
    assert_eq!(a1.summaries, a2.summaries);
    assert_eq!(a1.prepass, a2.prepass);
}

#[test]
fn conc_corpus_effects_are_sane() {
    let p = goverify_ir::testutil::load_corpus("conc");
    let a = analyze(&p, &Options::default());
    let close = p.lookup_func("(*example.com/conc.file).Close").unwrap();
    let e = &a.summaries[&close].effects;
    assert!(e.lock_ops.contains(&LockOp::Lock) && e.lock_ops.contains(&LockOp::Unlock),
        "Close locks and (deferred) unlocks: {e:?}");
    let fan = p.lookup_func("example.com/conc.Fan").unwrap();
    assert_ne!(a.summaries[&fan].effects.spawns, Spawns::None);
    assert!(!a.prepass[&fan].concurrency_clean);
}
```

Add `dump_prepass` and `dump_summaries` (in `engine.rs` or a small
`dump.rs`), both with signature
`(p: &Program, a: &Analysis, filter: Option<&str>) -> String` where
`filter` is a **substring** match on the function id (used by Task 15's
`--func` and Task 16's goldens): one line per matching function —
`example.com/conc.Fan value_clean=false concurrency_clean=false` and
`example.com/conc.Fan effects={spawns:Bounded chan:[Make,Recv,Send,Select] locks:[]} requires=0 ensures=0 provenance=Inferred`,
iterating the `BTreeMap`s (already sorted by FuncId; print sorted by
func *name* for human diffing — collect + sort lines).

- [ ] **Step 3b: Thread the solver through the engine** (spec §6: "the
engine calls through the trait, so phase 3 swaps the implementation, not
the call sites"). In `engine.rs`:

```rust
use goverify_solver::{SatResult, Solver, StubSolver};

use crate::summary::{BoundClause, instantiate_requires};

/// A reported violation. Phase 4 gives this real content; phase 2 only
/// needs it to exist so the discharge path is exercised end to end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub tag: String,
}

/// Discharge instantiated requires-clauses. Bug-finder semantics
/// (parent spec §8): only Sat reports; Unsat and Unknown (incl. timeout)
/// are silent. With StubSolver everything is Unknown ⇒ no findings.
pub fn discharge(obligations: &[BoundClause], solver: &mut dyn Solver) -> Vec<Finding> {
    obligations.iter()
        .filter(|_| solver.check_sat_assuming(&[]) == SatResult::Sat)
        .map(|o| Finding { tag: o.tag.clone() })
        .collect()
}

pub fn analyze(p: &Program, opts: &Options) -> Analysis {
    analyze_with_solver(p, opts, &|| Box::new(StubSolver))
}

pub fn analyze_with_solver(
    p: &Program,
    opts: &Options,
    mk_solver: &(dyn Fn() -> Box<dyn Solver> + Sync),
) -> Analysis {
    // …the body shown in Step 3; each SCC task creates its own solver
    // (`let mut solver = mk_solver();`) and, inside analyze_function, for
    // every Op::Call with a Static callee, instantiates the callee's
    // requires (`instantiate_requires(&callee_summary, args)`) and runs
    // `discharge` on them. Phase 2 summaries have no requires, so the
    // path yields zero findings on real corpora — the unit tests below
    // pin the semantics.
}
```

Unit tests (same `tests` module):

```rust
    #[test]
    fn discharge_with_stub_solver_reports_nothing() {
        let obligations = vec![BoundClause { tag: "nonnil".into(), vars: vec![] }];
        assert!(discharge(&obligations, &mut StubSolver).is_empty(),
            "Unknown must never produce a finding");
    }
```

(`goverify-solver` becomes a real dependency of goverify-analysis — it was
added to Cargo.toml in Task 12 Step 1.)

- [ ] **Step 4: Run everything**

Run: `mise x -- cargo test -p goverify-analysis && mise run test && mise run lint`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/goverify-analysis/ crates/goverify-ir/ Cargo.toml Cargo.lock
git commit -m "analysis: wave-parallel SCC engine, bounded fixpoint + widening, prepass wiring"
```

---

### Task 15: goverify-cli — `goverify debug` subcommands

**Files:**
- Modify: `crates/goverify-cli/src/main.rs`, `crates/goverify-cli/Cargo.toml`
- Test: `crates/goverify-cli/tests/debug_integration.rs`

**Interfaces:**
- Consumes: `Program::load_dir`, `CallGraph`, `Sccs`, `dump_*` (goverify-ir); `analyze`, `Options`, `dump_prepass`, `dump_summaries` (goverify-analysis); existing `Sidecar` flow.
- Produces: the phase's user-visible surface:
  `goverify debug <ir|callgraph|sccs|prepass|summary> [--gvir-dir DIR] [--func NAME] [PATTERNS…]`.
  Without `--gvir-dir`, extracts the current directory first (same flow as
  `goverify extract`, into a temp dir). `--func` filters `ir`/`summary`
  output to one function. Output goes to stdout; diagnostics to stderr.
  Exit 0 (dumps aren't findings), 2 on analyzer error.

- [ ] **Step 1: Write the failing integration test** (`tests/debug_integration.rs`; drive the built binary with `std::process::Command` via `env!("CARGO_BIN_EXE_goverify")`, extracting the conc corpus first with `goverify_ir::testutil::load_corpus`'s sidecar pattern — but here shell out to the binary's own `extract`):

```rust
//! End-to-end: goverify extract + goverify debug over the conc corpus.

use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

fn goverify(args: &[&str], cwd: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_goverify"))
        .args(args)
        .current_dir(cwd)
        .env("GOVERIFY_EXTRACTOR_DIR", repo_root().join("extractor"))
        .output()
        .expect("spawn goverify")
}

fn extract_conc(out: &Path) {
    let st = goverify(
        &["extract", "--out", out.to_str().unwrap(), "./..."],
        &repo_root().join("testdata/corpus/conc"),
    );
    assert!(st.status.success(), "extract failed: {}", String::from_utf8_lossy(&st.stderr));
}

#[test]
fn debug_ir_prints_lowered_function() {
    let dir = tempfile::tempdir().unwrap();
    extract_conc(dir.path());
    let out = goverify(
        &["debug", "ir", "--gvir-dir", dir.path().to_str().unwrap(),
          "--func", "example.com/conc.Fan"],
        &repo_root(),
    );
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains("func example.com/conc.Fan"), "{text}");
    assert!(text.contains("select blocking=true"), "{text}");
    assert!(text.contains("go "), "{text}");
}

#[test]
fn debug_prepass_and_summary_render() {
    let dir = tempfile::tempdir().unwrap();
    extract_conc(dir.path());
    for what in ["prepass", "summary", "callgraph", "sccs"] {
        let out = goverify(
            &["debug", what, "--gvir-dir", dir.path().to_str().unwrap()],
            &repo_root(),
        );
        assert!(out.status.success(), "debug {what}: {}",
            String::from_utf8_lossy(&out.stderr));
        assert!(!out.stdout.is_empty(), "debug {what} printed nothing");
    }
}
```

Cargo wiring: `goverify-cli/Cargo.toml` gains `goverify-ir`, `goverify-analysis` (workspace-path deps) and dev-dependency `tempfile`.

- [ ] **Step 2: Run to verify failure** — `mise x -- cargo test -p goverify-cli --test debug_integration` — FAIL (no `debug` subcommand).

- [ ] **Step 3: Implement.** Extend the clap enum in `main.rs`:

```rust
#[derive(Subcommand)]
enum Cmd {
    Extract { /* existing */ },
    /// Inspect the analyzer's view of a module (phase-2 spec §7).
    Debug {
        #[command(subcommand)]
        what: DebugWhat,
    },
}

#[derive(clap::Args)]
struct DebugArgs {
    /// Directory of pre-extracted .gvir files. When omitted, extracts the
    /// current directory into a temp dir first.
    #[arg(long)]
    gvir_dir: Option<PathBuf>,
    /// Restrict output to one function (exact ssa id).
    #[arg(long)]
    func: Option<String>,
    /// Go package patterns for extraction (ignored with --gvir-dir).
    #[arg(default_value = "./...")]
    patterns: Vec<String>,
}

#[derive(Subcommand)]
enum DebugWhat {
    Ir(DebugArgs),
    Callgraph(DebugArgs),
    Sccs(DebugArgs),
    Prepass(DebugArgs),
    Summary(DebugArgs),
}
```

Handler:

```rust
fn run_debug(what: DebugWhat) -> Result<(), Box<dyn std::error::Error>> {
    let (kind, args) = match what {
        DebugWhat::Ir(a) => ("ir", a),
        DebugWhat::Callgraph(a) => ("callgraph", a),
        DebugWhat::Sccs(a) => ("sccs", a),
        DebugWhat::Prepass(a) => ("prepass", a),
        DebugWhat::Summary(a) => ("summary", a),
    };
    let _tmp; // keep tempdir alive
    let gvir_dir = match args.gvir_dir {
        Some(d) => d,
        None => {
            let sidecar = Sidecar::build(&extractor_dir()?, &sidecar_build_dir())?;
            let tmp = tempfile::tempdir()?;
            let patterns: Vec<&str> = args.patterns.iter().map(String::as_str).collect();
            sidecar.extract(Path::new("."), &patterns, tmp.path())?;
            let d = tmp.path().to_path_buf();
            _tmp = tmp;
            d
        }
    };
    let program = goverify_ir::Program::load_dir(&gvir_dir)?;
    for d in program.diagnostics() {
        eprintln!("goverify: {d}");
    }
    // --func is a substring filter everywhere (help text says so).
    let selected = |name: &str| args.func.as_deref().is_none_or(|f| name.contains(f));
    match kind {
        "ir" => {
            for f in program.func_ids() {
                if program.func(f).is_some() && selected(program.func_name(f)) {
                    print!("{}", goverify_ir::dump_function(&program, f));
                    println!();
                }
            }
        }
        "callgraph" => {
            let g = goverify_ir::CallGraph::build(&program);
            print!("{}", goverify_ir::dump_callgraph(&program, &g));
        }
        "sccs" => {
            let g = goverify_ir::CallGraph::build(&program);
            let s = goverify_ir::Sccs::compute(&program, &g);
            print!("{}", goverify_ir::dump_sccs(&program, &s));
        }
        "prepass" | "summary" => {
            let a = goverify_analysis::analyze(&program, &goverify_analysis::Options::default());
            for d in &a.diagnostics {
                eprintln!("goverify: {d}");
            }
            if kind == "prepass" {
                print!("{}", goverify_analysis::dump_prepass(&program, &a, args.func.as_deref()));
            } else {
                print!("{}", goverify_analysis::dump_summaries(&program, &a, args.func.as_deref()));
            }
        }
        _ => unreachable!(),
    }
    Ok(())
}
```

**tempdir lifetime:** the `_tmp` sketch above doesn't compile as written
(assignment to an undeclared binding in one arm) — declare
`let mut _tmp: Option<tempfile::TempDir> = None;` before the match and
`_tmp = Some(tmp);` inside. The requirement: the temp dir outlives
`load_dir`.

- [ ] **Step 4: Run tests** — integration tests PASS.

- [ ] **Step 5: Lint + commit**

```bash
git add crates/goverify-cli/ Cargo.lock
git commit -m "cli: goverify debug ir|callgraph|sccs|prepass|summary"
```

---

### Task 16: Corpus expansion + dump-determinism suite

**Files:**
- Create: `testdata/corpus/ops/{go.mod,ops.go}`, `testdata/goldens/ops.ir.txt`, `testdata/goldens/conc.summary.txt`
- Modify: `crates/goverify-ir/tests/lower_golden.rs`, `mise.toml` (corpus task)

**Interfaces:**
- Consumes: everything (Tasks 1–15).
- Produces: the blocking-tier determinism guarantee for phase-2 artifacts: byte-identical debug dumps across two clean extract+analyze runs, plus goldens covering every op family.

- [ ] **Step 1: Write the op-family corpus module.** `testdata/corpus/ops/go.mod`:

```
module example.com/ops

go 1.25.10
```

`testdata/corpus/ops/ops.go` — one small function per op family, no imports (keeps the DAG tiny; `conc` already covers sync/chans):

```go
// Package ops exercises every value-op family for lowering goldens.
package ops

type pair struct {
	a, b int
	next *pair
}

func Deref(p *pair) int          { return p.a }                  // load, field-addr
func StoreIt(p *pair, v int)     { p.b = v }                     // store
func Idx(xs []int, i int) int    { return xs[i] }                // index-addr, load
func ArrIdx(xs [4]int, i int) int { return xs[i] }               // index
func Look(m map[string]int, k string) (int, bool) { v, ok := m[k]; return v, ok } // lookup comma-ok
func Sl(xs []int) []int          { return xs[1:3] }              // slice
func Arith(a, b int) int         { return a / b }                // binop div
func Narrow(x int64) int8        { return int8(x) }              // convert narrowing
func Assert(v any) (int, bool)   { n, ok := v.(int); return n, ok } // type-assert
func Closure(x int) func() int   { return func() int { return x } } // make-closure, freevar
func Iface(v any) any            { return v }                    // make-interface at call sites
func Ptr() *pair                 { return &pair{} }              // alloc heap
func Loop(xs []int) int {                                        // phi, branch, jump
	s := 0
	for _, x := range xs {
		s += x
	}
	return s
}
func Panics(v any) { panic(v) }                                  // panic
func Variadic(xs ...int) int     { return len(xs) }              // builtin len
```

- [ ] **Step 2: Extend the golden test** (`lower_golden.rs`):

```rust
#[test]
fn ops_ir_matches_golden() {
    check_golden("ops.ir.txt", &dump_module("ops", "example.com/ops"));
}
```

Generate with `UPDATE_GOLDENS=1`, **review the dump by hand** — this
review IS the acceptance test for lowering semantics. Verify, one by one:
`Deref` shows `field-addr … #0` + `load`; `Look` shows `lookup … ok=true`
then two `extract`; `Arith` shows `binop Div`; `Narrow` shows `convert`;
`Assert` shows `type-assert … ok=true`; `Closure` shows `make-closure`;
`Loop` shows `phi` and `branch`; `Variadic` shows `call-builtin len`. If
any op family is missing from the dump, the lowering (or the corpus
function) is wrong — fix before committing.

- [ ] **Step 3: Add the summary golden** for conc (guards effects + prepass output shape end-to-end). In `crates/goverify-analysis/tests/engine_corpus.rs`:

```rust
#[test]
fn conc_summaries_match_golden() {
    let p = goverify_ir::testutil::load_corpus("conc");
    let a = analyze(&p, &Options::default());
    // Only conc's own functions: stdlib summaries churn with Go bumps.
    let text = dump_summaries(&p, &a, Some("example.com/conc"));
    goverify_ir::testutil::check_golden("conc.summary.txt", &text);
}
```

(`dump_summaries`'s `filter` is the substring filter defined in Task 14;
`check_golden` lives in `goverify_ir::testutil` since Task 8.)

- [ ] **Step 4: The determinism test** — extend `crates/goverify-cli/tests/debug_integration.rs`:

```rust
#[test]
fn debug_dumps_are_byte_identical_across_extract_and_analyze_runs() {
    let (d1, d2) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    extract_conc(d1.path());
    extract_conc(d2.path());
    for what in ["ir", "callgraph", "sccs", "prepass", "summary"] {
        let o1 = goverify(&["debug", what, "--gvir-dir", d1.path().to_str().unwrap()],
                          &repo_root());
        let o2 = goverify(&["debug", what, "--gvir-dir", d2.path().to_str().unwrap()],
                          &repo_root());
        assert!(o1.status.success() && o2.status.success());
        assert_eq!(o1.stdout, o2.stdout, "debug {what} is nondeterministic");
    }
}
```

This is the phase-2 extension of the §12 determinism suite: it covers
extraction → lowering → call graph → SCC → rayon-parallel analysis →
dump, twice from clean.

- [ ] **Step 5: Wire the corpus task.** `mise.toml`:

```toml
[tasks.corpus]
description = "corpus + determinism suite (full extractor pipeline)"
run = [
  "cargo test -p goverify-extract --test extract_integration",
  "cargo test -p goverify-ir --test lower_golden --test lower_corpus --test callgraph_corpus",
  "cargo test -p goverify-analysis --test engine_corpus",
  "cargo test -p goverify-cli --test debug_integration",
]
```

- [ ] **Step 6: Run the whole blocking tier + timing check**

Run: `time mise run corpus && mise run test && mise run lint`
Expected: PASS; note the corpus wall-clock in the commit message — the
whole-DAG conc analysis must leave comfortable headroom in the 10-minute
budget (if it doesn't, the corpus modules are too dep-heavy — shrink them,
don't raise the budget).

- [ ] **Step 7: Commit**

```bash
git add testdata/ crates/ mise.toml
git commit -m "corpus: op-family module, ops/conc goldens, dump-determinism suite"
```

---

### Task 17: Property tests

**Files:**
- Create: `crates/goverify-ir/tests/props.rs`
- Modify: `crates/goverify-ir/Cargo.toml` (dev-dep `proptest`), root `Cargo.toml`

**Interfaces:**
- Consumes: `Program::from_packages`, `CallGraph`, `Sccs` (Tasks 5–10).
- Produces: the two §9.2 properties, bounded case counts (blocking tier).

- [ ] **Step 1: Add proptest.** Root `[workspace.dependencies]`: `proptest = "1"`. `goverify-ir` `[dev-dependencies]`: `proptest = { workspace = true }`.

- [ ] **Step 2: Write the properties** (`tests/props.rs`):

```rust
//! Property tests (phase-2 spec §9.2). Bounded cases: blocking tier.

use proptest::prelude::*;

use goverify_extract::gvir;
use goverify_ir::{CallGraph, Program, Sccs};

/// Arbitrary-ish instruction: known + unknown kinds, random operands and
/// sems left absent — exercises the malformed-input paths of lowering.
fn arb_instruction() -> impl Strategy<Value = gvir::Instruction> {
    let kinds = prop::sample::select(vec![
        "BinOp", "UnOp", "Store", "FieldAddr", "IndexAddr", "Lookup", "Slice",
        "Call", "Go", "Defer", "Select", "MakeClosure", "Phi", "Return",
        "Jump", "If", "Alloc", "TotallyUnknownKind",
    ]);
    (kinds, any::<u32>(), prop::collection::vec(any::<u32>(), 0..5))
        .prop_map(|(kind, register, operands)| gvir::Instruction {
            kind: kind.to_string(),
            register: register % 64,
            operands: operands.into_iter().map(|o| o % 64).collect(),
            ..Default::default()
        })
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

    /// Lowering totality: any structurally-valid package lowers without
    /// panicking, whatever the instruction contents.
    #[test]
    fn lowering_never_panics(instrs in prop::collection::vec(arb_instruction(), 0..30)) {
        let pkg = gvir::Package {
            import_path: "p".into(),
            functions: vec![gvir::Function {
                id: "p.F".into(),
                blocks: vec![gvir::BasicBlock { index: 0, instrs, succs: vec![] }],
                ..Default::default()
            }],
            ..Default::default()
        };
        let p = Program::from_packages(vec![pkg]);
        prop_assert!(p.lookup_func("p.F").is_some());
    }

    /// FuncId assignment, call graph, and SCC schedule are invariant under
    /// package input order.
    #[test]
    fn schedule_stable_under_package_reorder(seed in any::<u64>()) {
        // Fixed small program: 3 packages, cross-package static calls.
        let call = |target: &str| gvir::Instruction {
            kind: "Call".into(),
            sem: Some(gvir::instruction::Sem::Call(gvir::CallSem {
                static_callee: target.into(), ..Default::default() })),
            ..Default::default()
        };
        let func = |id: &str, callees: &[&str]| gvir::Function {
            id: id.into(),
            blocks: vec![gvir::BasicBlock {
                index: 0,
                instrs: callees.iter().map(|c| call(c))
                    .chain([gvir::Instruction { kind: "Return".into(), ..Default::default() }])
                    .collect(),
                succs: vec![],
            }],
            ..Default::default()
        };
        let pkg = |path: &str, fs: Vec<gvir::Function>| gvir::Package {
            import_path: path.into(), functions: fs, ..Default::default() };
        let mut pkgs = vec![
            pkg("a", vec![func("a.F", &["b.G", "c.H"])]),
            pkg("b", vec![func("b.G", &["c.H", "a.F"])]), // cross-package cycle a<->b
            pkg("c", vec![func("c.H", &[])]),
        ];
        // Deterministic pseudo-shuffle from the seed.
        pkgs.rotate_left((seed % 3) as usize);
        if seed % 2 == 0 { pkgs.swap(0, 1); }

        let p = Program::from_packages(pkgs);
        let g = CallGraph::build(&p);
        let s = Sccs::compute(&p, &g);
        let schedule_names: Vec<Vec<&str>> = s.schedule().iter()
            .map(|scc| scc.iter().map(|&f| p.func_name(f)).collect())
            .collect();
        prop_assert_eq!(schedule_names, vec![vec!["c.H"], vec!["a.F", "b.G"]]);
    }
}
```

- [ ] **Step 3: Run** — `mise x -- cargo test -p goverify-ir --test props` — PASS. Confirm the suite stays fast (bounded at 64 cases).

- [ ] **Step 4: Lint + commit**

```bash
git add crates/goverify-ir/ Cargo.toml Cargo.lock
git commit -m "ir: property tests — lowering totality, order-invariant schedule"
```

---

### Task 18: Fuzz target, docs, final sweep

**Files:**
- Create: `fuzz/fuzz_targets/ir_lower.rs`
- Modify: `fuzz/Cargo.toml`, `mise.toml` (fuzz task), `ARCHITECTURE.md`, `README.md`

**Interfaces:**
- Consumes: `Program::from_packages` (Task 5–7) — the reject-never-panic surface for `.gvir` bytes.

- [ ] **Step 1: Fuzz target.** `fuzz/fuzz_targets/ir_lower.rs`:

```rust
//! Decode arbitrary bytes as a gvir Package and lower it. Both stages
//! must reject or degrade — never panic (parent spec §12.4).

#![no_main]

use libfuzzer_sys::fuzz_target;
use prost::Message;

fuzz_target!(|data: &[u8]| {
    if let Ok(pkg) = goverify_extract::gvir::Package::decode(data) {
        let _ = goverify_ir::Program::from_packages(vec![pkg]);
    }
});
```

`fuzz/Cargo.toml`: add `goverify-ir = { path = "../crates/goverify-ir" }`
and `prost = "0.13"` to dependencies plus the `[[bin]]` block:

```toml
[[bin]]
name = "ir_lower"
path = "fuzz_targets/ir_lower.rs"
test = false
doc = false
```

`mise.toml` fuzz task runs both targets:

```toml
[tasks.fuzz]
description = "fuzz smoke run (nightly tier; needs rustup nightly)"
run = [
  "cargo +nightly fuzz run gvir_decode -- -max_total_time=60",
  "cargo +nightly fuzz run ir_lower -- -max_total_time=60",
]
dir = "{{cwd}}"
```

- [ ] **Step 2: Smoke the target** (needs nightly; if unavailable in this environment, `cargo fuzz build` alone still validates compilation — run what the sandbox permits and say which ran):

Run: `mise x -- cargo +nightly fuzz run ir_lower -- -max_total_time=30`
Expected: no crashes.

- [ ] **Step 3: Update docs.**
- `ARCHITECTURE.md`: `goverify-ir` and `goverify-analysis` entries change from "skeleton (phase 2)" to their real one-paragraph descriptions (IR + lowering + call graph/SCCs; engine + effects + prepass + placeholder summaries); `goverify-solver` notes "trait + stub; backends in phase 3". Keep it the *why* of boundaries, not a file tree.
- `README.md`: add `goverify debug ir ./...`-style example under the quickstart (verify the exact invocation works from a corpus module before writing it).

- [ ] **Step 4: Full pre-push sweep** (mirrors blocking CI):

Run: `mise run lint && mise run test && mise run corpus && mise run secrets && mise run audit`
Expected: all PASS.

- [ ] **Step 5: Commit**

```bash
git add fuzz/ mise.toml ARCHITECTURE.md README.md
git commit -m "fuzz: ir_lower target; docs: phase-2 architecture + debug quickstart"
```

---

## Execution notes

- Tasks are strictly ordered 1→18; Tasks 11–12 are independent of 8–10 and
  may run in parallel with them if using subagents, but nothing else
  reorders safely.
- Every task ends green: `mise run lint` + the named tests must pass
  before its commit.
- If `cargo +nightly` is unavailable for Task 18 Step 2, build-only
  (`cargo fuzz build`) is the fallback; note it in the commit message.
- The prost-generated Rust names used in code sketches
  (`gvir::instruction::Sem`, `gvir::TypeKind`, `r#type` fields) must be
  verified against the actual `gvir.v1.rs` codegen output in Task 5 before
  use; adjust mechanically if codegen names differ.
