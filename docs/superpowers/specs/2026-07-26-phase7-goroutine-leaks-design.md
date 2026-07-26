# Phase 7: Goroutine-leak checker (channels) — design

Date: 2026-07-26
Parent spec: `2026-07-16-goverify-design.md` §7 phase B, §15 item 7.
This wave implements the **channels-only** slice of phase B: a
`goroutine-leak` checker over the existing Effects lattice, plus the
closure-aware `Loc` extension it needs. WaitGroup modeling and phase C
(data races) are deferred (§10).

## 1. Goal and scope

Flag `go` statements that spawn a goroutine which, on some satisfiable
execution, blocks forever on a channel operation that nothing in the
spawning environment can ever unblock.

**In scope:**

- A new `goroutine-leak` checker in `goverify-checkers` (tags
  `chan-send-leak`, `chan-recv-leak`, `chan-select-leak`), severity
  **Error**.
- `Root::FreeVar(u32)` + closure-aware `rebase` in
  `goverify-analysis::effects`, backed by an ordered
  `Function::free_vars` list in `goverify-ir` (lowered from the
  already-emitted `FreeVar` aux values — **no extractor or `.gvir`
  schema change**).
- A thin SMT slice: reachability queries through the existing
  per-function encoding, plus send-ordinal counting for buffered
  channels with constant capacity.
- A new pinned concurrency-heavy shakeout repo with G-style gates,
  alongside the bbolt regression gate.

**Deliberately silent in v1 (not bugs, scope limits):**

