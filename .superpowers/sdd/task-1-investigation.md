# Task 1 investigation: C009c — wrapper ensures (H1) vs closure capture (H2)

## Baseline (step 0)

`git checkout -b summaries/followups` from `main` at `2d18f71`.

`mise run shakeout` (warm SMT cache from the merged wave — ≈30s, matching the
expected warm shape):

```
mise run shakeout 2>&1  45.29s user 8.69s system 180% cpu 29.897 total
```

Finding-header count:

```
$ grep -cE '^\S+\.go:[0-9]+:[0-9]+: ' .goverify/shakeout/baseline-461.txt
461
```

Exactly the expected 461. Presence/absence check:

```
$ grep -E 'compact\.go:26:23|command_surgery\.go:268:55|surgeon\.go:78:20' .goverify/shakeout/baseline-461.txt
compact.go:26:23: nil-deref: call to (*go.etcd.io/bbolt.Tx).Commit violates its nil-deref requirement [go.etcd.io/bbolt.Compact$2]
internal/surgeon/surgeon.go:78:20: overflow: integer overflow in go.etcd.io/bbolt/internal/surgeon.ClearPageElements [go.etcd.io/bbolt/internal/surgeon.ClearPageElements]
```

`compact.go:26:23` and `surgeon.go:78:20` present, `command_surgery.go:268:55`
absent — matches the brief exactly.

## Step 1-3: wrapper-ensures probes (H1)

Added `newA`, `NewTVia`, `NewTNamed` to `testdata/corpus/ensures/ensures.go`
and two `assert!` probes to `ensures_inference_over_corpus` in
`crates/goverify-checkers/tests/ensures_corpus.rs`.

**Result — both probes PASS (GREEN), contradicting the brief's prediction
that `NewTVia` would FAIL:**

```
$ mise x -- cargo test -p goverify-checkers --test ensures_corpus
running 1 test
test ensures_inference_over_corpus ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.65s
```

### Why: the pre-scouted H1 mechanism does not occur in this toolchain

The brief's H1 mechanism assumed `return db.beginTx()` lowers to a `Return`
with **one tuple-valued operand**, tripping `infer_ensures`'s arity check
(`nil.rs:196-199`, `vals.len() != results.len()`). Direct IR inspection shows
this is false for this Go/SSA version: `go/ssa` **always** splits a
forwarded multi-value call into `Extract` instructions before the `Return`,
so `vals.len()` always equals `results.len()`.

`mise x -- cargo run -p goverify-cli -- debug ir --func NewTVia` (bare
forwarding, the simplest shape):

```
func example.com/ensures.NewTVia (v1 bool, v2 bool)
  b1 -> []
    v3 = call example.com/ensures.newA(v1)
    v4 = extract v3 #0
    v5 = extract v3 #1
    return v4 v5
  b2 -> []
    v6 = call example.com/ensures.NewT(v1)
    v7 = extract v6 #0
    v8 = extract v6 #1
    return v7 v8
```

