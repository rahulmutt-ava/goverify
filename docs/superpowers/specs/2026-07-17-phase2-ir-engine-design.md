# goverify phase 2 — IR + engine core

**Date:** 2026-07-17
**Status:** Approved design
**Parent spec:** `2026-07-16-goverify-design.md` (§15 item 2)

## 1. Scope

Deliver the engine core with nothing solver-shaped in it:

- `.gvir` schema enrichment (one `schema_version` bump) + matching extractor
  changes.
- `goverify-ir`: typed analyzer IR, SSA→IR lowering, global cross-package
  call graph.
- `goverify-analysis`: SCC condensation scheduler, per-domain pre-pass,
  summary instantiation and bounded fixpoint over **in-memory** summaries.
- `goverify-solver`: the `Solver` trait (parent spec §8) plus a `StubSolver`
  answering `Unknown`.
- `goverify debug ir|callgraph|sccs|prepass|summary` — the phase's observable
  surface and test workhorse.

Out of scope (later phases): checkers, real terms/Z3, `.gvspec`
serialization, caches, baselines/SARIF.

Decisions taken during brainstorming:

1. **Enrich the `.gvir` schema** with structured instruction/type/const
   payloads rather than parsing ssa `String()` reprs in Rust.
2. **Lowering lives in Rust** (`goverify-ir`); the extractor stays a
   faithful, semantics-free serializer of x/tools SSA. Lowering iterations
   must not invalidate extraction caches.
3. **Summaries are in-memory only** with a placeholder formula type; no
   serialization until phase 3 gives formulas a real representation.
4. **The engine schedules the whole import DAG** (stdlib + deps), no
   targets-only mode.

## 2. `.gvir` schema enrichment

One coordinated bump: `schema_version` + `SCHEMA_VERSION` (Rust) +
`schemaVersion` (Go) together; regen via `mise run proto-gen`; commit
`extractor/gvirpb/`; determinism suite must pass unchanged.

### 2.1 Instructions

`Instruction` gains a `oneof sem` carrying structured payloads **only** for
kinds where `operands` + `type` are insufficient:

| Kind | Payload |
|---|---|
| `BinOp` | operator token |
| `UnOp` | operator token, comma-ok flag |
| `Call` / `Defer` / `Go` | static-callee function id **or** invoke-mode interface method (name + interface type id) |
| `FieldAddr` / `Field` | field index + field name |
| `TypeAssert` | asserted type id, comma-ok flag |
| `Extract` | tuple index |
| `Lookup` | comma-ok flag |
| `Alloc` | heap flag |
| `Select` | per-state (direction, channel operand, send-value operand), blocking flag |

Kinds fully described by operands (`Store`, `Phi`, `Jump`, `Return`,
`MakeSlice`, …) get no payload. `kind`/`detail` strings stay for debugging;
**the Rust loader never parses them.**

### 2.2 Types

`Type` gains a structured form alongside the display `repr`: a kind enum
(basic, named, pointer, slice, array, map, chan, struct, interface,
signature, tuple, type-param) plus component type ids (element, key, field
types + names, underlying id for named types, param/result tuples for
signatures, chan direction, array length).

### 2.3 Constants

Const `AuxValue`s gain a structured value: `oneof` over bool, i64,
big-int-as-decimal-string, float bits, string bytes, nil, complex-as-string.

The extractor records what x/tools SSA says and interprets nothing.

## 3. `goverify-ir`

### 3.1 Data model

Index-based arenas, no pointers:

- Global `TypeTable` interning structured types across packages, keyed by
  canonical repr string (per-package `.gvir` tables describe the same type
  independently; interning unifies them).
- Global `FuncId` interned by the stable string id
  (`(*pkg.T).Close`) — the key for call edges and summaries.
- Per-function `Function { params, values, blocks }` with `u32`-indexed
  vectors; every instruction keeps its `Position`.

### 3.2 Op set (~31 + intrinsics)

`assign`, `alloc`, `load`, `store`, `field-addr`, `index-addr`, `index`
(array/string value), `lookup` (map read), `slice`, `binop`, `unop`,
`convert`, `extract`, `phi`, `call`, `call-invoke`, `make-closure`,
`make-interface`, `make` (chan/map/slice), `send`, `recv`, `close`,
`select`, `go`, `defer`, `return`, `jump`, `branch`, `panic`,
`type-assert`, `havoc`; plus `lock`/`unlock`/`rlock`/`runlock` produced by
rewriting static calls to `sync.Mutex`/`sync.RWMutex` methods.

### 3.3 Lowering rules

- **Total, never fails.** Every SSA instruction lowers to something;
  anything unmodeled (`DebugRef`, unknown future kinds, exotic constructs)
  lowers to `havoc` of its result plus a counted diagnostic —
  degrade-never-die applied to the frontend.
- Comma-ok forms lower to the op plus an `extract`-visible tuple, matching
  SSA.
- `Range`/`Next` lower to havoc-producing loop primitives in v1 (map
  iteration order is unmodelable); loop *structure* survives via normal
  blocks and branches.