- Param-rooted channels (the spawner's caller may hold counterparts)
  and `Global`-rooted channels (any package can touch them).
- Channels that escape: stored to the heap, returned, passed to a
  havoc callee, or captured by an untracked closure (parent spec §7:
  "channels escaping into arbitrary heap structures degrade to
  no-report").
- Buffered channels with non-constant capacity; buffered channels
  whose sends sit in a CFG cycle (counting unsupported in v1 — a
  cyclic *unbuffered* send needs no counting and still reports).
- `select` with a `default` arm (never blocks).
- Anything either solver query returns Unknown on.

**Not building:** WaitGroup Add/Done/Wait intrinsics (§10), mutex
blocking, send-on-closed / double-close panics (a different bug class),
cross-op path-sensitivity in counterpart matching (§5's flow-insensitive
rule is a documented precision boundary).

## 2. Finding definition

A finding is raised at an `Op::Go` in function `f` when ALL hold:

1. **Blocking op on an identified channel.** The spawned callee's
   summarized `chan_ops`, rebased through the go site's arguments and
   closure bindings, contain `Send`, `Recv`, or a blocking `Select` on
   a `Loc` rooted at an **`Alloc` in `f`** (a `make(chan …)` in the
   spawner reaching the goroutine via argument or capture).
2. **No escape.** The channel's `Make` dst never: appears as the
   stored value of any `Store` (v1 makes no attempt to attribute
   stores back to locals — any store is an escape), is returned, is
   passed to a havoc callee, or is captured by a closure other than
   ones whose spawn/call flow the checker tracks. Any escape ⇒ silent.
3. **No counterpart.** The spawning environment — `f`'s own chan ops,
   its summarized callees' ops (already rebased into `f`'s summary by
   `effects::collect`), and sibling spawned goroutines — contains no op
   at the same `Loc` that could unblock it: `Recv`/recv-`Select` for a
   blocked send; `Send`/`Close`/send-`Select` for a blocked recv. Any
   op keyed at `Loc::Unknown` anywhere in the environment counts as a
   counterpart (may-alias) and suppresses.
4. **Solver confirmation** (bug-finder discipline, Sat = report):
   (a) the go site is reachable in `f`'s encoding; (b) the blocking op
   is reachable in the spawned callee's encoding; (c) for buffered
   channels, the blocking send's ordinal can exceed the constant
   capacity. Unsat/Unknown on any ⇒ silent.

The counterpart match is **flow-insensitive over effects sets**: a
`Recv` that exists only on paths not taken after the spawn still
suppresses. v1 accepts these false negatives to keep matching purely
relational; the caught class is "the counterpart never exists at all".

## 3. Closure-aware Loc machinery

Today `resolve_loc` maps `ValueKind::FreeVar` to `Loc::unknown()` and
`rebase` maps only `Param(i)` roots, so `go func(){ ch <- v }()` — the
most common leak shape — is invisible. The extension:

- **`goverify-ir`:** `Function` gains `free_vars: Vec<ValueId>`,
  collected during lowering from aux values with kind `"FreeVar"` in
  emission order (the extractor appends them in `fn.FreeVars` order;
  relative aux order is preserved). No proto change, no schema bump.
- **`effects::Root`** gains `FreeVar(u32)` (ordinal into
  `free_vars`), deriving the same traits; `resolve_loc` roots FreeVar
  values at their ordinal exactly as params root at theirs (missing or
  out-of-range ordinal ⇒ `Loc::unknown()`).
- **`rebase` at closure sites:** when a `Go`/`Defer`/`Call` callee
  value defs to `Op::MakeClosure{func, bindings}`, callee `FreeVar(i)`
  roots re-root through `resolve_loc(bindings[i])`, concatenating
  paths caller-first with the existing depth cap. (go/ssa's
  `StaticCallee()` unwraps `MakeClosure`, so the callee is already
  `Callee::Static`; only the bindings mapping is new.) A `FreeVar`
  root crossing a boundary with no bindings mapping degrades to
  `Unknown`, as `Alloc` does today.

Lock ops on captured mutexes get the same precision for free (phase-C
prep). The `Effects` codec change bumps `SCC_CACHE_VERSION`.

## 4. The checker

`goverify-checkers/src/leak.rs`, implementing only
`Checker::obligations` (`infer_requires`/`infer_ensures` return
nothing — leaks are not contracts, and an empty `infer_requires` means
the requires-lifting mechanism never touches this checker).

Per `Op::Go` in `f`: candidate generation (§2 rule 1) → escape walk
(§2 rule 2, a use-walk over the `Make` dst inside the checker, not a
new pass) → counterpart match (§2 rule 3, relational over the
converged summaries) → one `Obligation` per survivor carrying the
`f`-side reachability query; the callee-side query and capacity check
run through the same `discharge` callback. `Finding.pos` is the go
statement; `message` names the op kind and the channel's origin
(variable name when available) and — per the fingerprint INVARIANT —
never embeds a position.

## 5. SMT encoding

Deliberately thin; no new theory, no solver-layer changes:

- **Unbuffered (cap 0):** no counting. With no counterpart, the first
  send/recv blocks; the two queries are pure reachability through the
  existing per-function encodings.
- **Buffered, constant cap, acyclic sends:** the kth send on the Loc
  along a path blocks iff `k > cap`; encoded as per-path ordinals over
  the existing integer term language.
- **Buffered + cyclic sends, or non-constant cap:** Unknown ⇒ silent.

Capacity is read from the spawner's `Op::Make{Chan, args[0]}` when
const (v(0) is the capacity operand; unbuffered `make(chan T)` lowers
with cap const 0).

## 6. Degradation

Every failure mode degrades to silence, never crash or speculation:

- Havoc/bodyless/dynamic callees have `Effects::top()` — their
  `Unknown`-keyed ops are universal counterparts, muting the spawner.
- Solver Unknown/timeout drops the candidate (existing discipline).
- Recursion/SCC cycles: the checker reads converged summaries only.
- New code paths are total on fuzzed input: bad FreeVar ordinals,
  out-of-range `bindings[i]`, missing `MakeClosure` defs all resolve
  to `Loc::unknown()`; `resolve_loc`'s depth cap already bounds walks.
  No new byte parsers ⇒ no new fuzz target (nightly budget unchanged).
- `Root::FreeVar` derives `Ord` into the existing `BTreeMap` keying;
  emission paths stay sorted (determinism suite covers it).

## 7. Wiring

- `LeakChecker` joins `default_checkers()`; `version() = 1`.
- Severity Error: gates exit codes from day one. The shakeout triage
  gate (§9) is what makes this defensible — a confirmed FP on the pin
  blocks merge.
- Downstream is untouched: fingerprints (new tags slot into the
  existing `(checker, func, tag, message)` scheme), baseline, JSON/
  SARIF, `//goverify:ignore goroutine-leak` works day one.
- `SCC_CACHE_VERSION` bump (Effects codec). No `EXTRACT_CACHE_VERSION`
  change (extractor untouched). No schema version bump.

## 8. Testing

- **effects.rs units** via extended `testpkg` builders (`MakeClosure`/
  `FreeVar` helpers): FreeVar roots resolve like params; bindings
  re-root at go sites; bad ordinals degrade. `arb_loc` gains the
  `FreeVar` variant so the lattice property tests cover it.
- **Checker units:** table tests per §2 rule — candidate generation,
  each escape shape, each counterpart shape, capacity ordinals.
- **Corpus:** new `testdata/corpus/leak` module, `// want:` pins as
  leak/no-leak pairs, one per §2 rule: unbuffered send no receiver
  (report), closure-captured channel (report), cyclic unbuffered
  producer (report), buffered within cap (silent), counterpart exists
  (silent), param-rooted (silent), global-rooted (silent), heap-store
  escape (silent), select-with-default (silent), havoc suppression
  (silent). No `sync` import needed — keeps corpus runtime bounded.
- **Regression:** zero extractor changes ⇒ every existing corpus
  `.gvir` byte-identical (free invariant check). bbolt: 457 existing
  findings unchanged; new leak findings (expect ~0–2) manually triaged
  and pinned. Warm-time budget re-measured under the extra checker.
- **Caching:** `SCC_CACHE_VERSION` bump test (stale entries don't
  replay); cold/warm byte-equality extended over the leak corpus.

## 9. Shakeout

A second pinned repo, goroutine-heavy and small, chosen during
planning (candidates: `golang.org/x/sync` (errgroup) or a worker-pool
library), pinned by commit like bbolt. Gates in the G1–G4 style:

- **G1 determinism:** byte-identical output across repeated runs.
- **G2 triage:** every reported finding manually triaged with a
  written verdict; any confirmed FP blocks merge (Error severity).
- **G3 runtime:** cold/warm budgets recorded; warm within the
  established tolerance.
- **G4 cache:** cold and warm runs byte-identical.

bbolt remains the regression gate per §8.

## 10. Deferred / follow-up

- **WaitGroup** (`Add`/`Done`/`Wait` intrinsics + counter modeling) —
  the second half of parent-spec phase B; next wave.
- **Phase C data races** — lock-set analysis; `DeferredUnlock`
  distinction and captured-mutex Locs from this wave feed it.
- Buffered-cyclic send counting (loop-bound reasoning).
- Param-rooted channels via caller-side matching (needs cross-function
  obligation placement).
- Counterpart flow-sensitivity (path-aware matching — the §2
  false-negative class).
- `select` send/recv direction refinement in effects (`ChanOp::Select`
  is direction-blind today; v1 handles it conservatively — a Select
  counterpart matches either direction, and a blocking select
  candidate requires all arms unmatched).
