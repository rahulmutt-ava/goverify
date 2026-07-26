# Phase-7 shakeout: goroutine-leak gates (G1-G4)

Status: **ALL GATES PASS** (2026-07-27) — bbolt regression target.

Gates per [phase-7 design spec §9](2026-07-26-phase7-goroutine-leaks-design.md#9-shakeout)
and this wave's [task-11 brief](../../../.superpowers/sdd/2026-07-26-phase7-goroutine-leaks/task-11-brief.md).
bbolt is the **regression** gate (spec §8): the 457 nil/bounds findings
must not move, and any `goroutine-leak` finding must survive hand
triage. The concurrency-heavy pin (`golang.org/x/sync`) is Task 12's
scope and appends its own section below.

## Run parameters

- Wave tip (HEAD): `98583e2` (branch `worktree-phase7-goroutine-leaks`,
  Tasks 1-10).
- Wave base: `afe0a30` (`main`) — the phase-7 *plan* commit, i.e. the
  phase-6 tip plus planning docs and **no checker code**. Built in a
  detached `git worktree add <scratch>/base afe0a30` checkout, removed
  after use.
- Both binaries: `mise x -- cargo build --release -p goverify-cli`
  (new: `Finished release profile in 1m 39s`).
- bbolt: the existing cached checkout at `.goverify/shakeout/bbolt`
  @ `v1.4.0` (`0d51685`), the same pin every prior wave used —
  re-verified with `git log -1 --oneline` before starting.
- Shared shakeout cache: the existing pre-warmed
  `.goverify/shakeout/cache` (862 MB, populated by the phase-4/5a/5b/6
  runs), **reused in place** — the same methodology phase-5b's and
  phase-6's addenda used. Base-vs-new is compared against this one
  shared cache because *byte* identity only holds within a cache
  lineage (deviation #2: a live re-solve returns different, equally
  valid counter-models); cross-lineage comparisons below are made at
  `file:line:col tag` signature level instead.
- This worktree has no `.goverify/` of its own, so
  `.goverify/shakeout/{bbolt,cache}` were created as symlinks to the
  main checkout's, letting `mise run shakeout` reuse the pin and the
  warm cache unchanged. `.goverify/` is gitignored; nothing under it is
  tracked (`git ls-files .goverify` is empty).
- **No EDR first-exec stall.** Both fresh release binaries answered
  `--help` / `--version` in ~0.3 s. (An initial
  `timeout 25 ./target/release/goverify --help` reported exit 127 —
  that is macOS having no GNU `timeout` on PATH, *not* an exec stall;
  the plain invocation returned immediately. The cp-and-exec workaround
  was therefore never needed.)
- **Counting convention** (never `wc -l`, per the wave-2 lesson):

  ```sh
  # nil/bounds-family finding headers (the 457 regression set)
  grep -cE '^[^ ].*: (nil-deref|bounds|div-zero|overflow): ' out.txt
  # goroutine-leak finding headers — note the human renderer prints the
  # finding *tag*, not the checker name, so the leak checker's headers
  # read chan-send-leak / chan-recv-leak / chan-select-leak
  grep -cE '^[^ ].*: chan-(send|recv|select)-leak: ' out.txt
  ```

## Commands

`mise run shakeout` (Step 1, the sanctioned entrypoint) has no
format/timings knob and writes findings to stdout, so the gate runs
invoke the binary directly with exactly the arguments the script uses.
Every run below is one of:

```sh
W=<this worktree>            # or <scratch>/base for the base binary
CACHE=/Users/…/goverify/.goverify/shakeout/cache
export GOVERIFY_EXTRACTOR_DIR="$W/extractor"
cd .goverify/shakeout/bbolt
GOVERIFY_TIMINGS=1 "$W/target/release/goverify" check ./... \
  --cache-dir "$CACHE" > out.txt 2> out.stderr
```

Step 1 itself:

```sh
mise run shakeout > A-shakeout.txt 2> A-shakeout.stderr
# 31.86s total; stderr tail: "shakeout: exit 1 (0 clean / 1 findings)"
```

Fresh-cache (cold) runs substitute `--cache-dir "$(mktemp -d)"`.

## Deviations (recorded per the honesty rule)

1. **One fresh-cache run reported 462 nil/bounds findings instead of
   457 — run-to-run variance, demonstrated *within the same binary*,
   not a phase-7 effect.** Phase-6's addendum already recorded this
   class (its deviation #4: fresh caches under load gave 465 / 462 /
   457 across attempts, with 85-310 solver queries escalating to the
   10× retry tier). Rather than lean on that precedent, five
   binary × cache configurations were measured here (the escalation line
   is only emitted when the count is non-zero, so a blank means 0):

   | binary | cache | nil/bounds | retry escalations |
   |---|---|---|---|
   | new `98583e2` | shared warm | **457** | 0 (fully replayed) |
   | base `afe0a30` | shared warm | **457** | 0 (fully replayed) |
   | new `98583e2` | fresh, trial 1 | **462** | 189 |
   | new `98583e2` | fresh, trial 2 | **457** | 88 |
   | base `afe0a30` | fresh | **457** | 91 |

   The **same binary** produced 462 and then 457 on two fresh caches,
   and the 462 outlier is the run whose escalation count roughly
   doubled (189 vs. 88/91). Mechanism: `Limits::default()` sets
   `timeout_ms: 100`, escalating once to 10× on Unknown, and
   `goverify-cache`'s query layer caches Unknown as a real outcome
   ("Unknown IS cached — it is deterministic per solver build"). At a
   100 ms wall-clock budget that determinism is load-sensitive in
   practice: a handful of queries near the boundary flip between
   Unknown (⇒ silent, bug-finder semantics) and Sat (⇒ reported)
   depending on machine state, and whichever way they land gets frozen
   into that cache. Three of five configurations, including both
   binaries, agree on 457. All G1/G2/G3 gate numbers come from the
   shared-warm lineage; the fresh-cache runs appear in G4 (whose
   property — cold vs. warm self-consistency — held in every lineage)
   and here.
2. **Solver counter-model text (`with: p0 = …`) is not stable across a
   *live re-solve*, only across cache replay.** `cmp` between the new
   binary's fresh-cache trial 2 and the base binary's fresh-cache run
   differs at char 146492 even though their finding *sets* are
   signature-identical (457 vs 457, `comm -3` empty): Z3 returns a
   different satisfying assignment for the same query in a different
   process. This is why every byte-identity claim in this record is
   made *within* a cache lineage (G1's base-vs-new against the one
   shared cache; G3's repeats; G4's cold-vs-warm), and why cross-lineage
   comparisons are made at signature level. Not new to this wave, but
   not previously written down.
3. **`SCC_CACHE_VERSION` 3 (Task 3's codec `FreeVar` root) forces a
   one-time 18965-entry SCC recompute against the shared cache.** That
   was Step 1's `mise run shakeout` run: **31.86 s**, versus **3:29.9**
   for a genuinely cold run — the gap is the shared cache's `query`
   layer still being warm, so re-encoding reused solver results. Its
   output is byte-identical to every subsequent warm run, and the next
   run reported `scc cache 18965 hit / 0 miss`. Expected per spec §6 /
   the Task 3 cache-invalidation pins, not a regression.
4. **G2's zero-finding outcome is backed by a live-checker probe, not
   taken on trust.** Zero `goroutine-leak` findings on a target with
   five `go` statements is only meaningful if the checker actually ran;
   a deliberate leak probe was injected into the bbolt working copy and
   fired (G2 below), then removed and the output confirmed to
   round-trip byte-identically.
5. A stray untracked `.z3-trace` (0 bytes) sits in the bbolt clone,
   predating this session (also noted in the phase-5b and phase-6
   addenda); left alone, not part of this task's cleanup contract.

## Gate results

### G1 — nil/bounds regression: byte-identical 457, base vs. new

The brief's G1 asks only that the 457 counts be unchanged. The stronger
form was available and is what was measured: the base binary was built
and run **on the same machine, in the same session, against the same
shared cache** as the new one.

```sh
cmp B1.txt W1.txt     # base @ afe0a30  vs  new @ 98583e2
```

| | base `afe0a30` | new `98583e2` |
|---|---|---|
| nil/bounds headers | **457** | **457** |
| `goroutine-leak` headers | 0 (checker absent) | **0** |
| all finding headers | 457 | 457 |
| output bytes | 189110 | 189110 |
| `cmp` base vs new | — | **silent (byte-identical)** |
| exit | 1 | 1 |

- **`cmp B1.txt W1.txt`: silent.** The base and the wave tip produce
  byte-identical human output on bbolt — identical finding set,
  identical order, identical solver counter-models, identical rendered
  source lines. Not merely equal counts.
- Both binaries are self-consistent across repeats (`cmp B1 B2`
  silent; `cmp W1 W2`, `cmp W1 W3` silent).
- Cross-checked against the historical baseline file
  `.goverify/shakeout/baseline-457.txt`, comparing `file:line:col tag`
  signature sets:

  ```sh
  sig() { grep -E '^[^ ].*: (nil-deref|bounds|div-zero|overflow): ' "$1" \
          | sed -E 's/^([^ ]+): ([a-z-]+): .*/\1 \2/' | LC_ALL=C sort; }
  comm -23 base.sig new.sig   # only in the baseline file
  comm -13 base.sig new.sig   # only in phase-7
  ```

  - only in the baseline file: `tx.go:558:11 nil-deref`
  - only in phase-7: *(nothing)*

  That single difference is the already-documented one: the baseline
  file holds **458** headers including `tx.go:558:11`, which the
  retry-tier work in the phase-5a wave discharged — "current HEAD
  expectation is 457 with `tx.go:558:11` retry-discharged"
  (`docs/superpowers/plans/2026-07-23-phase5a-caching.md`). No other
  signature moved in either direction.
- Fresh-cache controls (deviation #1): three further cold runs on
  three separate `mktemp -d` caches — new binary twice, base binary
  once — report **462**, **457**, and **457** nil/bounds findings
  respectively. The new binary landing on both sides (462 then 457)
  localises the 462 outlier to run-to-run solver-timeout luck rather
  than to phase-7 code; the base binary's fresh run and the new
  binary's second fresh run are signature-identical to each other and
  to the shared-warm 457 set (`comm -3` empty in both directions).

→ **G1 PASS** (457 unchanged, byte-identical to base; 0 new leak
findings, inside the brief's expected 0-2 band).

### G2 — triage of every `goroutine-leak` finding on bbolt

**Findings to triage: 0.** No `chan-send-leak`, `chan-recv-leak`, or
`chan-select-leak` header appears in **any** of the ten unprobed
phase-7-binary runs in this record — the `mise run shakeout` run, the
three shared-warm repeats, both fresh-cache cold runs and their three
warm replays, and the post-probe restore run:

```sh
grep -cE '^[^ ].*: chan-(send|recv|select)-leak: ' W1.txt   # 0
```

Zero findings is an acceptable outcome for the regression target, but
it is only informative if the checker was live, and only useful if the
silence is explained. G2a and G2b establish both.

#### G2a — the checker is live on bbolt (dogfood probe)

`default_checkers()` is `vec![&NilChecker, &BoundsChecker,
&LeakChecker]` (`crates/goverify-checkers/src/lib.rs`) and `run_check`
uses it (`crates/goverify-cli/src/main.rs`) — unlike `debug findings`,
which still hardcodes `vec![&NilChecker]` (the pre-existing divergence
phase-6's addendum flagged as deviation #3; unchanged by this wave and
irrelevant to the `check` path measured here). Verified empirically by
appending the canonical reported shape (a copy of
`testdata/corpus/leak/leak.go`'s `LeakSendClosure`) to the bbolt
**working copy**'s `db.go`:

```go
// goverify shakeout probe (phase-7 Task 11, G2): the canonical
// chan-send-leak shape (mirrors testdata/corpus/leak LeakSendClosure).
func goverifyLeakProbe() {
	ch := make(chan int)
	go func() { ch <- 1 }()
	_ = ch
}
```

`go build ./...` clean. `goverify check ./... --cache-dir "$CACHE"`
then reports exactly one leak finding and leaves the regression set
untouched (nil/bounds still 457):

```
db.go:1399:2: chan-send-leak: goroutine go.etcd.io/bbolt.goverifyLeakProbe$1 may block forever: send on a spawner-created channel with no receive, close, or select in the spawning environment [go.etcd.io/bbolt.goverifyLeakProbe]
 1399 |  go func() { ch <- 1 }()
      |  ^
    path: db.go:1398
```

Cleanup: `git checkout -- db.go`, re-run, `cmp` against the pre-probe
warm output → **silent (byte-identical)**; `git status --porcelain`
shows only the pre-existing untracked `.z3-trace`.

#### G2b — per-`go`-site hand traces (why bbolt is genuinely silent)

bbolt @ v1.4.0 has **five** `go` statements in non-test code
(`grep -rn --include='*.go' -E '^\s*go\s+(func|[a-zA-Z_])' . |
grep -v _test.go`). Each is traced below against spec §2's four rules.
Outcome: **zero false positives**; three sites are silent *and* leak-free
in ground truth; one is not a candidate at all; and exactly one is a
genuine reachable leak that v1 deliberately cannot see, silenced by two
*documented* scope limits (spec §2 rule 1's nested-helper restriction
and rule 2's escape-on-return). That last one is a false negative, which
is a precision boundary rather than a blocker — G2 blocks on confirmed
FPs only.

**1. `db.go:980` — `go db.batch.trigger()`  → not a candidate (rule 1)**

```go
966: func (db *DB) Batch(fn func(*Tx) error) error {
967: 	errCh := make(chan error, 1)
…
980: 		go db.batch.trigger()
…
984: 	err := <-errCh
```

```go
1004: func (b *batch) trigger() {
1005: 	b.start.Do(b.run)
1006: }
```

Rule 1 requires a `Send`/`Recv`/blocking `Select` **syntactically
present in the spawned callee's own body**. `trigger`'s body is a single
`sync.Once.Do(b.run)` call — no channel op at all. The batch's sends
(`c.err <- trySolo`, `c.err <- err`, inside `(*batch).run`) are two call
levels down and, per rule 1, "a nested-helper op is never itself the
subject of a finding" in v1. No candidate is even constructed.

Independently, three further rules would each have silenced it: the
channel `errCh` is not the one the goroutine blocks on (`c.err` is
reached via `db.batch.calls`, a heap structure — `Loc::Unknown`
territory); `errCh` is stored into `call{…, err: errCh}` and appended to
a slice, an escape under rule 2; and `err := <-errCh` at db.go:984 is a
`Recv` counterpart in the spawning environment under rule 3.
**Verdict: correctly silent. Not a leak** — `run` always answers every
queued call, and `errCh` is buffered (cap 1) so even a departed
receiver cannot block the sender.

**2. `db.go:1246` — `go func() { for e := range ech { panic(…) } }()`
 → silenced by rule 2 (escape) and rule 3 (`close` counterpart)**

```go
1245: 	ech := make(chan error)
1246: 	go func() {
1247: 		for e := range ech {
1248: 			panic(fmt.Sprintf("freepages: failed to get all reachable pages (%v)", e))
1249: 		}
1250: 	}()
1251: 	tx.recursivelyCheckBucket(&tx.root, reachable, nofreed, HexKVStringer(), ech)
1252: 	close(ech)
```

Rule 1 *is* satisfied in shape: `ech` is an `Alloc` in the spawner
(`(*DB).freepages`), captured by the closure, and `range ech` lowers to
a `Recv` in the closure's own body. Then:

- **Rule 2 (escape) fails first:** `ech` is passed as an argument to a
  plain `Call` — `tx.recursivelyCheckBucket(…, ech)` at db.go:1251.
  Spec §2 rule 2 is strict here *even for a fully summarized static
  callee*, so the candidate is dropped.
- **Rule 3 would independently silence it:** `close(ech)` at db.go:1252
  is a `Close` at the same `Loc` in the spawning environment, and
  `Close` unblocks a blocked recv (a recv on a closed channel returns
  the zero value).

**Verdict: correctly silent. Not a leak** — the `range` terminates when
the spawner closes `ech` on line 1252, which it does unconditionally.
Two independent rules agree, and the ground truth agrees with both.

**3. `tx_check.go:30` — `go func() { defer close(ch); tx.check(cfg, ch) }()`
 → silenced by rule 1 (nested-helper) and rule 2 (returned)**

```go
21: func (tx *Tx) Check(options ...CheckOption) <-chan error {
…
29: 	ch := make(chan error)
30: 	go func() {
31: 		// Close the channel to signal completion.
32: 		defer close(ch)
33: 		tx.check(chkConfig, ch)
34: 	}()
35: 	return ch
36: }
```

This is the one site on bbolt where a *real* goroutine leak is
reachable: `ch` is unbuffered, `(*Tx).check` does `ch <- fmt.Errorf(…)`
on every inconsistency it finds, and a caller that abandons the returned
channel without draining it leaves the goroutine parked on that send
forever. (`Check`'s own doc comment does not tell callers they must
drain — it says only "An error is returned if any inconsistency is
found.")

v1 is silent, by two documented design decisions rather than by
accident:

- **Rule 1:** the spawned closure's *own* body contains no channel
  operation — only `defer close(ch)` and the call to `tx.check`. Every
  blocking send lives inside `(*Tx).check`, a helper. Spec §2 rule 1:
  "v1 has no callee-side instruction to anchor a reachability query on
  across a call boundary, so a nested-helper op is never itself the
  subject of a finding" (spec §10's deferred item "nested-helper
  blocking ops (cross-function obligation anchoring)").
- **Rule 2:** `ch` is `return ch`-ed at tx_check.go:35 — an escape.
  Even with cross-function anchoring, this candidate would need
  caller-side matching (spec §10's "param-rooted channels via
  caller-side matching") to say anything, because whether the leak
  happens is a property of `Check`'s *callers*.

**Verdict: KNOWN FALSE NEGATIVE, in scope for a later wave — not an
FP, and does not block.** Recorded here as the concrete real-world
instance motivating spec §10's nested-helper-anchoring and
caller-side-matching items.

**4./5. `cmd/bbolt/main.go:1133` and `:1280` —
`go checkProgress(results, finishChan, cmd.Stderr)`
 → silenced by rule 3 (`close` counterpart)**

```go
1132: 	finishChan := make(chan interface{})
1133: 	go checkProgress(results, finishChan, cmd.Stderr)
1134: 	defer close(finishChan)
```

(`:1279`-`:1281` is the identical shape in `runReads`.)

```go
1473: func checkProgress(results *BenchResults, finishChan chan interface{}, stderr io.Writer) {
1474: 	ticker := time.Tick(time.Second)
…
1476: 	for {
1477: 		select {
1478: 		case <-finishChan:
1479: 			return
1480: 		case t := <-ticker:
…
1486: 		}
1487: 	}
1488: }
```

Rule 1 is satisfied: a blocking `select` (no `default` arm) sits
syntactically in the spawned callee `checkProgress`, and its
`<-finishChan` arm rebases through the `Go`'s actuals to the `Alloc` at
main.go:1132 in the spawner. Rule 2 holds too — the only uses of
`finishChan` in the spawner are the `Go` argument (whitelisted for a
static callee) and `close(finishChan)` (the `close` builtin is
whitelisted) — so this site reaches rule 3 as a live `Select`
candidate, which is exactly the path the select-arm-dispatch fix
(`c82af40`, "chan-select-leak was unfireable") made reachable at all.
It is then correctly suppressed:

- **Rule 3:** the deferred `close(finishChan)` at main.go:1134 is a
  `Close` at the same `Loc` in the spawning environment, and per the
  select-arm dispatch a recv arm's unblocker set is `{Send, Close}` —
  matched. The `<-ticker` arm is separately not spawner-rooted
  (`ticker` comes from a `time.Tick` call inside the callee, not from
  an `Alloc` in the spawner), so it can never anchor on its own.

**Verdict: correctly silent. Not a leak** — `defer close(finishChan)`
runs on every exit path of `runWrites`/`runReads`, which is precisely
what unblocks the select. This is the most load-bearing of the five
traces: it is the one site that exercises the select path end to end
and reaches the counterpart test, and the counterpart test got the
right answer.

**G2 summary:** 0 findings; 0 false positives; 3 sites correctly
silent with ground truth agreeing; 1 documented false negative
(`(*Tx).Check`, spec §10 items); 1 site not a candidate at all.
→ **G2 PASS** (no FP ⇒ no blocker).

### G3 — determinism across repeats + warm wall-clock vs the 3.45 s phase-6 baseline

Three warm runs of the new binary against the shared cache, plus the
Step 1 `mise run shakeout` run and two warm runs of the base binary:

(The phase-6 column is that addendum's warm run 2 — the 3.45 s figure
this wave's brief names as the baseline.)

| | phase-6 warm baseline | new run 1 | new run 2 | new run 3 | base run 1 | base run 2 |
|---|---|---|---|---|---|---|
| extract cache | 220 hit / 0 extracted | 220 / 0 | 220 / 0 | 220 / 0 | 220 / 0 | 220 / 0 |
| extract+load | 0.98s | 1.01s | 0.93s | 0.97s | 1.04s | 0.99s |
| analyze | 2.34s | 2.29s | 2.32s | 2.30s | 2.27s | 2.30s |
| scc cache | 18965 hit / 0 miss | 18965 / 0 | 18965 / 0 | 18965 / 0 | 18965 / 0 | 18965 / 0 |
| scope+render | 0.05s | 0.06s | 0.05s | 0.05s | 0.07s | 0.08s |
| wall (`real`) | **3.45s** | **3.449s** | **3.378s** | **3.409s** | 3.513s | 3.442s |

- Warm mean, new binary: **3.412 s** vs the 3.45 s phase-6 baseline →
  **-1.1 %**, far inside the <15 % budget (and inside noise: the base
  binary measured *today* averages 3.478 s, so phase-7 is if anything
  marginally faster on this machine's current state).
- `analyze` is flat (2.29-2.32 s vs the baseline's 2.34 s) — as
  expected: the leak checker adds one encode plus ≤2 solver queries per
  `go` site, bbolt has five, and per G2b's traces *all five* are
  discharged by the candidate-scan / escape / counterpart rules before
  rule 4 issues any solver query at all. The measurable cost on this
  target is therefore just the candidate scan and the escape walk.
- **Byte-identity across repeats:** `cmp W1 W2`, `cmp W1 W3`,
  `cmp A-shakeout W1` all silent; all four outputs are 189110 bytes.
  (`A-shakeout` is the SCC-v3 recompute run of deviation #3 — the
  version bump changes cost, never output.)
- No `/Users`, `/private/tmp`, or `/var/folders` substring anywhere in
  the output (`grep -cE '/Users|/private/tmp|/var/folders' W1.txt` → 0).

→ **G3 PASS**.

### G4 — cold vs. warm byte-identical (cache correctness)

A fresh `mktemp -d` cache, one cold run then two warm runs of the same
binary:

| run | cache state | wall | extract | analyze | scc |
|---|---|---|---|---|---|
| cold | empty | **3:29.9** | 0 hit / 220 extracted, 3.04s | 206.74s | 0 hit / 18965 miss |
| warm 1 | populated | **4.403s** | 220 hit / 0, 1.87s | 2.13s | 18965 hit / 0 miss |
| warm 2 | populated | **3.298s** | 220 hit / 0, 0.98s | 2.13s | 18965 hit / 0 miss |

- `cmp C-cold.txt C-warm1.txt` → **silent**
- `cmp C-cold.txt C-warm2.txt` → **silent**
- All three report the same counts (462 nil/bounds, **0** leak).
- Cold→warm speedup: **~63×** (209.9 s → 3.3 s).

Repeated on a **second, independent** fresh cache (same binary), to
check the property is not an artifact of the first lineage's particular
solver outcomes:

| run | cache state | wall | nil/bounds | leak |
|---|---|---|---|---|
| cold | empty | 3:26.5 (analyze 202.33s, 88 escalations) | 457 | 0 |
| warm | populated | 4.312s | 457 | 0 |

- `cmp NF2-cold.txt NF2-warm.txt` → **silent**

→ **G4 PASS** (cache replay is output-preserving, in both lineages).

Per deviation #1, this fresh-cache lineage's absolute count is 462, not
the shared-warm 457. Signature-level delta vs. the 457 set:

- only in fresh-cold: `cmd/bbolt/main.go:1595:23 nil-deref`,
  `cmd/bbolt/main.go:1599:43 nil-deref`, `node.go:54:54 nil-deref`,
  `node.go:55:24 nil-deref`, `node.go:275:24 nil-deref`,
  `node.go:282:59 overflow`
- only in shared-warm: `node.go:246:31 overflow`

All seven sit at the solver's timeout/retry boundary — this cold run
escalated **189** queries to the retry tier, roughly double the 88/91
of the two other cold runs measured in the same session, and it is the
only one of the three that drifted. A **second** cold run of the *same
phase-7 binary* on a *different* fresh cache reported the plain 457
with 88 escalations and a signature set identical to the shared-warm
one, and it too was internally cold/warm-consistent. So G4's property —
cold and warm agreeing byte-for-byte — held in every lineage, and the
absolute-count drift is run-to-run solver-timeout luck, not a
phase-7 or a caching defect (deviation #1's table).

## Cleanup verification

```
$ cd .goverify/shakeout/bbolt && git status --porcelain
?? .z3-trace
```

- `db.go` restored via `git checkout -- db.go` after the G2a probe;
  post-restore `check` output `cmp`-identical to the pre-probe warm run.
- Base worktree (`git worktree add --detach <scratch>/base afe0a30`)
  removed with `git worktree remove --force <scratch>/base`.
- All three `mktemp -d` caches removed.
- bbolt clone re-verified at the pin after every run:
  `git log -1 --oneline` → `0d51685 Merge pull request #893 …`.
- `.goverify/shakeout/{bbolt,cache}` symlinks in this worktree are
  gitignored scaffolding; nothing under `.goverify/` is tracked.

---

# Phase-7 shakeout: the concurrency-heavy pin (`golang.org/x/sync`)

Status: **ALL GATES PASS** (2026-07-27) — concurrency-heavy target,
Task 12.

Gates per the same spec §9, and this wave's
[task-12 brief](../../../.superpowers/sdd/2026-07-26-phase7-goroutine-leaks/task-12-brief.md).
Where bbolt is the *regression* gate, `golang.org/x/sync` is the
*exercise* gate: `errgroup`, `semaphore` and `singleflight` are the
densest spawn/channel/WaitGroup code in the stdlib's orbit. The headline
result is **0 `goroutine-leak` findings, 0 false positives, and a
sharply-drawn precision boundary**: every channel-blocking op in
x/sync's non-test code sits one call level below its spawned callee, so
spec §2 rule 1's nested-helper restriction means the pin cannot
construct a single candidate. That is documented below per site with IR
evidence, not assumed.

## Run parameters

- Wave tip (HEAD): `18b71aa` (branch `worktree-phase7-goroutine-leaks`,
  Tasks 1-11), the same binary Task 11 built — `cargo build --release`
  inside the script reported `Finished release profile in 0.15s`
  (no-op). **No EDR first-exec stall**, consistent with Task 11.
- Pin: **`v0.10.0`**, the brief's default, kept because the tag *does*
  exist — verified before writing the script:

  ```sh
  git ls-remote --tags https://github.com/golang/sync | grep v0.10
  # 913fb63af28f446cd10c684ee847b5606cf328f7  refs/tags/v0.10.0
  ```

  (22 tags exist, newest `v0.22.0`; no substitution was needed. The
  script keeps the brief's `GOVERIFY_SHAKEOUT_CONC_REF` override.)
  Post-run re-verification: `git log -1 --oneline` → `913fb63
  singleflight: fix typo in singleflight_test.go`, `git describe --tags`
  → `v0.10.0`.
- Entrypoint: `scripts/shakeout_conc.sh` (a clone of
  `scripts/shakeout.sh` with the repo, pin var and `DIR` substituted),
  exposed as `mise run shakeout-conc`. Clone target
  `.goverify/shakeout/sync` — a real directory in this worktree
  (gitignored: `git check-ignore -v` → `.gitignore:2:.goverify/`),
  created by the first run.
- Shared shakeout cache: the script inherits `--cache-dir
  "$(pwd)/../cache"`, i.e. the same pre-warmed
  `.goverify/shakeout/cache` the bbolt pin uses (reached here through
  this worktree's existing symlink). x/sync's entries were added by the
  Step 1 run; `SCC_CACHE_VERSION` is already 3, so no version recompute
  happened.
- **Counting convention** — the same finding-header greps as the bbolt
  section, never `wc -l`.
- **Scope fact that shapes every number below:** `goverify check ./...`
  analyzes **non-test packages only**. `debug callgraph ./...` contains
  no `*_test` entry (`grep -c '_test'` → 1, and that one is
  `runtime.sync_test_runtime_blockUntilEmptyCleanupQueue`), and `debug
  summary ./errgroup/...` lists no `errgroup_test` function. So of the
  **21** `go` statements in the tree, only the **4** in non-test files
  are in scope:

  ```sh
  grep -rn --include='*.go' -E '^\s*go\s+(func|[a-zA-Z_])' . | grep -v _test.go
  # errgroup/errgroup.go:75         go func() { … }()          (*Group).Go
  # errgroup/errgroup.go:104        go func() { … }()          (*Group).TryGo
  # singleflight/singleflight.go:138  go g.doCall(c, key, fn)  (*Group).DoChan
  # singleflight/singleflight.go:167  go panic(e)              doCall$1
  ```

  `semaphore` and `syncmap` contain **no** `go` statement in non-test
  code at all. The 17 test-file spawns — including
  `errgroup_test.go:207`'s textbook `go func() { <-ch }()` — are never
  seen by this pin.

## Commands

`mise run shakeout-conc` (Step 1) writes findings to stdout and has no
timings knob, so the gate runs invoke the binary directly with exactly
the arguments the script uses:

```sh
W=<this worktree>
CACHE=/Users/…/goverify/.goverify/shakeout/cache
export GOVERIFY_EXTRACTOR_DIR="$W/extractor"
cd .goverify/shakeout/sync
GOVERIFY_TIMINGS=1 "$W/target/release/goverify" check ./... \
  --cache-dir "$CACHE" > out.txt 2> out.stderr
```

Step 1 itself, and a warm repeat of it:

```sh
mise run shakeout-conc > A-shakeout.txt 2> A-shakeout.stderr
# stderr tail: "shakeout: exit 1 (0 clean / 1 findings)"
# warm repeat: real 3.76s (see G3 — the task's wall includes a
# `git fetch --tags` network round trip and a no-op cargo build)
```

Fresh-cache (cold) runs substitute `--cache-dir "$(mktemp -d)"`.

## The finding set (2 findings, both pre-existing families)

```
semaphore/semaphore.go:85:20: nil-deref: call to (*container/list.List).Remove violates its nil-deref requirement [(*golang.org/x/sync/semaphore.Weighted).Acquire]
singleflight/singleflight.go:31:35: nil-deref: nil dereference in (*golang.org/x/sync/singleflight.panicError).Error [(*golang.org/x/sync/singleflight.panicError).Error]
```

- nil/bounds-family headers: **2**
- `chan-(send|recv|select)-leak` headers: **0**
- output: **845 bytes**, exit **1**, in *every* run recorded here.

Both nil-derefs are nil/bounds-checker output, outside phase-7's remit;
they are noted only so the total is accounted for. (Neither moved in any
configuration — see G4, where x/sync showed none of bbolt's
solver-timeout count drift.)

## Deviations (recorded per the honesty rule)

1. **Test files are out of scope, so the pin exercises far less
   concurrency than the tree contains.** 17 of 21 `go` sites live in
   `_test.go` files and are never analyzed (evidence in *Run
   parameters*). The brief's framing — "dense spawn/channel/WaitGroup
   usage" — is true of x/sync as a *repository*; it is only partly true
   of x/sync as an *analysis target*. Stated up front because it caps
   what a 0-finding result can mean.
2. **The dominant suppressor is not any single §2 rule but the
   combination of rule 1 (nested-helper) with `Loc::Unknown`
   saturation.** All three static spawn sites are silenced by rule 1
   *before* rules 2-4 are consulted, and all three would additionally
   be silenced by rule 3, because their spawners' converged effects
   carry a fully-saturated unknown bucket
   (`?:[Make,Send,Recv,Close,Select]`). Mechanism, traced in G2c: the
   `call-dyn` on the user-supplied callback (`f()` in `Go$1`/`TryGo$1`,
   `fn()` in `doCall$2`) fans the callgraph out to every function value
   of that signature, which lands these closures in one 208-member
   recursive SCC that widens to `Provenance::Havoc` / `Effects::top()`
   after `widen_after = 3` rounds; joining a top summary into the
   spawner saturates its unknown-loc chan bucket, and rule 3's
   may-alias clause (`unblocked_at(&Loc::unknown())`) then matches any
   candidate. **This is the precision boundary for callback-driven
   concurrency libraries**, and it is a property of the analysis
   pipeline (dynamic-dispatch fan-out ⇒ SCC widening), not of the leak
   checker.
3. **`mise run shakeout-conc` runs `git fetch --tags` on every
   invocation** (inherited verbatim from `scripts/shakeout.sh`), so the
   task's wall-clock includes a network round trip: 3.76 s for the task
   vs 1.12 s for the same analysis invoked directly. Not changed —
   keeping the two shakeout scripts structurally identical is worth
   more than shaving 2.6 s off a manual task.
4. **Cross-lineage byte-identity held here, unlike on bbolt.** bbolt's
   deviation #2 records that solver counter-model text is only stable
   within a cache lineage. On x/sync both fresh-cache lineages are
   `cmp`-silent against the shared-warm output. That is a *weaker,
   luckier* result, not a change to the invariant: with only two
   findings, both counter-models (`p0 = (ptr-addr #x0…)`, `p0 =
   ptr-nil`) happen to be the assignment Z3 returns every time. Byte
   comparisons in the gates below are still made within a lineage; the
   cross-lineage `cmp`s are reported as a bonus.
5. **The clone is clean** — `git status --porcelain` in
   `.goverify/shakeout/sync` prints nothing, so this pin has none of the
   stray `.z3-trace` that bbolt's clone carries.

## Gate results

### G1 — determinism: six runs, byte-identical

| run | invocation | cache | bytes | nil/bounds | leak |
|---|---|---|---|---|---|
| `A-shakeout` | `mise run shakeout-conc` (Step 1) | shared warm | 845 | 2 | **0** |
| `W1` | direct, `GOVERIFY_TIMINGS=1` | shared warm | 845 | 2 | **0** |
| `W2` | direct | shared warm | 845 | 2 | **0** |
| `W3` | direct | shared warm | 845 | 2 | **0** |
| `A2-shakeout` | `mise run shakeout-conc` (warm repeat) | shared warm | 845 | 2 | **0** |
| `R-restore` | direct, after the G2a probe was reverted | shared warm | 845 | 2 | **0** |

```sh
cmp W1 W2; cmp W1 W3            # silent
cmp A-shakeout W1; cmp A2 W1    # silent
cmp R-restore W1                # silent  (probe round-trip)
```

- All five `cmp`s silent — identical finding set, order, rendered source
  lines and solver counter-models.
- No `/Users`, `/private/tmp` or `/var/folders` substring in the output
  (`grep -cE '/Users|/private/tmp|/var/folders' W1.txt` → 0).
- Both fresh-cache lineages are additionally `cmp`-silent against `W1`
  (deviation #4).

→ **G1 PASS** (the brief asks for two byte-identical runs; six were
measured).

### G2 — triage of every `goroutine-leak` finding on x/sync

**Findings to triage: 0.** No `chan-send-leak`, `chan-recv-leak` or
`chan-select-leak` header appears in any of the six unprobed runs, or in
either fresh-cache lineage:

```sh
grep -cE '^[^ ].*: chan-(send|recv|select)-leak: ' W1.txt   # 0
```

Per the brief, zero findings is acceptable **only** with a liveness
proof (G2a) and hand traces of the plausible sites (G2b), plus a written
account of the FN surface (G2d).

#### G2a — the checker is live on x/sync (dogfood probe)

The canonical reported shape was appended to the x/sync **working
copy**'s `errgroup/errgroup.go` (a copy of
`testdata/corpus/leak/leak.go`'s `LeakSendClosure`):

```go
// goverify shakeout probe (phase-7 Task 12, G2a): the canonical
// chan-send-leak shape (mirrors testdata/corpus/leak LeakSendClosure).
func goverifyLeakProbe() {
	ch := make(chan int)
	go func() { ch <- 1 }()
	_ = ch
}
```

`go build ./...` clean. `goverify check ./... --cache-dir "$CACHE"` then
reports exactly one leak finding and leaves the nil/bounds set untouched
(still 2):

```
errgroup/errgroup.go:141:2: chan-send-leak: goroutine golang.org/x/sync/errgroup.goverifyLeakProbe$1 may block forever: send on a spawner-created channel with no receive, close, or select in the spawning environment [golang.org/x/sync/errgroup.goverifyLeakProbe]
  141 |  go func() { ch <- 1 }()
      |  ^
    path: errgroup/errgroup.go:140
```

The probe also **isolates the suppressor**: it fires from inside the
very package whose other functions carry saturated unknown buckets,
because rule 3's `env` is the *spawner's own* converged effects, not the
package's. `debug summary --func goverifyLeakProbe` confirms a clean
spawner:

```
errgroup.goverifyLeakProbe    effects={spawns:Bounded chan:{alloc:1:[Send] alloc:2:[Make]} locks:{}}
errgroup.goverifyLeakProbe$1  effects={spawns:None    chan:{fv:0:[Send]}                locks:{}}
```

— no `?` entry, so nothing suppresses it, and `fv:0` shows the phase-7
`Root::FreeVar` machinery resolving the captured channel. Cleanup: `git
checkout -- errgroup/errgroup.go`, re-run, `cmp` against the pre-probe
warm output → **silent** (`R-restore` in G1); `git status --porcelain`
empty.

#### G2b — per-`go`-site hand traces (why x/sync is genuinely silent)

All four in-scope sites are traced against spec §2's rules. Outcome:
**zero false positives**; three sites are silent *and* leak-free in
ground truth; one is not a candidate at all. IR quotes are from
`goverify debug ir`, effects quotes from `goverify debug summary`.

**1. `errgroup/errgroup.go:75` — `go func() { defer g.done(); … }()` in
`(*Group).Go` → not a candidate (rule 1, nested-helper)**

```go
69: func (g *Group) Go(f func() error) {
70: 	if g.sem != nil {
71: 		g.sem <- token{}
72: 	}
73:
74: 	g.wg.Add(1)
75: 	go func() {
76: 		defer g.done()
77:
78: 		if err := f(); err != nil {
79: 			g.errOnce.Do(func() { … })
85: 		}
86: 	}()
```

```go
36: func (g *Group) done() {
37: 	if g.sem != nil {
38: 		<-g.sem
39: 	}
40: 	g.wg.Done()
41: }
```

Rule 1 requires a `Send`/`Recv`/blocking `Select` **syntactically in the
spawned callee's own body**. `Go$1`'s lowered body contains none — the
whole body is a defer, a dynamic call and a `sync.Once.Do`:

```
func (*errgroup.Group).Go$1 ()
  b0: v3 = load v1
      defer call (*errgroup.Group).done(v3 v0)
      v4 = alloc heap=true
      v5 = load v2
      v6 = call-dyn v5()          ← f()
      …
  b2: v11 = make-closure (*errgroup.Group).Go$1$1 [v1 v4]
      v12 = call (*sync.Once).Do(v10 v11)
```

The only channel op the goroutine can reach, `<-g.sem`, lives in
`done` — reached through the `defer`, one call level down. Per spec §2
rule 1, "a nested-helper op is never itself the subject of a finding"
(§10's deferred "nested-helper blocking ops / cross-function obligation
anchoring"). **No candidate is constructed**, so rules 2-4 are never
consulted.

Two independent suppressors would have applied had rule 1 been relaxed.
`done`'s Recv is keyed `p0.f2` (i.e. `Param(0).sem`), and the `Go` site's
`make-closure … [v3 v4]` binds `v3`, the heap cell holding the receiver —
so the rebased spawner-side key is `alloc:3.f2`, exactly where `Go`'s own
effects record the counterpart:

```
(*errgroup.Group).Go  effects={spawns:Unbounded
  chan:{alloc:3.f2:[Send]  ?:[Make,Send,Recv,Close,Select]} …}
```

- **Rule 3, same `Loc`:** `alloc:3.f2:[Send]` — the `g.sem <- token{}`
  at line 71 is a `Send`, an unblocker for a blocked `Recv`.
- **Rule 3, `Loc::unknown()`:** the saturated `?` bucket matches
  anything (deviation #2).

**Verdict: correctly silent. Not a leak** — every `Go` that takes a
`sem` slot sends exactly one token, and the `done` deferred on the same
goroutine takes exactly one back; `g.sem` is buffered to the limit, so
neither side can park forever.

**2. `errgroup/errgroup.go:104` — the same closure in `(*Group).TryGo`
 → not a candidate (rule 1, nested-helper)**

```go
93: func (g *Group) TryGo(f func() error) bool {
94: 	if g.sem != nil {
95: 		select {
96: 		case g.sem <- token{}:
98: 		default:
99: 			return false
100: 		}
101: 	}
103: 	g.wg.Add(1)
104: 	go func() { … defer g.done() … }()
```

`TryGo$1`'s lowered body is instruction-for-instruction the same shape
as `Go$1`'s (defer `done`, `call-dyn v5()`, `sync.Once.Do`) — no channel
op of its own. **Rule 1: no candidate**, same nested-helper reason.

Secondary suppressors, again both live:

```
(*errgroup.Group).TryGo  effects={spawns:Unbounded
  chan:{alloc:3.f2:[Select]  ?:[Make,Send,Recv,Close,Select]} …}
```

- **Rule 3, same `Loc`:** `alloc:3.f2:[Select]` — the lowered
  `v12 = select blocking=false [send v11 <- v20]`. `Select` is in a
  `Recv` candidate's unblocker set. Worth naming the sharp edge: that
  entry is *direction-blind* (spec §10's "`ChanOp::Select` direction
  refinement"), so the match is not evidence the analysis understood
  which arm it was. Here the arm genuinely *is* the counterpart send, so
  direction-blindness costs nothing at this site — but it is luck, not
  reasoning.
- **Rule 3, `Loc::unknown()`:** saturated `?` bucket.

**Verdict: correctly silent. Not a leak** — identical token discipline
to site 1, with the send made conditional by a non-blocking select
(which by construction never blocks the spawner either).

**3. `singleflight/singleflight.go:138` — `go g.doCall(c, key, fn)` in
`(*Group).DoChan` → not a candidate (rule 1); would also fail rule 2 and
rule 3**

```go
121: func (g *Group) DoChan(key string, fn func() (interface{}, error)) <-chan Result {
122: 	ch := make(chan Result, 1)
…
133: 	c := &call{chans: []chan<- Result{ch}}
134: 	c.wg.Add(1)
135: 	g.m[key] = c
136: 	g.mu.Unlock()
137:
138: 	go g.doCall(c, key, fn)
139:
140: 	return ch
141: }
```

- **Rule 1:** `doCall`'s own lowered body is six allocs, two
  `make-closure`s, a `defer call … doCall$1`, a `call … doCall$2` and a
  branch — **no `send`, `recv` or `select`**. Every channel op is in
  `doCall$1`, the deferred closure: `send v60 <- v75` (the
  `ch <- Result{…}` loop at singleflight.go:177) and
  `v48 = select blocking=true []` (the `select {}` at :168). Both are
  one call level below the spawned callee ⇒ nested-helper ⇒ no
  candidate.
- **Rule 2 (escape) would also fail.** `ch` is `v4 = make Chan v45` in
  `DoChan`, and `DoChan`'s IR spills it three ways: `v25 = assign v4;
  store v24 <- v25` where `v24 = index-addr v23 …` and
  `v23 = alloc heap=true` — a `Store` of a tracked value into an
  untracked, non-channel cell (the slice backing for
  `[]chan<- Result{ch}`); `v27 = call-builtin append(v22 v26)` — a
  builtin outside the `close`/`len`/`cap` whitelist; and `return v31` /
  `return v44` — returned.
- **Rule 3 would also fire:**

  ```
  (*singleflight.Group).DoChan  effects={spawns:Unbounded
    chan:{alloc:4:[Make]  ?:[Make,Send,Recv,Close,Select]} …}
  ```

  The `?` bucket contains `Recv`/`Select`/`Close`, all unblockers for a
  blocked send.

**Verdict: correctly silent. Not a leak** — `ch` is
`make(chan Result, 1)`, and `doCall$1` sends **exactly one** `Result`
per registered channel (`for _, ch := range c.chans`), so the send
cannot block even though `DoChan`'s doc says "The returned channel will
not be closed" and a caller may abandon it. The cap-1 buffer is
precisely the mechanism that makes abandonment safe, and ground truth
agrees with the silence for a reason the checker's rule 2 (escape) also
happens to reach.

**4. `singleflight/singleflight.go:167` — `go panic(e)` inside
`doCall$1` → not a candidate (non-static `go` callee)**

```go
163: 		if e, ok := c.err.(*panicError); ok {
166: 			if len(c.chans) > 0 {
167: 				go panic(e)
168: 				select {} // Keep this goroutine around so that it will appear in the crash dump.
169: 			} else {
170: 				panic(e)
171: 			}
```

Lowered:

```
  b9: v47 = make-interface v35
      go call-builtin panic(v47)
      v48 = select blocking=true []
      v49 = extract v48 #0
      v50 = make-interface v84
      panic v50
```

`candidates` matches only `Op::Go { callee: Callee::Static(c) }` whose
target has a body; a `Callee::Builtin` go callee is skipped outright.
**No candidate.**

The adjacent `select {}` deserves an explicit note because it is a
*deliberately* permanently-blocked goroutine — exactly the shape the
checker's name suggests it should flag — and v1 is silent on it twice
over, both times correctly:

- v1's finding definition anchors **only at an `Op::Go`**. This
  `select {}` blocks the goroutine that is already running (the one
  spawned back at DoChan:138), and rule 1 requires the op to be in *that
  `Go`'s* spawned callee's own body — it is in `doCall$1`, one level
  down.
- Even reached, `candidates` has `if arms.is_empty() { continue; }`, so
  a zero-arm select can never become a candidate.

**Verdict: correctly silent, and correctly so on the merits** — the
comment on line 168 says the block is intentional (keep the goroutine in
the crash dump while the sibling `go panic` crashes the process). A tool
that flagged it would be wrong.

**`semaphore` — no `go` statement, hence no candidate anywhere.**
Recorded because the brief names it: `semaphore.go`'s channel machinery
(`ready := make(chan struct{})` at :69, the blocking `select` at :74-106,
`close(w.ready)` at :158) all executes on the **caller's** goroutine.
With no `Op::Go` in the package there is nothing for this checker to
anchor on, by definition — not a silence rule firing, an absence of
subject matter. (The one `semaphore` nil-deref finding, at
`s.waiters.Remove(elem)` on the cancellation path of that same
`Acquire` select, is the nil checker's output, not this checker's.)

#### G2c — the `Loc::Unknown` saturation, traced to its cause

Deviation #2's mechanism, since "Unknown contamination" is only a useful
finding if its origin is named:

```sh
goverify debug sccs ./errgroup/... | grep 'errgroup.Group).Go\$1'
# scc 2094 [recursive]: (*context.afterFuncCtx).cancel, … ,
#   (*golang.org/x/sync/errgroup.Group).Go$1, …          (208 members)
```

`Go$1` contains `v6 = call-dyn v5()` — the user's `f()`. Dynamic
dispatch fans out to every function value of that signature, which drags
208 functions (context cancellation closures, runtime internals,
`sync.OnceFunc$1`, …) into one recursive SCC. `Options::widen_after` is
3, so the SCC widens to `Summary::havoc()` — `Effects::top()`,
`provenance=Havoc`:

```
(*errgroup.Group).Go$1     … chan:{?:[Make,Send,Recv,Close,Select]} … provenance=Havoc
(*errgroup.Group).TryGo$1  … chan:{?:[Make,Send,Recv,Close,Select]} … provenance=Havoc
```

`effects::collect` joins a spawned goroutine's summary into its
spawner's, so `Go`/`TryGo`/`DoChan`/`doCall` all inherit the saturated
`?` bucket, and rule 3's may-alias clause matches every candidate in
them. **Consequence to carry forward:** even after §10's nested-helper
anchoring lands, callback-shaped concurrency code will stay silent until
dynamic-dispatch fan-out stops widening these SCCs to top. That is an
*analysis-pipeline* limit, upstream of the leak checker, and it is the
single most important thing this pin measured.

#### G2d — observed false-negative surface (documented, not failures)

- **WaitGroup ops are invisible to v1** (spec §10, phase B second half),
  and x/sync's *primary* blocking discipline is WaitGroup-based, not
  channel-based: `errgroup`'s `g.wg.Add(1)` / `g.wg.Done()` (in `done`)
  / `g.wg.Wait()` (in `Wait`), and `singleflight`'s `c.wg.Add(1)` /
  `c.wg.Wait()` (in `Do`) / `c.wg.Done()` (in `doCall$1`). In the IR
  these are ordinary `call (*sync.WaitGroup).Add/Done/Wait` instructions
  with no channel effect at all. A caller that misuses `errgroup`
  (`Go` after `Wait`, or a task that never returns) hangs in
  `g.wg.Wait()`, and v1 has no vocabulary for it. Observed FN surface,
  per the brief — not a gate failure.
- **Nested-helper anchoring (rule 1)** is the binding limit on this
  target: *all three* static spawn sites put their blocking op one call
  level below the spawned callee (`done`, `doCall$1`). Until §10's
  cross-function obligation anchoring lands, the checker's candidate
  scan cannot see any real x/sync channel op. This is the concrete
  real-world motivation for that queued item, alongside bbolt's
  `(*Tx).Check`.
- **`Loc::Unknown` saturation (G2c)** would be the next barrier even
  after that.
- No **can't-tell** verdicts: every site resolved to either
  "correctly silent, not a leak" or "not a candidate", each with IR
  evidence.

**G2 summary:** 0 findings; **0 false positives**; 4 in-scope sites
traced (3 correctly silent with ground truth agreeing, 1 not a candidate
at all); 1 deliberate-block site (`select {}`) correctly not flagged;
0 spawn sites in `semaphore`/`syncmap`; 2 documented FN surfaces
(WaitGroup, nested-helper anchoring) and 1 documented upstream precision
limit (SCC widening ⇒ Unknown saturation).
→ **G2 PASS** (no FP ⇒ no blocker).

### G3 — cold/warm wall-clock

| run | cache | extract cache | extract+load | analyze | scc cache | wall (`real`) |
|---|---|---|---|---|---|---|
| `W1` | shared warm | 67 hit / 0 | 0.40s | 0.70s | 7782 hit / 0 miss | **1.12s** |
| `W2` | shared warm | 67 hit / 0 | 0.42s | 0.67s | 7782 / 0 | **1.11s** |
| `W3` | shared warm | 67 hit / 0 | 0.38s | 0.71s | 7782 / 0 | **1.12s** |
| `C-cold` | fresh #1 | 0 hit / 67 extracted | 1.06s | 91.03s | 0 hit / 7782 miss | **1:32.1** |
| `C-warm1` | fresh #1, warm | 67 / 0 | 0.43s | 0.62s | 7782 / 0 | 1.10s |
| `C-warm2` | fresh #1, warm | 67 / 0 | 0.41s | 0.69s | 7782 / 0 | 1.13s |
| `NF2-cold` | fresh #2 | 0 / 67 | 1.02s | 92.95s | 0 / 7782 | **1:34.0** |
| `NF2-warm` | fresh #2, warm | 67 / 0 | 0.54s | 0.64s | 7782 / 0 | 1.27s |

- Warm mean (shared lineage): **1.117 s**. Cold: **92.1 s / 94.0 s**.
- Comfortably under bbolt's, as the brief expected: **3.1× faster warm**
  (1.12 s vs 3.41 s) and **2.3× faster cold** (92 s vs 3:29.9), on 67
  packages / 7782 SCCs vs bbolt's 220 / 18965.
- Cold→warm speedup: **~82×** (92.1 s → 1.12 s).
- `mise run shakeout-conc` measures **3.76 s** warm — 1.12 s of analysis
  plus a no-op `cargo build --release` and a `git fetch --tags` network
  round trip (deviation #3).
- Solver escalations to the retry tier: 31 on the Step 1 run (partly
  cold against the bbolt-shaped shared cache), 60 and 69 on the two
  cold runs, 0 on every fully-warm replay. Unlike bbolt, **no count
  ever drifted**: 2 nil/bounds + 0 leak in all eight runs.

→ **G3 PASS**.

### G4 — cold vs. warm byte-identical (cache correctness)

Two independent `mktemp -d` lineages, each one cold run followed by warm
replays of the same binary:

| lineage | comparison | result | nil/bounds | leak |
|---|---|---|---|---|
| fresh #1 | `cmp C-cold C-warm1` | **silent** | 2 | 0 |
| fresh #1 | `cmp C-cold C-warm2` | **silent** | 2 | 0 |
| fresh #2 | `cmp NF2-cold NF2-warm` | **silent** | 2 | 0 |

- All six outputs are 845 bytes with identical counts.
- Bonus (deviation #4): `cmp C-cold W1` and `cmp NF2-cold W1` are also
  silent — cross-lineage byte-identity, which bbolt could only achieve
  at signature level.

→ **G4 PASS** (cache replay is output-preserving, in both lineages).

## Cleanup verification

```
$ cd .goverify/shakeout/sync && git status --porcelain
$ git log -1 --oneline
913fb63 singleflight: fix typo in singleflight_test.go
$ git describe --tags
v0.10.0
```

- `errgroup/errgroup.go` restored via `git checkout --` after the G2a
  probe; post-restore `check` output `cmp`-identical to the pre-probe
  warm run (`R-restore` in G1). Working tree fully clean — no stray
  files at all.
- Both `mktemp -d` caches removed.
- `.goverify/shakeout/sync` (the clone) and `.goverify/shakeout/cache`
  (symlink to the shared cache) are gitignored scaffolding;
  `git ls-files .goverify` is empty.
- No `.gvir` schema drift: `git diff --stat proto/ extractor/` is empty
  (this wave never touches them).
