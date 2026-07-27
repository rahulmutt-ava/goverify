# Nested-helper anchoring (one hop) — goroutine-leak checker

Date: 2026-07-27
Status: approved
Predecessors: `2026-07-26-phase7-goroutine-leaks-design.md` (the checker
this extends; its §2 rule 1 is amended by this spec),
`2026-07-26-phase7-shakeout.md` (the evidence that motivated it).

## 1. Goal and context

Phase 7's leak checker only anchors a finding on a blocking op
**syntactically present in the spawned callee's own body** (§2 rule 1
of the phase-7 spec). The phase-7 shakeout found that this is the
binding constraint on real-code recall: every real blocking op observed
on bbolt and x/sync sits exactly one call level below the spawned
callee —

- bbolt `(*Tx).Check`: `go func() { defer close(ch); tx.check(cfg, ch) }()`
  — every send lives in `(*Tx).check`, a plain-`Call` helper;
- x/sync `(*errgroup.Group).Go`: `defer g.done()` — the `<-g.sem` recv
  lives in `done`, reached through a `Defer`;
- x/sync `(*singleflight.Group).doCall`: the send and the `select {}`
  live in `doCall$1`, a **deferred closure**.

This wave adds **one-hop cross-function obligation anchoring**: a
blocking op in a helper reached by a single static `Call`/`Defer` edge
from the spawned callee's body can now be the subject of a finding.

**Expectation setting (agreed at design time):** this wave alone is
expected to produce **zero new findings on the pinned shakeout
targets**. bbolt's real leak (`Tx.Check`) is additionally silenced by
rule 2 (`return ch` is an escape — needs the separate caller-side
matching item), and the x/sync sites are correctly silent via rule-3
counterparts or SCC-widening saturation. The acceptance bar is
fixture-level (corpus + unit tests); the shakeout re-run is a
**regression gate, not a recall gate**. The payoff is the capability
itself, which the later queue items (escape relaxation, caller-side
matching) stack on.

Scope decisions made in brainstorming:

- **Anchoring only.** No escape-rule relaxation, no caller-side
  matching, no WaitGroup work in this wave.
- **One hop.** Spawned callee → one static/deferred call → blocking op
  in that helper's own body. Every observed real shape is one hop;
  bbolt's single two-hop shape goes through `sync.Once.Do` (dynamic
  dispatch — unreachable at any depth). Internals are shaped so a later
  depth bump is mechanical, but no depth parameter is exposed.
- **Approach: syntactic second hop in the candidate scan** (leak.rs
  local change reusing `map_through_site`/`closure_bindings`/
  `prefixed`/`conjoin`), not effects-provenance (SCC-cache codec churn)
  and not helper inlining (a new SSA transformation).

## 2. Finding definition (amends phase-7 spec §2 rule 1)

Rule 1 becomes: a `Send`, `Recv`, or blocking `Select` is anchorable
when it is

1. syntactically present in the spawned callee `g`'s own body
   (existing behavior, unchanged), **or**
2. **NEW:** syntactically present in the own body of a helper `h`
   reached through a **single hop**: a `Call` or `Defer` instruction in
   `g`'s body whose callee is `Callee::Static(h)` with a non-empty body
   (each such instruction is its own anchor — two call sites to the
   same helper are two candidates, consistent with the existing
   sibling-obligation policy),
   where the op's channel operand — resolved in `h`'s frame, rebased
   through the `g→h` call site (argument positions for `Param(i)`
   roots, `closure_bindings(g)` + `fv_loc` for `FreeVar(i)` roots),
   then rebased through the go site as today — resolves to a `Loc`
   rooted at an **`Alloc` in `f`**.

Exclusions, all deliberate:

- **Depth ≥ 2 stays suppression-only.** A blocking op in `h`'s own
  callees contributes to summaries (rule-3 suppression) but is never
  anchored. Pinned by a corpus fixture as a tripwire for any future
  depth change.
- **`Go` edges in `g` are not hop edges.** A nested spawn is a
  *different* goroutine; anchoring its ops is a separate follow-up
  (§7).
- **`h == g` (direct recursion) yields no hop candidate.** `g`'s own
  body ops are already directly anchored; a second anchor through the
  recursive call site would double-report the same op.
- Dynamic/invoke/builtin callees and bodyless callees contribute no
  hop, exactly as they contribute no direct candidate today.

Rules 2 (escape) and 3 (counterpart) are **untouched** — see §5.

