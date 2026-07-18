# Phase 4: Nil-safety + bounds checkers — design

Date: 2026-07-18
Status: approved
Parent spec: `2026-07-16-goverify-design.md` (§7 phase A, §10 CLI, §12 testing, §15 item 4)

## 1. Context and goal

Phase 3 ended with a deliberately minimal `NilTracer`: entry-block
unconditional derefs plus constant-nil call arguments, built to prove the
extractor→IR→engine→cache→Z3→finding pipeline end to end. Its header says
what phase 4 must do: replace the heuristics with real path-sensitive
analysis behind the same `Checker` trait.

Phase 4 delivers the parent spec's phase-A bundle:

- a **gated-SSA discharge engine** — the shared symbolic-value machinery
  both checkers run on;
- the **nil-safety checker**, rewritten path-sensitively;
- the **bounds/overflow checker** (index, slice, division, narrowing);
- **`goverify check ./...`** with Infer-style violating-path traces and
  real exit codes (0 clean / 1 findings / 2 analyzer error);
- a **machine-checked corpus harness** (want-comments);
- an end-of-phase **shakeout on a pinned small real repo**.

### Non-goals

SARIF, baselines, diff-base, and the speed milestone (phase 5); the
annotation language (phase 6); concurrency checkers (phase 7);
interprocedural heap modeling; loop unrolling beyond the first iteration
(the depth stays a future knob). Overflow of ordinary `+`/`*` is **not** an
obligation: Go defines wrapping, and the parent spec's §7 obligation list
(dereference, index, slice op, division, narrowing conversion) deliberately
omits it.

## 2. Approach decision: gated SSA, not path enumeration

Three shapes were weighed for path sensitivity:

- **A. Bounded forward path enumeration** — free traces, simplest, but
  idiomatic Go (`if err != nil` every few lines) gives ~2^n paths; any
  path budget is exhausted precisely on the mid-size functions where bugs
  live, and the no-FP discipline then forces silence. FN cliff.
- **B. Gated-SSA / BMC-style encoding** — encode the whole function once,
  one canonical query per obligation; case-splitting is delegated to Z3's
  SAT core. Uniform coverage; the only FN sources are the loop bound and
  per-query timeouts. Traces must be reconstructed from the sat model.
- **C. k-bounded disjunctive forward execution (Pulse-style)** — strictly
  generalizes A; bounded resources and free traces, but which disjuncts
  survive a join is an opaque policy — FN behavior becomes "the 17th
  disjunct got dropped", and it is the most machinery of the three. It
  also reopens stateful solver reuse across `catch_unwind` (a carried
  phase-3 question), which B never asks.

**Decision: B.** It matches the existing architecture exactly — one
canonical query per obligation is the `discharge_query` + query-cache
contract from phase 3, unchanged — and the encoder is a pure function
from lowered IR to terms, which slots into the project's strongest test
muscles (goldens, differential harness, determinism suite, fuzz). The
verified enablers already exist: `Op::Phi { dst, edges }` carries
per-predecessor values, the `Term` surface has ite/arrays/datatypes/
`substitute`/`free_vars`, and the `Solver` trait exposes
`check_sat_assuming`. If the shakeout shows trace quality or query size
hurting, a C-style walker can be added behind the same `Checker` trait
without touching the solver layer — escape hatch, not starting point.

## 3. Function encoding

A new module in `goverify-analysis` (no new crate): a pure function from
a lowered `Func` to declarations + assertions over the existing `Term`
surface.

- **Cut back edges first** (DFS over block `succs`), yielding a DAG.
  Loops are analyzed as their first iteration — bounded unrolling with
  k = 1, achieved by cutting rather than duplicating blocks. Reachability
  stays **under-approximate**: `sat` always witnesses a genuine ≤ 1-
  iteration execution. Havocking loop-carried values instead would make
  `s[i]` under `i < len(s)` satisfiable with negative `i` — the classic
  false positive this design refuses. Bugs past iteration 1 are a
  disclosed FN class.
- **One Bool guard per block.** The entry guard is asserted true; a
  non-entry block's guard is the OR of its incoming edge guards; an edge
  guard is the source block's guard ∧ the branch condition along that
  edge (`If` cond register, negated on the else edge; `Jump` passes the
  guard through).
- **Phi nodes become ite chains** over incoming edge guards, using the
  per-predecessor values `Op::Phi` already carries. Back-edge inputs
  drop with the cut edges.
- **Pure ops assert unguarded defs** (`dst = f(operands)` — sound in SSA:
  values on unreached paths are defined but unconstrained). Any op the
  encoder does not model havocs its dst to a fresh unconstrained
  variable — the same degrade posture the IR lowering already uses.
- **Minimal memory model.** No heap. Loads havoc. Slices and strings get
  a datatype sort carrying a `len` field (via existing
  `dt_ctor`/`dt_get`), so `len(s)`, `MakeSlice`, and constant lengths
  constrain bounds queries; two uses of the same slice value see the
  same length. Arrays take static lengths from their type. Map lookups
  havoc; indexing a map is not a bounds obligation.
- **Size cap.** A function whose encoding exceeds a term-count cap is
  skipped with a diagnostic (degrade, never die).

An obligation is one canonical query — `entry ∧ reach(site) ∧ violation` —
dispatched through `discharge_query` unchanged, cache and all. Multiple
obligations over one function may batch via `check_sat_assuming`; each
still keys the query cache by its own canonical text.

## 4. Checkers

Both checkers follow parent §7's rule: local obligations are discharged
**under the function's inferred/declared preconditions**. That one rule
settles who reports what:

