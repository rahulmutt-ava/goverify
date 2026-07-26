# Phase 7: Goroutine-Leak Checker (Channels) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A `goroutine-leak` checker (tags `chan-send-leak`, `chan-recv-leak`, `chan-select-leak`, severity Error) that flags `go` statements spawning goroutines which provably (Sat) block forever on spawner-local channels, per `docs/superpowers/specs/2026-07-26-phase7-goroutine-leaks-design.md`.

**Architecture:** Extend the existing Effects lattice with closure awareness (`Root::FreeVar` + bindings-aware rebase + Make/Load rooting in `resolve_loc`), then add `goverify-checkers/src/leak.rs` implementing only `Checker::obligations`: relational candidate generation + escape walk + counterpart matching in Rust, discharged by a single conjoined SMT query (spawner encoding + prefix-renamed spawned-callee encoding + capacity conjunct). No extractor or `.gvir` schema change.

**Tech Stack:** Rust workspace (goverify-ir, goverify-analysis, goverify-checkers), Z3 via goverify-solver, Go corpus fixtures under `testdata/corpus/leak`.

## Global Constraints

- Determinism is the root invariant: no map-iteration order reaching output; sort before emitting. All new maps keyed for output are `BTreeMap`/`BTreeSet`.
- Parsers/consumers of bytes the analyzer didn't write must reject or degrade, never panic (fuzzed `.gvir` reaches all new code paths). Every new lookup is total: missing/out-of-range ids degrade to `Loc::unknown()` / skip.
- Errors degrade, never die: any unanalyzable shape ⇒ no finding (bug-finder: only Sat reports).
- `Finding.message` / `Obligation.message` must NEVER embed a source position (fingerprint invariant, `checker.rs:20-26`).
- No new workspace dependencies. No `.gvir` schema change (no `schema_version` bump). `SCC_CACHE_VERSION` 2→3 exactly once (Task 3).
- Run `mise run fmt` and `mise run lint` before each commit. Commits are unsigned in this sandbox (memory: goverify-sandbox-environment); message style `phase7: <what>`.
- The gitleaks pre-commit hook runs automatically; use `mise x -- cargo ...` if bare cargo is not on PATH.
- All cargo test invocations may need `mise x -- ` prefix depending on shell; `mise run test` runs the whole suite.

---

### Task 1: `Function::free_vars` in goverify-ir

**Files:**
- Modify: `crates/goverify-ir/src/func.rs` (Function struct, ~line 75)
- Modify: `crates/goverify-ir/src/lower.rs` (Function lowering, ~lines 35-140)
- Test: `crates/goverify-ir/src/lower.rs` (in-file `#[cfg(test)]` tests at bottom)

**Interfaces:**
- Consumes: existing `.gvir` aux values with `kind == "FreeVar"` (already emitted by the extractor in `fn.FreeVars` order, `extractor/emit.go:448`).
- Produces: `pub free_vars: Vec<ValueId>` on `goverify_ir::Function` — free-variable value ids in wire (aux-emission) order, exactly parallel to how `params` lists param ids. Task 2's `resolve_loc` and Task 4's rebase depend on this ordinal order matching `MakeClosure.bindings` order (go/ssa guarantees `Bindings[i]` corresponds to `FreeVars[i]`).

- [ ] **Step 1: Write the failing test**

In `crates/goverify-ir/src/lower.rs`'s test module (mirror the style of `param_id_zero_does_not_clobber_opaque_sentinel` around line 864 — hand-built `gvir::Function` via `Program::from_packages`):

```rust
#[test]
fn free_var_aux_values_populate_free_vars_in_order() {
    let p = Program::from_packages(vec![gvir::Package {
        import_path: "t".into(),
        functions: vec![gvir::Function {
            id: "t.F$1".into(),
            aux: vec![
                gvir::AuxValue {
                    id: 1,
                    kind: "FreeVar".into(),
                    repr: "ch".into(),
                    ..Default::default()
                },
                gvir::AuxValue {
                    id: 2,
                    kind: "Const".into(),
                    ..Default::default()
                },
                gvir::AuxValue {
                    id: 3,
                    kind: "FreeVar".into(),
                    repr: "n".into(),
                    ..Default::default()
                },
            ],
            blocks: vec![gvir::BasicBlock {
                index: 0,
                instrs: vec![gvir::Instruction {
                    kind: "Return".into(),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        }],
        ..Default::default()
    }]);
    let f = p.func(p.lookup_func("t.F$1").unwrap()).unwrap();
    assert_eq!(
        f.free_vars,
        vec![ValueId(1), ValueId(3)],
        "free_vars must list FreeVar aux ids in emission order, skipping non-FreeVar aux"
    );
    assert_eq!(f.value(ValueId(1)).kind, ValueKind::FreeVar);
}

#[test]
fn free_var_aux_id_zero_is_skipped() {
    // id 0 is the reserved opaque slot — a fuzzed FreeVar aux with id 0
    // must not enter free_vars (mirrors the params id!=0 guard).
    let p = Program::from_packages(vec![gvir::Package {
        import_path: "t".into(),
        functions: vec![gvir::Function {
            id: "t.F$1".into(),
            aux: vec![gvir::AuxValue {
                id: 0,
                kind: "FreeVar".into(),
                ..Default::default()
            }],
            blocks: vec![gvir::BasicBlock {
                index: 0,
                instrs: vec![gvir::Instruction {
                    kind: "Return".into(),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        }],
        ..Default::default()
    }]);
    let f = p.func(p.lookup_func("t.F$1").unwrap()).unwrap();
    assert!(f.free_vars.is_empty(), "id-0 FreeVar aux must be skipped");
}
```

Match the existing test module's imports/builders (some tests build `gvir::Package` inline exactly like this; reuse whatever local helpers exist).

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p goverify-ir free_var`
Expected: compile error — `free_vars` field does not exist.

- [ ] **Step 3: Implement**

In `crates/goverify-ir/src/func.rs`, add to `Function` (after `result_names`):

```rust
    /// Free-variable value ids in wire (aux-emission) order — parallel to
    /// go/ssa `fn.FreeVars`, so index i here corresponds to
    /// `Op::MakeClosure::bindings[i]` at every closure-creation site.
    /// Display/effects-resolution only — never hashed (hashes are computed
    /// over raw .gvir bytes in program.rs).
    pub free_vars: Vec<ValueId>,
```

In `crates/goverify-ir/src/lower.rs`, inside the aux loop (~line 69), collect ids under the same `a.id != 0 && slot exists` guard that writes the slot:

```rust
        let mut free_vars = Vec::new();
        for a in &gf.aux {
            let kind = match a.kind.as_str() { /* unchanged */ };
            if a.id != 0
                && let Some(slot) = values.get_mut(a.id as usize)
            {
                if matches!(kind, ValueKind::FreeVar) {
                    free_vars.push(ValueId(a.id));
                }
                *slot = ValueInfo { /* unchanged */ };
            }
        }
```

and add `free_vars,` to the `Function { ... }` construction (~line 125). If `cargo build` flags any other `Function { ... }` construction site (tests/fixtures), add `free_vars: Vec::new(),` there.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p goverify-ir`
Expected: PASS (including the two new tests and all existing lower/golden tests — this change must not alter any golden).

- [ ] **Step 5: Commit**

```bash
git add crates/goverify-ir/src/func.rs crates/goverify-ir/src/lower.rs
git commit -m "phase7: Function::free_vars (ordered FreeVar aux ids)"
```

---

### Task 2: `Root::FreeVar` + resolve_loc rooting extensions

**Files:**
- Modify: `crates/goverify-analysis/src/effects.rs` (Root enum ~line 51, `resolve_loc` ~line 215, `rebase`/`rebase_loc` ~lines 266-301, `collect` call sites ~lines 402/495/529, tests + proptest `arb_loc`)
- Possibly regenerate: goldens under `testdata/goldens/` if effects locs appear in summary dumps (see Step 6)

**Interfaces:**
- Consumes: `Function::free_vars` (Task 1).
- Produces:
  - `Root::FreeVar(u32)` variant (ordinal into `free_vars`), sorted between `Alloc` and `Unknown` in the derived `Ord`.
  - `resolve_loc` newly roots: `ValueKind::FreeVar` values at `Root::FreeVar(i)`; `Op::Make{..}` defs at `Root::Alloc(dst.0)` (alloc-site identity — previously Unknown, which made every `make(chan)` invisible); and walks `Op::Load`/`Op::Assign` as transparent hops (deref-collapsing: a cell and its loaded content share a Loc) under a new `MAX_LOC_HOPS: usize = 32` cap (the existing loop only bounded path growth; transparent hops need their own bound against malformed cyclic def chains).
  - `rebase` signature grows a second mapping: `pub fn rebase(callee: &Effects, caller_arg_loc: &dyn Fn(u32) -> Loc, caller_fv_loc: &dyn Fn(u32) -> Loc) -> Effects`. `Root::FreeVar(i)` re-roots through `caller_fv_loc(i)` exactly as `Param` does through `caller_arg_loc` (path concat, depth cap). Task 4 supplies real bindings; this task passes `&|_| Loc::unknown()` at every existing call site.

- [ ] **Step 1: Write the failing tests**

Add to `effects.rs`'s test module:

