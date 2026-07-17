# goverify phase 3 — Solver layer

**Date:** 2026-07-17
**Status:** Approved design
**Parent spec:** `2026-07-16-goverify-design.md` (§8 solver, §9 caching, §13 toolchain)
**Base:** `main` after the phase-2 merge (`d29bbe8`)

## 1. Goal and scope

Phase 3 replaces the phase-2 solver stub with the real solver layer and
proves the whole pipeline end-to-end: typed term language, canonical
SMT-LIB2 printer, `Z3Native` and `SmtLib2Process` backends, the query
cache, and a differential harness. Three scope decisions taken during
brainstorming:

1. **Full plumb + tracer obligation.** The engine is wired to build and
   discharge real obligations through the `Solver` trait, and one
   deliberately minimal "tracer" nil obligation exercises
   extractor → IR → engine → cache → Z3 → finding on the corpus. Phase 4
   grows the tracer into the real nil checker on proven rails.
2. **Effects-precision redesign (phase-2 final-review I2) is in scope.**
   Effects gain per-location identity so they no longer collapse to a
   global top. The deferred-`Unlock` lowering watch item from phase-2
   Task 7 is resolved here too.
3. **Phase-2 fast-follow batch is Task 1** (the eight minor deferred
   items listed in `.superpowers/sdd/progress.md`).

Out of scope (explicit non-goals):

- Real checker breadth — phase 4. The tracer stays minimal.
- `.gvspec` serialization and the extraction/summary cache layers —
  phase 5. Terms need no protobuf form yet; summaries stay in-memory.
- Annotation language — phase 6.
- Concurrency checkers — phase 7. Phase 3 only makes `Effects` precise
  enough that phase 7 doesn't inherit a lossy lattice.
- The polished `goverify check` UX — phase 4. Findings surface through
  `goverify debug findings`.

## 2. Crate layout

| Crate | Phase-3 change |
|---|---|
| `goverify-solver` | Rebuilt: sorts + terms, smart constructors, canonical printer, s-expr/term reader, `Z3Native`, `SmtLib2Process`. `Solver` trait signature unchanged; `Decl`/`Term` innards become the typed AST. |
| `goverify-cache` | **New.** Generic content-addressed store + the query-cache layer. |
| `goverify-checkers` | **New.** `Checker` trait embryo + the nil-deref tracer. |
| `goverify-analysis` | `PlaceholderFormula` → real formulas; effects redesign; obligation discharge loop. |
| `goverify-cli` | Solver/cache flags, `--emit-smt`, `goverify debug findings`. |

New dependencies: `z3-sys` (static bundled build). The mise toolchain
gains a pinned `z3` CLI (differential-harness default for
`--solver-cmd`). Both justified under the parent spec's few-dependencies
rule: they *are* the phase.

## 3. Term language

Quantifier-free, four theories (parent spec §5):

- `Sort = Bool | BitVec(w) | Array(Box<Sort>, Box<Sort>) | Datatype(DtId)`.
  Go integers are fixed-width bitvectors; slices/maps use array theory at
  the interface level; pointer nil-ness is a small ADT
  (`(Ptr nil | (addr (_ BitVec 64)))`).
- Terms: literals, variables (names derived deterministically from IR
  IDs), boolean connectives, `ite`, BV arithmetic/comparisons, array
  `select`/`store`, datatype constructors/testers/accessors.
- Every `Term` carries its sort. Sort-checked smart constructors reject
  ill-sorted construction at build time; ill-sorted terms are
  unconstructible through the public API.
- Representation: plain immutable boxed AST. No hash-consing/interning —
  obligation formulas are small; revisit only if profiling says so.

## 4. Canonical printer and the single-lowering rule

The canonical printer is the **only** Term → SMT-LIB2 lowering in the
codebase. Both backends consume its output, so the cached/emitted
artifact is byte-identical to what the solver actually solved — a
divergence between "what we hashed" and "what we solved" is structurally
impossible, not merely tested for.

- Output: fixed logic string per query class (`QF_BV`, `QF_ABV`,
  `QF_DT`, …), sorted declarations, stable symbol names from IR IDs,
  zero formatting variance.
- `blake3(canonical bytes)` is the query identity; `--emit-smt <dir>`
  dumps the texts.
- Consequence for testing: the differential harness guards transport and
  solver-build agreement; the printer itself is guarded by
  print → parse → reprint fixpoint properties (the reader from §6 makes
  this possible).

## 5. Backends

**`Z3Native`** — `z3-sys`, static bundled build (parent spec §13's
preferred path; the one-time cold compile is accepted and CI-cached; if
it proves unbearable mid-phase, the fallback is mise-pinned dynamic
libz3 behind the same API). One incremental context per rayon worker.
Per query: `push` → `Z3_parse_smtlib2_string(canonical text)` →
check-sat under per-query timeout (default 100 ms, configurable) and
memory cap → `pop`. Z3 error or crash ⇒ `Unknown`, worker context
rebuilt. `Z3_get_full_version` is the solver-identity string fed to the
query-cache key.

**`SmtLib2Process`** — pipes the same canonical bytes to `--solver-cmd`
(default: the mise-pinned `z3` binary). Timeout enforced by process
kill. Responses (`sat`/`unsat`/`unknown`, model s-exprs) go through the
hardened reader; malformed output is rejected without panicking and
degrades to `Unknown`.

Timeout semantics everywhere (parent spec §8): `Unknown` ⇒ no report.
Timeouts must never create false positives.

## 6. Reader

One s-expression/term reader in `goverify-solver`, shared by: response
parsing (`SmtLib2Process` models), the round-trip property suite, and a
fuzz target. It parses bytes the analyzer didn't write, so per the
project rule it rejects, never panics.

## 7. `goverify-cache`

