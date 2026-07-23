# Task 3 Report: Unbounded-elemOffset Positive-Polarity Overflow Pin

## Status: BLOCKED

**Critical Stop Condition Triggered:** The test suite PASSES without the want comment, indicating that the overflow obligation is NOT firing for the unbounded elemOffset call.

## What Was Attempted

Following the task brief, I implemented Step 1-2:

1. Added the `UnboundedElemOffset` fixture to `testdata/corpus/knownfp/knownfp.go` after line 424:
   ```go
   // wave-2 positive-polarity guard for task 4A (wave-2 spec §4): a
   // truly-unbounded `n` — a bare int parameter, no bounded-typed source
   // anywhere — must keep elemOffset's call-site "overflow" obligation
   // firing. 4A's widening-convert range model asserts only the SOURCE
   // TYPE's range on a conversion's dst; there is no conversion here, so
   // it must have nothing to say. Guards 4A against over-suppression.
   func UnboundedElemOffset(base uintptr, n int) uintptr {
   	return elemOffset(base, 16, n)
   }
   ```

2. Ran the corpus suite to verify the overflow obligation was firing (Step 2):
   ```bash
   mise x -- cargo test -p goverify-checkers --test knownfp_corpus
   ```

## RED Evidence

**Expected:** The test should FAIL with a set-equality mismatch showing an extra `("knownfp.go", 433, "overflow")` entry in `got` that `want` lacks.

**Actual:** The test **PASSED**, despite the fixture having no `// want: overflow` comment.

