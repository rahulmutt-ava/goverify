# Summaries Follow-up Wave Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Resolve the two analysis follow-ups from the interprocedural-summaries
wave — the C009c/`compact.go:26:23` residual FP (investigation-first: wrapper
ensures vs closure capture) and the C221 overflow precision regression
(investigation-first: bisect, then convert-modeling discharge or requires-form
restoration) — plus three test pins and three hygiene items, gated by a full
bbolt shakeout with zero unexplained deltas vs the 461 baseline.

**Architecture:** Spec: `docs/superpowers/specs/2026-07-21-summaries-followups-design.md`.
Two investigation→fix pairs with decision points resolved by evidence, riding
the existing machinery: `NilChecker::infer_ensures` return-site
candidate-and-check (nil.rs:173-254), the bounds checker's
manifest-vs-requires split (`params_only` + `propagate_requires`), and the
encoder's op-constraint match (encode.rs:~940-1055). Expected fix shapes,
pre-scouted: (a) forwarded-tuple return sites in `infer_ensures` delegate
proof to the callee's own ensures clauses; (b) widening int→int `Op::Convert`
asserts the source type's value range on the dst (ints are bitvectors — the
bound is the sound projection; the term language has no zext/sext).

**Tech Stack:** Rust workspace (`crates/goverify-*`), Z3 via `goverify-solver`,
corpus modules under `testdata/corpus/` extracted through the Go sidecar,
pinned bbolt v1.4.0 shakeout under `.goverify/shakeout/`.

## Global Constraints

- Run everything through mise: `mise x -- cargo <…>` for direct cargo, `mise run <task>` for named tasks (AGENTS.md).
- Commits are unsigned in this sandbox: `git -c commit.gpgsign=false commit …` (re-sign before pushing).
- Determinism is the root invariant: no map-iteration order reaching output; `BTreeMap`/`BTreeSet` only; clause order stays result-index × template order.
- Errors degrade, never die: any inference/binding/type failure = "no clause"/"no obligation", never a panic. Parsers of untrusted `.gvir` reject, never panic (fuzz targets stay green).
- Bug-finder polarity: obligations/requires fire on `Sat` only; ensures are proven by `Unsat` only — `Sat`/`Unknown` drop the clause (asserted-as-fact discipline).
- Corpus modules must NOT import stdlib packages with large transitive closures (`flag` empirically blew a corpus run past 30 min — knownfp.go:93-103). `sync`/`unsafe`-scale imports only, prefer none.
- Test assertions: plain `assert!`/`assert_eq!` with a message. Corpus pins are `// want: <tag>` comments; pins document ACTUAL behavior — never force them.
- Only Go code lives in `extractor/`; everything in this plan is Rust or corpus-Go. `.gvir` schema untouched (no proto change, no `schema_version` bump).
- Investigation tasks end in a written report + a decision that selects the sibling fix task's branch. Fix branches not selected are skipped, not improvised.

## File Structure

- `testdata/corpus/ensures/ensures.go` — wrapper-dispatch probes (Task 1), recursive + interface pins (Task 5)
- `crates/goverify-checkers/tests/ensures_corpus.rs` — harness asserts for the above
- `testdata/corpus/knownfp/knownfp.go` — closure-capture repro/pin (Tasks 1-2)
- `testdata/corpus/bounds/bounds.go` — C221 truncation repro (Tasks 3-4)
- `crates/goverify-checkers/src/nil.rs` — forwarded-return ensures (Task 2A), groundness comment (Task 6)
- `crates/goverify-analysis/src/encode.rs` — widening-convert range model (Task 4A), p\<i\> e2e pin test (Task 5)
- `crates/goverify-solver/src/process.rs` — chatty-test de-flake (Task 6)
- `docs/superpowers/specs/2026-07-20-interprocedural-summaries-design.md` — §6 wording (Task 6)
- `.superpowers/sdd/task-1-investigation.md`, `.superpowers/sdd/task-3-investigation.md` — investigation reports
- `docs/shakeout-phase4-bbolt.md` — gate addendum (Task 7)

---

### Task 1: C009c investigation — wrapper ensures (H1) vs closure capture (H2)

Establish, with evidence, why `compact.go:26:23` (`Compact$2`, the `walk`
closure) survived the summaries wave while its class sibling
(`command_surgery_meta.go:59:32`) was discharged. Two hypotheses (spec §3):

- **H1**: forwarding wrappers get no inferred ensures. Pre-scouted mechanism:
  Go SSA lowers `return db.beginTx()` as a Return with ONE tuple-valued
  operand (extractor copies operands verbatim, `extractor/emit.go:275-282`;
  `lower.rs:360-362` preserves 1:1), so `infer_ensures`' arity check
  (`nil.rs:196-199`, `vals.len() != results.len()`) drops ALL candidates for
  `DB.Begin`. Caveat: the real `Begin` has NAMED results plus a conditional
  deferred logger closure reading `err`, which in SSA becomes stores to
  named-result cells and a component-wise Return of loads — that shape may
  PASS the arity check and behave differently. Both shapes must be probed.
- **H2**: the deref subject at line 26 is a load from the closure's capture
  cell, ordered BEFORE the closure's own guarded `tx, err = dst.Begin(true)`
  store — no in-function fact (and no callee ensures at any in-function call
  site) constrains it. That is the declared closure-capture non-goal.

H1 and H2 are independent; both can be true. H1's fix helps every dispatch
wrapper; only H2's status decides whether `compact.go:26:23` itself can be
discharged this wave.

**Files:**
- Create: `.superpowers/sdd/task-1-investigation.md`
- Modify: `testdata/corpus/ensures/ensures.go` (two wrapper probes)
- Modify: `crates/goverify-checkers/tests/ensures_corpus.rs` (probe asserts)
- Modify: `testdata/corpus/knownfp/knownfp.go` (closure-capture repro)

**Interfaces:**
- Consumes: `ensures_vars(p, a, func) -> Vec<Vec<String>>` helper in
  `ensures_corpus.rs:31-47`; `// want:` pin syntax; knownfp harness
  (`knownfp_corpus.rs`) asserting pins == findings exactly.
- Produces: investigation report naming the broken link per hypothesis, and a
  DECISION selecting Task 2's branch(es): `2A` (H1 fix), `2B` (H2
  re-attribution), or both. Task 2 consumes the report verbatim.

- [ ] **Step 0: Create the branch and capture the 461 baseline**

```bash
cd /Users/rahul.muttineni/goverify
git checkout -b summaries/followups
time mise run shakeout 2>&1 | tee .goverify/shakeout/baseline-461.txt
grep -cE '^\S+\.go:[0-9]+:[0-9]+: ' .goverify/shakeout/baseline-461.txt
```