### 3.4 Guarantees to checkers (parent spec §4 made concrete)

- Every pointer dereference is exactly a `load`/`store`/`field-addr` chain.
- Every possible-nil source (params, loads, `lookup` results, non-ok
  `type-assert`, interface conversions) is identifiable from ops alone.
- Every index/slice/division/narrowing is its own op.
- Every concurrency effect is one of the concurrency ops: `make` of a
  chan, `send`, `recv`, `close`, `select`, `go`, the four lock intrinsics,
  or a `defer` of one of these.

## 4. Call graph and scheduler

### 4.1 Edges

One global graph over `FuncId`s, built from lowered IR across the whole DAG:

- `call` with static callee → precise edge.
- `call-invoke` → implements-based approximation: from all packages'
  `MethodSet` tables, index interface → concrete implementers (method-set
  inclusion; name + signature type match); an invoke of `I.M` gets edges to
  every implementer's `M`.
- Dynamic function-value calls → edges to every **address-taken** function
  (appearing as an `AuxValue` function reference or `make-closure` operand
  anywhere in the DAG) with matching signature type. Coarse, but extra
  edges only widen summaries — they never invent findings.

Bodyless functions (assembly, external linkage) and functions from packages
that failed extraction get **havoc summaries with no requires** (parent
spec §11): callers proceed, no false positives from missing info.

### 4.2 Scheduling

Tarjan SCC condensation with deterministic tie-breaking (iteration ordered
by stable `FuncId` string), reverse-topological schedule over the
condensation. An SCC is ready when all callee SCCs are summarized; ready
SCCs run in parallel on rayon. Determinism of *results* comes from
summaries being pure functions of their inputs, not from execution order.

### 4.3 Recursive SCCs

Bounded fixpoint: members start from the optimistic empty summary,
re-analyze until no member's summary changes, widen to havoc after k
rounds (default 3, configurable). "Changed" is structural equality on
clause sets — real machinery over placeholder formulas.

## 5. Pre-pass

Intraprocedural, syntactic, classified **per obligation domain**, not one
global bit:

- **value-clean**: no `load`/`store`/`field-addr` on non-locally-allocated
  pointers, no `index`/`lookup`/`slice`, no division/mod `binop`, no
  narrowing `convert`.
- **concurrency-clean**: none of the concurrency ops listed in §3.4, and
  all callees concurrency-clean.

Clean domains skip constraint generation in phase 4+. In phase 2 the
classification is computed, stored on the function, and dumped via
`goverify debug prepass` so the "most functions are trivially clean" claim
is measurable on real corpora.

## 6. Summary plumbing and stub solver

- `Summary { requires: Vec<Clause>, ensures: Vec<Clause>, effects: Effects,
  provenance }` with clause shapes from parent spec §5.
- Formula payload is `PlaceholderFormula` — a structural type (opaque tag +
  the interface variables it mentions). Phase 3 replaces it with real terms
  behind the same API. Call-site substitution (callee params → caller args)
  is implemented structurally now, so instantiation plumbing is real.
- **Effects are not placeholders.** Locks, channel ops, spawns
  (bounded/unbounded) are syntactic facts the IR carries; effect summaries
  and their propagation are fully functional in phase 2.
- `goverify-solver` ships the §8 `Solver` trait and `StubSolver` answering
  `Unknown` — which per timeout semantics means "no report". The engine
  calls through the trait, so phase 3 swaps the implementation, not the
  call sites.

## 7. Debug CLI

`goverify debug ir|callgraph|sccs|prepass|summary <target>` — canonical,
deterministic text renderings: sorted output, stable ids, no absolute
paths. These dumps are the phase's user-visible deliverable, its test
surface, and the debugging tool for later phases.

## 8. Error handling

Parent spec §11 applied:

- Missing/corrupt `.gvir` → skip + diagnostic; callees havoc'd.
- Schema or Go-version mismatch → actionable error naming both versions.
- Unknown instruction kind → `havoc` + counted diagnostic.
- Per-function `catch_unwind` in the scheduler from day one.

## 9. Testing

1. **Unit**: lowering per-op, Tarjan determinism, fixpoint
   convergence/widening, substitution.
2. **Property** (proptest): SCC schedule stable under function reordering;
   lowering totality (any decodable `.gvir` lowers without panic).
3. **Corpus**: extend `testdata/corpus/` with packages exercising each op
   family; determinism suite byte-compares all `debug` dumps across two
   clean runs.
4. **Curated goldens**: a handful of hand-reviewed golden dump files guard
   lowering semantics without golden-file sprawl.
5. **Fuzz**: extend the `.gvir` decoder target to the enriched schema; new
   target for the IR loader. Reject, never panic.
6. All wired into `mise run test` (blocking tier). The blocking-tier corpus
   run uses a tiny module with few stdlib deps so the whole-DAG analysis
   fits the 10-minute budget; a full avalanchego-scale run stays nightly
   territory (parent spec §12).
