# Task 3 investigation: UnboundedElemOffset silence — H-lift vs 4A over-suppression

Branch `followups/wave2`. Investigating why the planned regression pin

```go
func UnboundedElemOffset(base uintptr, n int) uintptr {
	return elemOffset(base, 16, n)
}
```

(added after `BranchElemOffset`, ~line 424 of
`testdata/corpus/knownfp/knownfp.go`) produces **no** finding, contrary to
the plan's prediction of an `overflow` finding at the `elemOffset` call
site.

**Verdict: H-lift confirmed.** The silence is sound, documented
requires-lifting behavior (the same self-consistency mechanism that keeps
`elemOffset` itself silent about its own unconstrained `n`), not a 4A
(widening-`Convert` range model) over-suppression bug. 4A's code path is
never touched by this fixture at all — there is no `Convert` instruction
lying between `UnboundedElemOffset`'s `n` and the call.

---

## 1. The lifting rule, with file:line citations

### 1.1 `Checker::infer_requires` contract

`crates/goverify-analysis/src/checker.rs:56-71` — the trait doc for
`infer_requires`: "Derive `f`'s own preconditions from its body... a
checker must only emit a requires-clause when the corresponding violation
path is confirmed `Sat`... `summary_of` lets a checker consult a callee's
already-inferred summary while deriving `f`'s own requires (requires
propagation through call chains via the existing SCC fixpoint)."

### 1.2 The generic lift: `shared::propagate_requires`

`crates/goverify-checkers/src/shared.rs:84-140` (`propagate_requires`,
doc at 84-88): for every static call site in `func`, instantiate each of
the callee's requires-clauses (tagged `tag`) with the call's actual
argument terms (`instantiate_requires`). The clause is lifted into
`func`'s own inferred requires **iff**:

- `params_only(&bound)` holds (`shared.rs:18-23`): every free variable of
  the instantiated bound is one of `func`'s own `p<i>` param names — i.e.
  the obligation, after substitution, is expressible purely over the
  *caller's* parameters, not over some non-parameter (memory load, global,
  etc.) value.
- `discharge(&enc.reach_query(bi, extra))` (extra = call-site guards +
  the instantiated violation) returns `SatResult::Sat` — the violation
  must be a live, reachable one, never `Unknown`/`Unsat`.

Both gates satisfied → `push_clause` (dedup) adds the bound as a new
requires-clause on `func`'s own summary (`shared.rs:123-137`).

`BoundsChecker::infer_requires` calls this generic helper for every tag —
`crates/goverify-checkers/src/bounds.rs:490-501` (`for tag in ["bounds",
"div-zero", "overflow"] { propagate_requires(...) }`), immediately after
computing `func`'s **own local** sites (`bounds.rs:464-489`, same
`params_only` + Sat-gating discipline for a site that originates
in `func`'s own body rather than at a call).

### 1.3 The other half: `own_preconditions` used as an assumption in `obligations`

`crates/goverify-checkers/src/shared.rs:25-33` (`own_preconditions`):
turns a function's already-inferred `Summary.requires` into query
conjuncts.

`crates/goverify-checkers/src/bounds.rs:505-537` (`obligations`): `let
pre = own_preconditions(&summary_of(f));` (line 517) — the function's
**own final summary's requires** (computed by `infer_requires` during the
earlier fixpoint phase) is asserted as an *assumed-true precondition*
both for `f`'s own local sites (line 529: `extra = pre.clone(); extra.push(site.violation)`)
and for `f`'s call-site obligations against its callees
(`shared::call_site_obligations`, `shared.rs:208-252`, called from
`bounds.rs:539-548` with `pre` passed straight through, used at
`shared.rs:235-237`: `extra = pre.to_vec(); extra.extend(assume(..)); extra.push(v);`).

**The precise mechanism**: if a call-site obligation's violation is
*the same fact* that `infer_requires` already lifted into `f`'s own
`requires` (because it was `params_only` and `Sat` at that same call
site), then when `obligations()` runs, `pre` already contains the
negation of that exact violation. The obligation's query becomes
`pre ∧ … ∧ violation`, which is `violation's-negation ∧ violation` —
an internal contradiction, discharged `Unsat` by the solver — so the
obligation, though *raised* (it's unconditionally pushed to `out` in
`call_site_obligations`, `shared.rs:238-247`, with no `params_only` gate
of its own), never becomes a `Finding` in the engine's sequential
findings pass (only `SatResult::Sat` promotes an `Obligation` to a
`Finding` — `crates/goverify-analysis/src/engine.rs:280-311`, `if
outcome.result == SatResult::Sat`).

### 1.4 Engine sequencing that makes this well-defined