```rust
    #[test]
    fn make_chan_dst_roots_at_alloc_site() {
        // t.F: v2 = MakeChan; Send v2 — the send's Loc must root at
        // Alloc(2), not Unknown (pre-phase-7 behavior).
        let mk = gvir_make_chan(2, 1); // register 2, cap operand id 1 (helper below)
        let mut send = instr("Send");
        send.operands = vec![2, 1];
        let p = Program::from_packages(vec![pkg(
            "t",
            vec![func("t.F", vec![block(0, vec![mk, send, instr("Return")], vec![])])],
        )]);
        let g = CallGraph::build(&p);
        let e = collect(&p, p.lookup_func("t.F").unwrap(), &g, &|_| Effects::empty());
        let want = Loc { root: Root::Alloc(2), path: vec![] };
        assert!(
            e.chan_ops.get(&want).is_some_and(|ops| ops.contains(&ChanOp::Send)),
            "Send on a make(chan) register must root at the Make's alloc site: {e:?}"
        );
    }

    #[test]
    fn free_var_value_roots_at_free_var_ordinal() {
        // Anon fn t.F$1 with FreeVar aux id 1; Send on it.
        // resolve_loc must produce Root::FreeVar(0).
        let mut send = instr("Send");
        send.operands = vec![1, 1];
        let p = Program::from_packages(vec![pkg(
            "t",
            vec![func_with_free_vars(
                "t.F$1",
                vec![1],
                vec![block(0, vec![send, instr("Return")], vec![])],
            )],
        )]);
        let g = CallGraph::build(&p);
        let e = collect(&p, p.lookup_func("t.F$1").unwrap(), &g, &|_| Effects::empty());
        let want = Loc { root: Root::FreeVar(0), path: vec![] };
        assert!(
            e.chan_ops.get(&want).is_some_and(|ops| ops.contains(&ChanOp::Send)),
            "{e:?}"
        );
    }

    #[test]
    fn load_hop_collapses_cell_and_content() {
        // v2 = Alloc (cell); v3 = Load v2; Send v3 — the loaded channel
        // shares the cell's Loc: Root::Alloc(2).
        let mut alloc = instr("Alloc");
        alloc.register = 2;
        alloc.sem = Some(Sem::Alloc(gvir::AllocSem { heap: true }));
        let mut load = instr("Load");
        load.register = 3;
        load.operands = vec![2];
        let mut send = instr("Send");
        send.operands = vec![3, 1];
        let p = Program::from_packages(vec![pkg(
            "t",
            vec![func("t.F", vec![block(0, vec![alloc, load, send, instr("Return")], vec![])])],
        )]);
        let g = CallGraph::build(&p);
        let e = collect(&p, p.lookup_func("t.F").unwrap(), &g, &|_| Effects::empty());
        let want = Loc { root: Root::Alloc(2), path: vec![] };
        assert!(
            e.chan_ops.get(&want).is_some_and(|ops| ops.contains(&ChanOp::Send)),
            "{e:?}"
        );
    }

    #[test]
    fn rebase_free_var_reroots_through_fv_loc() {
        let mut callee = Effects::empty();
        callee
            .chan_ops
            .entry(Loc { root: Root::FreeVar(0), path: vec![] })
            .or_default()
            .insert(ChanOp::Send);
        let rebased = rebase(
            &callee,
            &|_| Loc::unknown(),
            &|i| {
                assert_eq!(i, 0);
                Loc { root: Root::Alloc(7), path: vec![] }
            },
        );
        assert!(rebased.chan_ops.contains_key(&Loc { root: Root::Alloc(7), path: vec![] }));
    }

    #[test]
    fn rebase_free_var_without_mapping_degrades_to_unknown() {
        let mut callee = Effects::empty();
        callee
            .chan_ops
            .entry(Loc { root: Root::FreeVar(3), path: vec![] })
            .or_default()
            .insert(ChanOp::Send);
        let rebased = rebase(&callee, &|_| Loc::unknown(), &|_| Loc::unknown());
        assert_eq!(rebased.chan_ops.keys().collect::<Vec<_>>(), vec![&Loc::unknown()]);
    }
```

Helpers needed in `crates/goverify-analysis/src/testpkg.rs` (add now; Tasks 4/5 reuse them):

```rust
/// `register = make(chan T, <cap value id>)` — MakeChan wire shape:
/// operands [cap], per lower.rs's `"MakeChan" => Op::Make{args: vec![v(0)]}`.
pub fn gvir_make_chan(register: u32, cap_operand: u32) -> gvir::Instruction {
    gvir::Instruction {
        kind: "MakeChan".into(),
        register,
        operands: vec![cap_operand],
        ..Default::default()
    }
}

/// Function with FreeVar aux values (ids in order) and no params.
pub fn func_with_free_vars(
    id: &str,
    free_var_ids: Vec<u32>,
    blocks: Vec<gvir::BasicBlock>,
) -> gvir::Function {
    gvir::Function {
        id: id.into(),
        aux: free_var_ids
            .into_iter()
            .map(|fv| gvir::AuxValue {
                id: fv,
                kind: "FreeVar".into(),
                ..Default::default()
            })
            .collect(),
        blocks,
        ..Default::default()
    }
}
```

(`Sem`/`AllocSem` imports: follow the existing `lock_on_param_gets_param_rooted_loc` test which already imports `goverify_extract::gvir` + `Sem` locally.)

Also extend the proptest generator `arb_loc()` (~line 813) with `(0u32..4).prop_map(Root::FreeVar),` so the lattice-law property tests cover the new variant.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p goverify-analysis effects`
Expected: compile errors (no `Root::FreeVar`, `rebase` arity) — then after stubbing, assertion failures on the Make/Load rooting tests.

- [ ] **Step 3: Implement**

In `effects.rs`:

1. Add `FreeVar(u32)` to `Root` between `Alloc` and `Unknown`, with a doc comment: ordinal into the enclosing function's `free_vars`; identity is only meaningful inside that function; `rebase` maps it through closure bindings or degrades to `Unknown`.
2. `resolve_loc`: add `pub const MAX_LOC_HOPS: usize = 32;` and a `hops` counter incremented every loop iteration, degrading to `Loc::unknown()` when exceeded (keep the existing `path.len() > MAX_LOC_DEPTH` check too). Add the `ValueKind::FreeVar` arm (position in `f.free_vars`, mirroring the Param arm), and extend the def match:

```rust
        match defs.get(&cur) {
            Some(Op::FieldAddr { base, field, .. }) => {
                path.insert(0, *field);
                cur = *base;
            }
            Some(Op::Alloc { dst, .. }) | Some(Op::Make { dst, .. }) => {
                return Loc { root: Root::Alloc(dst.0), path };
            }
            // Deref-collapsing transparent hops: a cell and the value
            // loaded from it share one Loc (a captured `ch := make(chan)`
            // is an Alloc cell + Store + Load in SSA; collapsing keeps
            // the spawner-side and closure-side identities equal). Sound
            // for effects matching: over-merging can only suppress leak
            // findings, never manufacture them.
            Some(Op::Load { addr, .. }) => cur = *addr,
            Some(Op::Assign { src, .. }) => cur = *src,
            _ => return Loc::unknown(),
        }
```

Update `resolve_loc`'s doc comment to describe all three new behaviors.
3. `rebase`/`rebase_map`/`rebase_loc`: add the `caller_fv_loc: &dyn Fn(u32) -> Loc` parameter threaded through; `rebase_loc` gets:

```rust
        Root::FreeVar(i) => {
            let caller = caller_fv_loc(*i);
            let mut path = caller.path.clone();
            path.extend(loc.path.iter().copied());
            Loc { root: caller.root, path }.capped()
        }