## 3. Candidate scan and representation

`Candidate` (leak.rs) gains `hop: Option<Hop>`:

```rust
struct Hop {
    helper: FuncId,     // h — hosts the blocking op
    call_block: usize,  // block index of the Call/Defer in g
    call_instr: usize,  // instr index within call_block
    kind: HopKind,      // Call | Defer (docs/messages/tests only;
                        // the query anchors on call_block either way)
}
```

For a hop candidate, the existing fields keep their meaning with `h`
substituted for the op's host frame: `op_block`/`op_instr`/`callee_loc`
are resolved in `h`; `callee` remains `g` (the spawned function — the
message and the middle query frame need it); `alloc_value`/
`spawner_loc`/`arm_locs` remain `f`-frame, now produced by applying
`map_through_site` **twice** (h-frame → g-frame via the call site,
g-frame → f-frame via the go site). Composition safety is inherited:
each application enforces `MAX_LOC_DEPTH` and degrades to
`Loc::unknown()` past it, and a channel allocated inside `g` itself
roots at a `g`-local `Alloc` after the first mapping, which the second
mapping degrades to `Unknown` — so goroutine-internal channels drop
out, as today.

**Implementation amendment (Task 4):** the hop-site mapping is
`map_through_hop`, which adds one rule `map_through_site` doesn't
have: a base rooted at a *g-local* `Alloc` cell is bridged through
the cell's single stored value when that value canonically resolves
to a `Param`/`FreeVar` of `g` (mirroring `cap_of`'s cell form, one
level only — nested cells degrade to Unknown). Without it the
deferred-closure shape (§1's `doCall$1`) dead-ends: go/ssa spills a
closure-captured param to a cell and binds the cell, so the naive
mapping roots at Alloc-in-`g` and degrades. Two stores into the cell,
or non-Param/FreeVar content, degrade to Unknown as before.

Scan shape: inside the existing per-`Go`-site loop, after the direct
scan of `g`'s body, iterate `g`'s blocks in index order for
`Op::Call`/`Op::Defer` with a static bodied callee `h ≠ g`, and scan
`h`'s body in block/instr order for the same three op shapes with the
double rebase above. A blocking `Select` in `h` requires **every** arm
to map to `Root::Alloc` in `f` through both hops (mirror of the direct
rule). All lookups bounds-checked, total, panic-free; unmapped shapes
yield no candidate. Scan order is fully index-ordered in both frames —
no map iteration reaches the output.

## 4. Obligation query

A hop candidate's query is a **three-frame conjunction** (the direct
case keeps its existing two-frame form):

```text
  enc_f.reach_query(go_block,   own_preconditions(f))        // unprefixed
∧ prefixed("s_", enc_g.reach_query(call_block, own_preconditions(g)))
∧ prefixed("t_", enc_h.reach_query(op_block,   own_preconditions(h)))
```

`prefixed` is applied per side and `conjoin` folds left, exactly as the
two-frame case; the `debug_assert` on identical datatype decls holds
pairwise (all sides come from `encode_func_with`). `"s_"` and `"t_"`
share no name with each other or with the unprefixed side (existing
const names never start with either prefix — the same argument that
justified `"s_"` alone). The per-obligations-call `enc_cache:
HashMap<FuncId, Option<EncodedFunc>>` now also caches helper encodings;
it remains lookup-only (never iterated). An encoding failure on any of
the three frames skips the candidate, never reports.

## 5. Deliberate precision boundaries (approved at design time)

1. **Hop + buffered-const send → `CapClass::Silent`.** The
   buffered-send ordinal conjunct counts fills preceding the candidate;
   for a hop that count spans two frames, and for a `Defer` hop the
   DAG-ancestor logic is wrong outright — a deferred helper runs after
   **all** of `g`'s body, not after the defer site, so "sends in
   ancestor blocks of the defer instruction" is not "sends before the
   helper runs". Rather than ship a wrong count, hop candidates whose
   `cap_class` would be `BufferedConst` go `Silent`. Unbuffered sends,
   recvs (already reachability-only — the ordinal conjunct only ever
   applies to buffered **sends**), and selects report through the hop.
   §10 follow-up: cross-frame fill counting for plain-`Call` hops.
2. **A `Defer` hop anchors on defer-site reachability in `g`.** This
   over-approximates "the helper runs": the deferred call fires only
   when `g` returns (or panics), so a `g` that never returns never runs
   `h`. Accepted v1 over-approximation, same family as the existing
   reachability ≠ blocks-with-nothing-after approximations. §10
   follow-up: conjoin `g`-exit reachability for `Defer` hops.