- an unguarded param deref makes `reach ∧ p = nil` sat → that is a
  `requires p ≠ nil` clause on the function, not a report;
- the same query under the assumed requires is unsat → no local report;
- a caller passing something nilable trips the instantiated requires at
  the call site and gets the report **there**, with the callee clause
  named in the trace.

A guard dominating a deref makes reachability imply non-nil — which is
exactly the fix for the carried "entry-block-panic latent FP class".

**Nil** (rewrites `goverify-checkers/src/nil.rs`, same `Checker` trait):
obligation sites are Load/Store/Field/FieldAddr with pointer-typed
address. `infer_requires` covers param-rooted sites, sat-gated as today
but now path-sensitive. Call-site obligations instantiate callee
requires with **real symbolic argument terms** from the encoding —
no longer only literal nils.

**Bounds/overflow** (new `goverify-checkers/src/bounds.rs`):

- Index/IndexAddr/Slice → `0 ≤ idx < len` (slice expressions use the
  applicable bound: cap where Go checks cap);
- Div/Rem → `divisor ≠ 0`, plus the MinInt/−1 signed-division overflow
  case (cheap in bitvectors);
- narrowing Convert → value-fits-target, signed/unsigned-aware, building
  on the phase-3 `int_width` fix (float/int and signedness are distinct).

With a second checker live, requires clauses are **sorted by checker
name** when summaries merge — retiring carried item T12.

## 5. `check` command and traces

`goverify check [patterns]`: runs extract→lower→analyze→checkers, renders
findings, exits 0/1/2 (retiring the phase-3 exit-code stub). `debug
findings` remains as the raw view. Human output per finding:

- labeled source span — `file:line:col`, the source line, a caret span;
- the step-by-step violating path (Infer-style), one step per source
  position along the reconstructed path;
- concrete model values where the model names them (e.g. `i = 0`,
  `len(s) = 0`);
- the callee requires clause that fired, when the finding came from an
  instantiated summary.

**Trace reconstruction:** walk the CFG from entry, at each branch taking
the successor whose edge guard is true in the sat model. Guards are named
symbols, so the existing `Model` surface only needs per-symbol lookup; a
guard the model leaves unassigned is a don't-care and resolves by a
deterministic first-successor rule. Reconstruction is golden-tested on
the corpus.

## 6. Corpus harness

analysistest-style machine-checked comments: `// want: <tag>` on the line
of the expected finding (e.g. `// want: nil-deref`). The harness runs the
full pipeline over each corpus module and fails on **either** direction
of mismatch — an unexpected finding or an unsatisfied want.

- `testdata/corpus/nil/` grows beyond the phase-3 embryo: guards,
  loop-carried pointers, multi-hop call chains, interface-typed nils.
- New `testdata/corpus/bounds/`: index/slice/division/narrowing cases,
  both violating and safely-guarded.
- Rendered `check` output joins the byte-compare determinism suite.
- The differential Z3Native-vs-SmtLib2Process harness extends over the
  new obligation queries.

## 7. Shakeout

End-of-phase run against a **pinned checkout of `etcd-io/bbolt`**
(~10k LOC, pointer- and slice-heavy, pure Go — dense in exactly the two
obligation classes). A `mise` task drives it; it is not in blocking CI.
Exit criteria:

1. all findings triaged;
2. FP rate recorded in a short report committed under `docs/`;
3. every fixed FP lands a corpus case (parent §12 rule);
4. dynamic-dispatch precision observations (parent §16, carried T10
   watch-item) recorded for phase-5+ planning.

avalanchego remains the phase-5 benchmark target.

## 8. Error handling and budgets

- Encoder: unsupported shape → havoc; oversized function → skip with
  diagnostic. Never panic on analyzer-visible input (fuzz targets cover
  the encoder like every other parser/lowerer).
- Obligation queries default to a **250 ms** timeout (function-sized
  formulas); the 100 ms default stays for small requires-inference
  queries. Both configurable. Timeout/Unknown ⇒ silence (parent §8:
  timeouts must never create false positives).
- The findings pass already runs under `catch_unwind`; B's one-shot
  queries keep the carried "stateful solver across catch_unwind"
  question moot for this phase.

## 9. Carried items this phase retires or records

| Item (origin) | Disposition |
|---|---|
| entry-block-panic latent FP class (phase-3 triage) | fixed by reachability-conditioned queries (§4) |
| T12 checker-name-ordered requires (phase-2 triage) | implemented when bounds lands (§4) |
| stateful solver across `catch_unwind` (phase-2/3) | moot under one-shot queries (§8) |
| T10 dual-reach dispatch contribution drop (phase-3) | watch during shakeout, record in report (§7) |
| exit-code stub, `--func` findings filtering (phase-3 CLI) | `check` lands real exit codes; `check` takes package patterns only — no `--func` finding filter in phase 4 (YAGNI) |

The phase-1/3 deferred fast-follow batch (sidecar test dedup,
length-prefixed cache-key paths, `go_version()` error tests, `GoBuild`
Display text, spec §12 blocking-tier wording, `EngineConfig.limits`
deletion, process-backend `mem_mb` doc sentence) rides as Task 1 of the
phase-4 plan, following the phase-3 precedent.

## 10. Testing summary

Unit tests per encoder rule (guards, phi-ite, back-edge cut, len
datatype); property tests for encoder determinism and no-panic on
arbitrary lowered functions; corpus want-comment gates in blocking CI;
golden traces; rendered-output byte-compare determinism; differential
solver sweep over new queries; fuzz target over the encoder path.
Blocking-tier wall-clock budget unchanged (parent §12: 10 minutes).
