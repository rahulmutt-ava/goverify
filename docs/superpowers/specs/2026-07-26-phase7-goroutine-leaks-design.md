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
- Channels that escape: stored to the heap, returned, passed as an
  argument to any plain call (a strict whitelist — see §2 rule 2), or
  captured by an untracked closure (parent spec §7: "channels escaping
  into arbitrary heap structures degrade to no-report").
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

1. **Blocking op on an identified channel.** A `Send`, `Recv`, or
   blocking `Select` **syntactically present in the spawned callee's
   own body** — never a blocking op reached only through a helper the
   goroutine calls — whose channel operand, rebased through the go
   site's arguments and closure bindings, resolves to a `Loc` rooted at
   an **`Alloc` in `f`** (a `make(chan …)` in the spawner reaching the
   goroutine via argument or capture). A helper's own blocking ops still
   flow into the goroutine's *summarized* `chan_ops` (rebased in by
   `effects::collect`), and rule 3's counterpart match consults that
   summary — so a nested-helper op can suppress a sibling candidate —
   but v1 has no callee-side instruction to anchor a reachability query
   on across a call boundary, so a nested-helper op is never itself the
   subject of a finding (§10: "nested-helper blocking ops (cross-function
   obligation anchoring)").
2. **No escape.** The tracked channel value never: appears as the
   stored value of a `Store{addr, val}` whose `addr` is itself
   untracked (`val` tracked, `addr` not — a tracked value spilled into
   some *other*, non-channel cell). A `Store` INTO the channel's own
   already-tracked cell is whitelisted bookkeeping, not an escape, and
   instead grows the tracked-alias set to cover the stored content —
   this exemption is load-bearing: `ch := make(chan T)` (and every
   address-taken channel a closure later captures) lowers to exactly an
   `Alloc` cell plus a `Store` of the `Make` dst into it plus a `Load`
   back out per use, so without it every such candidate's own
   initializing store would silence it. Beyond that, the channel is
   returned, is passed as an argument to any plain `Call` — **even a
   summarized
   static callee** — with only a narrow builtin whitelist exempted
   (`close`/`len`/`cap`; a `Go`/`Defer` argument is separately
   whitelisted for a static callee or the `close` builtin, since those
   flows the checker tracks through directly), or is captured by a
   closure other than ones whose spawn/call flow the checker tracks.
   The strict Call rule holds even for a callee the engine has fully
   summarized, because effects don't model a callee storing its own
   parameter to the heap — a summary carries chan-op/escape info the
   checker consults, but not "does this callee stash its argument
   somewhere effects doesn't model," so passing the channel to *any*
   plain call is treated as an escape rather than trusting the summary.
   Any escape ⇒ silent. (§10: "arg-passing to summarized callees could
   stop escaping once effects model param stores.")
3. **No counterpart.** The spawning environment — `f`'s own chan ops,
   its summarized callees' ops (already rebased into `f`'s summary by
   `effects::collect`), and sibling spawned goroutines — contains no op
   at the same `Loc` that could unblock it: `Recv`/`Select`/`Close` for
   a blocked send; `Send`/`Select`/`Close` for a blocked recv (`Close`
   counts on both sides — a send on a closed channel panics rather than
   blocking forever, so it still counts as "not blocked forever", and a
   recv on a closed channel returns the zero value rather than
   blocking). Any op keyed at `Loc::Unknown` anywhere in the environment
   counts as a counterpart (may-alias) and suppresses.

   **Select-arm dispatch is a deliberate exception to the table above.**
   A Select candidate's *own* `ChanOp::Select` lands in `f`'s converged
   effects at exactly its own arm `Loc`s (the spawned closure's select
   rebases into the spawner's summary like any other op), so applying
   the generic table — which lists `Select` as an unblocker for both
   `Send` and `Recv` — to a Select candidate's own arms would make every
   blocking select find *itself* as its counterpart: `chan-select-leak`
   would be structurally unfireable for any input (corpus-discovered via
   `LeakSelectAllBlocked`). The implemented fix dispatches per arm and
   drops `Select` from the SAME-`Loc` unblocker set only for that
   dispatch — a send arm needs `{Recv, Close}`, a recv arm needs `{Send,
   Close}` — while keeping `Select` in the `Loc::unknown()` unblocker
   set: the candidate's own select never rebases to `Unknown`, so
   unknown-loc select evidence is necessarily a different, foreign
   select, and the may-alias rule still applies to it. `Send`/`Recv`
   candidates keep `Select` in their unblocker set unconditionally
   (rule 3's table, unchanged) — a self-match is structurally impossible
   there, since a Send/Recv candidate's own op is never itself a
   `Select` entry. Accepted residual v1 false positive: a genuine
   select-vs-select pairing over the same spawner-local channel is
   indistinguishable from this candidate's own select-evidence and still
   reports; gated by shakeout G2 triage.
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
- **`resolve_loc` also gained two root/hop rules the FreeVar work
  exposed as gaps.** `Op::Make{dst}` now roots at `Alloc(dst.0)`,
  exactly like `Op::Alloc` — pre-existing behavior rooted every
  `make(chan)` register at `Unknown`, which would have made every leak
  candidate invisible before counterpart matching ever ran. `Load{addr}`
  and `Assign{src}` are now transparent, deref-collapsing hops (a cell
  and its loaded/assigned content share one `Loc` — sound for effects
  matching, since over-merging can only suppress a finding, never
  manufacture one), bounded independently by `MAX_LOC_HOPS` (32) so a
  malformed/cyclic def chain can't loop the walk forever (orthogonal to
  `MAX_LOC_DEPTH`, which bounds path *growth*, not hop count).
- **Closure bindings are recovered structurally, not from an
  operand:** lowering drops the callee-slot operand from
  `MakeClosure`, so the checker recovers a callee's binding list by
  scanning `f`'s own body for that callee's `MakeClosure` site(s); a
  *unique* site's `bindings` are used to re-root `FreeVar(i)` through
  `resolve_loc(bindings[i])`, and ≥2 sites for the same callee
  (ambiguous — which binding list applies?) degrade to `Loc::unknown()`,
  same discipline as every other unresolvable shape.
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
- **Buffered, constant cap, acyclic sends:** the buffered ordinal is a
  `BV(32)` guard-indicator sum over the candidate block's DAG
  ancestors — each strict-ancestor block's send count on the same
  `Loc` gated by that block's own reachability guard (`ite(g_b, n,
  0)`), added to the sends preceding the candidate within its own
  block. The candidate is blocked iff this pending-before-candidate
  count is `>= cap` (encoded as `¬(pending <u cap)`; `BvCmpOp` has no
  unsigned-`≥` of its own).
- **Buffered + cyclic sends, or non-constant cap:** Unknown ⇒ silent.

Capacity is read from the spawner's `Op::Make{Chan, args[0]}` when
const (v(0) is the capacity operand; unbuffered `make(chan T)` lowers
with cap const 0).

**The conjoined query links no variables across its two frames.** The
go-site (f-side) query and the blocking-op (callee-side) query are
built independently, then the callee-side query's SMT consts are
prefix-renamed before conjunction purely to avoid name collisions — the
two frames end up sharing NO variables, so a go-site's actual argument
value is never linked to the callee's corresponding formal parameter.
Both frames being individually `Sat` is enough to report even when the
concrete argument passed at the go site would, in the real callee body,
make the blocking-op branch dead. This is sound for a bug-finder (a
real execution witnessing the callee-side op independently reachable
still exists somewhere), but it is the checker's main structural
false-positive source in v1 (§10: "link go-site actuals to callee
formals in the conjoined query").

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
  escape (silent), select-with-default (silent), and a genuinely
  unresolved dynamic call in the goroutine (report — `LeakDespiteOpaqueCall`).
  "Havoc-callee suppression (silent)" moved out of the corpus and into
  unit tests instead: a no-import corpus module has no way to reference
  a bodyless static callee, so `effects.rs`'s and `has_counterpart`'s own
  unit tests pin that a `Effects::top()`-summarized callee's
  Unknown-keyed ops are a universal (suppressing) counterpart.
  `LeakDespiteOpaqueCall` takes its place: an unresolved dynamic call
  inside the goroutine must NOT suppress, because the escape walk
  proves the channel can never reach it — but the fixture's dynamic
  callee value (`hook`) is deliberately given a signature that is
  **unique in the module** (no other address-taken function shares its
  shape). That uniqueness is load-bearing, not incidental: the
  call-graph's may-call resolution for a dynamic call is
  structural-signature-keyed CHA-style over-approximation — every
  address-taken function sharing the callee value's signature becomes a
  possible target, and their effects join into the goroutine's converged
  env. A signature with zero same-shape targets resolves to zero
  call-graph edges (genuinely unresolved, contributes no effects at
  all); a signature shared by even one other closure would instead pull
  that closure's effects into the join, and if any such target were
  itself havoc/unresolved, the join would put `{Send, Recv, Select}` at
  `Loc::unknown()` — the may-alias rule (rule 3) then treats that as a
  counterpart and suppresses every candidate in the spawner, including a
  genuine leak. This same-signature shape is a documented, accepted v1
  false-negative surface (conservative may-call resolution is correct
  engine behavior; it costs recall, never precision) and is deliberately
  not exercised by the corpus, precisely because it would assert away a
  known gap rather than pin a bug (§10: "dynamic-call effect
  contamination (signature-keyed may-call joins all-Unknown effects;
  type-flow or assignment-based narrowing would recover the FN
  surface)"). No `sync` import needed — keeps corpus runtime bounded.
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
- Buffered-channel classification is inconsistent between a lone `Recv`
  candidate and an all-recv-arm blocking `select`: `cap_class` gives the
  lone recv the buffered-recv refinement (a recv with no sender anywhere
  blocks forever regardless of capacity, so const cap `N > 0` still
  classifies `Unbuffered`), while a select whose arms are ALL recv-dir
  falls to `Silent` unless every arm's capacity is const 0 — the same
  reasoning applies arm-wise and would extend. False-negative only
  (`Silent` never reports). Adjacent to the direction-refinement bullet
  above but not covered by it: this one needs no new effect precision,
  only a per-arm application of the rule `cap_class` already has.
- Arg-passing to summarized callees could stop escaping once effects
  model param stores (§2 rule 2).
- Nested-helper blocking ops (cross-function obligation anchoring) —
  §2 rule 1's helper-side ops suppress via summaries today but can
  never anchor a finding of their own.
- Dynamic-call effect contamination (signature-keyed may-call joins
  all-Unknown effects; type-flow or assignment-based narrowing would
  recover the FN surface) — §8.
- Link go-site actuals to callee formals in the conjoined query — §5's
  unlinked-conjunction false-positive source.