`crates/goverify-analysis/src/engine.rs:150-205`: the wave-parallel SCC
fixpoint computes every function's **final** `Summary` (including
lifted requires) first. Only after every wave has finished
(`engine.rs:234-239`, "Findings pass... Every summary is final at this
point") does the single-threaded findings pass run `obligations()` with
`summary_of` reading the finalized `summaries` map (`engine.rs:244`).
So by the time `BoundsChecker::obligations(UnboundedElemOffset, ...)`
runs, `summary_of(UnboundedElemOffset)` already reflects whatever
`infer_requires` lifted for it earlier in the same `analyze_full` call —
this is not a race, it's the documented two-phase design.

### 1.5 Why `elemOffset` itself is silent about its own `n`

`elemOffset`'s body is `base + uintptr(n)*elemSize`. The `Convert(n int
→ uintptr)` is a same-width (64/64), sign-changing (`int` signed →
`uintptr` unsigned) convert → an `"overflow"` `Site` per
`bounds.rs:302-334` (`convert_sites`), violation `n < 0` (case `(true,
false)`, `wd == ws` branch, `bounds.rs:275-289`, `neg` only). This
violation is `params_only` (free var = `elemOffset`'s own `p2`) and
`Sat` (n unconstrained) → `elemOffset`'s own `infer_requires`
(`bounds.rs:470-489`, the *local*-sites loop, not `propagate_requires`)
lifts `n ≥ 0` (tag `overflow`) onto `elemOffset`'s own summary. Then
when `elemOffset`'s **own** `obligations()` runs, `pre =
own_preconditions(&summary_of(elemOffset))` already contains `n ≥ 0`,
so the *local* site obligation at line 405 (`site.violation = n < 0`)
combines with `pre` to `n≥0 ∧ n<0` = Unsat → silent. This is the exact
same self-consistency pattern documented at `bounds.rs:364-375` (comment
on `is_ground_or_param`/`expressible`, citing nil.rs's
`wrapper_does_not_self_report`).

---

## 2. Empirical confirmation

### 2a. Reproduced the observation

Re-added (temporarily) the exact fixture verbatim, no `// want:` comment,
directly after `BranchElemOffset`:

```go
func UnboundedElemOffset(base uintptr, n int) uintptr {
	return elemOffset(base, 16, n)
}
```

Ran `mise x -- cargo test -p goverify-checkers --test knownfp_corpus`
with **only** this addition present: the suite **passed** (no set
mismatch) — confirms the reported observation: no finding fires for the
bare-parameter call site.

### 2b. Direct observation of the inferred requires clause

Added a temporary `#[test]` to
`crates/goverify-checkers/tests/knownfp_corpus.rs` that runs the same
`analyze_full` as the corpus suite, looks up
`example.com/knownfp.UnboundedElemOffset`'s `FuncId`, and dumps
`a.summaries[fid].requires`:

```rust
#[test]
fn probe_unbounded_elem_offset_inferred_requires() {
    let p = goverify_ir::testutil::load_corpus("knownfp");
    let cfg = EngineConfig { opts: Options::default(), cache_dir: None, emit_smt: None };
    let checkers: Vec<&dyn goverify_analysis::Checker> = vec![&NilChecker, &BoundsChecker];
    let a = analyze_full(&p, &cfg, &checkers, &|_role| Box::new(Z3Native::new(limits())));
    let fid = p.lookup_func("example.com/knownfp.UnboundedElemOffset").unwrap();
    let summary = a.summaries.get(&fid).unwrap();
    eprintln!("UnboundedElemOffset requires: {:#?}", summary.requires);
    ...
}
```

Ran with `--nocapture`. Output (trimmed to the interesting clause):

```
UnboundedElemOffset requires: [
    Clause {
        tag: "overflow",
        formula: Formula {
            term: Term {
                node: Not(
                    Term { node: BvCmp { op: Slt,
                        lhs: Term { node: Var("p1"), sort: BitVec(64) },
                        rhs: Term { node: BvLit { width: 64, value: 0 }, sort: BitVec(64) } },
                        sort: Bool } ),
                sort: Bool } } } ]
free vars: {"p1": BitVec(64)}
test probe_unbounded_elem_offset_inferred_requires ... ok
```

I.e. `UnboundedElemOffset`'s own inferred summary carries exactly one
`overflow`-tagged requires clause, `¬(p1 < 0)` = `p1 ≥ 0`, over `p1` —
`UnboundedElemOffset`'s own second parameter, `n` (`base` is `p0`).
**This is the lift, directly observed**: it is not `elemOffset`'s
clause reused verbatim, it is a *new* clause on `UnboundedElemOffset`'s
own summary, instantiated over `UnboundedElemOffset`'s own param name.

### 2c. Differential probe — break `params_only`, watch the finding reappear

Added, alongside `UnboundedElemOffset`:

```go
func UnboundedElemOffset2(base uintptr, xs []int) uintptr {
	return elemOffset(base, 16, xs[0])
}
```