```

4. Update every existing `rebase(` call site (three in `collect`/`record_go_defer_callee`, plus tests) to pass `&|_| Loc::unknown()` as the new argument.
5. `Effects::top()` is unchanged (FreeVar never appears in top — it's a finite identity, not an op).

- [ ] **Step 4: Run the analysis crate tests**

Run: `cargo test -p goverify-analysis`
Expected: PASS (new tests + existing unit/prop tests).

- [ ] **Step 5: Run the wider suite and settle golden churn**

Run: `mise run test` (or at minimum `cargo test -p goverify-ir --test lower_golden --test lower_corpus && cargo test -p goverify-analysis --test engine_corpus && cargo test -p goverify-checkers`)

The Make-rooting change alters effects Locs in converged summaries, so any golden embedding a summary/effects dump (e.g. `engine_corpus`, `debug_integration`) may drift. For each drifted golden: eyeball the diff (expect ONLY chan/lock-op Loc keys moving from `Unknown` to `Alloc(n)` buckets — anything else is a bug), then regenerate with `UPDATE_GOLDENS=1 cargo test ...`. **Findings-count goldens (nil/bounds corpus) must NOT change** — nil/bounds never read effects; if they drift, stop and debug.

- [ ] **Step 6: Commit**

```bash
git add crates/goverify-analysis/src/effects.rs crates/goverify-analysis/src/testpkg.rs testdata/goldens
git commit -m "phase7: Root::FreeVar + Make/Load rooting in resolve_loc; fv-aware rebase"
```

---

### Task 3: SCC cache codec for `Root::FreeVar` + version bump

**Files:**
- Modify: `crates/goverify-analysis/src/scc_cache.rs` (`SCC_CACHE_VERSION` line 24, `encode_root`/`decode_root` ~lines 329-355, codec tests)

**Interfaces:**
- Consumes: `Root::FreeVar(u32)` (Task 2).
- Produces: wire tag `4` for `FreeVar` (`u32` ordinal payload, same shape as `Param`/`Alloc`); `SCC_CACHE_VERSION = 3` (rotates every cached entry — required because Task 2 changed both the Root value-space and `resolve_loc` semantics, so version-2 entries hold stale effects).

- [ ] **Step 1: Write the failing test**

Find the existing codec roundtrip test(s) in `scc_cache.rs` (search for `encode_effects`/`roundtrip` in its test module) and add:

```rust
    #[test]
    fn root_free_var_roundtrips() {
        let mut e = Effects::empty();
        e.chan_ops
            .entry(Loc { root: Root::FreeVar(5), path: vec![1] })
            .or_default()
            .insert(ChanOp::Send);
        let mut out = Vec::new();
        encode_effects(&e, &mut out);
        let mut input = out.as_slice();
        assert_eq!(decode_effects(&mut input), Some(e), "FreeVar root must roundtrip");
    }
```

If a proptest-based codec roundtrip exists, extend its `Root` strategy with `FreeVar` instead/additionally.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p goverify-analysis scc_cache`
Expected: FAIL — `encode_root` has no `FreeVar` arm (non-exhaustive match compile error).

- [ ] **Step 3: Implement**

```rust
        Root::FreeVar(i) => {
            out.push(4);
            put_u32(out, *i);
        }
```

and in `decode_root`:

```rust
        4 => Some(Root::FreeVar(take_u32(input)?)),
```

Bump `const SCC_CACHE_VERSION: u32 = 2;` → `3`, and extend its comment: `// 3: phase-7 Root::FreeVar + Make/Load rooting in resolve_loc`.

- [ ] **Step 4: Run tests**

Run: `cargo test -p goverify-analysis`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/goverify-analysis/src/scc_cache.rs
git commit -m "phase7: scc codec FreeVar root (tag 4); SCC_CACHE_VERSION 3"
```

---

### Task 4: Closure-aware rebase in `effects::collect`

**Files:**
- Modify: `crates/goverify-analysis/src/effects.rs` (`collect` ~line 431, `record_go_defer_callee` ~line 380; new `closure_bindings` helper; make `cyclic_blocks` `pub(crate)` → actually `pub` and re-export, Task 6 needs it)
- Modify: `crates/goverify-analysis/src/lib.rs` (re-export `cyclic_blocks` alongside the existing effects exports)
- Modify: `crates/goverify-analysis/src/testpkg.rs` (MakeClosure + Go-through-closure builders)

**Interfaces:**
- Consumes: `Root::FreeVar` + 3-arg `rebase` (Task 2); `Op::MakeClosure { dst, func, bindings }` (existing IR).
- Produces:
  - `pub fn closure_bindings(f: &Function) -> HashMap<FuncId, Option<Vec<ValueId>>>` in effects.rs — for each function that `f` creates a closure over: `Some(bindings)` if exactly one `MakeClosure` site exists, `None` if ≥2 (ambiguous — degrade). Exported from the crate root (Task 5's checker reuses it).
  - `collect` rebases every static callee's `FreeVar`-rooted effects through that callee's bindings (or Unknown when absent/ambiguous). Lowering drops the callee-slot value id (`lower.rs` operand skip), so bindings are found by this scan — never by operand inspection.
  - `pub use effects::{ .. , closure_bindings, cyclic_blocks}` from lib.rs.
  - testpkg gains:

```rust
/// `register = MakeClosure <fn_aux> [bindings…]` — wire shape per
/// lower.rs ~line 441: operands [fn, bindings…], fn is a Function aux.
pub fn make_closure(register: u32, fn_aux_operand: u32, bindings: Vec<u32>) -> gvir::Instruction {
    let mut operands = vec![fn_aux_operand];
    operands.extend(bindings);
    gvir::Instruction {
        kind: "MakeClosure".into(),
        register,
        operands,
        ..Default::default()
    }
}

/// Function with arbitrary aux values (Function refs, consts, …).
pub fn func_with_aux(
    id: &str,
    aux: Vec<gvir::AuxValue>,
    blocks: Vec<gvir::BasicBlock>,
) -> gvir::Function { /* same shape as func(), plus aux */ }

pub fn fn_aux(id: u32, target: &str) -> gvir::AuxValue {
    gvir::AuxValue {
        id,
        kind: "Function".into(),
        repr: target.into(),
        ..Default::default()
    }
}

pub fn const_int_aux(id: u32, v: i64) -> gvir::AuxValue {
    gvir::AuxValue {
        id,
        kind: "Const".into(),
        r#const: Some(gvir::ConstValue {
            value: Some(gvir::const_value::Value::Int(v)),
        }),
        ..Default::default()
    }
}
```

(Adjust `ConstValue` field shapes to the generated gvir types — check an existing const-building test for the exact prost struct.)

- [ ] **Step 1: Write the failing tests**

In effects.rs tests — the canonical closure-capture shape end-to-end at the effects level:

```rust
    #[test]
    fn go_closure_send_rebases_free_var_through_bindings() {
        // t.F: v2 = make(chan); v3 = MakeClosure t.F$1 [v2]; go t.F$1()
        // t.F$1: FreeVar fv1; Send fv1
        // F's effects must contain Send at Alloc(2) — the closure's
        // FreeVar(0) rebased through binding v2.
        let mut go = instr("Go");
        go.operands = vec![3]; // callee slot = closure register (dropped by lower)
        go.sem = Some(Sem::Call(gvir::CallSem {
            static_callee: "t.F$1".into(),
            ..Default::default()
        }));
        let mut send = instr("Send");
        send.operands = vec![1, 1];
        let p = Program::from_packages(vec![pkg(
            "t",
            vec![
                func_with_aux(
                    "t.F",
                    vec![fn_aux(1, "t.F$1")],
                    vec![block(
                        0,
                        vec![
                            gvir_make_chan(2, 1),
                            make_closure(3, 1, vec![2]),
                            go,
                            instr("Return"),
                        ],
                        vec![],
                    )],
                ),
                func_with_free_vars(
                    "t.F$1",
                    vec![1],
                    vec![block(0, vec![send, instr("Return")], vec![])],
                ),
            ],
        )]);
        let g = CallGraph::build(&p);
        let f1 = p.lookup_func("t.F$1").unwrap();
        // Drive with the real callee effects, as the fixpoint would:
        let f1_effects = collect(&p, f1, &g, &|_| Effects::empty());
        let e = collect(&p, p.lookup_func("t.F").unwrap(), &g, &|c| {
            if c == f1 { f1_effects.clone() } else { Effects::empty() }
        });
        let want = Loc { root: Root::Alloc(2), path: vec![] };
        assert!(
            e.chan_ops.get(&want).is_some_and(|ops| ops.contains(&ChanOp::Send)),
            "closure-captured send must rebase to the spawner's alloc site: {e:?}"
        );
        assert_eq!(e.spawns, Spawns::Bounded);
    }

    /// Two MakeClosure sites for the same target fn (fuzz shape) —
    /// bindings are ambiguous, FreeVar-rooted effects degrade to Unknown.
    #[test]
    fn ambiguous_make_closure_degrades_free_var_to_unknown() {
        let mut go = instr("Go");
        go.operands = vec![4];
        go.sem = Some(Sem::Call(gvir::CallSem {
            static_callee: "t.F$1".into(),
            ..Default::default()
        }));
        let mut send = instr("Send");
        send.operands = vec![1, 1];
        let p = Program::from_packages(vec![pkg(
            "t",
            vec![
                func_with_aux(
                    "t.F",
                    vec![fn_aux(1, "t.F$1")],
                    vec![block(
                        0,
                        vec![
                            gvir_make_chan(2, 1),
                            gvir_make_chan(5, 1),
                            make_closure(3, 1, vec![2]),
                            make_closure(4, 1, vec![5]),
                            go,
                            instr("Return"),
                        ],
                        vec![],
                    )],
                ),
                func_with_free_vars(
                    "t.F$1",
                    vec![1],
                    vec![block(0, vec![send, instr("Return")], vec![])],
                ),
            ],
        )]);
        let g = CallGraph::build(&p);
        let f1 = p.lookup_func("t.F$1").unwrap();
        let f1_effects = collect(&p, f1, &g, &|_| Effects::empty());
        let e = collect(&p, p.lookup_func("t.F").unwrap(), &g, &|c| {
            if c == f1 { f1_effects.clone() } else { Effects::empty() }
        });
        let send_at = |loc: &Loc| e.chan_ops.get(loc).is_some_and(|o| o.contains(&ChanOp::Send));
        assert!(send_at(&Loc::unknown()), "ambiguous bindings must degrade: {e:?}");
        assert!(!send_at(&Loc { root: Root::Alloc(2), path: vec![] }));
        assert!(!send_at(&Loc { root: Root::Alloc(5), path: vec![] }));
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p goverify-analysis effects`
Expected: first test FAILS — Send lands at `Loc::unknown()` (no bindings mapping yet).

- [ ] **Step 3: Implement**

In effects.rs:

```rust
/// One `MakeClosure` site per target function ⇒ its bindings; two or
/// more ⇒ `None` (ambiguous — a well-formed lowering emits exactly one
/// MakeClosure per anon fn per enclosing function, so ambiguity is a
/// fuzz/malformed shape and degrades). Lowering drops the go/call
/// instruction's callee-slot value id, so bindings can only be found by
/// this scan, never by operand inspection.
pub fn closure_bindings(f: &Function) -> HashMap<FuncId, Option<Vec<ValueId>>> {
    let mut m: HashMap<FuncId, Option<Vec<ValueId>>> = HashMap::new();
    for b in &f.blocks {
        for ins in &b.instrs {
            if let Op::MakeClosure { func, bindings, .. } = &ins.op {
                m.entry(*func)
                    .and_modify(|e| *e = None)
                    .or_insert_with(|| Some(bindings.clone()));
            }
        }
    }
    m
}

fn fv_loc(f: &Function, cb: &HashMap<FuncId, Option<Vec<ValueId>>>, c: FuncId, i: u32) -> Loc {
    match cb.get(&c) {
        Some(Some(bindings)) => bindings
            .get(i as usize)
            .map_or(Loc::unknown(), |&bv| resolve_loc(f, bv)),
        _ => Loc::unknown(),
    }
}
```

In `collect`: build `let cb = closure_bindings(f);` once, and change the three rebase sites:
- static `Op::Call`: `e.join(&rebase(&effects_of(*c), &|i| arg_loc(f, args, i), &|i| fv_loc(f, &cb, *c, i)));`
- `record_go_defer_callee`: thread `cb: &HashMap<FuncId, Option<Vec<ValueId>>>` as a parameter and use the same two closures for its `Callee::Static` else-branch rebase.
- the call-graph diff pass keeps `&|_| Loc::unknown()` for both mappings (no per-site context).

Make `cyclic_blocks` `pub` with a doc note (Task 6 uses it for the buffered-cyclic degrade), and re-export `closure_bindings, cyclic_blocks` from `lib.rs`.

- [ ] **Step 4: Run tests**

Run: `cargo test -p goverify-analysis`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/goverify-analysis/src/effects.rs crates/goverify-analysis/src/lib.rs crates/goverify-analysis/src/testpkg.rs
git commit -m "phase7: closure-aware effects rebase (MakeClosure bindings -> FreeVar roots)"
```

---

### Task 5: leak.rs — candidate generation + escape walk

**Files:**
- Create: `crates/goverify-checkers/src/leak.rs`
- Modify: `crates/goverify-checkers/src/lib.rs` (add `mod leak;` + `pub use leak::LeakChecker;` — do NOT add to `default_checkers()` yet; Task 7 does)
- Modify: `crates/goverify-checkers/src/testfix.rs` (chan-fixture builders as needed — reuse goverify-analysis testpkg patterns; testfix is this crate's `#[cfg(test)]` fixture home)

**Interfaces:**
- Consumes: `goverify_analysis::{closure_bindings, resolve_loc, rebase, Effects, ChanOp, Loc, Root}`; `goverify_ir::{Op, Callee, Function, FuncId, Program, ValueId, MakeKind, Pos}`.
- Produces (all `pub(crate)` in leak.rs, unit-tested directly; Task 7 composes them):

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CandKind { Send, Recv, Select }

#[derive(Debug, Clone)]
pub(crate) struct Candidate {
    pub go_block: usize,          // block index of the Op::Go in f
    pub go_pos: Option<Pos>,
    pub callee: FuncId,           // spawned function (has a body)
    pub op_block: usize,          // block index of the blocking op in callee
    pub kind: CandKind,
    pub alloc_value: ValueId,     // Root::Alloc id in f (escape/capacity subject)
    pub spawner_loc: Loc,         // Alloc-rooted Loc in f (counterpart key)
    /// Select candidates only: every arm's spawner-side Loc, paired with
    /// the arm's dir (1 = send, 2 = recv). Empty for Send/Recv candidates.
    pub arm_locs: Vec<(u32, Loc)>,
}

pub(crate) fn candidates(p: &Program, f: &Function) -> Vec<Candidate>;
pub(crate) fn escapes(f: &Function, ch: ValueId) -> bool;
```

- `candidates`: scans `f`'s blocks in index order, instrs in order. For each `Op::Go { callee: Callee::Static(c), args }` where `p.func(*c)` has a body: build the two per-site mappings (args via `resolve_loc`, free vars via `closure_bindings(f)` + the Task-4 `fv_loc` logic — both exported from goverify-analysis). Then scan the **callee's own body** (v1 detects only syntactically-direct blocking ops; transitive ops through helpers are candidates for suppression only, never for reporting): for `Op::Send{chan,..}` / `Op::Recv{chan,..}` → resolve `chan` in the callee, map its root through the site mappings (Param(i)→arg loc, FreeVar(i)→binding loc, else Unknown), keep iff the mapped root is `Root::Alloc(a)`. For `Op::Select{blocking: true, arms}` → every arm's chan must map to a `Root::Alloc` loc; the Candidate's `alloc_value`/`spawner_loc` is the FIRST arm's and `arm_locs` carries all of them with their dirs (Task 6 re-checks every arm). Non-blocking selects and dynamic/invoke/builtin go-callees produce nothing. (If `fv_loc` ended up private in Task 4, make it `pub` there rather than duplicating the bindings logic here.)
- `escapes`: whitelist-based use-walk (spec §2 rule 2, strict form). Two alias sets grown to fixpoint: `chans` (seeded with `ch`; grown through `Load{addr∈chans}→dst`, `Assign/Convert{src∈chans}→dst`, `Phi{any edge∈chans}→dst`, `Store{addr∈chans}→val` — the cell-content rule) and `closures` (`MakeClosure{bindings∩chans≠∅}→dst`). Then one scan; escape iff any member of either set appears in a non-whitelisted role:
  - **whitelisted:** `Send.chan`, `Recv.chan`, `CloseChan.chan`, `SelectArm.chan`; `Store` whose `addr∈chans` (defining/refilling the cell); alias-extender ops themselves; `MakeClosure` bindings; args of `Go`/`Defer` with `Callee::Static` or `Callee::Builtin("close")`; `Call{Callee::Builtin(n)}` args for `n ∈ {"close","len","cap"}`.
  - **escapes (everything else), explicitly including:** `Return.vals`; `Store.val` where `addr∉chans`; `Send.val`/`SelectArm.send` (the channel sent as a value); args of any plain `Call` (static included — a summarized callee could store the channel into the heap, which effects don't model; strict rule per spec amendment in Task 10); args of `Go`/`Defer` with dynamic/invoke callees; `Callee::Dynamic{value}` where value is a tracked closure or chan; `MakeInterface.src`; any operand position of any other op (`BinOp`, `Index`, `Lookup`, `Make` args, `TypeAssert`, `Extract`, `Panic`, …) — implement the fallback with an exhaustive-by-default helper that enumerates an op's operand `ValueId`s and reports membership.

- [ ] **Step 1: Add the wire builders to testfix.rs**

goverify-analysis's `testpkg` is crate-private, so COPY the Task-2/4 builders into `crates/goverify-checkers/src/testfix.rs` (duplication is the established pattern — testfix's header says it exists so checkers don't reach into analysis internals): `gvir_make_chan`, `func_with_free_vars`, `func_with_aux`, `fn_aux`, `make_closure`, `go_call` (exists in analysis testpkg; port it), plus these new ones. Wire shapes verified against `lower.rs`:

```rust
/// `<-ch` — Recv lowers from kind "UnOp" with Sem::Unop{op: "<-"}
/// (lower.rs ~line 197), register = dst, operands [chan].
pub(crate) fn recv(register: u32, chan_operand: u32) -> gvir::Instruction {
    gvir::Instruction {
        kind: "UnOp".into(),
        register,
        operands: vec![chan_operand],
        sem: Some(Sem::Unop(gvir::UnOpSem {
            op: "<-".into(),
            comma_ok: false,
        })),
        ..Default::default()
    }
}

/// `ch <- val` — operands [chan, val] (lower.rs "Send").
pub(crate) fn send(chan_operand: u32, val_operand: u32) -> gvir::Instruction {
    gvir::Instruction {
        kind: "Send".into(),
        operands: vec![chan_operand, val_operand],
        ..Default::default()
    }
}

/// `*addr = val` — operands [addr, val] (lower.rs "Store").
pub(crate) fn store(addr_operand: u32, val_operand: u32) -> gvir::Instruction {
    gvir::Instruction {
        kind: "Store".into(),
        operands: vec![addr_operand, val_operand],
        ..Default::default()
    }
}

/// `return vals…` — operands are the returned value ids.
pub(crate) fn ret(vals: Vec<u32>) -> gvir::Instruction {
    gvir::Instruction {
        kind: "Return".into(),
        operands: vals,
        ..Default::default()
    }
}

/// Select over `states` (dir 1 = send, 2 = recv) — kind "Select" +
/// SelectSem{states, blocking}; needs a register (lower bails without dst).
pub(crate) fn select(
    register: u32,
    states: Vec<(u32, u32, u32)>, // (dir, chan_operand, send_operand-or-0)
    blocking: bool,
) -> gvir::Instruction {
    gvir::Instruction {
        kind: "Select".into(),
        register,
        sem: Some(Sem::Select(gvir::SelectSem {
            blocking,
            states: states
                .into_iter()
                .map(|(dir, chan_operand, send_operand)| gvir::SelectState {
                    dir,
                    chan_operand,
                    send_operand,
                })
                .collect(),
        })),
        ..Default::default()
    }
}
```

(`go_call` with args: extend the ported builder so `go_call_args("t.G", vec![2])` produces operands `[0, 2]` — callee slot then args, matching lower's skip(1).)

- [ ] **Step 2: Write the failing tests**

Two fully-worked examples; the rest follow the same skeleton with the listed op substitutions:

```rust
    /// F: v2 = make(chan); go t.G(v2). G(p1): p1 <- x.
    /// One Send candidate rooted at F's alloc site.
    #[test]
    fn direct_param_send_yields_send_candidate() {
        let p = Program::from_packages(vec![pkg(
            "t",
            vec![
                func_with_aux(
                    "t.F",
                    vec![const_int_aux(1, 0)], // cap 0 operand
                    vec![block(
                        0,
                        vec![gvir_make_chan(2, 1), go_call_args("t.G", vec![2]), ret(vec![])],
                        vec![],
                    )],
                ),
                func_with_params(
                    "t.G",
                    vec![gvir::Param { id: 1, name: "c".into(), r#type: 0 }],
                    vec![block(0, vec![send(1, 1), ret(vec![])], vec![])],
                ),
            ],
        )]);
        let f = p.func(p.lookup_func("t.F").unwrap()).unwrap();
        let cands = candidates(&p, f);
        assert_eq!(cands.len(), 1, "exactly one candidate: {cands:?}");
        assert_eq!(cands[0].kind, CandKind::Send);
        assert_eq!(cands[0].alloc_value, ValueId(2));
        assert_eq!(cands[0].spawner_loc, Loc { root: Root::Alloc(2), path: vec![] });
        assert_eq!(cands[0].callee, p.lookup_func("t.G").unwrap());
    }

    /// Return of the channel is an escape.
    #[test]
    fn return_of_channel_escapes() {
        let p = Program::from_packages(vec![pkg(
            "t",
            vec![func_with_aux(
                "t.F",
                vec![const_int_aux(1, 0)],
                vec![block(0, vec![gvir_make_chan(2, 1), ret(vec![2])], vec![])],
            )],
        )]);
        let f = p.func(p.lookup_func("t.F").unwrap()).unwrap();
        assert!(escapes(f, ValueId(2)), "returned channel must escape");
    }
```

Remaining tests, same skeleton, one per rule (write each out — the deltas are single instructions):

```rust
    // candidates():
    #[test] fn closure_free_var_recv_yields_recv_candidate() {}
    //   F: make_chan(2,1); make_closure(3, fn_aux, vec![2]); go "t.F$1" (operands [3]);
    //   t.F$1 = func_with_free_vars with recv(2, 1). Expect CandKind::Recv at Alloc(2).
    #[test] fn param_rooted_channel_yields_no_candidate() {}
    //   F has param id 1 (no make); go_call_args("t.G", vec![1]) → empty.
    #[test] fn dynamic_go_callee_yields_no_candidate() {}
    //   go with empty static_callee/builtin (Callee::Dynamic) → empty.
    #[test] fn bodyless_go_callee_yields_no_candidate() {}
    //   go_call_args("ext.NoBody", vec![2]) with no ext.NoBody function → empty.
    #[test] fn nonblocking_select_yields_no_candidate() {}
    //   callee body select(2, vec![(1, 1, 1)], false) → empty.
    #[test] fn blocking_select_all_alloc_arms_yields_select_candidate() {}
    //   two makes in F, both bound into the closure; callee selects over both
    //   FreeVars, blocking=true → CandKind::Select with arm_locs.len() == 2.

    // escapes():
    #[test] fn plain_chan_op_uses_do_not_escape() {}
    //   gvir_make_chan(2, 1); send(2, 1); recv(3, 2); close via a
    //   Call{Builtin "close"} with operands [0, 2] → false.
    #[test] fn store_to_foreign_addr_escapes() {}
    //   alloc register 3 (other cell) + store(3→no: store(addr=3, val=2)) → true.
    #[test] fn store_into_own_cell_does_not_escape_and_content_aliases() {}
    //   Alloc cell reg 2; store(2, 3) where 3 = make_chan register; escapes(f, ValueId(2)) → false.
    #[test] fn plain_call_arg_escapes() {}
    //   call("t.G") with operands [0, 2] → true (strict rule).
    #[test] fn static_go_arg_does_not_escape() {}
    //   go_call_args("t.G", vec![2]) → false.
    #[test] fn closure_value_returned_escapes() {}
    //   make_closure(3, aux, vec![2]); ret(vec![3]) → true.
    #[test] fn make_interface_escapes() {}
    //   kind "MakeInterface", register 3, operands [2] → true.
```

- [ ] **Step 3: Run to verify failure**

Run: `cargo test -p goverify-checkers leak`
Expected: compile error (module doesn't exist), then failures as functions are stubbed.

- [ ] **Step 4: Implement `candidates` and `escapes`** as specified in Interfaces. Keep both functions total and panic-free: every id lookup bounds-checked; unknown shapes in `candidates` degrade to "no candidate", unknown roles in `escapes` degrade to `true`.

- [ ] **Step 5: Run tests**

Run: `cargo test -p goverify-checkers leak`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/goverify-checkers/src/leak.rs crates/goverify-checkers/src/lib.rs crates/goverify-checkers/src/testfix.rs
git commit -m "phase7: leak checker candidate scan + escape walk"
```

---

### Task 6: leak.rs — counterpart matching + capacity classification

**Files:**
- Modify: `crates/goverify-checkers/src/leak.rs`

**Interfaces:**
- Consumes: `Candidate`/`CandKind` (Task 5); `Effects` from `summary_of(f).effects` (converged, includes every callee's + spawned goroutine's ops); `goverify_analysis::cyclic_blocks` (Task 4 export).
- Produces:

```rust
pub(crate) fn has_counterpart(env: &Effects, loc: &Loc, kind: CandKind) -> bool;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CapClass {
    Unbuffered,            // cap const 0 — reachability-only discharge
    BufferedConst(u64),    // cap const N>0, candidate op acyclic — ordinal conjunct
    Silent,                // non-const cap, or buffered send in a CFG cycle
}

pub(crate) fn cap_class(p: &Program, f: &Function, cand: &Candidate) -> CapClass;
```

- `has_counterpart` handles `Send`/`Recv` only (callers dispatch Select per-arm — see Task 7). Unblocker sets: blocked `Send` ⇒ `{Recv, Select, Close}`; blocked `Recv` ⇒ `{Send, Select, Close}` (`Close` conservatively counts for a blocked send too: it would panic-unblock, which is not "blocked forever"; `CandKind::Select` passed here returns `true` — suppress — as a defensive default). Check the candidate's `Loc` AND `Loc::unknown()` (may-alias rule): `env.chan_ops.get(loc).is_some_and(|ops| unblockers.iter().any(|u| ops.contains(u))) || <same at Loc::unknown()>`.
- `cap_class` — capacity of `cand.alloc_value` in `f`: if its def is `Op::Make{kind: MakeKind::Chan, args}` → `args[0]`'s `ValueKind::Const(ConstVal::Int(n))` (walk `Assign` chains via `crate::shared::canonical_value` first); if its def is `Op::Alloc` (cell form) → exactly one `Store{addr == alloc_value}` in `f` whose canonical val defs to `Make{Chan,..}` → that cap; anything else (no const, multiple stores, missing def) → non-const. Then: cap 0 ⇒ `Unbuffered`. cap N>0 with `kind == Send` and the candidate's `op_block` NOT cyclic in the callee (`cyclic_blocks(callee_fn)[op_block]`) ⇒ `BufferedConst(N)`. cap N>0 with `kind == Recv` ⇒ `Unbuffered`-equivalent reachability (a recv with zero senders anywhere blocks regardless of buffer — return `Unbuffered`). `kind == Select` ⇒ `Unbuffered` iff EVERY arm's `Root::Alloc(a)` channel has cap const 0 (extract `a` from each `arm_locs` entry's root), else `Silent` (v1). Buffered cyclic send / non-const ⇒ `Silent`.

- [ ] **Step 1: Write the failing tests**

`has_counterpart` tests build `Effects` values directly (no Program needed); two fully-worked examples:

```rust
    fn env_with(loc: Loc, ops: &[ChanOp]) -> Effects {
        let mut e = Effects::empty();
        e.chan_ops.entry(loc).or_default().extend(ops.iter().copied());
        e
    }

    #[test]
    fn send_candidate_suppressed_by_recv_at_same_loc() {
        let l = Loc { root: Root::Alloc(2), path: vec![] };
        let env = env_with(l.clone(), &[ChanOp::Send, ChanOp::Recv]);
        assert!(has_counterpart(&env, &l, CandKind::Send));
    }

    #[test]
    fn send_candidate_not_suppressed_by_sibling_send() {
        let l = Loc { root: Root::Alloc(2), path: vec![] };
        let env = env_with(l.clone(), &[ChanOp::Send, ChanOp::Make]);
        assert!(!has_counterpart(&env, &l, CandKind::Send), "Send/Make are not unblockers for a blocked send");
    }
```

Remaining tests, same shapes (write each out):

```rust
    #[test] fn send_candidate_suppressed_by_any_unknown_loc_unblocker() {}
    //   env has Recv at Loc::unknown(), candidate loc Alloc(2) → true.
    #[test] fn recv_candidate_suppressed_by_close() {}
    //   env Close at candidate loc, CandKind::Recv → true.
    #[test] fn recv_candidate_unsuppressed_by_recv() {}
    //   env Recv-only at candidate loc, CandKind::Recv → false.
    #[test] fn select_arm_dispatch_suppressed_if_any_arm_matched() {}
    //   arm_locs [(1, la), (2, lb)]; env has Recv at la only → the Task-7
    //   per-arm dispatch expression (any arm: has_counterpart with dir 1 →
    //   CandKind::Send, dir 2 → CandKind::Recv) is true (the send arm can
    //   pair). Empty env → false.
```

`cap_class` tests reuse Task 5's program builders (`gvir_make_chan` cap operand pointing at a `const_int_aux`; candidates built by hand or via `candidates()`):

```rust
    #[test] fn cap_class_unbuffered_from_const_zero() {}
    //   const_int_aux(1, 0); make_chan(2, 1) → CapClass::Unbuffered.
    #[test] fn cap_class_buffered_const_acyclic_send() {}
    //   const_int_aux(1, 3); Send candidate in a straight-line callee →
    //   CapClass::BufferedConst(3).
    #[test] fn cap_class_silent_for_nonconst_cap() {}
    //   cap operand is a param id → CapClass::Silent.
    #[test] fn cap_class_silent_for_cyclic_buffered_send() {}
    //   callee block containing the Send loops to itself (succs include
    //   its own index), cap 3 → CapClass::Silent.
    #[test] fn cap_class_cell_form_single_store_resolves() {}
    //   F: Alloc cell reg 4; make_chan(2, 1) with cap const 3; store(4, 2);
    //   candidate alloc_value = ValueId(4) → CapClass::BufferedConst(3).
    #[test] fn cap_class_cell_form_two_stores_is_silent() {}
    //   as above plus a second store(4, 5) → CapClass::Silent.
    #[test] fn buffered_recv_is_reachability_only() {}
    //   cap const 3, CandKind::Recv → CapClass::Unbuffered.
```

- [ ] **Step 2: Run to verify failure** — `cargo test -p goverify-checkers leak` → compile errors/failures.

- [ ] **Step 3: Implement** per the Interfaces block. Keep the unblocker sets as a `match` returning `&'static [ChanOp]`.

- [ ] **Step 4: Run tests** — `cargo test -p goverify-checkers` → PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/goverify-checkers/src/leak.rs
git commit -m "phase7: leak counterpart matching + capacity classification"
```

---

### Task 7: leak.rs — obligations (conjoined query) + Checker impl + registration

**Files:**
- Modify: `crates/goverify-checkers/src/leak.rs` (Checker impl, query construction)
- Modify: `crates/goverify-checkers/src/lib.rs` (`default_checkers()` gains `&LeakChecker`)

**Interfaces:**
- Consumes: Tasks 5-6 functions; `goverify_analysis::{encode_func_with, Checker, Obligation, Summary}`; `crate::shared::own_preconditions`; `goverify_solver::{Query, Term, Sort, BvBinOp, BvCmpOp}` (confirm exact `BvBinOp::Add` / `BvCmpOp::Uge` variant names in `crates/goverify-solver/src/term.rs` before use — `Add`/`Ult` are confirmed to exist; check `Uge` and pick the available unsigned-≥ or negate `Ult`).
- Produces: `pub struct LeakChecker;` implementing `Checker` with `name() = "goroutine-leak"`, `version() = 1`, empty `infer_requires`/`infer_ensures`, and `obligations` emitting tags `chan-send-leak` / `chan-recv-leak` / `chan-select-leak`. Registration makes `//goverify:ignore goroutine-leak` valid automatically (`compile_program` validates against `default_checkers()` names) and rotates `CacheConfigKey.checkers`.

Query construction (the heart of the task):

```rust
/// Renames every const in `q` with `prefix`, substituting in all asserts.
/// Returns None if substitution fails (sort mismatch can't happen for a
/// pure rename, but degrade rather than unwrap on adversarial input).
fn prefixed(q: &Query, prefix: &str) -> Option<Query> {
    let map: std::collections::BTreeMap<String, Term> = q
        .consts
        .iter()
        .map(|(n, s)| (n.clone(), Term::var(&format!("{prefix}{n}"), s.clone())))
        .collect();
    let mut asserts = Vec::with_capacity(q.asserts.len());
    for t in &q.asserts {
        asserts.push(t.substitute(&map).ok()?);
    }
    Some(Query {
        logic: q.logic,
        datatypes: q.datatypes.clone(),
        consts: q.consts.iter().map(|(n, s)| (format!("{prefix}{n}"), s.clone())).collect(),
        asserts,
    })
}

/// f-side query ∧ prefix-renamed callee-side query. Both encodings share
/// the same two datatype decls (ptr, GoSeq), so f's set is kept as-is.
/// Consts sorted for canonical determinism.
fn conjoin(mut fq: Query, cq: Query) -> Query {
    fq.consts.extend(cq.consts);
    fq.consts.sort();
    fq.consts.dedup();
    fq.asserts.extend(cq.asserts);
    fq
}
```

Per surviving candidate:
1. `enc_f = encode_func_with(p, f, summary_of)` (once per function, before the candidate loop; `Err` ⇒ return no obligations — the engine's probe already emits the diagnostic).
2. `enc_c = encode_func_with(p, cand.callee, summary_of)` (`Err` ⇒ skip candidate). Cache per-callee in a local `HashMap<FuncId, Option<EncodedFunc>>` — several go-sites often share a callee.
3. f-side: `enc_f.reach_query(cand.go_block, own_preconditions(&summary_of(f)))`.
4. callee-side extra: `own_preconditions(&summary_of(cand.callee))`, plus for `CapClass::BufferedConst(cap)` the ordinal conjunct built from `enc_c.guards`:

```rust
// Blocks that can precede the candidate on a DAG path = reverse-
// reachability over enc_c.dag_succs from op_block (small BFS).
// pending = Σ over strict ancestors b: ite(guard_b, sends_on_loc_in(b), 0)
//         + sends_on_loc_in(op_block) before the candidate instr.
// Blocked ⇔ pending ≥ cap (buffer already full when the candidate fires;
// no recv counterpart exists, so nothing drains it).
let mut pending = Term::bv_lit(32, before_in_own_block as u128);
for (b, n) in ancestor_send_counts {
    let inc = Term::ite(
        enc_c.guards[b].clone(),
        Term::bv_lit(32, n as u128),
        Term::bv_lit(32, 0),
    )?;
    pending = Term::bv_bin(BvBinOp::Add, pending, inc)?;
}
extra.push(Term::bv_cmp(BvCmpOp::Uge, pending, Term::bv_lit(32, cap as u128))?);
```

  where `sends_on_loc_in(b)` counts `Op::Send` instrs in callee block `b` whose chan resolves (callee-side) to the candidate's callee-side loc. Any `Err` from term construction ⇒ skip candidate (degrade).
5. `cq = prefixed(&enc_c.reach_query(cand.op_block, extra), "s_")` (`None` ⇒ skip).
6. `Obligation { tag, message, pos: cand.go_pos, query: conjoin(fq, cq) }`.

Messages (position-free; callee name is deterministic and position-independent):
- Send: `format!("goroutine {callee} may block forever: send on a spawner-created channel with no receive, close, or select in the spawning environment", callee = p.func_name(cand.callee))`
- Recv: same with `"receive on a spawner-created channel with no send, close, or select"`.
- Select: same with `"blocking select whose channels have no counterpart operations"`.

- [ ] **Step 1: Read `crates/goverify-solver/src/term.rs` enums** — confirm `BvBinOp::Add` and the unsigned-≥ comparison variant name; adjust the snippet if it is e.g. `Uge`/`Ge`/negated-`Ult`.

- [ ] **Step 2: Write the failing tests**

```rust
    #[test]
    fn prefixed_renames_consts_and_free_vars() {
        let q = Query {
            logic: Logic::All,
            datatypes: vec![],
            consts: vec![("g0".into(), Sort::Bool)],
            asserts: vec![Term::var("g0", Sort::Bool)],
        };
        let pq = prefixed(&q, "s_").expect("pure rename cannot fail");
        assert_eq!(pq.consts, vec![("s_g0".into(), Sort::Bool)]);
        let fv = pq.asserts[0].free_vars();
        assert!(fv.contains_key("s_g0") && !fv.contains_key("g0"), "{fv:?}");
    }

    #[test]
    fn leak_end_to_end_reports_via_z3() {
        // Hand-built two-function package (Task 5's
        // direct_param_send_yields_send_candidate fixture shape, with a
        // real chan type + int cap const aux so encoding succeeds), run
        // through goverify_analysis::analyze_full with
        // EngineConfig::default-ish + Z3Native (mirror how
        // crates/goverify-checkers/tests/nil_corpus.rs constructs cfg and
        // backend, or testfix's z3_discharge plumbing if it exposes one),
        // checkers = vec![&LeakChecker].
        // Assert exactly one finding: checker "goroutine-leak",
        // tag "chan-send-leak", severity Error.
    }

    #[test]
    fn leak_suppressed_when_recv_follows_spawn() {
        // Same fixture + a Recv on the channel in F after the Go →
        // zero findings.
    }

    #[test]
    fn ignore_name_is_valid() {
        // default_checkers() now contains "goroutine-leak" — assert
        // default_checkers().iter().any(|c| c.name() == "goroutine-leak").
    }
```

For the e2e fixtures: the encoder needs types to succeed — give the hand-built package a chan type entry and int types following testfix's `ty()` helpers (`gvir::TypeKind::Chan` with `elem`); if `encode_func_with` degrades on unknown types by havocking values, reachability of an unconditional single block is still provable — try the minimal shape first and only add types if the query comes back Unknown/Unsat.

- [ ] **Step 3: Run to verify failure** — `cargo test -p goverify-checkers leak` → failures (no Checker impl).

- [ ] **Step 4: Implement** `prefixed`, `conjoin`, the ancestor BFS + send counting, and `impl Checker for LeakChecker`:

```rust
impl Checker for LeakChecker {
    fn name(&self) -> &'static str {
        "goroutine-leak"
    }

    fn infer_requires(
        &self,
        _p: &Program,
        _f: FuncId,
        _summary_of: &dyn Fn(FuncId) -> Summary,
        _discharge: &mut dyn FnMut(&Query) -> SatResult,
    ) -> Vec<Clause> {
        Vec::new() // leaks are not contracts; nothing lifts (spec §4)
    }

    fn obligations(
        &self,
        p: &Program,
        f: FuncId,
        summary_of: &dyn Fn(FuncId) -> Summary,
    ) -> Vec<Obligation> {
        let Some(func) = p.func(f) else { return Vec::new() };
        let Ok(enc_f) = encode_func_with(p, f, summary_of) else { return Vec::new() };
        let env = summary_of(f).effects;
        let mut out = Vec::new();
        for cand in candidates(p, func) {
            // Select: every arm's channel must pass the escape check and
            // no arm may have a counterpart; Send/Recv use the single loc.
            let (escaped, suppressed) = match cand.kind {
                CandKind::Select => (
                    cand.arm_locs.iter().any(|(_, l)| match &l.root {
                        Root::Alloc(a) => escapes(func, ValueId(*a)),
                        _ => true,
                    }),
                    cand.arm_locs.iter().any(|(dir, l)| {
                        let k = if *dir == 1 { CandKind::Send } else { CandKind::Recv };
                        has_counterpart(&env, l, k)
                    }),
                ),
                k => (
                    escapes(func, cand.alloc_value),
                    has_counterpart(&env, &cand.spawner_loc, k),
                ),
            };
            if escaped || suppressed { continue; }
            let class = cap_class(p, func, &cand);
            if class == CapClass::Silent { continue; }
            /* encode callee (cached), build extra, prefixed, conjoin — as
               specified above; push Obligation */
        }
        out
    }
}
```

Add `&LeakChecker` to `default_checkers()` (`vec![&NilChecker, &BoundsChecker, &LeakChecker]`).

- [ ] **Step 5: Run tests** — `cargo test -p goverify-checkers` → PASS (incl. Z3 e2e).

- [ ] **Step 6: Run the full suite** — `mise run test`. The registration changes `CacheConfigKey.checkers`, and `annot` corpus goldens list valid ignore names in bad-annotation messages — if any golden embeds the checker-name list, review + `UPDATE_GOLDENS=1`.

- [ ] **Step 7: Commit**

```bash
git add crates/goverify-checkers/src/leak.rs crates/goverify-checkers/src/lib.rs testdata/goldens
git commit -m "phase7: LeakChecker obligations (conjoined spawn/block query); register checker"
```

---

### Task 8: corpus module `testdata/corpus/leak` + `leak_corpus` test

**Files:**
- Create: `testdata/corpus/leak/go.mod` (`module example.com/leak` + `go 1.25.10` — copy `testdata/corpus/conc/go.mod`)
- Create: `testdata/corpus/leak/leak.go`
- Create: `crates/goverify-checkers/tests/leak_corpus.rs` (mirror `nil_corpus.rs` verbatim in structure)
- Create: golden file (whatever path `check_golden("leak_corpus", ...)` resolves to — see `testutil::check_golden`)
- Modify: `mise.toml` — add `--test leak_corpus` to the goverify-checkers cargo-test line in `[tasks.corpus]`

**Interfaces:**
- Consumes: the full pipeline (extractor → .gvir → IR → engine → Z3 → findings); `goverify_ir::testutil::{load_corpus, wants, check_golden}`; `// want: <tag>` pins are TRAILING comments on the finding's position line, tags `[a-z0-9-]+`.
- Produces: the phase-7 acceptance corpus. Fixture ↔ spec-rule map (each its own top-level function; **no imports** — keeps corpus runtime bounded):

```go
package leak

// Reported: unbuffered send, closure capture (THE canonical shape).
func LeakSendClosure() {
	ch := make(chan int)
	go func() { ch <- 1 }() // want: chan-send-leak
	_ = ch
}

// Reported: unbuffered send, channel passed as go-arg to a named fn.
func LeakSendParam() {
	ch := make(chan int)
	go produce(ch) // want: chan-send-leak
}

func produce(c chan int) { c <- 1 }

// Reported: recv with no sender and no close.
func LeakRecvClosure() {
	ch := make(chan int)
	go func() { <-ch }() // want: chan-recv-leak
}

// Reported: cyclic producer on an unbuffered channel (first send blocks;
// no counting needed).
func LeakLoopProducer(xs []int) {
	ch := make(chan int)
	go func() {
		for _, x := range xs {
			ch <- x
		}
	}() // want: chan-send-leak
}

// Reported: buffered, const cap 1, second acyclic send overflows.
func LeakBufferedOverflow() {
	ch := make(chan int, 1)
	go func() {
		ch <- 1
		ch <- 2
	}() // want: chan-send-leak
}

// Silent: counterpart recv exists in the spawner.
func NoLeakPaired() int {
	ch := make(chan int)
	go func() { ch <- 1 }()
	return <-ch
}

// Silent: buffered send fits the buffer; goroutine terminates.
func NoLeakBufferedFits() {
	ch := make(chan int, 1)
	go func() { ch <- 1 }()
}

// Silent: close is a (conservative) counterpart for the blocked recv.
func NoLeakClosed() {
	ch := make(chan int)
	go func() { <-ch }()
	close(ch)
}

// Silent: param-rooted — the caller may hold receivers.
func NoLeakParamRooted(ch chan int) {
	go func() { ch <- 1 }()
}

// Silent: channel escapes via heap store.
var sink chan int

func NoLeakEscapeStore() {
	ch := make(chan int)
	sink = ch
	go func() { ch <- 1 }()
}

// Silent: channel escapes via return.
func NoLeakEscapeReturn() chan int {
	ch := make(chan int)
	go func() { ch <- 1 }()
	return ch
}

// Silent: channel passed to a plain call (strict arg-escape rule).
func NoLeakEscapeCall() {
	ch := make(chan int)
	drain(ch)
	go func() { ch <- 1 }()
}

func drain(c chan int) { go func() { <-c }() }

// Silent: global-rooted channel — any package can hold a counterpart
// (the candidate root is Global, not Alloc-in-spawner).
var gch = make(chan int)

func NoLeakGlobalRooted() {
	go func() { gch <- 1 }()
}

// Reported: an unresolved dynamic call in the goroutine does NOT
// suppress — it contributes no effects, and the escape walk proves the
// channel can never reach it, so no one can unblock the send. (True
// havoc-callee suppression — a BODYLESS static callee whose top()
// effects put Unknown ops in the environment — cannot be built in a
// no-import corpus module; it is pinned by the effects/has_counterpart
// unit tests instead. Task 10 records this in spec §8.)
var hook func()

func LeakDespiteOpaqueCall() {
	ch := make(chan int)
	go func() {
		hook()
		ch <- 1
	}() // want: chan-send-leak
}

// Silent: select with default never blocks.
func NoLeakSelectDefault() {
	ch := make(chan int)
	go func() {
		select {
		case ch <- 1:
		default:
		}
	}()
}

// Reported: blocking select, all arms dead.
func LeakSelectAllBlocked() {
	a := make(chan int)
	b := make(chan int)
	go func() {
		select {
		case <-a:
		case b <- 1:
		}
	}() // want: chan-select-leak
}
```

**Pin-position note:** `Finding.pos` is the `Op::Go` instruction's position — for a multi-line `go func(){...}()` literal, verify during Step 3 whether go/ssa attributes the Go instr to the `go` keyword line or the closing `}()` line, and move the `// want:` trailing comments to whichever line the finding actually lands on (the want-parser requires trailing-comment position). Single-line forms (`LeakSendParam`) are unambiguous.

- [ ] **Step 1: Write the corpus module and the failing test**

`leak_corpus.rs` — copy `nil_corpus.rs` wholesale, replacing: `load_corpus("nil")` → `load_corpus("leak")`, `NilChecker` → `LeakChecker`, filter `"example.com/nil"` → `"example.com/leak"`, golden name → `"leak_corpus"`. Keep the wants-match assertion block (~line 92 pattern: findings must equal the `wants("leak")` set) and the run-twice determinism assertion.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p goverify-checkers --test leak_corpus`
Expected: FAIL — missing golden (run once to see actual findings before trusting them).

- [ ] **Step 3: Triage the actual output**

Compare the findings against the fixture intent above, fixture by fixture. Every mismatch is a bug in Tasks 5-7 (or a mis-set want-line per the pin-position note) — fix the code or the pin, never delete a fixture to make the suite pass. When findings match intent: `UPDATE_GOLDENS=1 cargo test -p goverify-checkers --test leak_corpus`, then re-run without the env var.

- [ ] **Step 4: Wire into the corpus task**

In `mise.toml` `[tasks.corpus]`, extend the checkers line: `"cargo test -p goverify-checkers --test nil_corpus --test bounds_corpus --test knownfp_corpus --test ensures_corpus --test scc_cache_invalidation --test leak_corpus",`

Run: `mise run corpus`
Expected: PASS (full determinism suite green — this also re-validates that zero extractor changes kept every existing corpus `.gvir` byte-identical).

- [ ] **Step 5: Commit**

```bash
git add testdata/corpus/leak crates/goverify-checkers/tests/leak_corpus.rs testdata/goldens mise.toml
git commit -m "phase7: leak corpus module + e2e corpus test + goldens"
```

---

### Task 9: cache invalidation coverage

**Files:**
- Modify: `crates/goverify-checkers/tests/scc_cache_invalidation.rs`

**Interfaces:**
- Consumes: existing invalidation-test harness (read the file first; it pins that stale cache entries don't replay across version/config changes).
- Produces: two additional pins:
  1. **Warm replay parity for leak findings:** run the leak corpus twice through the same cache dir (mirror how the existing test drives `analyze_full` with `cache_dir`); second (warm) run's `dump_findings` output must be byte-identical to the cold run's, and `scc_cache_hits > 0`. (Leak obligations ride the existing findings-payload caching; this pins that the conjoined-query findings replay correctly.)
  2. **Checker-set rotation:** analyzing with `vec![&NilChecker]` then with `default_checkers()` over the same cache dir must NOT replay the two-checker findings from the one-checker entries (assert cold-miss on the second config — follow the assertion pattern the file already uses for config-key rotation, if present; if the file has no such pattern, assert the second run's findings include `goroutine-leak` findings, which stale replay would omit).

- [ ] **Step 1: Read the existing test file**, adopt its fixture/temp-dir/backend conventions exactly.
- [ ] **Step 2: Write both tests; run to see the first fail** if leak findings don't replay (any failure here is a real bug — findings caching is checker-agnostic, so expect pass-on-first-run; the value is the pin).
- [ ] **Step 3: Run** `cargo test -p goverify-checkers --test scc_cache_invalidation` → PASS.
- [ ] **Step 4: Commit**

```bash
git add crates/goverify-checkers/tests/scc_cache_invalidation.rs
git commit -m "phase7: cache invalidation pins for leak findings"
```

---

### Task 10: docs + spec amendments

**Files:**
- Modify: `docs/superpowers/specs/2026-07-26-phase7-goroutine-leaks-design.md`
- Modify: `README.md` (checker list / findings documentation — find the section documenting nil/bounds checkers and severity)
- Modify: `ARCHITECTURE.md` (goverify-checkers crate description)

**Interfaces:** none (documentation).

Spec amendments (implementation-discovered refinements, each replacing the corresponding sentence):
1. §2 rule 2: the escape rule is the strict whitelist form — passing the channel as an argument to any plain `Call` (even a summarized one) is an escape, because effects do not model a callee storing its parameter to the heap. (§10 gains a follow-up: "arg-passing to summarized callees could stop escaping once effects model param stores".)
2. §2 rule 1: the blocking op must be **syntactically in the spawned function's own body**; blocking ops reached transitively through helpers the goroutine calls are used for suppression (via summaries) but never reported (no callee-side instruction to anchor the reachability query). §10 gains: "nested-helper blocking ops (cross-function obligation anchoring)".
3. §3: note that `resolve_loc` also gained `Op::Make` alloc-site rooting and deref-collapsing `Load`/`Assign` hops (with `MAX_LOC_HOPS`) — pre-existing behavior rooted `make(chan)` registers at `Unknown`, which would have made every candidate invisible; and that closure bindings are recovered by scanning for the callee's unique `MakeClosure` site because lowering drops the callee-slot operand (≥2 sites degrade to Unknown).
4. §5: the buffered ordinal is a BV(32) guard-indicator sum over the candidate block's DAG ancestors; blocked ⇔ pending-before-candidate ≥ cap.
5. §8: the "havoc-callee suppression (silent)" corpus item moves to unit tests (a no-import corpus module cannot reference a bodyless callee); the corpus instead pins `LeakDespiteOpaqueCall` — an unresolved dynamic call in the goroutine must NOT suppress, which is correct because the escape walk proves the channel cannot reach it.

README: add the `goroutine-leak` checker (channels-only, Error severity, the three tags, one-paragraph description + the `//goverify:ignore goroutine-leak` example). ARCHITECTURE: one sentence in the checkers-crate paragraph.

- [ ] **Step 1: Make all edits.**
- [ ] **Step 2: Self-check** — grep the spec for the four amended sentences; confirm no contradiction remains (especially §2's old "can't attribute back to a tracked local" phrasing — already replaced pre-plan, but verify).
- [ ] **Step 3: Commit**

```bash
git add docs/superpowers/specs/2026-07-26-phase7-goroutine-leaks-design.md README.md ARCHITECTURE.md
git commit -m "phase7: spec amendments (strict escape, direct-op limit) + checker docs"
```

---

### Task 11: bbolt shakeout regression (G-gates, existing pin)

**Files:**
- Create: `docs/superpowers/specs/2026-07-26-phase7-shakeout.md` (results record — follow the phase-6 addendum's format: `git show 225e946 --stat` to locate it and mirror)

**Interfaces:** consumes the built CLI + pinned bbolt via `mise run shakeout`.

- [ ] **Step 1: Run** `mise run shakeout` (release build first run — note the EDR new-binary exec stall hazard from memory [[sentinelone-exec-stall]]: if the run hangs ~50min at exec, use the cp-and-exec workaround from that memory or park and continue with Task 12 prep).
- [ ] **Step 2: Gate G1 (regression):** count findings via the finding-header grep (NEVER `wc -l` — memory: goverify-phase1-state). The 457 nil/bounds findings must be unchanged. New `goroutine-leak` findings are expected to number ~0-2.
- [ ] **Step 3: Gate G2 (triage):** manually triage EVERY `goroutine-leak` finding on bbolt with a written verdict (real leak / FP / can't-tell). **A confirmed FP blocks the wave** (Error severity) — fix the checker and re-run, don't rationalize.
- [ ] **Step 4: Gates G3/G4:** run twice — byte-identical output; record cold/warm wall-clock vs the phase-6 baseline (warm 3.45s) and confirm the warm delta is within noise (<15% — the leak checker adds one encode + ≤2 queries per go-site, and bbolt has few).
- [ ] **Step 5: Write the results doc + commit**

```bash
git add docs/superpowers/specs/2026-07-26-phase7-shakeout.md
git commit -m "phase7: bbolt shakeout regression record (G1-G4)"
```

---

### Task 12: concurrency-heavy shakeout pin

**Files:**
- Create: `scripts/shakeout_conc.sh` (clone of `scripts/shakeout.sh` with: repo `https://github.com/golang/sync`, `PIN="${GOVERIFY_SHAKEOUT_CONC_REF:-v0.10.0}"` — verify the tag exists with `git ls-remote --tags https://github.com/golang/sync | grep v0.10` and pick the newest stable if not, `DIR=".goverify/shakeout/sync"`)
- Modify: `mise.toml` (new task `[tasks.shakeout-conc]`, same shape as `[tasks.shakeout]`, description "phase-7 shakeout: goverify check over pinned golang.org/x/sync (manual; clones on first run)")
- Modify: `docs/superpowers/specs/2026-07-26-phase7-shakeout.md` (append the x/sync section)

**Interfaces:** consumes the phase-7 checker end-to-end on real goroutine-heavy code (errgroup, semaphore, singleflight — dense spawn/channel/WaitGroup usage; WaitGroup ops are invisible to v1, which this run documents as observed FN surface, not failure).

- [ ] **Step 1: Write the script + mise task.** `chmod +x scripts/shakeout_conc.sh`. Run `mise run shakeout-conc`.
- [ ] **Step 2: Gates (same G1-G4 style):**
  - G1 determinism: two runs byte-identical.
  - G2 triage: every `goroutine-leak` finding gets a written verdict in the results doc. Confirmed FP ⇒ blocks the wave; fix and re-run. Zero findings is an acceptable outcome — record which silence rules fired on the plausible-leak sites you spot-check (pick ≥3 goroutine spawns in errgroup/singleflight, trace by hand which of escape/counterpart/param-root/Unknown silenced them) so the precision boundary is documented, not assumed.
  - G3 runtime: record cold/warm wall-clock (x/sync is small; expect well under bbolt's).
  - G4 cache: cold vs warm byte-identical.
- [ ] **Step 3: Append results to the shakeout doc + commit**

```bash
git add scripts/shakeout_conc.sh mise.toml docs/superpowers/specs/2026-07-26-phase7-shakeout.md
git commit -m "phase7: x/sync concurrency shakeout pin + gates record"
```

---

## Final verification (after all tasks)

- [ ] `mise run fmt && mise run lint && mise run test && mise run corpus` — all green.
- [ ] `mise run secrets && mise run audit` — green (blocking CI tier).
- [ ] Grep sanity: `grep -rn "wc -l" scripts/ docs/superpowers/specs/2026-07-26-phase7-shakeout.md` — finding counts use the header grep.
- [ ] Confirm no `.gvir` schema drift: `git diff --stat proto/ extractor/` is empty (this wave never touches them).

## Follow-up queue (plan owner; do NOT implement)

- WaitGroup intrinsics + counter modeling (parent-spec phase B, second half).
- Phase C data races (lock-set over Effects; `DeferredUnlock` + captured-mutex Locs now ready).
- Buffered-cyclic send counting (loop-bound reasoning).
- Param-rooted channels via caller-side obligation placement.
- Nested-helper blocking ops (cross-function obligation anchoring — Task 10 amendment 2).
- Escape rule: stop treating args-to-summarized-callees as escapes once effects model param stores (Task 10 amendment 1).
- Counterpart flow-sensitivity (path-aware matching — the spec §2 documented FN class).
- `ChanOp::Select` direction refinement in effects (direction-blind today; v1 selects are conservative both ways).
- `BinOp` Eq/Neq on channels currently escapes (conservative default) — whitelist nil-compares if a fixture needs it.
- `debug findings` still uses `vec![&NilChecker]`, not `default_checkers()` (pre-existing divergence, now three checkers wide).