Expected: 461 finding headers (warm run ≈31 s if the SMT cache is intact from
the merged wave; cold ≈163 s). `.goverify/` is untracked — the capture is
working evidence, not a committed artifact; record the count in the report.
Confirm `compact.go:26:23` is present and `command_surgery.go:268:55` absent:

```bash
grep -E 'compact\.go:26:23|command_surgery\.go:268:55|surgeon\.go:78:20' .goverify/shakeout/baseline-461.txt
```

Expected: `compact.go:26:23` and `surgeon.go:78:20` lines; no `268:55` line.

- [ ] **Step 1: Add the two wrapper probes to the ensures corpus**

Append to `testdata/corpus/ensures/ensures.go`:

```go
// newA is a second NewT-shaped constructor so the dispatch wrapper
// below has two distinct callees (the DB.Begin shape).
func newA(fail bool) (*T, error) {
	if fail {
		return nil, errOp
	}
	return &T{}, nil
}

// NewTVia is a bare forwarding dispatch wrapper: each return site
// forwards a callee's whole tuple (`return f(...)`), which SSA lowers
// as a single tuple-valued Return operand. Probes whether wrapper
// ensures survive tuple forwarding (C009c hypothesis H1, arity form).
func NewTVia(fail, alt bool) (*T, error) {
	if alt {
		return newA(fail)
	}
	return NewT(fail)
}

// NewTNamed is the real DB.Begin shape: NAMED results plus a deferred
// closure reading err, which forces SSA to materialize named-result
// cells (returns become stores + a component-wise Return of loads).
// Probes the second H1 form.
func NewTNamed(fail bool) (t *T, err error) {
	defer func() { _ = err }()
	if fail {
		return newA(fail)
	}
	return NewT(fail)
}
```

- [ ] **Step 2: Add the (expected-RED) harness asserts**

Append to the `ensures_inference_over_corpus` test in
`crates/goverify-checkers/tests/ensures_corpus.rs`:

```rust
    assert!(
        ensures_vars(&p, &a, "example.com/ensures.NewTVia")
            .contains(&vec!["r0".to_string(), "r1".to_string()]),
        "NewTVia: forwarding wrapper must inherit the (T, error) correlation: {:?}",
        ensures_vars(&p, &a, "example.com/ensures.NewTVia")
    );
    assert!(
        ensures_vars(&p, &a, "example.com/ensures.NewTNamed")
            .contains(&vec!["r0".to_string(), "r1".to_string()]),
        "NewTNamed: named-results+defer wrapper must inherit the correlation: {:?}",
        ensures_vars(&p, &a, "example.com/ensures.NewTNamed")
    );
```

- [ ] **Step 3: Run and record which probes fail**

Run: `mise x -- cargo test -p goverify-checkers --test ensures_corpus`
Expected: `NewTVia` FAILS (empty — the arity trip). `NewTNamed` outcome is
OPEN — record it either way. If it PASSES, `DB.Begin` (same shape) most
likely already has its ensures and H1 does not explain the bbolt site at all;
if it FAILS, note the exact failure shape (empty vs missing correlation).
These are investigation probes: a failing assert here is evidence, not
breakage — leave them failing until Task 2 (or delete the assert in Task 2B
if the investigation concludes the behavior is the accepted boundary — the
report must say which).

- [ ] **Step 4: Add the closure-capture repro to the knownfp corpus**

Append to `testdata/corpus/knownfp/knownfp.go` (this mirrors `Compact$2`:
a closure derefs a captured guarded call result BEFORE its own guarded
reassignment; capture-by-reference is forced by the reassignment):

```go
// KNOWN-FP(closure-capture): FP/requires-lifting — the deref subject
// inside the closure is a load from the capture cell, sequenced before
// the closure's own guarded reassignment; no in-function fact
// constrains it (bbolt exemplar C009c, compact.go:26:23 / Compact$2).
// Declared non-goal since the fix-wave; this pin tripwires the
// boundary. Verdict pending task-1 investigation — if the closure
// analyzes differently than bbolt's, rewrite this header to match
// observed behavior.
func useT(t *T) int {
	return t.X
}

func CaptureLoop(fail bool) int {
	t, err := NewT2(fail)
	if err != nil {
		return 0
	}
	n := 0
	f := func() {
		n += useT(t) // want: nil-deref
		t, _ = NewT2(false)
	}
	f()
	f()
	return n
}
```

