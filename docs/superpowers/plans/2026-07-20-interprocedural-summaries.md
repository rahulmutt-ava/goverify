# Interprocedural Summaries Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Infer callee postconditions (`ensures`), assert them at static call
sites in the SMT encoder, raise nil-deref obligations on summary-constrained
call results, and canonicalize deref subjects through Assign/ChangeType chains
— killing the dominant requires-lifting FP classes and restoring the
FillPercent real-bug detection.

**Architecture:** Three mechanisms riding the existing summary/SCC
infrastructure (spec: `docs/superpowers/specs/2026-07-20-interprocedural-summaries-design.md`).
(1) `NilChecker::infer_ensures` candidate-and-check templates written into the
dead `Summary.ensures` field; the encoder (`encode_func_with`) binds `p<i>`/`r<i>`
vars at each static call and asserts the bound clauses gated on the call's
block guard. (2) `nil.rs::obligations` additionally qualifies deref subjects
that resolve to a static-call result whose callee summary constrains that
result index. (3) Deref subjects canonicalize through same-function
`Op::Assign` chains before `params_only` decides expressibility.
Prerequisite: interfaces get Ptr sort so `error` values and `err != nil`
guards encode at all.

**Tech Stack:** Rust workspace (`crates/goverify-*`), Z3 via `goverify-solver`,
corpus modules under `testdata/corpus/` extracted through the Go sidecar.

## Global Constraints

- Run everything through mise: `mise x -- cargo <…>` for direct cargo, `mise run <task>` for named tasks (AGENTS.md).
- Commits are unsigned in this sandbox: `git -c commit.gpgsign=false commit …`.
- Determinism is the root invariant: no map-iteration order reaching output; clause order = instruction order × template order; `BTreeMap`/`BTreeSet` only.
- Errors degrade, never die: any inference/binding/type failure = "no clause"/"no obligation", never a panic. Parsers of untrusted `.gvir` reject, never panic.
- Bug-finder semantics: `Sat` ⇒ finding; `Unsat`/`Unknown` stay silent. Ensures inversely: proof requires `Unsat`; `Sat`/`Unknown` drops the clause.
- Only Go code lives in `extractor/`; everything in this plan is Rust. `.gvir` schema is untouched (no proto change, no `schema_version` bump).
- Test assertions: house style is plain `assert!`/`assert_eq!` with a message (this repo is not testify/Go).

---

### Task 1: Interface values encode as Ptr

`error`-typed values currently havoc (`sort_of` returns `None` for
`TypeKind::Interface`), so `err != nil` guards, the correlation template, and
caller-side discharges are all inexpressible. Model interfaces as Ptr-sorted:
nil-ness is the only observation; `Eq`/`Neq` already work for every sort
(`binop_term`, encode.rs:952-956), and `const_term` already maps
`ConstVal::Nil` to `ptr_nil()` for any Ptr-sorted type (encode.rs:359-363).

**Files:**
- Modify: `crates/goverify-analysis/src/encode.rs:108-120` (`sort_of`)
- Test: `crates/goverify-analysis/src/encode.rs` (tests module at the bottom)

**Interfaces:**
- Consumes: existing `sort_of(types: &TypeTable, t: TypeId) -> Option<Sort>`.
- Produces: `sort_of` returns `Some(ptr_sort())` for `TypeKind::Interface`. Every later task relies on `error`-typed values having Ptr terms.

- [ ] **Step 1: Write the failing test**

In encode.rs's `#[cfg(test)] mod tests`, following the local fixture style
(`gvir::Package` hand-built like the existing tests):

```rust
#[test]
fn interface_values_get_ptr_sort() {
    use goverify_extract::gvir;
    // Type 1 = interface (e.g. error), function takes one interface param.
    let package = gvir::Package {
        import_path: "t".into(),
        types: vec![gvir::Type {
            id: 1,
            repr: "error".into(),
            kind: gvir::TypeKind::Interface as i32,
            ..Default::default()
        }],
        functions: vec![gvir::Function {
            id: "t.F".into(),
            params: vec![gvir::Param {
                id: 1,
                name: "err".into(),
                r#type: 1,
            }],
            blocks: vec![gvir::BasicBlock {
                index: 0,
                instrs: vec![gvir::Instruction {
                    kind: "Return".into(),
                    ..Default::default()
                }],
                succs: vec![],
                preds: vec![],
            }],
            ..Default::default()
        }],
        ..Default::default()
    };
    let p = goverify_ir::Program::from_packages(vec![package]);
    let f = p.lookup_func("t.F").unwrap();
    let enc = encode_func(&p, f).unwrap();
    let func = p.func(f).unwrap();
    let t = enc.value(func.params[0]).expect("interface param must have a term");
    assert_eq!(t.sort(), &ptr_sort(), "interfaces model as Ptr (nil-ness only)");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `mise x -- cargo test -p goverify-analysis interface_values_get_ptr_sort`
Expected: FAIL — `interface param must have a term` (sort_of returns None, value never declared).

- [ ] **Step 3: Implement**

In `sort_of` (encode.rs:112-119), add one arm before the catch-all:

```rust
        TypeKind::Pointer { .. } => Some(ptr_sort()),
        TypeKind::Slice { .. } => Some(seq_datatype().sort()),
        // An interface value is modeled as an opaque Ptr: nil-ness is the
        // only observation the theory makes (`err != nil` guards, the
        // (T, error) ensures correlation). Everything else about it stays
        // unconstrained — over-approximate, never wrong.
        TypeKind::Interface => Some(ptr_sort()),
        _ => None,
```

- [ ] **Step 4: Run the full test suite**

Run: `mise run test`
Expected: the new test passes. Corpus pins may shift (interface values now
modeled): if `knownfp_corpus_findings_match_want_comments`,
`nil_corpus_findings_match_want_comments`, or the check goldens fail, inspect
each delta — a `// want:` pin that disappeared or appeared purely from
interface modeling gets its pin/comment updated with a note citing this task;
golden drift is refreshed with `UPDATE_GOLDENS=1 mise run test` **after**
reviewing the diff. Record what moved in the commit message. If nothing
fails, say so in the commit message ("no corpus impact").

- [ ] **Step 5: Commit**

```bash
git add -A
git -c commit.gpgsign=false commit -m "summaries: model interface values as Ptr (task 1)"
```

---

### Task 2: `instantiate_ensures` — the result-var binding dual

**Files:**
- Modify: `crates/goverify-analysis/src/summary.rs` (extend `bind`, add `instantiate_ensures`)
- Test: same file's tests module

**Interfaces:**
- Consumes: `Summary`, `Clause`, `BoundClause`, `iface_var_name` (summary.rs).
- Produces: `pub fn instantiate_ensures(callee: &Summary, arg_terms: &[Option<Term>], result_terms: &[Option<Term>]) -> Vec<BoundClause>` — binds `p<i>` → `arg_terms[i]` AND `r<i>` → `result_terms[i]`; any unresolvable var ⇒ `bound`/`violation` both `None`. Re-exported from `goverify-analysis`'s lib.rs alongside `instantiate_requires`. Task 4's encoder calls this.

- [ ] **Step 1: Write the failing tests**

Append to summary.rs's tests module:

```rust
    fn nonnil_result_clause(result: u32) -> Clause {
        let v = IfaceVar::Result(result);
        let r = Term::var(&iface_var_name(&v), ptr_sort());
        Clause {
            tag: "nil-deref".into(),
            formula: Formula {
                term: Term::not(ptr_is_nil(r).unwrap()).unwrap(),
            },
        }
    }

    fn callee_with_ensures(ensures: Vec<Clause>) -> Summary {
        Summary {
            ensures,
            ..Summary::default()
        }
    }

    #[test]
    fn ensures_binds_result_terms() {
        let callee = callee_with_ensures(vec![nonnil_result_clause(0)]);
        let dst = Term::var("v7", ptr_sort());
        let bound = instantiate_ensures(&callee, &[], &[Some(dst)]);
        assert_eq!(bound.len(), 1);
        let b = bound[0].bound.as_ref().expect("r0 must bind to the dst term");
        let free: Vec<&String> = b.free_vars().keys().collect();
        assert_eq!(free, vec!["v7"], "bound clause is over the caller's dst");
    }

    #[test]
    fn ensures_missing_result_term_means_unbindable() {
        // Discarded component (`b, _ := f()`): no Extract, no term — the
        // clause must be skipped, never mis-bound.
        let callee = callee_with_ensures(vec![nonnil_result_clause(1)]);
        assert_eq!(
            instantiate_ensures(&callee, &[], &[Some(ptr_nil()), None])[0].bound,
            None,
            "missing result term: cannot evaluate; do not assert"
        );
    }

    #[test]
    fn ensures_mixed_param_and_result_vars_bind_both() {
        // Clause over p0 and r0 (future-proofing: arg-dependent ensures).
        let p0 = Term::var("p0", ptr_sort());
        let r0 = Term::var("r0", ptr_sort());
        let both = Clause {
            tag: "nil-deref".into(),
            formula: Formula {
                term: Term::or(vec![
                    Term::not(ptr_is_nil(p0).unwrap()).unwrap(),
                    Term::not(ptr_is_nil(r0).unwrap()).unwrap(),
                ])
                .unwrap(),
            },
        };
        let callee = callee_with_ensures(vec![both]);
        let out = instantiate_ensures(
            &callee,
            &[Some(Term::var("va", ptr_sort()))],
            &[Some(Term::var("vd", ptr_sort()))],
        );
        let b = out[0].bound.as_ref().expect("both vars bindable");
        let mut free: Vec<&String> = b.free_vars().keys().collect();
        free.sort();
        assert_eq!(free, vec!["va", "vd"]);
    }

    #[test]
    fn requires_binding_still_rejects_result_vars() {
        // Regression guard on the existing behavior: instantiate_requires
        // must keep refusing r<i> vars (they have no meaning pre-call).
        let callee = callee_with(vec![nonnil_result_clause(0)]);
        assert_eq!(
            instantiate_requires(&callee, &[Some(ptr_nil())])[0].violation,
            None
        );
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `mise x -- cargo test -p goverify-analysis instantiate_ensures ensures_`
Expected: FAIL to compile — `instantiate_ensures` not defined.

- [ ] **Step 3: Implement**

In summary.rs, generalize `bind` and add the new entry point. Replace the
existing `bind` (summary.rs:111-123) with:

```rust
fn bind(f: &Formula, arg_terms: &[Option<Term>]) -> Option<(Term, Term)> {
    bind_with(f, arg_terms, &[])
}

/// The general binder: p<i> free vars map to arg_terms[i], r<i> free vars
/// to result_terms[i]. Any var that is neither, or whose slot is
/// missing/None, makes the clause unevaluable (None).
fn bind_with(
    f: &Formula,
    arg_terms: &[Option<Term>],
    result_terms: &[Option<Term>],
) -> Option<(Term, Term)> {
    let mut map = BTreeMap::new();
    for (name, _sort) in f.term.free_vars() {
        let t = if let Some(rest) = name.strip_prefix('p') {
            let idx: u32 = rest.parse().ok()?;
            arg_terms.get(idx as usize)?.clone()?
        } else if let Some(rest) = name.strip_prefix('r') {
            let idx: u32 = rest.parse().ok()?;
            result_terms.get(idx as usize)?.clone()?
        } else {
            return None;
        };
        map.insert(name, t);
    }
    let bound = f.term.substitute(&map).ok()?;
    let violation = Term::not(bound.clone()).ok()?;
    Some((bound, violation))
}