```
running 1 test
test knownfp_corpus_findings_match_want_comments ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

## Critical Finding

The test passing indicates that **the analyzer is NOT reporting an overflow finding** for the `elemOffset(base, 16, n)` call at line 433 in `UnboundedElemOffset`. This violates the task's core requirement:

**"A truly-unbounded `n` — a bare int parameter, no bounded-typed source anywhere — must keep elemOffset's call-site 'overflow' obligation firing."**

## Root Cause Analysis

Task 4A introduces a widening-convert range model that asserts only the SOURCE TYPE's range on a conversion's destination. The task comment states:

> "4A's widening-convert range model asserts only the SOURCE TYPE's range on a conversion's dst; there is no conversion here, so it must have nothing to say."

The fact that the overflow obligation is NOT firing suggests **task 4A is over-suppressing the unbounded-parameter case**. The absence of any type conversion means 4A has no range assertion to apply, and the unbounded `int` parameter should still trigger an overflow obligation.

## What This Means

This is exactly the scenario the brief's CRITICAL STOP CONDITION warns about:

> "If the suite PASSES instead, STOP: 4A is over-suppressing exactly what this pin guards — report as a finding, do not add the want comment to paper over it."

The pin's purpose was to guard against 4A over-suppressing the unbounded-elemOffset case, but 4A is already suppressing it. Adding the want comment would paper over this issue rather than expose it.

## Files Affected

- `testdata/corpus/knownfp/knownfp.go`: **No changes committed** (fixture was tested then reverted)

## Recommendation

This task is BLOCKED pending investigation/fix of task 4A's widening-convert range model. The over-suppression needs to be addressed before this positive-polarity pin can be meaningfully added. The fact that an unbounded `int` parameter flowing into `elemOffset` produces no overflow finding is the precise bug this pin was designed to catch.

---

**Next Steps:**
1. Investigate task 4A's widening-convert range model implementation
2. Determine why unbounded parameters trigger no overflow obligation
3. Fix the over-suppression

---

## Corrected implementation (post-investigation)

The BLOCKED status above was itself the mis-diagnosis. A follow-up
investigation (`.superpowers/sdd/task-3-investigation.md`) refuted the
"4A over-suppression" theory and established the real mechanism:
**requires-lifting** (`shared::propagate_requires`, invoked from
`BoundsChecker::infer_requires`, `bounds.rs:490-501`). The plan's
`UnboundedElemOffset` fixture is a bare-parameter pass-through, so its
call-site overflow obligation against `elemOffset` is expressible purely
over its own params and gets lifted onto its own inferred `requires`
instead of surviving as a live obligation — this is sound, documented
behavior, reproducible at every commit before and after task 4A (task
4A never even applies here: there is no `Op::Convert` in the fixture's
body). Direct evidence (temporary probe test): `UnboundedElemOffset`'s
inferred summary carries exactly one `overflow`-tagged clause,
`¬(p1<0)`, over its own `p1` (= `n`).

The controller accepted the H-lift verdict and asked me to implement the
corrected Task 3 as a documented plan deviation: keep the plan's original
fixture as a **silent pin** (documenting exactly why it stays silent),
and add a **second, positive-polarity fixture** whose unbounded value is
NOT expressible over its enclosing function's parameters (so it cannot
be lifted), which is the shape that actually exercises "4A didn't
discharge a real overflow" per spec §4 item 2.

### Fixtures as landed

Appended immediately after `BranchElemOffset` in
`testdata/corpus/knownfp/knownfp.go`:

```go
// task-3 investigation (2026-07-22): PLAN DEVIATION, silent pin — the
// wave-2 plan predicted this exact shape would produce a call-site
// "overflow" finding (elemOffset's summary has an overflow requirement
// on unconstrained `n`). It does not, and it never did, at any commit
// before or after task 4A: `n` here is a BARE PARAMETER of
// UnboundedElemOffset, so the call-site obligation against elemOffset's
// `n`-requirement is expressible purely over UnboundedElemOffset's own
// params (`params_only`) and Sat (n truly unconstrained) —
// BoundsChecker's requires-lifting (`shared::propagate_requires`,
// invoked from `infer_requires`, bounds.rs:490-501) lifts it onto
// UnboundedElemOffset's OWN inferred requires instead of leaving it as a
// live call-site obligation. Verified directly (temporary probe test):
// UnboundedElemOffset's inferred summary carries exactly one
// overflow-tagged clause, `¬(p1<0)`, over its own p1 (=n). No corpus
// function calls UnboundedElemOffset, so that lifted requirement is
// never discharged anywhere — silent everywhere, by SOUND design. This
// is NOT a 4A discharge: there is no `Op::Convert` anywhere in this
// function's body (n is already `int`; the only Convert in the family
// is `uintptr(n)` inside elemOffset's OWN body, on elemOffset's OWN
// param), so 4A's widening-Convert range model never touches this
// fixture at all. See .superpowers/sdd/task-3-investigation.md for the
// full evidence (mechanism citations, RED/GREEN probe outputs).
//
// Deliberately kept without a want-pin comment on the call below: under
// the suite's set-equality check, any finding ever arriving at that call
// trips the suite immediately. If that happens, requires-lifting's
// boundary moved — investigate before pinning the new behavior; do not
// just add a pin for it.
func UnboundedElemOffset(base uintptr, n int) uintptr {
	return elemOffset(base, 16, n)
}

// task-3 investigation (2026-07-22): the corrected, POSITIVE-polarity
// counterpart to UnboundedElemOffset above (wave-2 spec §4 item 2 — "this
// pin proves 4A didn't discharge real overflows"). `unboundedN` is a
// package-level var: a memory load, not a parameter, so the value
// reaching elemOffset's `n` is NOT expressible over GlobalElemOffset's
// own params (`params_only` fails) — the call-site "overflow" obligation
// against elemOffset's requirement CANNOT be lifted, and must survive to
// the findings pass and be REPORTED. There is no guard on `unboundedN`
// anywhere, so the violation is genuinely reachable. This is the fixture
// that actually exercises 4A's widening-`Op::Convert` range model on a
// case it must NOT discharge: there is no Convert here either (same
// reasoning as above), so 4A has nothing to assert and nothing to
// suppress — the obligation must fire on its own.
var unboundedN int