Note: knownfp.go's existing helpers are the source of truth for constructor
names — if it has no `NewT2`-shaped `(T, error)` constructor already (check
the `Compact` exemplar's callee, e.g. `beginTx`), reuse that one and rename
accordingly. Do not import anything.

- [ ] **Step 5: Run knownfp and pin ACTUAL behavior**

Run: `mise x -- cargo test -p goverify-checkers --test knownfp_corpus`
Three possible outcomes, all informative — adjust the pin to match reality:
1. Finding fires at the `useT(t)` line → keep the `// want:` pin (H2
   reproduced in-corpus).
2. No finding → delete the `// want:`, keep the case with a comment noting
   the corpus closure discharges (or is skipped) while bbolt's does not —
   then H2 evidence must come from reading the bbolt lowering (step 6).
3. Extraction/analysis error → record it; closures may be outside the
   extractor's lowering — that itself is the H2 mechanism, document it.

Also run `mise run corpus` — the ensures-module additions must not disturb
other modules' pins or goldens (`nil.check.txt` etc. must be diff-clean).

- [ ] **Step 6: Decide H2 from the evidence**

If step 5 outcome 1: H2 confirmed empirically. Otherwise confirm by tracing:
does `Compact$2` in bbolt lower with the line-26 subject as a load from a
capture cell with no prior in-function store? Use the debug CLI on the
extracted bbolt `.gvir` (IR dump is StubSolver-safe — checker state is not
needed for this question):

```bash
mise x -- cargo run -p goverify-cli -- debug ir <extracted-compact-gvir> | grep -A5 -B5 "Commit"
```

(The extracted `.gvir` for bbolt lives under the shakeout cache dir tree;
locate with `find .goverify/shakeout -name '*.gvir' | head`. If the debug
subcommand's flags differ, `mise x -- cargo run -p goverify-cli -- debug --help`
is authoritative.)

- [ ] **Step 7: Write the investigation report + decision**

Create `.superpowers/sdd/task-1-investigation.md` covering: baseline capture
count; per-probe outcomes (NewTVia / NewTNamed / CaptureLoop) with exact
test output; the H1 verdict (which wrapper shapes lose ensures, and at which
line of `infer_ensures`); the H2 verdict (what the line-26 subject actually
is); and the DECISION line:

> Task 2 branches selected: [2A and/or 2B], because …

Rule: 2A is selected iff at least one wrapper probe is RED (the fix is
justified by the probe, independent of bbolt). 2B is selected iff the
bbolt site's blocker is the capture cell (H2) — i.e. fixing H1 alone cannot
discharge `compact.go:26:23`.

- [ ] **Step 8: Commit**

```bash
git add -A
git -c commit.gpgsign=false commit -m "followups: C009c investigation — wrapper-ensures + closure-capture probes, verdicts (task 1)"
```

(Committing the RED probes is intentional — they are the failing tests Task 2
flips. If Task 2A is NOT selected, Task 2 removes or inverts them per the
report before the branch's gates run.)

---

### Task 2: C009c resolution — forwarded-return ensures (2A) and/or re-attribution (2B)

Execute ONLY the branch(es) the task-1 report selected.

**Files:**
- Modify: `crates/goverify-checkers/src/nil.rs:173-254` (`infer_ensures` — branch 2A)
- Modify: `testdata/corpus/knownfp/knownfp.go` (finalize the CaptureLoop header — branch 2B)
- Modify: `crates/goverify-checkers/tests/ensures_corpus.rs` (only if 2A not selected: align probes with the documented boundary)

**Interfaces:**
- Consumes: `Site` collection loop (nil.rs:193-203), `site_nonnil` closure
  (nil.rs:215-223), `nonnil_result_clause(i: u32) -> Option<Clause>`
  (nil.rs:52), `correlation_clause(e: u32, i: u32) -> Option<Clause>`
  (nil.rs:64), `shared::canonical_value(func, v) -> ValueId`,
  `summary_of: &dyn Fn(FuncId) -> Summary`, `Provenance::Inferred`,
  `Summary`/`Clause` with derived `PartialEq`.
- Produces (2A): `infer_ensures` treats `return g(...)` tuple-forwarding
  sites as proven-by-callee-clause; wrapper functions inherit exactly the
  clauses ALL their forwarded callees (and explicit sites) support. No
  signature changes — later tasks and the engine are untouched.

#### Branch 2A: forwarded-tuple return sites delegate to callee ensures

- [ ] **Step A1: Confirm the failing tests**

Run: `mise x -- cargo test -p goverify-checkers --test ensures_corpus`
Expected: the task-1 probe asserts still FAIL (RED baseline for this fix).

- [ ] **Step A2: Implement the forwarded-site variant**

In `nil.rs`, replace the sites collection inside `infer_ensures`
(currently nil.rs:191-206) with a two-variant site list. The key soundness
fact: a forwarded site returns the callee's tuple unchanged, and the result
lists match index-for-index, so the callee's `r<i>`-named clauses are
verbatim true of the wrapper's results on that path — proof delegates to
clause membership in the callee's Inferred summary (in-flight SCC-local or
finalized, whichever `summary_of` serves).

```rust
        enum Site {
            /// Component-wise return: prove candidates with the SMT
            /// per-site checks, as before.
            Explicit(usize, Vec<ValueId>),
            /// `return g(...)` forwarding a static callee's whole
            /// tuple: a candidate clause is proven at this site iff
            /// the callee's own Inferred summary carries it verbatim
            /// (result lists align 1:1, so its r<i> ARE our r<i>).
            Forwarded(FuncId),
        }
        let mut sites: Vec<Site> = Vec::new();
        for (bi, b) in func.blocks.iter().enumerate() {
            for ins in &b.instrs {
                let Op::Return { vals } = &ins.op else {
                    continue;
                };
                if vals.len() == results.len() {
                    sites.push(Site::Explicit(bi, vals.clone()));
                    continue;
                }
                // A single operand forwarding a static call's tuple,
                // with the callee's result list matching this
                // function's exactly. Anything else keeps the old
                // conservative behavior: drop ALL candidates.
                let forwarded = (vals.len() == 1 && results.len() > 1)
                    .then(|| crate::shared::canonical_value(func, vals[0]))
                    .and_then(|canon| {
                        func.blocks.iter().flat_map(|b| &b.instrs).find_map(
                            |i| match &i.op {
                                Op::Call {
                                    dst: Some(d),
                                    callee: Callee::Static(g),
                                    ..
                                } if *d == canon => Some(*g),
                                _ => None,
                            },
                        )
                    })
                    .filter(|g| {
                        p.func(*g).is_some_and(|gf| {
                            matches!(
                                p.types().kind(gf.sig),
                                TypeKind::Signature { results: rs, .. }
                                    if *rs == results
                            )
                        })
                    });
                match forwarded {
                    Some(g) => sites.push(Site::Forwarded(g)),
                    None => return Vec::new(),
                }
            }
        }
```

(Adjust the `Callee`/`Op` import paths to the file's existing `use` set;
`Op::Call`'s exact field spelling in this IR is the neighboring
`call_result_of` in shared.rs:169-201 — mirror it.)

Then extend the proof sides. Add one helper closure above the template loop:

```rust
        // A forwarded site proves `want` iff the callee already
        // carries it. Havoc summaries never prove anything.
        let forwarded_proves = |g: FuncId, want: &Clause| -> bool {
            let s = summary_of(g);
            s.provenance == goverify_analysis::Provenance::Inferred
                && s.ensures
                    .iter()
                    .any(|c| c.tag == want.tag && c.formula == want.formula)
        };
```

and rewrite the two template checks to build the candidate clause FIRST and
dispatch per site variant:

```rust
        let mut out = Vec::new();
        for &i in &ptr_results {
            // Template 1: unconditional.
            let t1 = nonnil_result_clause(i as u32);
            let t1_proven = t1.as_ref().is_some_and(|c| {
                sites.iter().all(|s| match s {
                    Site::Explicit(bi, vals) => site_nonnil(*bi, vals[i]),
                    Site::Forwarded(g) => forwarded_proves(*g, c),
                })
            });
            if t1_proven {
                if let Some(c) = t1 {
                    push_clause(&mut out, c);
                }
                continue; // correlation is subsumed
            }
            // Template 2: correlate with each error result. A
            // forwarded site accepts the callee's correlation OR its
            // stronger unconditional clause.
            for &e in &error_results {
                if e == i {
                    continue;
                }
                let Some(c2) = correlation_clause(e as u32, i as u32) else {
                    continue;
                };
                let proven = sites.iter().all(|s| match s {
                    Site::Explicit(bi, vals) => {
                        let err_is_nil_literal = matches!(
                            func.value(vals[e]).kind,
                            ValueKind::Const(ConstVal::Nil)
                        );
                        if err_is_nil_literal {
                            site_nonnil(*bi, vals[i])
                        } else {
                            true // Go-idiom rule: non-literal error ⇒ non-nil
                        }
                    }
                    Site::Forwarded(g) => {
                        forwarded_proves(*g, &c2)
                            || t1.as_ref().is_some_and(|c| forwarded_proves(*g, c))
                    }
                });
                if proven {
                    push_clause(&mut out, c2);
                }
            }
        }
        out
```

Preserve the existing `sites.is_empty()` early return between collection and
templates. Determinism: site order is block/instr order; `forwarded_proves`
scans a `Vec` — no map iteration introduced.

- [ ] **Step A3: Run the probes**

Run: `mise x -- cargo test -p goverify-checkers --test ensures_corpus`
Expected: `NewTVia` PASSES. `NewTNamed` per the task-1 report (if its RED
cause was arity/forwarding, it flips too; if it was something else the report
scoped out, its assert must have been adjusted in task 1's report terms).

- [ ] **Step A4: Add a unit test for the mixed/degenerate shapes**

In nil.rs's tests module, following the local fixture style (the neighboring
`unguarded_param_deref_infers_nonnil_requires` at nil.rs:374 and the testfix
builders are the source of truth for fixture spelling):

```rust
    #[test]
    fn forwarding_to_summaryless_callee_infers_nothing() {
        // Wrapper forwards a callee whose summary is havoc: no clause
        // may be inherited (havoc never proves).
        // Build: t.G with (ptr, error) sig and a body; t.W with the
        // same sig whose single Return operand is t.G's call dst.
        // summary_of = |_| Summary::havoc().
        // assert: NilChecker.infer_ensures(&p, w, &havoc_summaries,
        //         &mut z3_discharge()).is_empty()
    }
```

Write the fixture out fully (mirroring the file's existing gvir builders) —
the assertion is `assert!(clauses.is_empty(), "havoc callee must not seed wrapper ensures: {clauses:?}")`.

- [ ] **Step A5: Full checker suite + corpus**

Run: `mise x -- cargo test -p goverify-checkers && mise run corpus`
Expected: green, EXCEPT possibly knownfp pins that the new wrapper ensures
genuinely discharge — inspect any flip, and if it is a sound discharge
(dominating guard + now-inherited ensures), flip the pin with a
`FIXED (followups 2026-07-21, task 2A): formerly KNOWN-FP …` header, mirroring
the summaries-wave convention. Golden drift in `testdata/goldens/*.check.txt`:
review the diff, then `UPDATE_GOLDENS=1 mise x -- cargo test -p goverify-cli`
and re-review.

- [ ] **Step A6: Commit**

```bash
git add -A
git -c commit.gpgsign=false commit -m "followups: infer_ensures delegates forwarded-tuple return sites to callee clauses (task 2A)"
```

#### Branch 2B: re-attribute `compact.go:26:23` to the closure-capture family

- [ ] **Step B1: Finalize the CaptureLoop pin header**

Rewrite the task-1 placeholder header on the knownfp `CaptureLoop` case to
state the CONFIRMED mechanism from the report (capture-cell load sequenced
before the closure's guarded store; which lowering ops are involved), drop
the "verdict pending" sentence, and name the re-attribution: C009c's
surviving member belongs with the closure/cobra capture family (C027 et
al.), not the postcondition-lifting family the summaries wave targeted.

- [ ] **Step B2: Run knownfp to confirm the pin matches**

Run: `mise x -- cargo test -p goverify-checkers --test knownfp_corpus`
Expected: PASS (pin == behavior). The shakeout-doc note lands in Task 7's
addendum, not here.

- [ ] **Step B3: Commit**

```bash
git add -A
git -c commit.gpgsign=false commit -m "followups: C009c compact.go survivor re-attributed to closure-capture family, pin finalized (task 2B)"
```

---

### Task 3: C221 investigation — bisect the regression, confirm the discharge path

**Files:**
- Create: `.superpowers/sdd/task-3-investigation.md`
- Modify: `testdata/corpus/bounds/bounds.go` (RED repro)

**Interfaces:**
- Consumes: shakeout runner (`scripts/shakeout.sh`, findings render
  `<file>:<line>:<col>: <tag>: …`), the 16 wave commits `31a50cf..77b602f`,
  bounds checker split (`infer_requires` bounds.rs:465-489 params_only+Sat
  gate; `obligations` bounds.rs:525-537 expressible gate;
  `convert_sites`/`convert_violation` bounds.rs:260-332).
- Produces: the regression mechanism (which commit, which gate flipped), a
  RED corpus repro, and a DECISION selecting Task 4's branch: `4A`
  (widening-convert range model, discharge outright — default expectation)
  or `4B` (restore the requires form).

- [ ] **Step 1: Bisect with targeted probes**

The predicate: `command_surgery.go:268:55` present (pre-regression) vs
`surgeon.go:78:20` manifest present (post-regression). Probe the two suspect
commits FIRST (spec §4): `9994c53` (task 6: bounds.rs → `encode_func_with`)
and `5549cb7` (task 8: canonicalization); fall back to binary search over
`31a50cf..77b602f` only if both probes contradict the hypotheses. Each probe
runs in a worktree that shares the bbolt checkout and SMT cache via symlink
(each is a COLD solver run, ≈163 s — checker changes invalidate the cache):

```bash
cd /Users/rahul.muttineni/goverify
git worktree add /private/tmp/claude-502/-Users-rahul-muttineni-goverify/649b49a2-2410-47b3-90bc-bb31be61564b/scratchpad/c221-probe <commit>
cd /private/tmp/claude-502/-Users-rahul-muttineni-goverify/649b49a2-2410-47b3-90bc-bb31be61564b/scratchpad/c221-probe
ln -s /Users/rahul.muttineni/goverify/.goverify .goverify
mise run shakeout 2>&1 | grep -E 'surgeon\.go:78:20|command_surgery\.go:268:55' || echo "NEITHER"
cd /Users/rahul.muttineni/goverify
git worktree remove --force /private/tmp/claude-502/-Users-rahul-muttineni-goverify/649b49a2-2410-47b3-90bc-bb31be61564b/scratchpad/c221-probe
```

Record per-probe: commit, which signature(s) appeared. The FIRST commit whose
run shows `surgeon.go:78:20` (and no `268:55`) is the culprit.

- [ ] **Step 2: Confirm the mechanism in the culprit diff**

Read the culprit commit against the bounds split and answer precisely: did
`infer_requires` stop emitting the `¬violation` clause for
`ClearPageElements` (params_only fail? discharge flipped Sat→Unsat/Unknown
under the summary-fed encoding?), or did `obligations`' `pre` stop carrying
it? (`pre = own_preconditions(&summary_of(f))` — both deltas trace to the
requires clause vanishing; the question is WHY.) Quote the responsible lines
in the report.

- [ ] **Step 3: Write the RED corpus repro**

Append to `testdata/corpus/bounds/bounds.go` the guarded-truncation shape
(no `// want:` — the case asserts SILENCE, so today's firing makes the
corpus test RED):

```go
// count is an opaque uint16 source: its call result is havoc, but its
// SORT is BitVec(16), so ≤65535 is intrinsic — the widening int()
// conversion is what severs the bound today (C221 exemplar,
// surgeon.go:78:20 / ClearPageElements).
func count() uint16 { return 42 }

func ClearElems(start int) uint16 {
	n := int(count())
	if start < 0 || start >= n {
		return 0
	}
	return uint16(start)
}

// One unbounded and one bounded caller: under the requires-form
// fallback (task 4B) only the unbounded one may fire; under the
// convert-model discharge (task 4A) both stay silent.
func ClearElemsUnbounded(i int) uint16 { return ClearElems(i) }

func ClearElemsBounded() uint16 { return ClearElems(3) }
```

Run: `mise x -- cargo test -p goverify-checkers --test bounds_corpus`
Expected: FAIL — an overflow finding fires (at the `uint16(start)` line
and/or a caller) with no matching want. Record WHERE it fires — that must
match the bisected mechanism (manifest inside `ClearElems` mirrors bbolt's
regressed state). If the corpus shape does NOT reproduce (fires nowhere),
adjust it using the step-2 mechanism (e.g. the regression may need a summary
-bearing callee in scope) until it does; the repro must be RED before Task 4.

- [ ] **Step 4: Report + decision**

`.superpowers/sdd/task-3-investigation.md`: probe table, culprit commit,
mechanism quote, repro behavior, and the DECISION:

> Task 4 branch selected: [4A convert-model discharge | 4B requires-form
> restoration], because …

Default is 4A (spec §4): the range model discharges BOTH sites and
`infer_requires`' Sat-reachability gate then never mints the call-site
clause. Select 4B only if step 2 shows the regression mechanism ALSO damages
other findings (i.e. the vanished requires is a symptom of a broader
discharge regression that must be repaired on its own terms), or 4A's model
provably cannot bound this shape.

- [ ] **Step 5: Commit**

```bash
git add -A
git -c commit.gpgsign=false commit -m "followups: C221 bisect + mechanism, RED truncation repro (task 3)"
```

---

### Task 4: C221 fix — widening-convert range model (4A) or requires restoration (4B)

Execute ONLY the branch task 3 selected.

**Files:**
- Modify: `crates/goverify-analysis/src/encode.rs:1031-1039` (the
  `Op::Convert` arm in the op-constraint match — branch 4A)
- Test: `crates/goverify-analysis/src/encode.rs` (tests module)
- Modify: `testdata/corpus/bounds/bounds.go` + goldens (flip per branch)

**Interfaces:**
- Consumes: `int_repr(types, t) -> Option<(u32, bool)>` (encode.rs:46-62,
  width + signedness), `Term::bv_lit(width, value)`, `Term::bv_cmp(BvCmpOp,
  lhs, rhs)`, `Term::and(vec)`, the existing uintptr `Op::Convert` arm
  (encode.rs:1031-1039), `sort_of` yielding `Sort::BitVec(w)` for ints.
- Produces (4A): every widening int→int `Op::Convert` dst carries a
  source-range constraint; downstream checkers see `int(u16value) ≤ 65535`
  for free. No API change — the arm returns `Option<Term>` like its
  neighbors.

#### Branch 4A: widening int→int converts assert the source type's range

- [ ] **Step A1: Confirm the RED repro**

Run: `mise x -- cargo test -p goverify-checkers --test bounds_corpus`
Expected: FAIL (task-3 repro fires).

- [ ] **Step A2: Write the failing unit test**

In encode.rs's tests module, mirroring the existing Convert fixture style
(the uintptr-provenance tests around encode.rs:1865-1885 show the exact
`Instruction { kind: "Convert", … }` spelling and the basic-type table
entries; those neighboring tests are the source of truth for field names):

```rust
    #[test]
    fn widening_convert_bounds_dst_by_source_range() {
        // f(p0 uint16): v2 = Convert p0 -> int(64).
        // The dst must be provably ≤ 65535: reach ∧ (v2 >u 65535) Unsat.
        // FIXTURE: copy the uintptr Convert test's package literal
        // (encode.rs:1865-1885 region) and change it to: two basic
        // types (id 1 repr "uint16", id 2 repr "int", both
        // TypeKind::Basic), one param (id 1, type 1), one instruction
        // (kind "Convert", register 2, r#type 2, operands vec![1]),
        // then a bare Return. The neighboring test is the source of
        // truth for every field spelling.
        let p = build_widening_convert_fixture();
        let f = p.lookup_func("t.F").unwrap();
        let enc = encode_func(&p, f).unwrap();
        let dst = enc.value(goverify_ir::ValueId(2)).unwrap().clone();
        let over = Term::bv_cmp(
            BvCmpOp::Ugt,
            dst,
            Term::bv_lit(64, 65_535),
        )
        .unwrap();
        let mut solver = goverify_solver::Z3Native::new(goverify_solver::SolverLimits {
            timeout_ms: 5_000,
            mem_mb: 1024,
        });
        let mut discharge = |q: &goverify_solver::Query| {
            goverify_solver::discharge_query(q, &mut solver, None, None).result
        };
        assert_eq!(
            discharge(&enc.reach_query(0, vec![over])),
            goverify_solver::SatResult::Unsat,
            "widening from uint16 must bound the dst by 65535"
        );
    }
```

(If `BvCmpOp` has no `Ugt`, use `Ult` with swapped operands — the enum in
`goverify-solver` is authoritative.) Write the fixture completely in the
real test. Run: `mise x -- cargo test -p goverify-analysis widening_convert`
Expected: FAIL with `Sat` (dst is havoc today).

- [ ] **Step A3: Implement the arm**

Replace the `Op::Convert` arm (encode.rs:1031-1039) with the merged form —
int-int widening first, then the existing uintptr pointer case:

```rust
        Op::Convert { dst, src } => {
            let d = t(dst)?;
            // int → int WIDENING: the dst holds the src's exact value,
            // but the term language has no zext/sext node, so assert
            // the sound projection — the src TYPE's value range at the
            // dst width. (Sign-extension preserves signed order, so a
            // signed source's range is signed-compared regardless of
            // the dst's own signedness; an unsigned source zero-
            // extends, so Ule against 2^ws−1 suffices.) Narrowing and
            // same-width converts stay havoc: their range adds nothing
            // beyond the dst sort's intrinsic width.
            if let (Some((ws, ss)), Some((wd, _))) = (
                int_repr(p.types(), func.value(*src).ty),
                int_repr(p.types(), func.value(*dst).ty),
            ) {
                if wd <= ws {
                    return None;
                }
                return if ss {
                    let max = (1u128 << (ws - 1)) - 1;
                    let min = (1u128 << wd) - (1u128 << (ws - 1));
                    let lo = Term::bv_cmp(
                        BvCmpOp::Sle,
                        Term::bv_lit(wd, min),
                        d.clone(),
                    )
                    .ok()?;
                    let hi =
                        Term::bv_cmp(BvCmpOp::Sle, d, Term::bv_lit(wd, max)).ok()?;
                    Term::and(vec![lo, hi]).ok()
                } else {
                    let max = (1u128 << ws) - 1;
                    Term::bv_cmp(BvCmpOp::Ule, d, Term::bv_lit(wd, max)).ok()
                };
            }
            // A pointer minted from uintptr arithmetic is never nil
            // (fix-wave fix 3) — unchanged.
            if d.sort() != &ptr_sort()
                || !uintptr_provenance(p, func, *src, 0, &mut BTreeSet::new())
            {
                return None;
            }
            Term::not(ptr_is_nil(d).ok()?).ok()
        }
```

Also update the trailing catch-all comment (encode.rs:1052-1053) — it says
"Convert havocs except the uintptr-provenance arm"; now it is "except the
uintptr-provenance and int-widening arms". Keep the existing arm's doc
comment (encode.rs:1024-1030) attached to the pointer half.

- [ ] **Step A4: Run the unit test, then the repro**

Run: `mise x -- cargo test -p goverify-analysis widening_convert`
Expected: PASS.
Run: `mise x -- cargo test -p goverify-checkers --test bounds_corpus`
Expected: PASS — `ClearElems`/callers all silent (the manifest query is now
Unsat, and `infer_requires`' Sat gate never mints the caller clause).

- [ ] **Step A5: Full sweep for collateral flips**

Run: `mise x -- cargo test -p goverify-checkers && mise x -- cargo test -p goverify-analysis && mise run corpus`
The new constraints can only shrink model space — expect discharges, never
new findings. Inspect EVERY flipped pin/golden line: sound discharges get
the `FIXED (followups 2026-07-21, task 4A)` header treatment; anything
suspicious (a TP-shaped case going quiet) stops the task — do not refresh
goldens over an unexplained flip. `encode_props` must stay green
(`mise x -- cargo test -p goverify-analysis --test encode_props`).

- [ ] **Step A6: Commit**

```bash
git add -A
git -c commit.gpgsign=false commit -m "followups: widening int converts assert source-range on dst — C221 discharge (task 4A)"
```

#### Branch 4B: restore the call-site requires form (fix-3 parity)

Only if task 3 selected it. The concrete change depends on the bisected
mechanism (a discharge regression under `encode_func_with`, or a
params_only classification shift); the invariant deliverables are fixed:

- [ ] **Step B1**: repair the mechanism named in the report (smallest change
  that makes `infer_requires` mint `ClearPageElements`' `¬violation` clause
  again), with a unit test in bounds.rs's tests module pinning it RED→GREEN.
- [ ] **Step B2**: update the task-3 corpus repro pins: `// want: overflow`
  on `ClearElemsUnbounded`'s call line only; `ClearElems` and
  `ClearElemsBounded` silent. Run `bounds_corpus` GREEN.
- [ ] **Step B3**: full sweep as in A5; commit as
  `followups: restore C221 call-site requires form (task 4B)`.

---

### Task 5: Test pins — recursive ensures, interface non-case, p\<i\> binding

**Files:**
- Modify: `testdata/corpus/ensures/ensures.go`
- Modify: `crates/goverify-checkers/tests/ensures_corpus.rs`
- Modify: `testdata/corpus/nil/nil.go` (caller-discharge pin)
- Test: `crates/goverify-analysis/src/encode.rs` (p\<i\> pin, tests module)

**Interfaces:**
- Consumes: `ensures_vars` helper; `encode_func_with(p, f, summary_of)`;
  the `call_ensures_constrain_single_result` fixture pattern
  (encode.rs:2599-2711) — hand-built `Summary` + `Z3Native` discharge.
- Produces: tripwires only — no behavior change anywhere.

- [ ] **Step 1: Recursive-ensures corpus cases**

Append to `testdata/corpus/ensures/ensures.go`:

```go
// Rec2's recursion is irrelevant to its result: the single return
// site yields a fresh allocation, so the unconditional ensures must
// be inferred even though Rec2 forms a recursive SCC — pins the
// simultaneous-fixpoint soundness examined in the summaries wave's
// final review (self-consultation via the in-flight summary).
func Rec2(n int) *T {
	if n > 0 {
		_ = Rec2(n - 1)
	}
	return &T{}
}

// Rec forwards its own recursive result: the optimistic fixpoint
// starts it clause-free and nothing independent ever proves the
// candidate, so the converged summary must STAY clause-free — the
// inference must not bootstrap a self-justifying ensures.
func Rec(n int) *T {
	if n == 0 {
		return &T{}
	}
	return Rec(n - 1)
}
```

Harness adds (in `ensures_inference_over_corpus`):

```rust
    assert_eq!(
        ensures_vars(&p, &a, "example.com/ensures.Rec2"),
        vec![vec!["r0".to_string()]],
        "Rec2: recursion-independent unconditional ensures must survive the SCC fixpoint"
    );
    assert!(
        ensures_vars(&p, &a, "example.com/ensures.Rec").is_empty(),
        "Rec: a self-forwarding recursion must not bootstrap its own ensures: {:?}",
        ensures_vars(&p, &a, "example.com/ensures.Rec")
    );
```

Caution: `Rec`'s expected emptiness assumes the current fixpoint semantics;
if Task 2A's forwarded-site handling changes the observed result (a
single-result forward is an EXPLICIT site — `vals.len()==1==results.len()` —
so it should not), pin observed behavior and explain in the comment; an
unexplained non-empty result here is a soundness alarm, stop and investigate.

And the caller-discharge pin in `testdata/corpus/nil/nil.go` (corpus modules
are separate Go modules, so nil.go gets its own local recursive twin; nil.go's
`T` already exists — reuse it). No `// want:` — the case asserts silence:

```go
func recFresh(n int) *T {
	if n > 0 {
		_ = recFresh(n - 1)
	}
	return &T{}
}

// Deref of a recursive callee's constrained result: obligation raised
// (r0 constrained by the inferred unconditional ensures), discharged
// by the same clause. Silent = the recursive clause is actually
// CONSUMED at a call site, not just emitted into the summary.
func UseRecFresh() int { return recFresh(1).X }
```

- [ ] **Step 2: Interface-result non-case pin**

Append to `testdata/corpus/ensures/ensures.go`:

```go
type Iface interface{ M() }

type impl struct{ x int }

func (i *impl) M() {}

// AsIface returns a typed-nil-prone interface: on the fail path the
// wrapped *impl is nil while the interface value itself is a
// MakeInterface product. Inference must NOT claim ¬is_nil(r0) —
// interfaces are Ptr-sorted since the summaries wave, and this pins
// the boundary against the Go-idiom under-approximation silently
// widening to interface results.
func AsIface(fail bool) Iface {
	var p *impl
	if !fail {
		p = &impl{}
	}
	return p
}
```

Harness:

```rust
    assert!(
        ensures_vars(&p, &a, "example.com/ensures.AsIface").is_empty(),
        "AsIface: typed-nil-prone interface result must stay clause-free: {:?}",
        ensures_vars(&p, &a, "example.com/ensures.AsIface")
    );
```

- [ ] **Step 3: Run the corpus additions**

Run: `mise x -- cargo test -p goverify-checkers --test ensures_corpus --test nil_corpus`
Expected: PASS if behavior already matches the pins (these are tripwires —
they may pass first try; that is fine and still valuable). Any FAIL: the pin
found a live bug — stop, investigate, report to the controller rather than
bending the pin.

- [ ] **Step 4: p\<i\>-bearing ensures e2e pin (dead capability)**

In encode.rs's tests module, clone the `call_ensures_constrain_single_result`
fixture (encode.rs:2599-2711) into a new test with these deltas: the caller
takes one `*T` param (`gvir::Param { id: 1, … r#type: 2 }`) passed as the
call's operand (`operands: vec![1]`, register 2 for the dst), and the
hand-built summary's clause relates p0 and r0 —
`¬is_nil(p0) ⇒ ¬is_nil(r0)`, i.e. `or(is_nil(p0), ¬is_nil(r0))`:

```rust
        let p0 = Term::var("p0", ptr_sort());
        let r0 = Term::var("r0", ptr_sort());
        let clause_term = Term::or(vec![
            ptr_is_nil(p0).unwrap(),
            Term::not(ptr_is_nil(r0).unwrap()).unwrap(),
        ])
        .unwrap();
```

Assertion: under `encode_func_with`, `reach ∧ ¬is_nil(param) ∧ is_nil(dst)`
is Unsat — proving the encoder bound `p0` to the caller's ARGUMENT term (the
receiver-position mechanics: a method receiver is operand 0, exactly this
binding path) and `r0` to the dst:

```rust
        let with = encode_func_with(&p, caller, &summary_of).unwrap();
        let param = with.value(goverify_ir::ValueId(1)).unwrap().clone();
        let dst = with.value(goverify_ir::ValueId(2)).unwrap().clone();
        let q = with.reach_query(
            0,
            vec![
                Term::not(ptr_is_nil(param).unwrap()).unwrap(),
                ptr_is_nil(dst).unwrap(),
            ],
        );
        assert_eq!(
            discharge(&q),
            goverify_solver::SatResult::Unsat,
            "p<i>-bearing ensures must bind p0 to the call ARGUMENT and r0 to the dst"
        );
```

Name it `call_ensures_bind_param_vars_to_arguments`. This exercises
`bind_with`'s `p<i>` branch (summary.rs:124-134) end-to-end — dead capability
today (no checker emits p-vars), flagged unverified in the summaries wave's
task-4 review.

Run: `mise x -- cargo test -p goverify-analysis call_ensures_bind_param_vars`
Expected: PASS. If it FAILS, the binding path has a real bug — report, don't
bend.

- [ ] **Step 5: Commit**

```bash
git add -A
git -c commit.gpgsign=false commit -m "followups: pins — recursive ensures fixpoint, interface non-case, p<i> arg binding (task 5)"
```

---

### Task 6: Hygiene — spec §6 wording, chatty-test de-flake, groundness comment

**Files:**
- Modify: `docs/superpowers/specs/2026-07-20-interprocedural-summaries-design.md:228-232`
- Modify: `crates/goverify-solver/src/process.rs:252-269`
- Modify: `crates/goverify-checkers/src/nil.rs:282-292`

**Interfaces:** none — doc/comment/test-robustness only; zero behavior change.

- [ ] **Step 1: Fix the §6 clause-order wording**

In the predecessor spec's §6 determinism bullet, the text says clause order
is "template-order × result-index"; the implementation (`infer_ensures`
loop nesting: result index OUTER, template inner) emits result-index-major.
Change the phrase to "result-index × template-order" and leave the rest of
the bullet untouched. One-line diff; the doc records history, so add nothing
else.

- [ ] **Step 2: De-flake the chatty-solver test**

In `solve_text_drains_a_chatty_solver_without_downgrading_to_unknown`
(process.rs:252-269): the verdict-vs-deadline race is inherently temporal
(spec §6: generous bound, not a logical-event rewrite), so raise the
deadline headroom:

```rust
        SolverLimits {
            // Generous headroom: the assertion is drain-vs-deadline
            // CORRECTNESS (a drained `sat` must never downgrade to
            // Unknown), not latency. 2 s flaked under CI load; a
            // regression (undrained pipe → deadline hit) still fails
            // fast enough at 30 s.
            timeout_ms: 30_000,
            mem_mb: 1024,
        },
```

Run: `mise x -- cargo test -p goverify-solver solve_text_drains`
Expected: PASS (and promptly — the healthy path never approaches the
deadline).

- [ ] **Step 3: Rewrite the stale groundness comment**

Replace the comment block at nil.rs:282-292 (it predates canonicalization
and misnames the encoder) with:

```rust
        // Local manifest sites: the subject is CANONICALIZED through
        // same-function Assign/ChangeType chains first (`canonical_value`,
        // depth-capped), then classified: ground (const nil reached
        // through modeled ops) or params-only (preconditions decide).
        // A `Const` value's own encoded term is never free-vars-empty —
        // `encode_func_with` ties it down with a separate `v<id> = <lit>`
        // assert rather than inlining the literal (`declare_value`) — so
        // groundness is read off the canonical value's `ValueKind`, not
        // the term. Matched narrowly to `ConstVal::Nil` (not any
        // `Const(_)`): a pointer-typed value tagged `Const` with a
        // mismatched constant (malformed/fuzzed .gvir) gets no defining
        // assert from `declare_value`, so treating it as ground would
        // manufacture a finding off a genuinely free variable.
```

- [ ] **Step 4: Lint + commit**

Run: `mise run lint && mise x -- cargo test -p goverify-solver`
Expected: green.

```bash
git add -A
git -c commit.gpgsign=false commit -m "followups: hygiene — §6 clause-order wording, chatty-test headroom, groundness comment (task 6)"
```

---

### Task 7: Blocking gate + bbolt shakeout + gate addendum

**Files:**
- Modify: `docs/shakeout-phase4-bbolt.md` (new addendum section)
- Modify: goldens under `testdata/goldens/` only if a gate rerun surfaces reviewed drift

**Interfaces:**
- Consumes: everything above; baseline capture from Task 1 step 0
  (`.goverify/shakeout/baseline-461.txt`, 461 findings).
- Produces: spec §7 gate verdicts G1-G5, recorded for the plan owner.

- [ ] **Step 1: Full blocking tier**

```bash
mise run lint
mise run test
mise run corpus
mise run secrets
mise run audit
```

Expected: all green. Record `time` for test + corpus (G5). The known
load-sensitive solver test was de-flaked in Task 6; any OTHER flake gets
investigated, not re-run into silence.

- [ ] **Step 2: Shakeout ×3 with determinism check (G4)**

```bash
rm -rf .goverify/shakeout/cache        # force one cold run
time mise run shakeout 2>&1 | tee /private/tmp/claude-502/-Users-rahul-muttineni-goverify/649b49a2-2410-47b3-90bc-bb31be61564b/scratchpad/shakeout-cold.txt
time mise run shakeout 2>&1 | tee /private/tmp/claude-502/-Users-rahul-muttineni-goverify/649b49a2-2410-47b3-90bc-bb31be61564b/scratchpad/shakeout-warm1.txt
time mise run shakeout 2>&1 | tee /private/tmp/claude-502/-Users-rahul-muttineni-goverify/649b49a2-2410-47b3-90bc-bb31be61564b/scratchpad/shakeout-warm2.txt
for f in shakeout-cold shakeout-warm1 shakeout-warm2; do
  grep -E '^\S+\.go:[0-9]+:[0-9]+: ' "/private/tmp/claude-502/-Users-rahul-muttineni-goverify/649b49a2-2410-47b3-90bc-bb31be61564b/scratchpad/$f.txt" | cut -d' ' -f1-2 | sort > "/private/tmp/claude-502/-Users-rahul-muttineni-goverify/649b49a2-2410-47b3-90bc-bb31be61564b/scratchpad/$f.sigs"
done
cmp /private/tmp/claude-502/-Users-rahul-muttineni-goverify/649b49a2-2410-47b3-90bc-bb31be61564b/scratchpad/shakeout-cold.sigs /private/tmp/claude-502/-Users-rahul-muttineni-goverify/649b49a2-2410-47b3-90bc-bb31be61564b/scratchpad/shakeout-warm1.sigs
cmp /private/tmp/claude-502/-Users-rahul-muttineni-goverify/649b49a2-2410-47b3-90bc-bb31be61564b/scratchpad/shakeout-warm1.sigs /private/tmp/claude-502/-Users-rahul-muttineni-goverify/649b49a2-2410-47b3-90bc-bb31be61564b/scratchpad/shakeout-warm2.sigs
```

Expected: both `cmp`s silent (signature-level determinism; witness `with:`
lines may differ — that is the known cosmetic nondeterminism, outside the
gate key). G4 verdict = the cmp results.

- [ ] **Step 3: Evaluate G1-G3**

```bash
grep -E 'compact\.go:26:23' /private/tmp/claude-502/-Users-rahul-muttineni-goverify/649b49a2-2410-47b3-90bc-bb31be61564b/scratchpad/shakeout-warm1.txt
grep -E 'surgeon\.go:78:20|command_surgery\.go:268:55' /private/tmp/claude-502/-Users-rahul-muttineni-goverify/649b49a2-2410-47b3-90bc-bb31be61564b/scratchpad/shakeout-warm1.txt
grep -E '^\S+\.go:[0-9]+:[0-9]+: ' .goverify/shakeout/baseline-461.txt | cut -d' ' -f1-2 | sort > /private/tmp/claude-502/-Users-rahul-muttineni-goverify/649b49a2-2410-47b3-90bc-bb31be61564b/scratchpad/baseline.sigs
diff /private/tmp/claude-502/-Users-rahul-muttineni-goverify/649b49a2-2410-47b3-90bc-bb31be61564b/scratchpad/baseline.sigs /private/tmp/claude-502/-Users-rahul-muttineni-goverify/649b49a2-2410-47b3-90bc-bb31be61564b/scratchpad/shakeout-warm1.sigs
```

- **G1**: `compact.go:26:23` per the task-1 verdict — absent (discharged), or
  present WITH the re-attribution documented (branch 2B).
- **G2**: `surgeon.go:78:20` per the task-3/4 branch — both signatures absent
  (4A discharge), or exactly `command_surgery.go:268:55` present (4B parity).
- **G3**: walk the FULL diff — every departed and arrived signature must be
  attributed to a specific task (2A wrapper-ensures discharges, 4A
  convert-model discharges, nothing else). ANY unattributed delta blocks the
  addendum: root-cause it first. This is the finding-suppression tripwire.

- [ ] **Step 4: Write the addendum**

Append to `docs/shakeout-phase4-bbolt.md` a section
`## Summaries follow-up wave addendum (2026-07-21)` mirroring the prior
addendum's structure: baseline/current totals, G1-G5 verdicts with evidence
(signatures, per-task attribution table for the G3 diff, cold/warm timings),
the C009c re-attribution note if branch 2B ran (surviving member reclassified
to the closure-capture family, with the knownfp pin as tripwire), and open
items for the plan owner.

- [ ] **Step 5: Commit**

```bash
git add -A
git -c commit.gpgsign=false commit -m "followups: bbolt shakeout re-run addendum — gate verdicts G1-G5 (task 7)"
```

---

## Self-review notes (already applied)

- Spec coverage: §1/§2 scope+order → task sequence; §3 → Tasks 1-2 (H1/H2
  probes, both branches); §4 → Tasks 3-4 (bisect, 4A/4B); §5 pins → Task 5;
  §6 hygiene → Task 6; §7 gates → Task 7 + Task 1 step 0 baseline; §8
  invariants → Global Constraints.
- Branch discipline: investigation reports (Tasks 1/3) are the ONLY selectors
  of fix branches (Tasks 2/4); unselected branches are skipped, and Task 1's
  RED probes are explicitly owned by Task 2 (flip, or align with the
  documented boundary — never left dangling into Task 7's gates).
- Type/name consistency: `Site::Explicit/Forwarded` and `forwarded_proves`
  exist only in Task 2A; `ensures_vars` spelling matches
  ensures_corpus.rs:31; `int_repr`/`bv_lit`/`bv_cmp` signatures verified
  against term.rs:111,204,216; fixture spellings defer to named neighboring
  tests (encode.rs:2599, nil.rs:374) — the stated source-of-truth rule.
- Known judgment calls: `NewTNamed`/`CaptureLoop` corpus outcomes are
  deliberately open (investigation evidence, pin-actual-behavior rule);
  `Rec`'s emptiness and `AsIface`'s silence are tripwires that may pass
  immediately — a first-try PASS is the expected good outcome, a FAIL is a
  real alarm, both documented in-place.