Here the argument to `elemOffset`'s `n` is `xs[0]`, a memory load — not
expressible over `UnboundedElemOffset2`'s own params (`xs` itself is a
param, but the *loaded element* is not one of the `p<i>` interface
vars), so `propagate_requires`'s `params_only` gate must fail and no
lift can occur; the call-site obligation must survive to the findings
pass undischarged-by-`pre`.

Ran the suite with **both** `UnboundedElemOffset` (bare param, no want)
and `UnboundedElemOffset2` (memory load, no want) present. Result: the
suite **failed** with exactly the predicted extra finding:

```
left:  {..., ("knownfp.go", 436, "bounds"), ("knownfp.go", 436, "overflow"), ...}
right: {..., (no line-436 entries), ...}
```

Line 436 is `return elemOffset(base, 16, xs[0])` inside
`UnboundedElemOffset2`. Two new findings appear there:
- `"bounds"` — expected and unrelated to this investigation: `xs[0]` is
  itself an unguarded index into a possibly-empty slice.
- `"overflow"` — **this is the call-site obligation against
  `elemOffset`'s `n ≥ 0` requirement, now firing**, exactly the same
  obligation family that stayed silent for the bare-parameter
  `UnboundedElemOffset` at line 429 (which produced **no** extra entry
  in `left` in this same run).

This is the decisive differential: identical callee (`elemOffset`),
identical requirement, identical checker and code path — the only
difference is whether the caller's argument is expressible over the
caller's own parameters. Bare param → lifted → silent. Memory load →
not liftable → obligation survives → `Sat` → `Finding`.

### 2d. Does 4A's widening-`Convert` path touch this fixture at all?

No. 4A (per the `BranchElemOffset` comment at
`testdata/corpus/knownfp/knownfp.go:408-421` and the mechanism in
`bounds.rs:302-334`/`260-295`, `convert_sites`/`convert_violation`) only
asserts a range fact on the **destination of a `Convert` instruction**
(specifically: `wd < ws` narrowing, or `wd == ws && sd != ss`
sign-changing-same-width — `bounds.rs:315-317`). `UnboundedElemOffset`'s
body is:

```go
func UnboundedElemOffset(base uintptr, n int) uintptr {
	return elemOffset(base, 16, n)
}
```

There is no `Convert` instruction here at all — `n` (already `int`)
flows straight into the call as `int`, and the only `Convert` in the
whole family (`uintptr(n)`) lives inside `elemOffset`'s own body, on
`elemOffset`'s own parameter (`p2`), not on any value in
`UnboundedElemOffset`. 4A's range-assertion machinery has nothing to
attach to in the caller. This rules out "4A is over-suppressing" as an
explanation for `UnboundedElemOffset`'s silence: the code path 4A
touches is simply never reached by this fixture.

---

## 3. Restoration

```
git checkout -- testdata/corpus/knownfp/knownfp.go
git checkout -- crates/goverify-checkers/tests/knownfp_corpus.rs
git status --short
```

confirmed clean on both files after restore (see final `git status`
output in the session — only pre-existing `.superpowers/` report-file
edits remain dirty, no test/fixture files).

---

## 4. Verdict

**H-lift confirmed.** `UnboundedElemOffset`'s silence is exactly the
mechanism the competing hypothesis described: `n` is a bare parameter,
so the call-site obligation against `elemOffset`'s `n ≥ 0`
("overflow") requirement is expressible purely over
`UnboundedElemOffset`'s own `p1`, and is `Sat` at that call site
(`n` truly unconstrained) — so `shared::propagate_requires`
(`crates/goverify-checkers/src/shared.rs:84-140`, invoked from
`BoundsChecker::infer_requires`, `crates/goverify-checkers/src/bounds.rs:490-501`)
lifts it onto `UnboundedElemOffset`'s own summary instead of leaving it
as a live call-site obligation. When the findings pass later runs
`BoundsChecker::obligations` for `UnboundedElemOffset`
(`bounds.rs:505-550`), `own_preconditions(&summary_of(f))`
(`shared.rs:25-33`) reasserts that same lifted `n ≥ 0` fact as `pre`,
which contradicts the call-site violation `n < 0` and discharges
`Unsat` — so the obligation, though raised, never promotes to a
`Finding` (`engine.rs:280-311` gates promotion on `SatResult::Sat`).
This is sound, documented behavior (the same self-consistency pattern
that already keeps `elemOffset` silent about its own body), not a bug
in 4A's widening-`Convert` range model — which, per §2d, this fixture
never even reaches (no `Convert` instruction exists in
`UnboundedElemOffset`'s body).

The plan mis-predicted the fixture's shape (a bare-parameter pass-through
gets lifted and silenced by design), not a 4A defect. If the intent was
to pin a *live* unbounded-`elemOffset` regression, `UnboundedElemOffset2`
(memory-load shape, §2c) is the fixture that actually reproduces a firing
`overflow` finding and would need a `// want: overflow` (plus the
incidental `// want: bounds` for the `xs[0]` index) if adopted.