func GlobalElemOffset(base uintptr) uintptr {
	return elemOffset(base, 16, unboundedN) // want: overflow
}
```

`GlobalElemOffset`'s package-level-var shape (the plan's preferred shape)
worked cleanly and yielded exactly one extra finding — no fallback to a
2c-style struct/slice load was needed.

### TDD discipline — exact outputs

**RED** (both fixtures present, Pin B's call *without* its want comment):

```
$ mise x -- cargo test -p goverify-checkers --test knownfp_corpus
...
thread 'knownfp_corpus_findings_match_want_comments' panicked ...
assertion `left == right` failed: known-FP pins vs current analyzer behavior
  left: {("knownfp.go", 89, "bounds"), ("knownfp.go", 211, "bounds"), ("knownfp.go", 389, "bounds"), ("knownfp.go", 474, "overflow"), ("knownfp.go", 494, "bounds"), ("knownfp.go", 532, "nil-deref")}
 right: {("knownfp.go", 89, "bounds"), ("knownfp.go", 211, "bounds"), ("knownfp.go", 389, "bounds"), ("knownfp.go", 494, "bounds"), ("knownfp.go", 532, "nil-deref")}
test result: FAILED. 0 passed; 1 failed
```

Exactly one extra entry, `("knownfp.go", 474, "overflow")` — line 474 is
`GlobalElemOffset`'s call (Pin B) — with no entry at all for Pin A's
call line (455, `UnboundedElemOffset`). Matches the expected RED exactly:
no STOP condition triggered.

(One intermediate false-RED along the way, fixed before this run: an
early draft of Pin A's comment prose contained the literal substring
`` `// want:` `` inside explanatory text — `wants_in`'s parser
(`goverify-ir/src/testutil.rs:66`, `line.split("// want:")`) matches that
substring anywhere on a line, not just in a real pin position, so it was
mis-parsed as a spurious want-pin with a garbage tag. Reworded to avoid
the literal `// want:` substring outside of real pins.)

**GREEN** (Pin B's call annotated `// want: overflow`):

```
$ mise x -- cargo test -p goverify-checkers --test knownfp_corpus
running 1 test
test knownfp_corpus_findings_match_want_comments ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

Re-ran once more after commit against the committed tree — still green.

### Files changed

- `testdata/corpus/knownfp/knownfp.go` — the only file staged/committed
  (+51 lines: both fixtures + comments). `git diff --stat` confirms no
  other file touched by this change.
- `.superpowers/sdd/task-3-investigation.md` — investigation evidence
  (written in the prior phase, referenced by comments here).
- `.superpowers/sdd/task-3-report.md` — this report (append-only).

`crates/goverify-checkers/tests/knownfp_corpus.rs` was used for a
temporary probe test during the *investigation* phase and was fully
reverted (`git checkout --`) before this implementation phase began; it
carries no diff in the final commit.

### Commit

```
git add testdata/corpus/knownfp/knownfp.go
git commit --no-gpg-sign -m "wave2: elemOffset positive-polarity pin (non-liftable shape) + lifted-shape silent pin — plan shape corrected, see task-3 investigation"
```

Committed as `9a8d40e` on `followups/wave2`, 1 file changed (51
insertions), staged and committed exactly as instructed — no other files
staged.

### Self-review

- Both fixtures use tabs for Go indentation (verified with `cat -t`),
  consistent with the rest of the corpus file.
- Pin A's silent status is documented as fragile-by-design and
  explicitly flagged for re-investigation (not just re-pinning) if it
  ever starts producing a finding — this satisfies the "legible
  deviation" requirement rather than silently matching the plan's
  original prediction.
- Pin B avoids side obligations: `GlobalElemOffset` takes only `base`,
  so there is no other bounds/nil-deref site in its body — confirmed by
  the RED output showing exactly one extra entry, not more.
- No lint/build run beyond the corpus test and a manual `gofmt -l` (no
  output = clean) was performed for this change; the change is
  Go-fixture-only (`testdata/corpus/knownfp/knownfp.go`), not
  Rust/Go-toolchain source, so `mise run lint`/`test` (which already
  includes this corpus suite via the checkers test) is the relevant
  gate and was run directly above.
- Working tree left with only the two intentional report-file edits
  (`task-3-investigation.md`, this file) dirty beyond the committed
  fixture change; confirmed via `git status --short`.
4. Return to Task 3 to add the working pin
