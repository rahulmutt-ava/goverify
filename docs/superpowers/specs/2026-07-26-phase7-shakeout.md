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
