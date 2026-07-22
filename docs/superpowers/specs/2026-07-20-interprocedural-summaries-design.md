# Interprocedural summaries design

2026-07-20. Follow-up to the FP/encoding fix-wave
(`2026-07-20-fp-encoding-fixwave-design.md`) and the phase-4 bbolt
shakeout triage (`docs/shakeout-phase4-bbolt.md`). Chosen over roadmap
phase 5 (caching) because one mechanism family — callee postconditions
plus their consumption at call sites — addresses three open items at
once: the 102-finding FP/requires-lifting bucket, a large share of the
83-class gate-1 residual (call/closure-boundary shapes), and the
restoration of the FillPercent real-bug detection lost to the fix-wave
(`cmd/bbolt/main.go:1191`, the wave's recorded principal cost).

## 1. Goal and scope

Three cooperating mechanisms, all riding the existing summary/SCC
infrastructure:

| # | Mechanism | What it buys |
|---|-----------|--------------|
| 1 | Ensures inference (`NilChecker::infer_ensures`) + encoder-side substitution at static call sites | Kills the dominant `err==nil ⇒ result!=nil` FP classes (C009c canonical shape; the bulk of the 78 requires-lifting classes) |
| 2 | Manifest obligations on summary-constrained call-result subjects | Restores FillPercent detection at the true first-failure site (`main.go:1191`); generalizes to every ignored-error deref of an in-corpus callee with an inferred result correlation |
| 3 | ChangeType/Assign deref-subject canonicalization before `params_only` | Closes the documented FN where a copy's checked-deref assumption silently discharges an unrelated parameter's requires (threat-model §"Assign/ChangeType copies", knownfp `NamedPtr` block) |

**Out of scope** (explicit non-goals, each its own future item):

- Closure / dynamic-dispatch lifting — postconditions through
  function-valued fields, invoke edges, and closure captures (the cobra
  `ExactArgs` classes C027/C216/C257/C402; captures C210/C233). Needs
  an ensures-intersection semantics across possible callees; deferred
  to a follow-up wave.
- Bounds/range ensures — numeric return-range and `len` postconditions
  (C101, C181, C229 and kin). The plumbing built here is
  checker-generic; only `NilChecker` infers ensures this cycle.
- Summary persistence — the `goverify-cache` `Store` stays query-only;
  the `"summary"` layer remains roadmap phase 5.
- Caller-fact-into-callee lifting beyond what call-site requires
  checking already provides (e.g. "all callers establish X ⇒ assume X
  in callee" whole-program reasoning).
- The annotation language (phase 6) — no manual contract surface is
  added; `NEVER_NIL_RESULT` is retained only for extern functions.

## 2. Approach and alternatives

Substitution happens in the **encoder** (`goverify-analysis::encode`),
not per-checker. `op_def`'s `Op::Call` arm stops being pure havoc: a
static callee's proven ensures are bound to the call's argument and
result terms and asserted into the caller's gated-SSA encoding, exactly
where `NEVER_NIL_RESULT` asserts `¬is_nil(dst)` today. Because the
facts live in the encoding, every downstream query composes with them
for free — requires discharge, fix-2b dominance assumptions, and the
new call-result obligations — with a single integration point.

Alternatives rejected:

- **Checker-side substitution** (a dual of `propagate_requires` that
  conjoins instantiated ensures into each discharge query): keeps the
  encoder summary-free, but every checker re-implements the threading,
  and a fact needed transitively (call → guard → later deref) does not
  compose — it would re-derive path reasoning the gated-SSA encoding
  already performs, and under-deliver on exactly the C009c
  guard-threading shape.
- **Manual contract table** (grow `NEVER_NIL_RESULT` into hand-written
  per-function ensures, no inference): cannot scale to in-corpus
  callees (all 78 requires-lifting classes are in-corpus), does not
  restore FillPercent, and duplicates the phase-6 annotation surface.

## 3. Mechanism 1 — ensures inference and substitution

### 3.1 Inference (`infer_ensures`)

New `Checker` trait method
(`crates/goverify-analysis/src/checker.rs`):

```rust
fn infer_ensures(&self, p: &Program, f: FuncId,
                 summary_of: &dyn Fn(FuncId) -> Summary,
                 discharge: &mut dyn FnMut(Query) -> SatResult) -> Vec<Clause>
```

Default implementation returns empty; only `NilChecker` implements it
this cycle. `analyze_function` (engine.rs) calls it alongside
`infer_requires` every fixpoint round and writes the result into the
currently-dead `Summary.ensures` field (summary.rs:48-54).

Inference is **candidate-and-check** over a fixed template set,
instantiated per result index from the function's signature types:

1. `¬is_nil(r_i)` — for each pointer-sorted result `i`.
2. `is_nil(r_e) ⇒ ¬is_nil(r_i)` — for each pointer result `i` paired
   with each `error`-interface-typed result `e` (the `(T, error)`
   correlation; `error`-typed results are identified from `.gvir` type
   info).

Validation reuses the function's own whole-function encoding: for each
candidate, query whether any return site can violate it — return site
reached (under its path guard) ∧ the candidate, with each `r_i` bound
to that site's returned term, is false. **Unsat across all return
sites ⇒ the clause is a proven postcondition and enters the summary.
Sat or Unknown ⇒ dropped.** Only proven clauses are ever asserted in
callers, so an ensures is always an over-approximation of callee
behavior — the sound direction for facts consumed as assumptions.
Queries run on the `Infer`-role backend (two-tier timeouts apply) and
hit the existing query cache.

**Go-idiom rule for the correlation template** (planning amendment,
2026-07-20): the dominant real shape returns *global sentinel errors*
(`return nil, ErrDatabaseNotOpen`) whose loads are havoc'd, so a
strict per-site SMT proof of `is_nil(r_e) ⇒ ¬is_nil(r_i)` is
unobtainable exactly where the mechanism matters. Validation of the
correlation template is therefore per return site: when the error
component is the **literal nil constant**, the paired result must be
SMT-proven non-nil (reach ∧ is_nil Unsat); any other error expression
is treated as a non-nil error (the universal Go idiom). This is a
deliberate under-approximation, documented in `docs/threat-model.md`
alongside the uintptr one: a callee returning a nil-valued error
*variable* with a nil result yields a wrong caller-side discharge.
The unconditional `¬is_nil(r_i)` template stays strictly proven.

**Interface modeling prerequisite** (planning amendment, 2026-07-20):
`sort_of` currently gives interface types no sort, so `error`-typed
values havoc entirely — the callee's `err != nil` guards, the
caller's discharge guard, and the correlation template are all
inexpressible without it. This cycle extends `sort_of` with
`TypeKind::Interface → Ptr` (nil-ness is the only modeled
observation; `Eq`/`Neq` already encode for every sort). Corpus and
check goldens are expected to churn and are re-baselined under
review.

Fixpoint interaction: `Summary` equality drives SCC convergence and
already compares the full struct, so ensures participate with no engine
change; if `PartialEq` derivation turns out to skip `ensures`, fixing
that is part of this work. Recursive-SCC widening to
`Summary::havoc()` drops ensures — sound, since empty ensures is the
weakest postcondition. `read_slot` resolution (in-flight SCC-local →
finalized → havoc) is unchanged.

### 3.2 Substitution (`encode.rs`)

The encoder gains a summary resolver handle with `read_slot` semantics
(during the fixpoint it sees in-flight summaries; during the findings
pass, finalized ones). In `op_def`, for
`Op::Call { callee: Static(fid), dst, args }` where the callee summary
has `Inferred` provenance and non-empty ensures:

- Bind `p<i>` → the caller's argument terms, `r<i>` → the destination's
  component terms. Multi-value results bind through the same tuple
  accessor terms the existing `Extract` handling reads. This is
  `instantiate_ensures`, the result-var dual of `instantiate_requires`
  (summary.rs:92-123), lifting the current "a `Result` var makes a
  clause unbindable" restriction (summary.rs:114-118) for the ensures
  path only.
- Assert each successfully bound clause **gated on the call's block
  guard** (a postcondition holds only on executions that performed the
  call) — the same shape as today's `NEVER_NIL_RESULT` assertion
  (encode.rs:894-904), which inferred ensures subsume for in-corpus
  callees. The extern table stays for out-of-corpus constructors
  (`flag.NewFlagSet`).
- A clause whose binding fails (an argument term the encoder cannot
  produce) is skipped silently — weaker, never wrong.
- Invoke/dynamic call edges get no ensures this cycle (multiple
  possible callees would need intersection semantics; see non-goals).

Worked canonical case (C009c / knownfp.go:301-341): `beginTx` earns
`is_nil(r_1) ⇒ ¬is_nil(r_0)`; at `tx, err := dst.Begin(true)` the bound
clause is asserted; the caller's `err != nil` early-return guard is
already in the gated-SSA encoding; the `tx.Commit()` receiver
obligation goes Unsat. No checker changed.

## 4. Mechanism 2 — obligations on call-result subjects

Today `nil.rs`'s `obligations()` raises a manifest nil-deref only when
the subject `is_const_nil`, has no free variables, or is params-only
(nil.rs:149-157). A havoc'd call result never qualifies — which is why
`main.go:1191` (`b, _ := tx.CreateBucketIfNotExists(...);
b.FillPercent = ...`) was never flagged in any run, baseline included.

Extension: a deref subject that resolves — through same-function
Assign/Extract chains — to **result `i` of a static call whose callee
summary has `Inferred` provenance and contains an ensures clause
mentioning `r_i`** also qualifies for a manifest obligation.

The gating rule is the load-bearing decision. Qualifying *all* call
results would flood findings wherever inference failed (widened SCCs,
template misses, Unknowns): those results stay unconstrained, so every
unconditional deref would go Sat. Requiring a clause that constrains
the specific result index means **every new finding is backed by a
proven callee correlation**:

- FillPercent: `CreateBucketIfNotExists` earns
  `is_nil(r_err) ⇒ ¬is_nil(r_0)`; the caller discards `err`, nil stays
  feasible, the obligation fires at the true first-failure site.
- Guarded caller: the same clause plus the caller's `err != nil` guard
  discharges the obligation — no FP.
- Inference miss: no qualifying clause, no obligation — the FN remains,
  as it does today. Incremental coverage, not a soundness regression.

Finding messages name the correlation (e.g. "result of
CreateBucketIfNotExists may be nil when its error result is non-nil;
error is discarded here") so triage can distinguish these from classic
manifest derefs.

## 5. Mechanism 3 — ChangeType/Assign subject canonicalization

The queued fix for the threat-model §"Assign/ChangeType copies can
silently discharge an unrelated deref's requires" FN
(threat-model.md:85-100). Before `params_only` (shared.rs:18-23)
decides a deref subject's expressibility, the subject term is
canonicalized through same-function `Op::Assign` chains (`ChangeType`
lowers to `Assign`, lower.rs:292-297) — a value-numbering resolve of
`v_q → p0`, depth-capped at 64 like `AddrKey` (encode.rs:569-582)
against crafted chains.

Effect: a deref of a `ChangeType`/`Assign` copy of a parameter now
emits the `¬is_nil(p0)` requires it always should have, so the copy's
checked-deref assumption (shared.rs:53-82) can no longer transfer
non-nilness to the parameter via the `Assign` equality (encode.rs:759)
without the corresponding requires surfacing at call sites. Callers
passing nil are flagged again. `Op::Convert` stays deliberately opaque
(uintptr-provenance semantics untouched). The knownfp `NamedPtr` block
flips from KNOWN-FN to FIXED, and the threat-model paragraph is
updated to describe the fix.

Ordering note: mechanism 2's subject resolution (Assign/Extract chains)
and this canonicalization share the same resolver; it is written once.

## 6. Determinism, soundness, degradation, performance

- **Determinism.** Ensures are a pure function of deterministic
  encodings and callee summaries; clause order is result-index ×
  template-order (no map iteration); the encoder asserts clauses in
  summary `Vec` order; the findings pass runs on finalized summaries
  only. The corpus determinism suite is the enforcement, as always.
- **Soundness direction.** An asserted ensures is only ever a proven
  (Unsat-negation) over-approximation; dropping a clause — on Sat,
  Unknown, binding failure, or widening — is always safe (weaker
  summary). Mechanism 2 can only *add* findings backed by proven
  correlations; discharges it enables are solver-verified against the
  same encoding.
- **Degradation.** House rule holds: any failure in inference,
  binding, or type identification degrades to "no clause" with a
  diagnostic where applicable — never a panic. Per-function
  `catch_unwind` in the findings pass stays.
- **Query cache.** Ensures change canonical SMT text, so cache keys
  miss once and re-fill — expected. Check goldens' `with:` witness
  lines will churn; refreshing them is in scope.
- **Performance.** At most ~2 extra `Infer`-tier queries per pointer
  result per function per fixpoint round, plus marginally larger caller
  encodings. Corpus-suite runtime and bbolt shakeout wall-clock are
  measured and reported at the gate. Watch item: the corpus CI budget
  (transitive-closure blowup history, ~30-minute ceiling).

## 7. Testing

- **Unit:** `instantiate_ensures` — `r<i>` binding, multi-return tuple
  components, `p<i>`/`r<i>` mixed clauses, binding-failure fallback;
  template validation against hand-built encodings (all-paths-non-nil,
  err-correlated, genuinely-nilable → no clause); canonicalization
  resolver including the depth cap.
- **Corpus (blocking):**
  - Flip `testdata/corpus/knownfp/knownfp.go`'s postcondition block
    (`beginTx`/`commitTx`, `// want: nil-deref` at line 335) to FIXED.
  - New red case — FillPercent shape: `b, _ := f(); b.X` where `f`
    has the err-correlation, `// want: nil-deref`.
  - New green siblings: same callee with `err != nil` guard → no
    finding; unconditional-non-nil constructor callee → no finding on
    unguarded deref.
  - Canonicalization: `NamedPtr` KNOWN-FN block flips to FIXED — a
    caller passing nil through the ChangeType copy is flagged.
  - Determinism suite green; `check` goldens refreshed.
- **Props/fuzz:** `encode_props` domain extended so summary-bearing
  encodings are exercised; no new byte-parsing surface, `fuzz/` targets
  unchanged.

## 8. Acceptance gate

Blocking tier: corpus (including the flips and new cases above) +
determinism + `lint`/`secrets`/`audit` as usual.

Then a gated bbolt re-run (`mise run shakeout`) against the fix-wave
addendum baseline (509 findings), results recorded as a new addendum in
`docs/shakeout-phase4-bbolt.md`:

1. **Hard gate:** `cmd/bbolt/main.go:1191` is detected (the FillPercent
   restoration — the fix-wave's recorded principal cost repaid).
2. **Requires-lifting bucket** (102 findings at baseline) materially
   reduced, with per-class accounting against the 78 distilled
   PHASE5-NOTE payloads (which classes died, which survived and why —
   expected survivors: the closure/cobra and bounds-flavored classes
   excluded by the non-goals).
3. **New findings:** every new finding class triaged with an FP-rate
   report. Report-only — the plan owner accepts the wave or iterates;
   no numeric target is pre-committed.
4. Corpus runtime and shakeout wall-clock deltas reported (see §6).

## 9. Follow-ups seeded, not taken

- Closure/dynamic-dispatch ensures (cobra classes) — next wave
  candidate once static-call ensures are proven on bbolt.
- Bounds/range ensures on the same plumbing.
- Summary persistence layer on `Store` (roadmap phase 5), which this
  work makes more valuable (summaries now carry ensures worth caching).
- Shared cycle-guarded Named resolver in encode.rs (pre-existing
  fuzz-reachable abort class, filed during the fix-wave) — unrelated to
  this mechanism family but adjacent in file; not bundled here.