## 6. What does NOT change

- **Escape (rule 2):** the walk still runs in `f` over `alloc_value`,
  strict whitelist form, verbatim. Passing the channel from `g` into
  `h` happens outside `f`'s body and was never `f`-escape territory;
  parity with the direct case, where `g`'s internal uses are likewise
  not escape-walked. The existing safety nets for goroutine-internal
  spills (summary saturation → rule-3 may-alias suppression) apply
  identically.
- **Counterpart (rule 3):** the env is still `f`'s own summary, which
  already contains `h`'s ops rebased in transitively by
  `effects::collect`. No new self-suppression arises: an op kind is
  never in its own unblocker set, and the rule-3 select amendment
  (phase-7 spec §2 rule 3) already excludes `Select` from same-loc
  unblocker sets.
- **Tags:** the same three (`chan-send-leak`, `chan-recv-leak`,
  `chan-select-leak`) — baselines and fingerprints stay stable.
- **Severity:** Error, unchanged.
- **Schema/proto/extractor:** untouched. No new dependencies.

## 7. Messages and versioning

Hop messages name both functions, position-free:

> goroutine `{g}` may block forever: send **in helper `{h}`** on a
> spawner-created channel with no receive, close, or select in the
> spawning environment

(and the recv/select analogues). Direct-candidate messages are
byte-identical to today's.

`LeakChecker::version()` bumps 1 → 2. Checker `(name, version)` pairs
are salt inputs to the SCC cache (`scc_cache.rs::open`), so the bump
rotates every cached findings entry; **no `SCC_CACHE_VERSION` bump**
(the cached codec is unchanged — `Candidate`/`Hop` are checker-internal
and never serialized).

## 8. Testing and acceptance

Unit tests (leak.rs, existing style — explicit `t`-free Rust tests,
deterministic fixtures via `testfix`):

- hop candidate via plain `Call` with param-passed channel;
- hop candidate via `Defer` of a static method (`done` shape);
- hop candidate via a **deferred closure** with a free-var-captured
  channel (`doCall$1` shape; exercises `closure_bindings(g)`);
- `h == g` recursion produces no hop candidate;
- depth-2 (helper's helper) stays silent — the tripwire pin;
- hop + buffered-const send is `Silent`; hop recv on a buffered channel
  still reports (reachability-only);
- hop candidate suppressed by a rule-3 counterpart in `f`;
- an `f`-side escape silences a hop candidate;
- query shape: three-frame conjunction, `s_`/`t_` const sets disjoint
  from each other and from the unprefixed side;
- select-in-helper: all-arms-must-map rule through both hops.

Corpus (`testdata` leak module): fixtures modeled on the three real
shapes (plain-call helper send, defer-helper recv, deferred-closure
send/select), one counterpart-suppressed nested shape (silent), one
depth-2 shape (silent pin), with `// want:` pins where a finding is
expected. e2e z3 pair: nested send reports; suppressed when `f` recvs.

Gates, in order:

1. `mise run fmt && mise run lint && mise run test && mise run corpus`
   — all green (corpus includes the determinism suite).
2. `mise run secrets && mise run audit` — green.
3. bbolt shakeout regression: **expected 457 byte-identical** (count
   via the finding-header grep, never `wc -l`). Any delta is
   adjudicated per-finding and recorded before it can pass.
4. x/sync re-run: expected still silent. Record in the shakeout doc
   that the `done`/`doCall$1` shapes now **construct candidates** and
   are silenced by rule 3 / saturation — that trace is the evidence the
   hop machinery works on real code even though the count is 0.

## 9. Documentation updates

- Phase-7 design spec §2 rule 1: rewritten to the one-hop form (§2 of
  this spec); §10: the "nested-helper blocking ops" item is consumed
  and replaced by the new residuals below.
- README: the `goroutine-leak` checker paragraph gains one sentence on
  one-hop helper anchoring.
- Shakeout record: re-run section per gate 3/4 above.

## 10. Follow-up queue additions (do NOT implement)

- Depth ≥ 2 anchoring (generalize the hop to a capped chain).
- Cross-frame buffered fill counting for plain-`Call` hops.
- `Defer`-hop exit-reachability conjunct (close the "g never returns"
  over-approximation).
- Nested-spawn anchoring (`Go` edges inside the spawned callee).