/// A callee ensures-clause instantiated at a call site: p<i> := the
/// caller's arg terms, r<i> := the call's result terms (the dst for a
/// single-value call; the Extract dsts for a tuple call). Same None
/// contract as `instantiate_requires`.
pub fn instantiate_ensures(
    callee: &Summary,
    arg_terms: &[Option<Term>],
    result_terms: &[Option<Term>],
) -> Vec<BoundClause> {
    callee
        .ensures
        .iter()
        .map(|c| match bind_with(&c.formula, arg_terms, result_terms) {
            Some((b, v)) => BoundClause {
                tag: c.tag.clone(),
                bound: Some(b),
                violation: Some(v),
            },
            None => BoundClause {
                tag: c.tag.clone(),
                bound: None,
                violation: None,
            },
        })
        .collect()
}
```

Note `bind` keeps rejecting `r<i>` because it passes an empty `result_terms`
(`.get(idx)?` fails) — `requires_binding_still_rejects_result_vars` pins this.
Export `instantiate_ensures` from `crates/goverify-analysis/src/lib.rs` next to
`instantiate_requires`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `mise x -- cargo test -p goverify-analysis summary`
Expected: PASS (new tests + all existing summary tests).

- [ ] **Step 5: Commit**

```bash
git add -A
git -c commit.gpgsign=false commit -m "summaries: instantiate_ensures binds r<i> result vars (task 2)"
```

---

### Task 3: `Checker::infer_ensures` trait surface + engine collection

**Files:**
- Modify: `crates/goverify-analysis/src/checker.rs` (trait method with default)
- Modify: `crates/goverify-analysis/src/engine.rs:388-435` (`analyze_function`)
- Test: `crates/goverify-analysis/src/engine.rs` tests module

**Interfaces:**
- Consumes: `Checker` trait, `analyze_function`, `Summary`.
- Produces: trait method `fn infer_ensures(&self, p: &Program, f: FuncId, summary_of: &dyn Fn(FuncId) -> Summary, discharge: &mut dyn FnMut(&Query) -> SatResult) -> Vec<Clause>` (default: empty). `analyze_function` unions checker ensures into `Summary.ensures` (name-sorted checker order, same as requires). Task 5 implements it for `NilChecker`; Task 4's encoder reads `Summary.ensures`.

- [ ] **Step 1: Write the failing test**

In engine.rs tests, add a checker that emits one ensures clause and assert it
lands in the summary — and that widening drops it:

```rust
    /// Emits one ensures clause for every function: engine-plumbing probe.
    struct EnsuresChecker;
    impl Checker for EnsuresChecker {
        fn name(&self) -> &'static str {
            "ensures-probe"
        }
        fn infer_requires(
            &self,
            _p: &Program,
            _f: FuncId,
            _summary_of: &dyn Fn(FuncId) -> Summary,
            _discharge: &mut dyn FnMut(&Query) -> SatResult,
        ) -> Vec<crate::summary::Clause> {
            Vec::new()
        }
        fn infer_ensures(
            &self,
            _p: &Program,
            _f: FuncId,
            _summary_of: &dyn Fn(FuncId) -> Summary,
            _discharge: &mut dyn FnMut(&Query) -> SatResult,
        ) -> Vec<crate::summary::Clause> {
            use goverify_solver::{Term, ptr_is_nil, ptr_sort};
            let r0 = Term::var("r0", ptr_sort());
            vec![crate::summary::Clause {
                tag: "nil-deref".into(),
                formula: crate::summary::Formula {
                    term: Term::not(ptr_is_nil(r0).unwrap()).unwrap(),
                },
            }]
        }
        fn obligations(
            &self,
            _p: &Program,
            _f: FuncId,
            _summary_of: &dyn Fn(FuncId) -> Summary,
        ) -> Vec<crate::checker::Obligation> {
            Vec::new()
        }
    }

    #[test]
    fn checker_ensures_land_in_summaries() {
        let p = Program::from_packages(vec![pkg("t", vec![straight("t.F", vec![])])]);
        let checkers: Vec<&dyn Checker> = vec![&EnsuresChecker];
        let a = analyze_full(&p, &EngineConfig::default(), &checkers, &|_role| {
            Box::new(StubSolver)
        });
        let f = p.lookup_func("t.F").unwrap();
        assert_eq!(
            a.summaries[&f].ensures.len(),
            1,
            "infer_ensures output must reach Summary.ensures: {:?}",
            a.summaries[&f]
        );
        assert_eq!(a.summaries[&f].ensures[0].tag, "nil-deref");
    }

    #[test]
    fn widening_drops_ensures() {
        // Recursive SCC + widen_after 0: the widened summary is havoc,
        // whose ensures are empty — an ensures clause must never survive
        // widening (soundness: empty is the weakest postcondition).
        let p = Program::from_packages(vec![pkg(
            "t",
            vec![
                straight("t.Even", vec![call("(*sync.Mutex).Lock"), call("t.Odd")]),
                straight("t.Odd", vec![call("t.Even")]),
            ],
        )]);
        let checkers: Vec<&dyn Checker> = vec![&EnsuresChecker];
        let cfg = EngineConfig {
            opts: Options { widen_after: 0 },
            ..EngineConfig::default()
        };
        let a = analyze_full(&p, &cfg, &checkers, &|_role| Box::new(StubSolver));
        let even = p.lookup_func("t.Even").unwrap();
        assert_eq!(a.summaries[&even].provenance, Provenance::Havoc);
        assert!(
            a.summaries[&even].ensures.is_empty(),
            "widening must drop ensures: {:?}",
            a.summaries[&even]
        );
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `mise x -- cargo test -p goverify-analysis checker_ensures widening_drops`
Expected: FAIL to compile — `infer_ensures` is not a trait member (the probe
checker's impl block has an unknown method).

- [ ] **Step 3: Implement**

checker.rs — add to the `Checker` trait, after `infer_requires`:

```rust
    /// Derive `f`'s own postconditions from its body: Bool clauses over
    /// r<i> (and, when arg-dependent, p<i>) interface vars. Same solver
    /// discipline as `infer_requires`, with the polarity INVERTED: a
    /// clause may only be emitted when its violation is proven
    /// unreachable (`Unsat`) at every return site — `Sat` and `Unknown`
    /// both drop the candidate. Emitted ensures are asserted as facts in
    /// callers (encode_func_with), so an unproven clause here is a wrong
    /// discharge there. Default: no postconditions (sound: weakest).
    fn infer_ensures(
        &self,
        p: &Program,
        f: FuncId,
        summary_of: &dyn Fn(FuncId) -> Summary,
        discharge: &mut dyn FnMut(&Query) -> SatResult,
    ) -> Vec<Clause> {
        let _ = (p, f, summary_of, discharge);
        Vec::new()
    }
```

engine.rs `analyze_function` (lines 410-421) — collect ensures alongside
requires:

```rust
        let mut requires = Vec::new();
        let mut ensures = Vec::new();
        for checker in checkers {
            let mut discharge =
                |q: &Query| discharge_query(q, &mut *backend, cache, emit_dir).result;
            requires.extend(checker.infer_requires(p, f, summary_of, &mut discharge));
            ensures.extend(checker.infer_ensures(p, f, summary_of, &mut discharge));
        }

        Summary {
            effects,
            requires,
            ensures,
            ..Summary::default()
        }
```

Nothing else changes: `Summary` derives `PartialEq` over all fields, so SCC
convergence and the `dump_summaries` `ensures=` count already work.

- [ ] **Step 4: Run tests to verify they pass**

Run: `mise x -- cargo test -p goverify-analysis`
Expected: PASS (both new tests; existing engine tests unaffected — `FakeChecker`
etc. pick up the default method).

- [ ] **Step 5: Commit**

```bash
git add -A
git -c commit.gpgsign=false commit -m "summaries: Checker::infer_ensures + engine collection (task 3)"
```

---

### Task 4: Encoder asserts callee ensures at static call sites

**Files:**
- Modify: `crates/goverify-analysis/src/encode.rs` (`encode_func_with`, `encode_call_ensures`, `call_result_terms`)
- Modify: `crates/goverify-analysis/src/lib.rs` (export `encode_func_with`)
- Test: encode.rs tests module

**Interfaces:**
- Consumes: `instantiate_ensures` (Task 2), `Summary`/`Provenance` (same crate).
- Produces: `pub fn encode_func_with(p: &Program, f: FuncId, summary_of: &dyn Fn(FuncId) -> Summary) -> Result<EncodedFunc, String>`. `encode_func(p, f)` becomes a delegate passing `&|_| Summary::havoc()` — every existing caller keeps its exact behavior (havoc summaries carry no ensures). Tasks 5-7 call `encode_func_with` from the checkers.

- [ ] **Step 1: Write the failing tests**

In encode.rs tests (using its local fixture helpers; mirror the existing
call-instruction shape from engine/checker tests — operands `[callee-slot, args…]`,
`sem.static_callee`):

```rust
    /// t.Caller() { v2 = call t.Mk(); v3 = FieldAddr v2 .0 } with t.Mk
    /// given an ensures ¬is_nil(r0): reach ∧ is_nil(v2) must be Unsat
    /// under encode_func_with, and Sat under plain encode_func (havoc).
    #[test]
    fn call_ensures_constrain_single_result() {
        use goverify_extract::gvir;
        use goverify_extract::gvir::instruction::Sem;
        let mut call = gvir::Instruction {
            kind: "Call".into(),
            register: 2,
            r#type: 2, // *T
            operands: vec![0],
            ..Default::default()
        };
        call.sem = Some(Sem::Call(gvir::CallSem {
            static_callee: "t.Mk".into(),
            ..Default::default()
        }));
        let package = gvir::Package {
            import_path: "t".into(),
            types: vec![
                gvir::Type {
                    id: 1,
                    repr: "T".into(),
                    kind: gvir::TypeKind::Struct as i32,
                    ..Default::default()
                },
                gvir::Type {
                    id: 2,
                    repr: "*T".into(),
                    kind: gvir::TypeKind::Pointer as i32,
                    elem: 1,
                    ..Default::default()
                },
            ],
            functions: vec![
                gvir::Function {
                    id: "t.Mk".into(),
                    blocks: vec![gvir::BasicBlock {
                        index: 0,
                        instrs: vec![gvir::Instruction {
                            kind: "Return".into(),
                            ..Default::default()
                        }],
                        succs: vec![],
                        preds: vec![],
                    }],
                    ..Default::default()
                },
                gvir::Function {
                    id: "t.Caller".into(),
                    blocks: vec![gvir::BasicBlock {
                        index: 0,
                        instrs: vec![
                            call,
                            gvir::Instruction {
                                kind: "Return".into(),
                                ..Default::default()
                            },
                        ],
                        succs: vec![],
                        preds: vec![],
                    }],
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let p = goverify_ir::Program::from_packages(vec![package]);
        let mk = p.lookup_func("t.Mk").unwrap();
        let caller = p.lookup_func("t.Caller").unwrap();

        let r0 = Term::var("r0", ptr_sort());
        let mut mk_summary = crate::summary::Summary::default();
        mk_summary.ensures = vec![crate::summary::Clause {
            tag: "nil-deref".into(),
            formula: crate::summary::Formula {
                term: Term::not(ptr_is_nil(r0).unwrap()).unwrap(),
            },
        }];
        let summary_of = move |f: goverify_ir::FuncId| {
            if f == mk {
                mk_summary.clone()
            } else {
                crate::summary::Summary::default()
            }
        };

        let mut solver = goverify_solver::Z3Native::new(goverify_solver::SolverLimits {
            timeout_ms: 5_000,
            mem_mb: 1024,
        });
        let mut discharge = |q: &goverify_solver::Query| {
            goverify_solver::discharge_query(q, &mut solver, None, None).result
        };

        let with = encode_func_with(&p, caller, &summary_of).unwrap();
        let dst = with.value(goverify_ir::ValueId(2)).unwrap().clone();
        let q = with.reach_query(0, vec![ptr_is_nil(dst).unwrap()]);
        assert_eq!(
            discharge(&q),
            goverify_solver::SatResult::Unsat,
            "asserted ensures must make is_nil(dst) unreachable"
        );

        let without = encode_func(&p, caller).unwrap();
        let dst = without.value(goverify_ir::ValueId(2)).unwrap().clone();
        let q = without.reach_query(0, vec![ptr_is_nil(dst).unwrap()]);
        assert_eq!(
            discharge(&q),
            goverify_solver::SatResult::Sat,
            "plain encode_func keeps the havoc'd dst"
        );
    }

    #[test]
    fn havoc_provenance_summaries_assert_nothing() {
        // Same fixture as above but the summary_of hands back a summary
        // with Provenance::Havoc carrying (bogus) ensures: the encoder
        // must ignore it — havoc summaries are never trusted for facts.
        // (Build identical package; set mk_summary.provenance =
        //  crate::summary::Provenance::Havoc; expect Sat.)
    }
```

Fill the second test by copying the first's fixture and flipping
`mk_summary.provenance = crate::summary::Provenance::Havoc;` — expected
verdict `Sat`.

- [ ] **Step 2: Run tests to verify they fail**

Run: `mise x -- cargo test -p goverify-analysis call_ensures havoc_provenance`
Expected: FAIL to compile — `encode_func_with` not defined.

- [ ] **Step 3: Implement**

In encode.rs:

```rust
pub fn encode_func(p: &Program, f: FuncId) -> Result<EncodedFunc, String> {
    encode_func_with(p, f, &|_| crate::summary::Summary::havoc())
}

/// `encode_func` plus callee postconditions: after the defining
/// equalities, every static call whose callee summary is Inferred and
/// carries ensures gets each clause bound (p<i> := arg terms, r<i> :=
/// result terms) and asserted GATED on the call's block guard
/// (¬g_b ∨ clause): a postcondition holds only on executions that
/// performed the call. Unbindable clauses are skipped — weaker, never
/// wrong.
pub fn encode_func_with(
    p: &Program,
    f: FuncId,
    summary_of: &dyn Fn(FuncId) -> crate::summary::Summary,
) -> Result<EncodedFunc, String> {
    // …existing encode_func body through encode_load_forwarding…
    encode_call_ensures(p, func, summary_of, &mut enc);
    if enc.asserts.len() > ASSERT_CAP {
        // …existing cap check (unchanged, now also covers ensures asserts)…
    }
    Ok(enc)
}
```

Concretely: rename the existing body into `encode_func_with`, add the
`summary_of` parameter, insert the `encode_call_ensures` call between
`encode_load_forwarding(func, &mut enc);` and the `ASSERT_CAP` check, and make
`encode_func` the two-line delegate above.

Then the substitution pass and the result-term resolver:

```rust
/// The r<i> binding targets for a call's results: a single-value call
/// binds r0 to the dst's own term; a tuple call binds r<i> to the term
/// of the first `Extract { tuple: dst, index: i }` dst in instruction
/// order (deterministic; SSA emits at most one per index — crafted
/// .gvir with duplicates just picks the first). A component with no
/// Extract (`b, _ := …`) or no term stays None.
fn call_result_terms(func: &Function, dst: Option<ValueId>, enc: &EncodedFunc) -> Vec<Option<Term>> {
    let Some(d) = dst else { return Vec::new() };
    if let Some(t) = enc.values.get(&d) {
        return vec![Some(t.clone())]; // single-value call: dst has a sort
    }
    // Tuple-typed dst (no sort): collect Extract components.
    let mut out: Vec<Option<Term>> = Vec::new();
    for b in &func.blocks {
        for ins in &b.instrs {
            let Op::Extract { dst: ed, tuple, index } = &ins.op else {
                continue;
            };
            if *tuple != d {
                continue;
            }
            let i = *index as usize;
            if out.len() <= i {
                out.resize(i + 1, None);
            }
            if out[i].is_none() {
                out[i] = enc.values.get(ed).cloned();
            }
        }
    }
    out
}

/// Assert every bindable ensures clause of every static callee with an
/// Inferred summary, gated on the call's block guard. Havoc-provenance
/// summaries are never consulted for facts.
fn encode_call_ensures(
    p: &Program,
    func: &Function,
    summary_of: &dyn Fn(FuncId) -> crate::summary::Summary,
    enc: &mut EncodedFunc,
) {
    let _ = p;
    for (bi, b) in func.blocks.iter().enumerate() {
        for ins in &b.instrs {
            let Op::Call {
                dst,
                callee: Callee::Static(c),
                args,
            } = &ins.op
            else {
                continue;
            };
            let s = summary_of(*c);
            if s.provenance != crate::summary::Provenance::Inferred || s.ensures.is_empty() {
                continue;
            }
            let arg_terms: Vec<Option<Term>> =
                args.iter().map(|a| enc.values.get(a).cloned()).collect();
            let result_terms = call_result_terms(func, *dst, enc);
            let Some(g) = enc.guards.get(bi).cloned() else {
                continue;
            };
            let Ok(ng) = Term::not(g) else { continue };
            for bc in crate::summary::instantiate_ensures(&s, &arg_terms, &result_terms) {
                let Some(bound) = bc.bound else { continue };
                if let Ok(gated) = Term::or(vec![ng.clone(), bound]) {
                    enc.asserts.push(gated);
                }
            }
        }
    }
}
```

Export `encode_func_with` from lib.rs next to `encode_func`. The
`NEVER_NIL_RESULT` arm in `op_def` stays untouched (extern constructors have
no inferred summary; the two mechanisms are disjoint).

- [ ] **Step 4: Run tests to verify they pass**

Run: `mise x -- cargo test -p goverify-analysis`
Expected: PASS, including all pre-existing encode tests (encode_func behavior
is bit-identical via the havoc delegate).

- [ ] **Step 5: Commit**

```bash
git add -A
git -c commit.gpgsign=false commit -m "summaries: encoder asserts callee ensures at static call sites (task 4)"
```

---

### Task 5: `NilChecker::infer_ensures` — templates, validation, Go-idiom rule

**Files:**
- Modify: `crates/goverify-checkers/src/nil.rs` (implement `infer_ensures`)
- Modify: `crates/goverify-checkers/src/shared.rs` (result-type helpers, if shared with bounds later)
- Create: `testdata/corpus/ensures/go.mod`, `testdata/corpus/ensures/ensures.go`
- Create: `crates/goverify-checkers/tests/ensures_corpus.rs`
- Modify: `docs/threat-model.md` (Go-idiom under-approximation entry)

**Interfaces:**
- Consumes: `encode_func_with` (Task 4), `Checker::infer_ensures` slot (Task 3), `iface_var_name`.
- Produces: `NilChecker::infer_ensures` emitting clauses tagged `"nil-deref"` over `r<i>` vars: unconditional `¬is_nil(r_i)` and the correlation `¬is_nil(r_e) ∨ ¬is_nil(r_i)` (≡ `is_nil(r_e) ⇒ ¬is_nil(r_i)`). Tasks 6-7 consume these through the encoder and the obligation gate.

- [ ] **Step 1: Create the corpus module (the failing end-to-end test)**

`testdata/corpus/ensures/go.mod` — copy `testdata/corpus/nil/go.mod`, module
path `example.com/ensures`.

`testdata/corpus/ensures/ensures.go`:

```go
// Package ensures exercises postcondition (ensures) inference: the
// unconditional never-nil template and the (T, error) correlation,
// validated per return site with the Go-idiom rule for non-literal-nil
// error expressions (sentinel errors).
package ensures

type T struct{ X int }

type opError struct{}

func (e *opError) Error() string { return "op failed" }

// A package-level sentinel: loads of it are havoc'd, which is exactly
// why the correlation template needs the Go-idiom rule.
var errOp = &opError{}

// MakeT always returns a fresh allocation: unconditional ensures
// ¬is_nil(r0) must be proven (Alloc dsts are never nil).
func MakeT() *T { return &T{} }

// NewT is the canonical constructor shape: (nil, sentinel) on failure,
// (alloc, nil) on success. The correlation is_nil(r1) ⇒ ¬is_nil(r0)
// must be emitted: the failure site passes by the idiom rule (non-
// literal-nil error), the success site by SMT proof (alloc non-nil,
// error component is the nil literal).
func NewT(fail bool) (*T, error) {
	if fail {
		return nil, errOp
	}
	return &T{}, nil
}

// MayNil returns (nil, nil) on one path: NEITHER template may validate
// (the success-shaped site pairs a nil literal error with a nil
// result — the SMT check must reject).
func MayNil(b bool) (*T, error) {
	if b {
		return nil, nil
	}
	return &T{}, nil
}
```

`crates/goverify-checkers/tests/ensures_corpus.rs`:

```rust
//! End-to-end ensures inference over a real extracted corpus module:
//! unconditional never-nil, the (T, error) correlation under the
//! Go-idiom rule, and the (nil, nil) rejection case.

use goverify_analysis::{EngineConfig, Options, analyze_full};
use goverify_checkers::NilChecker;
use goverify_solver::{SolverLimits, Z3Native};

fn analysis() -> (goverify_ir::Program, goverify_analysis::Analysis) {
    let p = goverify_ir::testutil::load_corpus("ensures");
    let checkers: Vec<&dyn goverify_analysis::Checker> = vec![&NilChecker];
    let a = analyze_full(
        &p,
        &EngineConfig {
            opts: Options::default(),
            cache_dir: None,
            emit_smt: None,
        },
        &checkers,
        &|_role| {
            Box::new(Z3Native::new(SolverLimits {
                timeout_ms: 5_000,
                mem_mb: 1024,
            }))
        },
    );
    (p, a)
}

/// The sorted free-var names of every "nil-deref" ensures clause.
fn ensures_vars(
    p: &goverify_ir::Program,
    a: &goverify_analysis::Analysis,
    func: &str,
) -> Vec<Vec<String>> {
    let f = p.lookup_func(func).expect(func);
    a.summaries[&f]
        .ensures
        .iter()
        .filter(|c| c.tag == "nil-deref")
        .map(|c| {
            let mut v: Vec<String> = c.formula.term.free_vars().keys().cloned().collect();
            v.sort();
            v
        })
        .collect()
}

#[test]
fn ensures_inference_over_corpus() {
    let (p, a) = analysis();
    assert_eq!(
        ensures_vars(&p, &a, "example.com/ensures.MakeT"),
        vec![vec!["r0".to_string()]],
        "MakeT: unconditional never-nil result"
    );
    assert!(
        ensures_vars(&p, &a, "example.com/ensures.NewT")
            .contains(&vec!["r0".to_string(), "r1".to_string()]),
        "NewT: the (T, error) correlation must be emitted: {:?}",
        ensures_vars(&p, &a, "example.com/ensures.NewT")
    );
    assert!(
        ensures_vars(&p, &a, "example.com/ensures.MayNil").is_empty(),
        "MayNil returns (nil, nil): no nil-deref ensures may validate: {:?}",
        ensures_vars(&p, &a, "example.com/ensures.MayNil")
    );
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `mise x -- cargo test -p goverify-checkers --test ensures_corpus`
Expected: FAIL — `MakeT: unconditional never-nil result` (ensures empty; the
default trait method emits nothing).

- [ ] **Step 3: Implement `NilChecker::infer_ensures`**

In nil.rs, add to `impl Checker for NilChecker`:

```rust
    /// Postconditions (spec §3.1): two templates per pointer-sorted
    /// result, candidate-and-check against the function's own encoding.
    ///
    /// 1. Unconditional ¬is_nil(r_i): proven iff, at EVERY return site,
    ///    reach(site) ∧ is_nil(returned term) is Unsat.
    /// 2. Correlation ¬is_nil(r_e) ∨ ¬is_nil(r_i) (≡ is_nil(r_e) ⇒
    ///    ¬is_nil(r_i)) for each error-typed result e, tried only when
    ///    template 1 failed (subsumption). Per-site Go-idiom rule
    ///    (spec planning amendment): a site whose error component is
    ///    the LITERAL nil constant needs the SMT proof on the paired
    ///    result; any other error expression is treated as a non-nil
    ///    error — the documented under-approximation (threat model).
    ///
    /// Every failure degrades to "no clause": missing terms, arity
    /// mismatches, Sat, Unknown. No return sites ⇒ no ensures.
    fn infer_ensures(
        &self,
        p: &Program,
        f: FuncId,
        summary_of: &dyn Fn(FuncId) -> Summary,
        discharge: &mut dyn FnMut(&Query) -> SatResult,
    ) -> Vec<Clause> {
        let Some(func) = p.func(f) else {
            return Vec::new();
        };
        let Ok(enc) = goverify_analysis::encode_func_with(p, f, summary_of) else {
            return Vec::new();
        };
        // Result types from the signature.
        let TypeKind::Signature { results, .. } = p.types().kind(func.sig) else {
            return Vec::new();
        };
        let results = results.clone();
        // Return sites: (block index, returned ValueIds). Arity mismatch
        // anywhere (malformed .gvir) drops ALL candidates.
        let mut sites: Vec<(usize, Vec<ValueId>)> = Vec::new();
        for (bi, b) in func.blocks.iter().enumerate() {
            for ins in &b.instrs {
                if let Op::Return { vals } = &ins.op {
                    if vals.len() != results.len() {
                        return Vec::new();
                    }
                    sites.push((bi, vals.clone()));
                }
            }
        }
        if sites.is_empty() {
            return Vec::new();
        }
        let ptr_results: Vec<usize> = (0..results.len())
            .filter(|&i| {
                goverify_analysis::sort_of(p.types(), results[i])
                    .is_some_and(|s| s == goverify_solver::ptr_sort())
            })
            .collect();
        let error_results: Vec<usize> = (0..results.len())
            .filter(|&i| is_error_type(p.types(), results[i]))
            .collect();

        // A site's returned component proven non-nil?
        let mut site_nonnil = |bi: usize, v: ValueId| -> bool {
            let Some(t) = enc.value(v).cloned() else {
                return false;
            };
            let Ok(is_nil) = ptr_is_nil(t) else {
                return false;
            };
            discharge(&enc.reach_query(bi, vec![is_nil])) == SatResult::Unsat
        };

        let mut out = Vec::new();
        for &i in &ptr_results {
            // Template 1: unconditional.
            if sites.iter().all(|(bi, vals)| site_nonnil(*bi, vals[i])) {
                if let Some(c) = nonnil_result_clause(i as u32) {
                    push_clause(&mut out, c);
                }
                continue; // correlation is subsumed
            }
            // Template 2: correlate with each error result.
            for &e in &error_results {
                if e == i {
                    continue;
                }
                let proven = sites.iter().all(|(bi, vals)| {
                    let err_is_nil_literal = matches!(
                        func.value(vals[e]).kind,
                        ValueKind::Const(ConstVal::Nil)
                    );
                    if err_is_nil_literal {
                        site_nonnil(*bi, vals[i])
                    } else {
                        true // Go-idiom rule: non-literal error ⇒ non-nil
                    }
                });
                if proven && let Some(c) = correlation_clause(e as u32, i as u32) {
                    push_clause(&mut out, c);
                }
            }
        }
        out
    }
```

Plus the two clause builders and the error-type test (file-private helpers in
nil.rs):

```rust
/// ¬is_nil(r<i>) as a canonical ensures clause. None on term-construction
/// failure (degrade).
fn nonnil_result_clause(i: u32) -> Option<Clause> {
    let r = Term::var(
        &goverify_analysis::iface_var_name(&goverify_analysis::IfaceVar::Result(i)),
        goverify_solver::ptr_sort(),
    );
    Some(Clause {
        tag: "nil-deref".into(),
        formula: Formula {
            term: Term::not(ptr_is_nil(r).ok()?).ok()?,
        },
    })
}

/// ¬is_nil(r<e>) ∨ ¬is_nil(r<i>): "never both nil", the (T, error)
/// correlation in disjunctive normal form.
fn correlation_clause(e: u32, i: u32) -> Option<Clause> {
    let rv = |idx: u32| {
        Term::var(
            &goverify_analysis::iface_var_name(&goverify_analysis::IfaceVar::Result(idx)),
            goverify_solver::ptr_sort(),
        )
    };
    Some(Clause {
        tag: "nil-deref".into(),
        formula: Formula {
            term: Term::or(vec![
                Term::not(ptr_is_nil(rv(e)).ok()?).ok()?,
                Term::not(ptr_is_nil(rv(i)).ok()?).ok()?,
            ])
            .ok()?,
        },
    })
}

/// The predeclared `error` interface: Named{name: "error"} over an
/// Interface underlying. Deliberately narrow — a custom interface that
/// happens to embed error is not a correlation anchor.
fn is_error_type(types: &goverify_ir::TypeTable, t: goverify_ir::TypeId) -> bool {
    match types.kind(t) {
        TypeKind::Named { name, underlying } => {
            name == "error" && matches!(types.kind(*underlying), TypeKind::Interface)
        }
        _ => false,
    }
}
```

Adjust to the actual `iface_var_name`/`IfaceVar` re-export paths (they live in
`goverify_analysis::summary`; check lib.rs and re-export if missing). If the
extractor represents `error` directly as `TypeKind::Interface` with repr
`"error"` rather than Named (verify against the `ensures` corpus's `.gvir` by
running the test with `dbg!(p.types().kind(results[e]))`), extend
`is_error_type` to accept that shape too — the corpus test is the ground
truth.

- [ ] **Step 4: Run the corpus test until green**

Run: `mise x -- cargo test -p goverify-checkers --test ensures_corpus`
Expected: PASS all three assertions.

- [ ] **Step 5: Threat-model entry**

In `docs/threat-model.md`, under the deliberate under-approximations section
(after the uintptr entry, threat-model.md:80-100 region), add:

```markdown
- **Go-idiom error correlation (ensures inference).** The
  `is_nil(err) ⇒ ¬is_nil(result)` postcondition template validates per
  return site: a site whose error component is the literal nil constant
  must SMT-prove the paired result non-nil, but a site returning any
  other error expression (a sentinel global, a wrapped error) is
  accepted as returning a non-nil error without proof — the universal
  Go idiom, unprovable locally because sentinel loads are havoc'd. A
  callee that returns a nil-valued error *variable* together with a nil
  result earns a wrong ensures, and callers guarding `err != nil` get a
  wrong discharge (false negative). The unconditional `¬is_nil(result)`
  template carries no such assumption (strictly proven).
```

- [ ] **Step 6: Run the full suite and commit**

Run: `mise run test`
Expected: PASS (summaries for corpus functions change — `dump_summaries`-based
engine corpus goldens may show new `ensures=` counts; refresh with
`UPDATE_GOLDENS=1` after reviewing).

```bash
git add -A
git -c commit.gpgsign=false commit -m "summaries: NilChecker ensures inference with Go-idiom correlation rule (task 5)"
```

---

### Task 6: Checkers encode with summaries — the C009c discharge

Switch both checkers' encodings to `encode_func_with` so asserted callee
ensures participate in every requires-inference gate, obligation query, and
checked-deref interaction. This is the step that discharges the canonical
guarded-constructor FP.

**Files:**
- Modify: `crates/goverify-checkers/src/nil.rs:60,127` (both `encode_func` calls)
- Modify: `crates/goverify-checkers/src/bounds.rs` (its `encode_func` calls — same two spots in `infer_requires`/`obligations`)
- Modify: `testdata/corpus/knownfp/knownfp.go` (flip the postcondition pins)
- Test: existing corpus tests

**Interfaces:**
- Consumes: `encode_func_with` (Task 4), populated ensures (Task 5).
- Produces: `NilChecker`/`BoundsChecker` reason under callee postconditions. No signature changes.

- [ ] **Step 1: Flip the corpus pins first (failing test)**

In `testdata/corpus/knownfp/knownfp.go`:

1. The `Compact` case (the `// want: nil-deref` on `return commitFn(tx)`,
   ~line 335): delete the `// want: nil-deref` comment and rewrite the block
   header from `KNOWN-FP(phase-5): FP/requires-lifting — err==nil ⇒ result!=nil
   postcondition not lifted…` to:

```go
// FIXED (interprocedural summaries, 2026-07-20): formerly
// KNOWN-FP(phase-5) FP/requires-lifting — `beginTx`'s inferred
// correlation ensures (is_nil(err) ⇒ ¬is_nil(tx), validated under the
// Go-idiom rule for the errConstructFailed sentinel returns) is now
// asserted at both call sites in Compact; each `err != nil` guard then
// renders is_nil(tx) unreachable at the commitFn call, discharging the
// propagated nil-deref requirement. Kept as the green regression case
// for exemplar C009c.
```

2. The `UseHandle` case (the `// want: nil-deref` on `return handleID(h)`,
   ~line 295): same treatment IF it goes green in step 3 (its `newHandle`
   must validate the correlation — check and flip only what actually passes;
   a case that stays red keeps its KNOWN-FP header).

- [ ] **Step 2: Run to verify the corpus test fails**

Run: `mise x -- cargo test -p goverify-checkers --test knownfp_corpus`
Expected: FAIL — findings still contain the Compact/UseHandle rows but the
wants no longer do (pins flipped before the fix: red).

- [ ] **Step 3: Implement the switch**

In nil.rs `infer_requires` (line 60) and `obligations` (line 127), replace:

```rust
        let Ok(enc) = encode_func(p, f) else {
            return Vec::new();
        };
```

with:

```rust
        let Ok(enc) = goverify_analysis::encode_func_with(p, f, summary_of) else {
            return Vec::new();
        };
```

(drop the now-unused `encode_func` import if nothing else uses it). Apply the
identical change to bounds.rs's two encode sites. `infer_ensures` (Task 5)
already uses `encode_func_with`.

- [ ] **Step 4: Run the corpus tests**

Run: `mise x -- cargo test -p goverify-checkers`
Expected: `knownfp_corpus` PASSES with the flipped pins. If `UseHandle` (or any
other pinned case) unexpectedly stays red or goes green, adjust its pin to
match observed behavior with a comment explaining why (the pins document
actual behavior — never force them).
Also run: `mise run test` — nil/bounds corpus and check goldens will move
(discharged FPs disappear from `nil.check.txt`); review the diff, then
`UPDATE_GOLDENS=1 mise x -- cargo test -p goverify-cli` to refresh.

- [ ] **Step 5: Commit**

```bash
git add -A
git -c commit.gpgsign=false commit -m "summaries: checkers encode with callee ensures; knownfp C009c pins flip green (task 6)"
```

---

### Task 7: Obligations on summary-constrained call results — FillPercent restoration

**Files:**
- Modify: `crates/goverify-checkers/src/shared.rs` (`canonical_value`, `call_result_of`)
- Modify: `crates/goverify-checkers/src/nil.rs` (`obligations` qualification)
- Modify: `testdata/corpus/nil/nil.go` (red + green cases)
- Test: `crates/goverify-checkers/src/shared.rs` unit tests + nil corpus

**Interfaces:**
- Consumes: ensures summaries (Task 5), `encode_func_with` checkers (Task 6).
- Produces: `pub(crate) fn canonical_value(func: &Function, v: ValueId) -> ValueId` (Assign-chain resolution, depth-capped 64 — Task 8 reuses it) and `pub(crate) fn call_result_of(func: &Function, v: ValueId) -> Option<(FuncId, u32)>`.

- [ ] **Step 1: Corpus red/green cases (failing test)**

Append to `testdata/corpus/nil/nil.go`:

```go
// --- interprocedural summaries: obligations on call-result subjects ---

type Bucket struct{ Fill int }

type createError struct{}

func (e *createError) Error() string { return "create failed" }

var errCreate = &createError{}

// createBucket carries the inferred correlation err==nil ⇒ result!=nil
// (Go-idiom rule: the failure site returns a sentinel).
func createBucket(fail bool) (*Bucket, error) {
	if fail {
		return nil, errCreate
	}
	return &Bucket{}, nil
}

// IgnoredErr discards the error and dereferences the result — the
// FillPercent restoration shape (bbolt cmd/bbolt/main.go:1191): the
// discarded error leaves the correlation unbindable, the result stays
// possibly-nil, and the deref must now be flagged at the true
// first-failure site.
func IgnoredErr(fail bool) int {
	b, _ := createBucket(fail)
	return b.Fill // want: nil-deref
}

// GuardedErr checks the error first: the asserted correlation plus the
// guard discharge the deref. No finding.
func GuardedErr(fail bool) int {
	b, err := createBucket(fail)
	if err != nil {
		return 0
	}
	return b.Fill
}

// fresh always allocates: the unconditional ensures discharges the
// unguarded deref of its result. No finding.
func fresh() *Bucket { return &Bucket{} }

func UnguardedFresh() int { return fresh().Fill }
```

- [ ] **Step 2: Run to verify it fails**

Run: `mise x -- cargo test -p goverify-checkers --test nil_corpus`
Expected: FAIL — the want at `b.Fill` in `IgnoredErr` has no matching finding
(call-result subjects still don't qualify for obligations).

- [ ] **Step 3: Write the resolver unit tests**

In shared.rs (new tests module if none exists there — helpers come from
`crate::testfix`):

```rust
#[cfg(test)]
mod tests {
    use goverify_ir::{Callee, Op, ValueId};

    use super::*;
    use crate::testfix::{block, call_static, instr, pkg_with_ptr_types};

    /// t.F(p *T) { v3 := p (Assign); v4 := v3 (Assign); … }: the chain
    /// canonicalizes to the param.
    #[test]
    fn canonical_value_follows_assign_chains() {
        let mut a1 = instr("ChangeType"); // lowers to Op::Assign
        a1.register = 3;
        a1.r#type = 2;
        a1.operands = vec![1];
        let mut a2 = instr("ChangeType");
        a2.register = 4;
        a2.r#type = 2;
        a2.operands = vec![3];
        let f = goverify_extract::gvir::Function {
            id: "t.F".into(),
            params: vec![goverify_extract::gvir::Param {
                id: 1,
                name: "p".into(),
                r#type: 2,
            }],
            blocks: vec![block(0, vec![a1, a2, instr("Return")], vec![])],
            ..Default::default()
        };
        let p = pkg_with_ptr_types(vec![f]);
        let func = p.func(p.lookup_func("t.F").unwrap()).unwrap();
        assert_eq!(canonical_value(func, ValueId(4)), ValueId(1));
        assert_eq!(canonical_value(func, ValueId(3)), ValueId(1));
        assert_eq!(canonical_value(func, ValueId(1)), ValueId(1));
    }

    /// A crafted self-cycle (dst == src) must terminate at the depth cap.
    #[test]
    fn canonical_value_survives_crafted_cycles() {
        let mut a = instr("ChangeType");
        a.register = 3;
        a.r#type = 2;
        a.operands = vec![3]; // v3 := v3
        let f = goverify_extract::gvir::Function {
            id: "t.F".into(),
            blocks: vec![block(0, vec![a, instr("Return")], vec![])],
            ..Default::default()
        };
        let p = pkg_with_ptr_types(vec![f]);
        let func = p.func(p.lookup_func("t.F").unwrap()).unwrap();
        let _ = canonical_value(func, ValueId(3)); // must return, not hang
    }

    /// t.F() { v2 := call t.K() } — v2 resolves to (t.K, 0).
    #[test]
    fn call_result_of_single_value_call() {
        let f = goverify_extract::gvir::Function {
            id: "t.F".into(),
            blocks: vec![block(
                0,
                vec![call_static("t.K", 2, 2, vec![]), instr("Return")],
                vec![],
            )],
            ..Default::default()
        };
        let p = pkg_with_ptr_types(vec![f]);
        let k = p.lookup_func("t.K").unwrap();
        let func = p.func(p.lookup_func("t.F").unwrap()).unwrap();
        assert_eq!(call_result_of(func, ValueId(2)), Some((k, 0)));
        assert_eq!(call_result_of(func, ValueId(1)), None);
    }
}
```

(For the tuple/Extract path, the nil-corpus `IgnoredErr` case is the
end-to-end coverage; a hand-built Extract fixture needs a tuple type id and
adds little.)

- [ ] **Step 4: Implement**

shared.rs:

```rust
/// Resolve `v` through same-function `Op::Assign` chains (`ChangeType`
/// lowers to Assign — lower.rs) to its root value. Depth-capped at 64,
/// mirroring AddrKey's cap: crafted .gvir may fabricate cycles or
/// arbitrarily long chains, and untrusted bytes must degrade (return
/// the current value), never hang or recurse unboundedly.
pub(crate) fn canonical_value(func: &Function, v: ValueId) -> ValueId {
    let mut cur = v;
    for _ in 0..64 {
        let src = func.blocks.iter().flat_map(|b| &b.instrs).find_map(|ins| {
            match &ins.op {
                Op::Assign { dst, src } if *dst == cur && *src != cur => Some(*src),
                _ => None,
            }
        });
        match src {
            Some(s) => cur = s,
            None => return cur,
        }
    }
    cur
}

/// `v` resolved (through Assign chains) to "result <index> of a static
/// call": the call's own dst for a single-value call, or an Extract
/// component of a tuple call. None for anything else.
pub(crate) fn call_result_of(func: &Function, v: ValueId) -> Option<(FuncId, u32)> {
    let cur = canonical_value(func, v);
    for b in &func.blocks {
        for ins in &b.instrs {
            match &ins.op {
                Op::Call {
                    dst: Some(d),
                    callee: Callee::Static(c),
                    ..
                } if *d == cur => return Some((*c, 0)),
                Op::Extract { dst, tuple, index } if *dst == cur => {
                    let t = canonical_value(func, *tuple);
                    for b2 in &func.blocks {
                        for i2 in &b2.instrs {
                            if let Op::Call {
                                dst: Some(d),
                                callee: Callee::Static(c),
                                ..
                            } = &i2.op
                                && *d == t
                            {
                                return Some((*c, *index));
                            }
                        }
                    }
                    return None;
                }
                _ => {}
            }
        }
    }
    None
}
```

nil.rs `obligations`, in the manifest-sites loop (after the `expressible`
computation, nil.rs:149-170) — replace the plain `continue` for inexpressible
subjects:

```rust
            let is_const_nil = matches!(func.value(*subject).kind, ValueKind::Const(ConstVal::Nil));
            let expressible = is_const_nil || subj.free_vars().is_empty() || params_only(&subj);
            // Summary-constrained call results (spec §4): a deref of a
            // static call's result qualifies iff the callee's INFERRED
            // summary carries a nil-deref ensures clause constraining
            // that exact result index — every finding this raises is
            // backed by a proven callee correlation, and results the
            // inference couldn't characterize stay silent (no FP flood
            // where inference failed).
            let call_result = if expressible {
                None
            } else {
                crate::shared::call_result_of(func, *subject).filter(|(callee, idx)| {
                    let s = summary_of(*callee);
                    s.provenance == goverify_analysis::Provenance::Inferred
                        && s.ensures.iter().any(|c| {
                            c.tag == "nil-deref"
                                && c.formula
                                    .term
                                    .free_vars()
                                    .keys()
                                    .any(|n| n == &format!("r{idx}"))
                        })
                })
            };
            if !expressible && call_result.is_none() {
                continue; // havoc'd heap value: silent (spec §4)
            }
            let Ok(is_nil) = ptr_is_nil(subj) else {
                continue;
            };
            let mut extra = pre.clone();
            extra.extend(assume(*bi, *ii));
            extra.push(is_nil);
            let message = match call_result {
                Some((callee, _)) => format!(
                    "possibly-nil result of {} dereferenced in {}",
                    p.func_name(callee),
                    p.func_name(f)
                ),
                None => format!("nil dereference in {}", p.func_name(f)),
            };
            out.push(Obligation {
                tag: "nil-deref".into(),
                message,
                pos: pos.clone(),
                query: enc.reach_query(*bi, extra),
            });
```

(`Provenance` needs re-exporting from goverify-analysis's lib.rs if it isn't
already — check; `summary.rs` declares it public.)

- [ ] **Step 5: Run tests to verify they pass**

Run: `mise x -- cargo test -p goverify-checkers`
Expected: shared.rs unit tests PASS; `nil_corpus` PASS — `IgnoredErr` red,
`GuardedErr`/`UnguardedFresh` silent. If `GuardedErr` unexpectedly fires,
debug the correlation binding (both Extracts present? guard encoding?) before
touching the corpus — the green sibling is the FP-flood guard.
Then: `mise run test` and refresh check goldens (`UPDATE_GOLDENS=1`) after
reviewing — `nil.check.txt` gains the IgnoredErr finding.

- [ ] **Step 6: Commit**

```bash
git add -A
git -c commit.gpgsign=false commit -m "summaries: obligations on summary-constrained call results — FillPercent shape restored (task 7)"
```

---

### Task 8: ChangeType/Assign subject canonicalization before `params_only`

**Files:**
- Modify: `crates/goverify-checkers/src/nil.rs` (canonicalize deref subjects in `infer_requires` + `obligations`)
- Modify: `testdata/corpus/nil/nil.go` (red case)
- Modify: `testdata/corpus/knownfp/knownfp.go` (KNOWN-FN block → FIXED)
- Modify: `docs/threat-model.md:85-100` (composition-FN paragraph)

**Interfaces:**
- Consumes: `canonical_value` (Task 7).
- Produces: deref subjects resolve through Assign chains before expressibility decisions; the documented FN closes.

- [ ] **Step 1: Corpus red case (failing test)**

Append to `testdata/corpus/nil/nil.go`:

```go
// --- interprocedural summaries: ChangeType/Assign canonicalization ---

type NamedPtr *T

// chained reaches p only through ChangeType copies (each lowers to an
// SSA Assign). The deref subject must canonicalize back to p so
// chained emits its own ¬nil(p) requires — previously the copy's
// checked-deref assumption silently discharged p without any caller
// ever being told (the documented Assign/ChangeType composition FN).
func chained(p *T) int {
	q := NamedPtr(p)
	r := (*T)(q)
	return r.X
}

func BadChained() int { return chained(nil) } // want: nil-deref
```

- [ ] **Step 2: Run to verify it fails**

Run: `mise x -- cargo test -p goverify-checkers --test nil_corpus`
Expected: FAIL — no finding at the `BadChained` want (chained infers no
requires today).

- [ ] **Step 3: Implement**

In nil.rs, both loops over `sites` currently read the raw subject. In
`infer_requires` (lines 71-96), canonicalize before the term lookup:

```rust
        for (bi, ii, subject, _pos) in &sites {
            let canon = crate::shared::canonical_value(func, *subject);
            let Some(subj) = enc.value(canon).cloned() else {
                continue;
            };
```

and in `obligations`' manifest loop (lines 149-153), the same two lines plus
the groundness read off the canonical value:

```rust
        for (bi, ii, subject, pos) in &sites {
            let canon = crate::shared::canonical_value(func, *subject);
            let Some(subj) = enc.value(canon).cloned() else {
                continue;
            };
            let is_const_nil = matches!(func.value(canon).kind, ValueKind::Const(ConstVal::Nil));
```

(Task 7's `call_result_of(func, *subject)` already canonicalizes internally —
leave that call on the raw subject.) The encoding's `Assign` defining equality
(`v_q = p0`) makes the canonical term semantically identical to the original,
so discharge queries are unchanged in meaning; what changes is that
`params_only` now sees `p0` and emits the requires clause.
`checked_deref_assumptions` stays on raw subjects — assumptions on the copy
already transfer through the equality.

- [ ] **Step 4: Flip the knownfp block + threat model**

- `testdata/corpus/knownfp/knownfp.go` (~lines 164-201, the `KNOWN-FN
  (fix-wave, undischarged)` / `f`/`NamedPtr` block): rewrite the header to
  `FIXED (interprocedural summaries, 2026-07-20):` keeping the shape
  description, and state the applied fix: deref subjects canonicalize through
  same-function Assign/ChangeType chains (depth-capped 64) before
  `params_only`, so the copy's deref emits `¬nil(p0)` itself; the live red
  coverage is `nil.go`'s `chained`/`BadChained`.
- `docs/threat-model.md:85-100` ("Assign/ChangeType copies can silently
  discharge an unrelated deref's requires"): rewrite to past tense — fixed by
  subject canonicalization (cite `shared::canonical_value`, depth cap 64,
  applied in `nil.rs` before expressibility); note the residual: `Op::Convert`
  chains remain deliberately opaque (uintptr provenance), and `bounds.rs`
  subjects are not canonicalized this cycle (its violation terms are index
  expressions, not copyable pointer subjects).

- [ ] **Step 5: Run tests to verify they pass**

Run: `mise x -- cargo test -p goverify-checkers && mise run test`
Expected: `nil_corpus` PASS with `BadChained` red; `knownfp_corpus` PASS (the
NamedPtr block carries no want, so nothing flips there — verify no NEW
knownfp finding appeared from canonicalization; if one did, it's a behavior
change to pin and explain). Refresh check goldens after review.

- [ ] **Step 6: Commit**

```bash
git add -A
git -c commit.gpgsign=false commit -m "summaries: canonicalize deref subjects through Assign/ChangeType before params_only (task 8)"
```

---

### Task 9: Full blocking gate + determinism + docs sweep

**Files:**
- Modify: goldens under `testdata/goldens/` (as needed, reviewed)
- Modify: `ARCHITECTURE.md` (one line: encoder consumes summaries via `encode_func_with`)

**Interfaces:**
- Consumes: everything above.
- Produces: a green blocking tier; the branch is mergeable modulo the shakeout gate (Task 10).

- [ ] **Step 1: Run the whole blocking tier**

```bash
mise run lint
mise run test
mise run corpus
mise run secrets
mise run audit
```

Expected: all green. `mise run corpus` includes the determinism suite — the
new ensures corpus module and the encoder changes must produce byte-identical
`.gvir`/findings across runs (they will unless something introduced map-order;
if it fails, find the nondeterminism — do not paper over).

- [ ] **Step 2: Timing check**

Time `mise run corpus` and `mise run test` (plain `time`, before/after this
branch via `git stash` if needed) and record both deltas in the commit
message — spec §6 requires reporting them, and the ~30-minute corpus ceiling
is a watch item. If corpus time regressed by more than ~20%, profile which
task's queries dominate (likely `infer_ensures` candidates) before merging —
cheap mitigation: skip template 1's per-site queries when the returned value
is a `ConstVal::Nil` (fail fast without the solver).

- [ ] **Step 3: ARCHITECTURE.md line**

In the analysis-crate section, note: "The encoder can consume summaries:
`encode_func_with` asserts callee `ensures` (inferred postconditions) at
static call sites; `encode_func` is the summary-free delegate."

- [ ] **Step 4: Commit**

```bash
git add -A
git -c commit.gpgsign=false commit -m "summaries: blocking gate green; timing + architecture notes (task 9)"
```

---

### Task 10: Gated bbolt shakeout re-run + addendum

**Files:**
- Modify: `docs/shakeout-phase4-bbolt.md` (new addendum section)

**Interfaces:**
- Consumes: the merged mechanism stack; baseline = fix-wave addendum (509 findings).
- Produces: the spec §8 gate verdicts, recorded for the plan owner.

- [ ] **Step 1: Run the shakeout**

```bash
time mise run shakeout
```

Capture the findings output and wall-clock. This is the long manual step —
budget accordingly.

- [ ] **Step 2: Evaluate the gates (spec §8)**

1. **Hard gate:** a nil-deref finding at `cmd/bbolt/main.go:1191`
   (`runWritesWithSource`, the `b.FillPercent` store) exists. If absent, the
   wave has failed its headline goal — debug (is `CreateBucketIfNotExists`'s
   correlation inferred? does the subject resolve through the Extract?)
   before writing any addendum.
2. **Requires-lifting bucket:** count surviving findings from the 102-finding
   baseline bucket; account per-class against the 78 PHASE5-NOTE payloads
   (`docs/shakeout-phase4-bbolt.md:596-677`) — died / survived+why. Expected
   survivors: closure/cobra classes (C027, C216, C257, C258, C402…) and
   bounds-flavored classes (C101, C181, C223-C225, C229…) per the spec's
   non-goals.
3. **New findings:** diff new finding signatures vs baseline; triage EVERY new
   class (TP / FP with reason); compute the FP rate among new findings.
   Report-only — no numeric target.
4. Report corpus + shakeout wall-clock deltas (from Task 9 and step 1).

- [ ] **Step 3: Write the addendum**

Append to `docs/shakeout-phase4-bbolt.md` a section titled
`## Interprocedural-summaries re-run addendum (2026-07-20)` mirroring the
fix-wave addendum's structure: baseline/current totals, the four gate
verdicts with evidence (finding signatures, class ids), and open items for
the plan owner (accept the wave / iterate on specific new-FP classes).

- [ ] **Step 4: Commit**

```bash
git add -A
git -c commit.gpgsign=false commit -m "summaries: bbolt shakeout re-run addendum — gate verdicts (task 10)"
```

---

## Self-review notes (already applied)

- Spec coverage: §3.1 → Tasks 3+5; §3.2 → Task 4; §4 → Task 7; §5 → Task 8;
  §6 (determinism/perf) → Task 9; §7 corpus flips → Tasks 6-8; §8 gates →
  Task 10; planning amendments (interface Ptr, Go-idiom rule) → Tasks 1 and 5.
- The `NEVER_NIL_RESULT` table is intentionally NOT extended (errors.New etc.
  unnecessary under the Go-idiom rule — YAGNI).
- Type/name consistency: `instantiate_ensures(callee, arg_terms, result_terms)`
  (Tasks 2/4), `encode_func_with(p, f, summary_of)` (Tasks 4/5/6),
  `canonical_value(func, v)` / `call_result_of(func, v)` (Tasks 7/8) — one
  spelling each throughout.
- Known judgment calls the executor may hit: exact gvir fixture field spellings
  in Tasks 1/4/7 unit tests (mirror the neighboring tests in the same file —
  they are the source of truth); `is_error_type`'s Named-vs-Interface shape
  (Task 5 step 3 says how to verify against the real corpus).