Two operands per `Return`, not one — the arity check never trips. The
correlation clause (`nil.rs`'s template 2) is then also *trivially* provable
for this shape: at each site, the error component (`v5`/`v8`) is a `load`/
`extract`, never a literal `Const(Nil)`, so `err_is_nil_literal` is false and
the Go-idiom under-approximation (`nil.rs:245`, "non-literal error ⇒
non-nil") accepts the site unconditionally, with no SMT call and no
dependency on the callee's own summary at all.

`debug ir --func NewTNamed` (named results + defer, the real `DB.Begin`
shape) confirms the second predicted form — named-result cells forced by
the defer, stores then component-wise loads before `Return`:

```
func example.com/ensures.NewTNamed (v1 bool)
  b2 -> []
    v7 = call example.com/ensures.newA(v1)
    v8 = extract v7 #0
    v9 = extract v7 #1
    store v2 <- v8
    store v3 <- v9
    v10 = load v2
    v11 = load v3
    return v10 v11
  b3 -> []
    v12 = call example.com/ensures.NewT(v1)
    v13 = extract v12 #0
    v14 = extract v12 #1
    store v2 <- v13
    store v3 <- v14
    v15 = load v2
    v16 = load v3
    return v15 v16
```

Still 2-operand `Return`s. Same trivial pass.

### Cross-check against the real `(*bbolt.DB).Begin`

`debug ir --func "DB).Begin"` on the vendored bbolt checkout
(`.goverify/shakeout/bbolt`) shows the **real** `Begin` is structurally
isomorphic to `NewTNamed`: named `t, err` result cells, a deferred logger
closure, per-branch `store`s to the named cells, and `return <load> <load>`
at every site (including a return-site block, `b3`, that go/ssa emits for
the compiler-generated recover path and is unreachable from any real
branch — it is not targeted as a successor of any other block, but even
so its error component is a `load`, not a literal `Nil`, so it does not
break template 2 either). Arity is 2-vs-2 at every site, exactly like the
isolated probes.

(`debug summary`/`debug prepass` cannot answer "does the checker-inferred
ensures for the real `Begin` actually get produced" — that CLI path calls
`goverify_analysis::analyze()`, which runs with `checkers: &[]` and
`StubSolver` (`engine.rs:78-88`); it reports `ensures=0` for *every*
function including the isolated corpus's own `NewT`, which we know from
`ensures_corpus.rs` does get a correlation clause under the real
checker+Z3 pipeline. This is a tooling-path limitation, not evidence about
`Begin` — recorded so Task 2 doesn't reach for the same dead end.)

### H1 verdict

**Refuted as pre-scouted.** The arity-mismatch mechanism at `nil.rs:196-199`
does not fire for either forwarding shape in this codebase's Go/SSA
lowering, because `go/ssa` never emits a single tuple-valued `Return`
operand for `return f(...)` — it always inserts per-component `Extract`s
first. Both wrapper probes (`NewTVia`, `NewTNamed`) pass, and the real
`(*bbolt.DB).Begin` is IR-isomorphic to the passing `NewTNamed` shape. There
is no RED wrapper-ensures probe to justify an H1 fix, and no basis to
believe `DB.Begin` loses its ensures via this mechanism either.

The two `assert!` probes in `ensures_corpus.rs` are committed **as
currently-passing regression guards** (not as RED tests waiting to be
flipped) — see DECISION below for what this means for Task 2's branch
selection.

## Step 4-5: closure-capture repro (H2)

`testdata/corpus/knownfp/knownfp.go` already had a `beginTx`/`txn`/
`commitTx` triple (the FIXED regression case for the non-closure half of
C009c). Per the brief's own-helpers-first note, `CaptureLoop` reuses those
instead of inventing a `NewT2`-shaped constructor:

```go
func CaptureLoop(fail bool) int {
	t, err := beginTx(fail, false)
	if err != nil {
		return 0
	}
	n := 0
	f := func() {
		n += commitTx(t) // want: nil-deref
		t, _ = beginTx(false, false)
	}
	f()
	f()
	return n
}
```

**Result — outcome 1 (finding fires, pin holds):**

```
$ mise x -- cargo test -p goverify-checkers --test knownfp_corpus
running 1 test
test knownfp_corpus_findings_match_want_comments ... ok
```

Confirmed directly via `debug findings`:

```
knownfp.go:457:16: nil: call to example.com/knownfp.commitTx violates its nil-deref requirement [example.com/knownfp.CaptureLoop$1]
```

Line 457 is exactly the `commitTx(t)` call under `// want: nil-deref`. H2
reproduces in-corpus — no need to fall back to the "outcome 2/3" paths.

`mise run corpus` (full determinism suite) is diff-clean:

```
[corpus] $ cargo test -p goverify-extract --test extract_integration     ... 5 passed
[corpus] $ cargo test -p goverify-ir --test lower_golden --test lower_corpus --test callgraph_corpus  ... 9 passed
[corpus] $ cargo test -p goverify-analysis --test engine_corpus          ... 3 passed
[corpus] $ cargo test -p goverify-checkers --test nil_corpus --test bounds_corpus --test knownfp_corpus ... 5 passed
[corpus] $ cargo test -p goverify-cli --test debug_integration           ... 9 passed
```

All green, no golden/pin drift. (`ensures_corpus.rs` is not part of the
`mise run corpus` task list in `mise.toml` — `testdata/corpus/ensures` has
no other consumer, so the wrapper-probe additions there carry zero
golden-drift risk for other modules.)

### Step 6: confirm H2 by tracing the real `Compact$2`

`debug ir --func "Compact\$2"` on the vendored bbolt checkout:

```
func go.etcd.io/bbolt.Compact$2 (v1 [][]byte, v2 []byte, v3 []byte, v4 uint64)
  b1 -> [4, 5]
    v18 = load v7
    v19 = call (*go.etcd.io/bbolt.Tx).Commit(v18)
    v20 = binop Neq v19 v68
    branch v20
  ...
  b5 -> [6, 7]
    v27 = load v9
    v28 = call (*go.etcd.io/bbolt.DB).Begin(v27 v73)
    v29 = extract v28 #0
    store v7 <- v29
    v30 = extract v28 #1
    store v8 <- v30
    ...
```

`v7` is the capture cell for `tx`. Its **only** load that feeds a deref
(`b1`, the `tx.Commit()` at line 26) precedes, in the closure's own CFG, its
**only** store (`b5`, the guarded `tx, err = dst.Begin(true)` reassignment
at line 31) — `b1` branches to `b5` as a successor, never the reverse, so
`b1`'s load can never observe `b5`'s store from the *same* invocation. Every
value `v7` could hold at `b1` comes from either the enclosing `Compact`'s
original `dst.Begin(true)` (line 11, outside this function entirely) or a
**prior invocation** of this same closure — neither is a fact this
function's own encoding can see. This is byte-for-byte the same shape as
the `CaptureLoop`/`CaptureLoop$1` repro: a load of the capture cell
strictly precedes, in local control flow, the closure's own guarded store
to that cell.

### H2 verdict

**Confirmed**, both empirically (the in-corpus `CaptureLoop` repro
reproduces the finding exactly as specified) and structurally (the real
`Compact$2`'s `tx` deref at line 26 is a load of the capture cell `v7`
strictly before that cell's only in-function store). No in-function fact,
and no in-function call-site ensures application, constrains `v7` at the
`b1` load — this is the declared closure-capture non-goal, and it is the
actual, sole blocker for `compact.go:26:23`.

## DECISION

> Task 2 branches selected: **2B only**, because both H1 wrapper probes
> (`NewTVia`, `NewTNamed`) pass — the pre-scouted arity mechanism
> (`nil.rs:196-199`) does not occur in this toolchain's SSA lowering
> (`go/ssa` always splits forwarded tuple returns into per-component
> `Extract`s before `Return`, confirmed both in the isolated corpus and by
> direct IR inspection of the real `(*bbolt.DB).Begin`, which is
> structurally isomorphic to the passing `NewTNamed` probe) — so there is
> no RED wrapper-ensures probe to justify an H1 fix, and 2A's selection
> rule ("iff at least one wrapper probe is RED") is not met. H2 is
> confirmed both empirically (`CaptureLoop`'s `// want: nil-deref` pin
> holds under the real checker+Z3 pipeline) and via direct IR trace of the
> real `Compact$2` (the capture-cell load at line 26 strictly precedes the
> closure's own guarded store to that cell in local control flow) — the
> bbolt site's blocker is unambiguously the capture cell, meeting 2B's
> selection rule ("iff the bbolt site's blocker is the capture cell").

### Note to Task 2 on the (non-RED) `ensures_corpus.rs` probes

The two probes added in `ensures_corpus.rs` (`NewTVia`, `NewTNamed`)
currently **pass** — they are not the RED tests the brief anticipated
Task 2 would flip. Recommendation: keep them as-is; they are legitimate
regression coverage for "a forwarding/named-result wrapper inherits its
callees' `(T, error)` correlation," which is true and worth guarding, even
though it turns out to require no fix. Task 2A is not selected, so no code
change is needed on the H1 side; Task 2 does not need to touch these
asserts at all (they are neither RED-to-flip nor in need of removal —
"remove or invert" per the brief's step 8 parenthetical applies only if a
probe is red and 2A is skipped, which is not this probes' situation: they
were never red).