Generic content-addressed store, designed once so phase 5 adds layers
instead of migrating code:

- blake3 keys; layout `<root>/<layer>/<hh>/<hash>` where `hh` is the
  first two hex chars. Root: `~/.cache/goverify/` default, project-local
  `.goverify/cache` mode, both overridable (parent spec §9).
- Atomic writes (tempfile + rename), advisory file locking for
  concurrent runs, corrupt/truncated entries are misses and rebuilt.
- **Query-cache layer:** key =
  `blake3(canonical text ⊕ solver identity ⊕ solver version ⊕ timeout/memory limits)`;
  value = `SatResult` plus, for `sat`, the model text so warm runs can
  render traces without re-solving. `Unknown` is cached too — it is
  deterministic per solver build, which is exactly why solver identity
  and limits are in the key.

## 8. Engine plumbing and the tracer

- `PlaceholderFormula` is replaced by `Formula`: a `Term` whose free
  variables are `IfaceVar`s (param *i* / result *i*).
  `instantiate_requires` keeps its shape and its None-binding semantics
  ("cannot evaluate ⇒ do not report"), now substituting caller-side
  terms for interface variables.
- The engine gains an obligation discharge loop: after the pre-pass,
  checkers visit the function and emit obligations of the form
  `path condition ∧ ¬safety condition`. A single solver-layer entry
  point renders the canonical text exactly once, keys the query cache
  with it, and on a miss drives the backend with those same bytes.
  `sat` = finding; `unsat`/`unknown` = silence. `value_clean` functions
  skip constraint generation entirely, as designed.
- **Tracer** (in `goverify-checkers`, the nil checker's embryo): a
  minimal symbolic environment (params symbolic, constants concrete,
  loads havoc). It (a) infers `requires p != nil` for parameters
  unconditionally dereferenced with no dominating nil check — Z3 decides
  whether the nil path is satisfiable — and (b) checks instantiated
  requires at call sites, so a literal-`nil` argument yields the first
  end-to-end finding. Corpus cases pin both directions (finding fires;
  guarded deref stays silent).

## 9. Effects redesign (I2)

`Effects` gain location identity; the global collapse-to-top is removed:

- `Loc { root, path }` with
  `Root = Param(u32) | Global(id) | Alloc(site) | Unknown` and a bounded
  field path (default depth 2); overflow degrades the root to `Unknown`.
  "Top" becomes the finite `Unknown → all ops` bucket.
- `lock_ops` / `chan_ops` become `BTreeMap<Loc, BTreeSet<Op>>`
  (deterministic iteration); `Spawns` is unchanged; join is pointwise
  union — still commutative, idempotent, monotone (property-tested).
- Summary instantiation rebases callee `Param`-rooted locations through
  caller arguments when the argument is itself path-shaped; otherwise
  the entry lands in `Unknown`.
- `defer mu.Unlock()` (phase-2 Task 7 watch item) is recognized during
  collection and recorded as distinct `DeferredUnlock` /
  `DeferredRUnlock` ops on the receiver's location, giving phase C the
  distinction natively.

## 10. CLI surface

- `goverify debug findings` — stable text output of discharge-loop
  findings, used by corpus goldens.
- `--emit-smt <dir>`, `--solver-cmd <bin>`, `--solver-timeout-ms`,
  cache-root override flags.
- Exit codes unchanged from parent spec §10.

## 11. Error handling

Parent spec §11 applies verbatim; phase-3 specifics: solver crash →
worker restart, in-flight obligation counts as timeout (`Unknown`);
reader rejects malformed solver output without panicking; corrupt cache
entry = miss; a `z3-sys` build/version mismatch with the pinned CLI is
surfaced in the differential harness, not silently tolerated.

## 12. Testing

- **Unit:** ill-sorted construction rejected; printer goldens;
  cache-key composition; effects rebase cases.
- **Property (proptest):** term → canonical → parse → reprint fixpoint;
  cache-key stability under declaration reordering; effects join laws.
- **Fuzz (cargo-fuzz):** the s-expr/term reader (new target, wired into
  nightly alongside `gvir_decode` and `ir_lower` — nightly wiring is in
  each task's file list this time).
- **Differential:** `Z3Native` vs `SmtLib2Process(z3)` must agree —
  small fixed query set in the blocking tier, full sweep nightly.
- **Corpus:** tracer nil cases with `// want` comments; the determinism
  suite extends to byte-comparing `--emit-smt` artifacts across double
  runs.
- Blocking tier stays within the parent spec's 10-minute budget; the
  z3-sys build is CI-cached.

## 13. Task shape (plan's job to finalize)

Task 1 = phase-2 fast-follow batch (eight minor items from
`progress.md`: T2 universe-scope Named test; T5 `diags.len()==1`
assert; T6 succs + FuncId-order doc comments; T7 RWMutex test; T11
`Solver` doc comments; T12 `Result → None` test; T15 `--func` warn on
callgraph/sccs; T17 ops-corpus plain recv + Extract/TypeAssert in the
property kind pool). Then roughly: term language → printer + reader →
`goverify-cache` → `Z3Native` (incl. z3-sys/mise plumbing) →
`SmtLib2Process` + differential harness → effects redesign →
formula/summary swap → tracer + discharge loop → corpus/determinism/CLI
→ fuzz + docs.

## 14. Open questions (deferred, not blocking)

- Whether `Z3_parse_smtlib2_string` per query ever shows up in profiles
  at real-repo scale — if so, the single-lowering rule still holds; the
  fix would be caching parsed declaration prefixes, not a second
  lowering.
- Model-text cache entries could grow large on pathological queries;
  cap-and-degrade (re-solve on render) if it ever matters.
- Effects path-depth bound (2) is a guess; phase 7 tunes it with real
  checker FP/FN data.
