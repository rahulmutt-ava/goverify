# Nested-Helper Anchoring (One Hop) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let the goroutine-leak checker anchor a finding on a blocking op that sits inside a helper reached by a single static `Call`/`Defer` from the spawned callee's body (spec: `docs/superpowers/specs/2026-07-27-nested-helper-anchoring-design.md`).

**Architecture:** All checker changes are local to `crates/goverify-checkers/src/leak.rs`: the candidate scan gains a second, one-hop rebase (`map_through_hop`, with a single-store cell bridge for captured params) and each hop candidate's obligation becomes a three-frame conjoined reachability query (`f` reaches the go site ∧ `s_`-prefixed `g` reaches the call/defer site ∧ `t_`-prefixed `h` reaches the op). Escape and counterpart logic are untouched. `LeakChecker::version()` bumps 1→2, which rotates the SCC cache via the checker-version salt.

**Tech Stack:** Rust (workspace crates `goverify-checkers`, tests against `goverify-analysis`/`goverify-solver`/Z3), Go corpus fixtures under `testdata/corpus/leak/`.

## Global Constraints

- **Determinism is the root invariant**: no map-iteration order may reach any output; every scan below iterates blocks/instrs in index order.
- **Errors degrade, never die**: every id/index lookup is bounds-checked; unrecognized shapes yield no candidate; encoding/substitution failure skips the candidate, never panics, never reports.
- **No new dependencies.** No `.gvir` schema, proto, or extractor changes (the only Go code lives in `extractor/`, untouched except corpus fixtures under `testdata/`).
- **Tags unchanged** (`chan-send-leak`, `chan-recv-leak`, `chan-select-leak`); direct-candidate messages byte-identical to today's.
- Toolchain commands go through mise (sandbox RUSTUP_HOME relocation): `mise x -- cargo …`, `mise run fmt|lint|test|corpus`.
- Blocking gate before push: `mise run lint`, `mise run test` (includes corpus determinism), `mise run secrets`, `mise run audit`.
- Commits are unsigned in this environment (expected).
- Finding counts are ALWAYS via the header grep (`grep -cE '^[^ ].*: chan-(send|recv|select)-leak: '`), never `wc -l`.

## File map

| File | Change |
|---|---|
| `crates/goverify-checkers/src/leak.rs` | `scan_body` refactor; `Hop`/`HopKind`; hop scan; `map_through_hop` + `bridge_cell`; `cap_class` hop gate; three-frame `obligations`; hop messages; `version()` 2; all unit + e2e tests |
| `crates/goverify-checkers/src/testfix.rs` | `defer_call_args`, `defer_call_via_closure` |
| `testdata/corpus/leak/leak.go` | 6 new fixtures (3 reported, 1 suppressed, 2 silent boundary pins) |
| `docs/superpowers/specs/2026-07-26-phase7-goroutine-leaks-design.md` | §2 rule 1 rewrite; §10 item swap |
| `docs/superpowers/specs/2026-07-27-nested-helper-anchoring-design.md` | §3 cell-bridge amendment |
| `docs/superpowers/plans/2026-07-26-phase7-goroutine-leaks.md` | tick the consumed queue item |
| `README.md` | one sentence in the `goroutine-leak` checker paragraph |
| `docs/superpowers/specs/2026-07-26-phase7-shakeout.md` | re-run record section |

---

### Task 1: Extract `scan_body` from `candidates` (pure refactor)

**Files:**
- Modify: `crates/goverify-checkers/src/leak.rs` (the `candidates` function, currently ~lines 129–247)

**Interfaces:**
- Consumes: existing `map_through_site`, `resolve_loc`, `closure_bindings`, `Candidate`, `CandKind`.
- Produces: `struct BodyHit { op_block: usize, op_instr: usize, kind: CandKind, callee_loc: Loc, mapped: Loc, arm_locs: Vec<(u32, Loc)> }`, `fn scan_body(host_f: &Function, map: &dyn Fn(&Loc) -> Loc) -> Vec<BodyHit>`, `fn candidate_from(go_block: usize, go_pos: Option<Pos>, callee: FuncId, hit: BodyHit) -> Option<Candidate>` — Task 2 adds a `hop` parameter to `candidate_from` and calls `scan_body` with a composed map.

This is a behavior-preserving refactor: the three-op-shape match currently inlined in `candidates` moves into `scan_body`, parameterized over the loc-mapping function, so Task 2 can reuse it for helper bodies with a composed two-hop map. No new tests; the existing leak suite is the safety net.

- [ ] **Step 1: Run the existing suite to get a green baseline**

Run: `mise x -- cargo test -p goverify-checkers --lib`
Expected: PASS (all existing leak tests green).

- [ ] **Step 2: Replace `candidates` with the factored form**

Delete the body of `candidates` (keep its doc comment, and note in it that the per-op scan now lives in `scan_body`) and add, directly above it:

```rust
/// One blocking op found in a host function's own body whose channel
/// (every arm's channel, for a Select) maps to `Root::Alloc` in the
/// spawner under the caller-supplied `map` — the raw material
/// `candidates` wraps into a `Candidate`. `map` is the frame-mapping
/// composition: go-site only for the direct scan; hop-site then go-site
/// for the helper scan (Task 2 of the 2026-07-27 wave).
struct BodyHit {
    op_block: usize,
    op_instr: usize,
    kind: CandKind,
    /// Resolved in the host's own frame; `Loc::unknown()` for Select.
    callee_loc: Loc,
    /// Spawner-frame; `Root::Alloc` guaranteed by construction (first
    /// arm's loc, for a Select).
    mapped: Loc,
    /// Select only: every arm's spawner-frame loc, paired with the
    /// arm's dir (1 = send, 2 = recv). Empty for Send/Recv.
    arm_locs: Vec<(u32, Loc)>,
}

/// Scans `host_f`'s own body, in block/instr index order, for
/// `Send`/`Recv`/blocking-`Select` ops whose channel maps to a
/// `Root::Alloc` in the spawner under `map`. A blocking `Select` needs
/// EVERY arm mapped to `Root::Alloc`. Total and panic-free: an
/// unmapped or unrecognized shape yields no hit.
fn scan_body(host_f: &Function, map: &dyn Fn(&Loc) -> Loc) -> Vec<BodyHit> {
    let mut out = Vec::new();
    for (op_block, blk) in host_f.blocks.iter().enumerate() {
        for (op_instr, ins) in blk.instrs.iter().enumerate() {
            match &ins.op {
                Op::Send { chan, .. } | Op::Recv { chan, .. } => {
                    let callee_loc = resolve_loc(host_f, *chan);
                    let mapped = map(&callee_loc);
                    if matches!(mapped.root, Root::Alloc(_)) {
                        let kind = if matches!(ins.op, Op::Send { .. }) {
                            CandKind::Send
                        } else {
                            CandKind::Recv
                        };
                        out.push(BodyHit {
                            op_block,
                            op_instr,
                            kind,
                            callee_loc,
                            mapped,
                            arm_locs: Vec::new(),
                        });
                    }
                }
                Op::Select {
                    arms,
                    blocking: true,
                    ..
                } => {
                    if arms.is_empty() {
                        continue;
                    }
                    let mut mapped_arms: Vec<(u32, Loc)> = Vec::with_capacity(arms.len());
                    let mut all_alloc = true;
                    for arm in arms {
                        let arm_loc = resolve_loc(host_f, arm.chan);
                        let mapped = map(&arm_loc);
                        if !matches!(mapped.root, Root::Alloc(_)) {
                            all_alloc = false;
                            break;
                        }
                        mapped_arms.push((arm.dir, mapped));
                    }
                    if !all_alloc {
                        continue;
                    }
                    let Some(first) = mapped_arms.first().map(|(_, l)| l.clone()) else {
                        continue;
                    };
                    out.push(BodyHit {
                        op_block,
                        op_instr,
                        kind: CandKind::Select,
                        callee_loc: Loc::unknown(),
                        mapped: first,
                        arm_locs: mapped_arms,
                    });
                }
                _ => {}
            }
        }
    }
    out
}

/// Wraps a `BodyHit` into a `Candidate` at this go site. `None` only on
/// a non-Alloc `mapped` root, which `scan_body` never produces — kept
/// total rather than panicking on that invariant.
fn candidate_from(
    go_block: usize,
    go_pos: Option<Pos>,
    callee: FuncId,
    hit: BodyHit,
) -> Option<Candidate> {
    let Root::Alloc(a) = hit.mapped.root else {
        return None;
    };
    Some(Candidate {
        go_block,
        go_pos,
        callee,
        op_block: hit.op_block,
        op_instr: hit.op_instr,
        kind: hit.kind,
        callee_loc: hit.callee_loc,
        alloc_value: ValueId(a),
        spawner_loc: Loc {
            root: Root::Alloc(a),
            path: hit.mapped.path,
        },
        arm_locs: hit.arm_locs,
    })
}
```

New `candidates` body:

```rust
pub(crate) fn candidates(p: &Program, f: &Function) -> Vec<Candidate> {
    let cb = closure_bindings(f);
    let mut out = Vec::new();
    for (bi, b) in f.blocks.iter().enumerate() {
        for ins in &b.instrs {
            let Op::Go {
                callee: Callee::Static(c),
                args,
            } = &ins.op
            else {
                continue;
            };
            let c = *c;
            let Some(callee_f) = p.func(c) else {
                continue;
            };
            if callee_f.blocks.is_empty() {
                continue;
            }
            let go_map = |l: &Loc| map_through_site(l, f, args, &cb, c);
            for hit in scan_body(callee_f, &go_map) {
                out.extend(candidate_from(bi, ins.pos.clone(), c, hit));
            }
        }
    }
    out
}
```

- [ ] **Step 3: Run the suite to verify behavior is preserved**

Run: `mise x -- cargo test -p goverify-checkers --lib && mise x -- cargo test -p goverify-checkers --test leak_corpus`
Expected: PASS, same test count as Step 1.

- [ ] **Step 4: Format and commit**

```bash
mise run fmt
git add crates/goverify-checkers/src/leak.rs
git commit -m "hop: factor candidate scan into scan_body (pure refactor)"
```

---

### Task 2: `Hop` + plain-`Call` hop candidates

**Files:**
- Modify: `crates/goverify-checkers/src/leak.rs`

**Interfaces:**
- Consumes: `scan_body`, `candidate_from`, `map_through_site`, `closure_bindings` (Task 1).
- Produces: `pub(crate) struct Hop { helper: FuncId, call_block: usize, call_instr: usize, kind: HopKind }` with `#[derive(Debug, Clone, PartialEq, Eq)]`; `pub(crate) enum HopKind { Call }` with `#[derive(Debug, Clone, Copy, PartialEq, Eq)]` (Task 3 adds `Defer`); `Candidate.hop: Option<Hop>`; `candidate_from(go_block, go_pos, callee, hit, hop: Option<Hop>)`. Test helper `hop_send_pkg(f_blocks: Vec<gvir::BasicBlock>, cap: i64) -> Program` reused by Tasks 5–7.

- [ ] **Step 1: Write the failing tests**

In `mod tests`, add (near `direct_param_send_yields_send_candidate`):

```rust
/// F: v2 = make(chan, cap); go t.G(v2) [+ extra F instrs]. G(p1):
/// call t.H(p1). H(p1): p1 <- p1 — the bbolt (*Tx).check shape.
/// Shared by the hop candidate/cap_class/obligations/e2e tests.
fn hop_send_pkg(f_blocks: Vec<gvir::BasicBlock>, cap: i64) -> Program {
    Program::from_packages(vec![pkg(
        "t",
        vec![
            func_with_aux("t.F", vec![const_int_aux(1, cap)], f_blocks),
            func_with_params(
                "t.G",
                vec![gvir::Param {
                    id: 1,
                    name: "c".into(),
                    r#type: 0,
                }],
                vec![block(
                    0,
                    vec![call_static("t.H", 2, 0, vec![1]), ret(vec![])],
                    vec![],
                )],
            ),
            func_with_params(
                "t.H",
                vec![gvir::Param {
                    id: 1,
                    name: "c".into(),
                    r#type: 0,
                }],
                vec![block(0, vec![send(1, 1), ret(vec![])], vec![])],
            ),
        ],
    )])
}

/// The default spawner block for `hop_send_pkg`: make + go + return.
fn hop_f_blocks() -> Vec<gvir::BasicBlock> {
    vec![block(
        0,
        vec![
            gvir_make_chan(2, 1),
            go_call_args("t.G", vec![2]),
            ret(vec![]),
        ],
        vec![],
    )]
}

/// One Send hop candidate: the op lives in H, one plain Call below G,
/// anchored at G's call site (spec §2 rule 1 case 2).
#[test]
fn plain_call_hop_send_yields_hop_candidate() {
    let p = hop_send_pkg(hop_f_blocks(), 0);
    let f = p.func(p.lookup_func("t.F").unwrap()).unwrap();
    let cands = candidates(&p, f);
    assert_eq!(cands.len(), 1, "exactly one hop candidate: {cands:?}");
    let c = &cands[0];
    assert_eq!(c.kind, CandKind::Send);
    assert_eq!(c.callee, p.lookup_func("t.G").unwrap());
    let hop = c.hop.as_ref().expect("hop candidate");
    assert_eq!(hop.helper, p.lookup_func("t.H").unwrap());
    assert_eq!(hop.kind, HopKind::Call);
    assert_eq!((hop.call_block, hop.call_instr), (0, 0));
    assert_eq!((c.op_block, c.op_instr), (0, 0));
    assert_eq!(c.alloc_value, ValueId(2));
    assert_eq!(
        c.spawner_loc,
        Loc {
            root: Root::Alloc(2),
            path: vec![]
        }
    );
}

/// G both sends AND calls itself recursively: the direct scan anchors
/// the send; the recursive call site (h == g) must NOT anchor it again.
#[test]
fn recursive_callee_yields_single_direct_candidate() {
    let p = Program::from_packages(vec![pkg(
        "t",
        vec![
            func_with_aux("t.F", vec![const_int_aux(1, 0)], hop_f_blocks()),
            func_with_params(
                "t.G",
                vec![gvir::Param {
                    id: 1,
                    name: "c".into(),
                    r#type: 0,
                }],
                vec![block(
                    0,
                    vec![
                        call_static("t.G", 2, 0, vec![1]),
                        send(1, 1),
                        ret(vec![]),
                    ],
                    vec![],
                )],
            ),
        ],
    )]);
    let f = p.func(p.lookup_func("t.F").unwrap()).unwrap();
    let cands = candidates(&p, f);
    assert_eq!(cands.len(), 1, "one direct candidate only: {cands:?}");
    assert!(cands[0].hop.is_none(), "recursive site must not anchor");
}

/// G calls M, M calls H, H sends: two hops down — no candidate (the v1
/// boundary this wave deliberately keeps; spec §2 exclusions).
#[test]
fn depth_two_helper_yields_no_candidate() {
    let param_c = || gvir::Param {
        id: 1,
        name: "c".into(),
        r#type: 0,
    };
    let p = Program::from_packages(vec![pkg(
        "t",
        vec![
            func_with_aux("t.F", vec![const_int_aux(1, 0)], hop_f_blocks()),
            func_with_params(
                "t.G",
                vec![param_c()],
                vec![block(
                    0,
                    vec![call_static("t.M", 2, 0, vec![1]), ret(vec![])],
                    vec![],
                )],
            ),
            func_with_params(
                "t.M",
                vec![param_c()],
                vec![block(
                    0,
                    vec![call_static("t.H", 2, 0, vec![1]), ret(vec![])],
                    vec![],
                )],
            ),
            func_with_params(
                "t.H",
                vec![param_c()],
                vec![block(0, vec![send(1, 1), ret(vec![])], vec![])],
            ),
        ],
    )]);
    let f = p.func(p.lookup_func("t.F").unwrap()).unwrap();
    assert!(
        candidates(&p, f).is_empty(),
        "depth-2 op must stay suppression-only"
    );
}

/// F: two chans; go t.G(a, b); G calls t.H(a, b); H: blocking select
/// [recv a, send b]. Every arm must map through BOTH hops (mirror of
/// the direct all-arms rule).
#[test]
fn select_in_helper_all_arms_yield_select_hop_candidate() {
    let param2 = || {
        vec![
            gvir::Param {
                id: 1,
                name: "a".into(),
                r#type: 0,
            },
            gvir::Param {
                id: 2,
                name: "b".into(),
                r#type: 0,
            },
        ]
    };
    let p = Program::from_packages(vec![pkg(
        "t",
        vec![
            func_with_aux(
                "t.F",
                vec![const_int_aux(1, 0)],
                vec![block(
                    0,
                    vec![
                        gvir_make_chan(2, 1),
                        gvir_make_chan(3, 1),
                        go_call_args("t.G", vec![2, 3]),
                        ret(vec![]),
                    ],
                    vec![],
                )],
            ),
            func_with_params(
                "t.G",
                param2(),
                vec![block(
                    0,
                    vec![call_static("t.H", 3, 0, vec![1, 2]), ret(vec![])],
                    vec![],
                )],
            ),
            func_with_params(
                "t.H",
                param2(),
                vec![block(
                    0,
                    vec![select(3, vec![(2, 1, 0), (1, 2, 2)], true), ret(vec![])],
                    vec![],
                )],
            ),
        ],
    )]);
    let f = p.func(p.lookup_func("t.F").unwrap()).unwrap();
    let cands = candidates(&p, f);
    assert_eq!(cands.len(), 1, "one select hop candidate: {cands:?}");
    assert_eq!(cands[0].kind, CandKind::Select);
    assert!(cands[0].hop.is_some());
    assert_eq!(cands[0].arm_locs.len(), 2);
    assert!(
        cands[0]
            .arm_locs
            .iter()
            .all(|(_, l)| matches!(l.root, Root::Alloc(_)))
    );
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `mise x -- cargo test -p goverify-checkers --lib`
Expected: compile FAIL — `Candidate` has no field `hop`, `HopKind` not found.

- [ ] **Step 3: Implement `Hop` and the hop scan**

Above `Candidate`, add:

```rust
/// How a hop candidate's helper is reached from the spawned callee's
/// body. Documentation/message value only — the obligation anchors on
/// `call_block` reachability either way (a Defer hop's over-approximation
/// is a documented spec §5.2 boundary).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HopKind {
    Call,
}

/// The one-hop record (spec §2 rule 1 case 2): the blocking op lives in
/// `helper`'s own body, reached by the static `Call`/`Defer` at
/// (`call_block`, `call_instr`) in the spawned callee's body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Hop {
    /// `h` — the function hosting the blocking op.
    pub helper: FuncId,
    /// Block index of the `Call`/`Defer` in the spawned callee `g`.
    pub call_block: usize,
    /// Instruction index within `call_block`.
    pub call_instr: usize,
    pub kind: HopKind,
}
```

Add to `Candidate` (with `#[derive(Debug, Clone)]` unchanged):

```rust
    /// `None` for a direct candidate (op in the spawned callee's own
    /// body); `Some` when the op sits one static Call/Defer below it,
    /// in `hop.helper`'s body — `op_block`/`op_instr`/`callee_loc` then
    /// index/resolve in the HELPER's frame, and the obligation becomes
    /// a three-frame conjunction (2026-07-27 wave).
    pub hop: Option<Hop>,
```

Update `Candidate`'s doc comments for `op_block`, `op_instr`, `callee_loc` to say "the op's host frame (the spawned callee, or `hop.helper` for a hop candidate)". Thread `hop: Option<Hop>` through `candidate_from` as a fifth parameter (`hop,` placed after `arm_locs` in the struct literal), passing `None` at the direct-scan call site. Fix the two existing direct-candidate constructor sites in tests if any construct `Candidate` literally (none do — they go through `candidates`).

Inside `candidates`'s go-site loop, after the direct `scan_body` loop, add:

```rust
            // One-hop scan (spec §2 rule 1 case 2): a blocking op in a
            // helper h reached by a single static Call/Defer in g's own
            // body. h == g is skipped — g's own ops are already directly
            // anchored, and a recursive-call anchor would double-report
            // them. Go edges are NOT hop edges (a nested spawn is a
            // different goroutine; spec §10). Order stays fully
            // index-ordered: direct hits first, then hop sites in g's
            // block/instr order, then op order within each helper.
            let cb_g = closure_bindings(callee_f);
            for (hb, hblk) in callee_f.blocks.iter().enumerate() {
                for (hi, hins) in hblk.instrs.iter().enumerate() {
                    let (hop_kind, h, hargs) = match &hins.op {
                        Op::Call {
                            callee: Callee::Static(h),
                            args,
                            ..
                        } => (HopKind::Call, *h, args),
                        _ => continue,
                    };
                    if h == c {
                        continue;
                    }
                    let Some(helper_f) = p.func(h) else {
                        continue;
                    };
                    if helper_f.blocks.is_empty() {
                        continue;
                    }
                    let hop_map = |l: &Loc| {
                        let g_loc = map_through_site(l, callee_f, hargs, &cb_g, h);
                        map_through_site(&g_loc, f, args, &cb, c)
                    };
                    for hit in scan_body(helper_f, &hop_map) {
                        out.extend(candidate_from(
                            bi,
                            ins.pos.clone(),
                            c,
                            hit,
                            Some(Hop {
                                helper: h,
                                call_block: hb,
                                call_instr: hi,
                                kind: hop_kind,
                            }),
                        ));
                    }
                }
            }
```

In `obligations`, add a transitional guard as the FIRST statement of the candidate loop (removed by Task 6, which builds the real three-frame query):

```rust
            if cand.hop.is_some() {
                // Transitional (this wave's obligations task replaces it
                // with the three-frame query): a hop candidate's
                // op_block indexes the HELPER's frame, and the two-frame
                // query below would anchor it against g's encoding.
                continue;
            }
```

- [ ] **Step 4: Run to verify the new tests pass and nothing regressed**

Run: `mise x -- cargo test -p goverify-checkers --lib && mise x -- cargo test -p goverify-checkers --test leak_corpus`
Expected: PASS (new tests included).

- [ ] **Step 5: Format, lint, commit**

```bash
mise run fmt && mise run lint
git add crates/goverify-checkers/src/leak.rs
git commit -m "hop: Hop record + plain-Call one-hop candidate scan"
```

---

### Task 3: `Defer` hop candidates

**Files:**
- Modify: `crates/goverify-checkers/src/testfix.rs` (two helpers)
- Modify: `crates/goverify-checkers/src/leak.rs`

**Interfaces:**
- Consumes: Task 2's hop scan.
- Produces: `HopKind::Defer`; testfix `defer_call_args(target: &str, args: Vec<u32>) -> gvir::Instruction` and `defer_call_via_closure(target: &str, closure_register: u32) -> gvir::Instruction` (mirrors of `go_call_args`/`go_call_via_closure`; lower.rs treats `"Defer"` identically to `"Go"` at line ~438).

- [ ] **Step 1: Add the testfix helpers**

In `testfix.rs`, below `go_call_via_closure`:

```rust
/// `defer <target>(args…)` — static deferred call; same operand
/// convention as `go_call_args` (`[callee slot, args…]`, slot dropped by
/// lowering — lower.rs's "Call" | "Defer" | "Go" arm).
pub(crate) fn defer_call_args(target: &str, args: Vec<u32>) -> gvir::Instruction {
    let mut operands = vec![0];
    operands.extend(args);
    gvir::Instruction {
        kind: "Defer".into(),
        operands,
        sem: Some(Sem::Call(gvir::CallSem {
            static_callee: target.into(),
            ..Default::default()
        })),
        ..Default::default()
    }
}

/// `defer <closure>()` where `<target>` is invoked through a
/// `MakeClosure` register — mirrors `go_call_via_closure`: operands are
/// just `[closure_register]`, bindings carried by the MakeClosure site.
pub(crate) fn defer_call_via_closure(target: &str, closure_register: u32) -> gvir::Instruction {
    gvir::Instruction {
        operands: vec![closure_register],
        ..defer_call_args(target, vec![])
    }
}
```

- [ ] **Step 2: Write the failing test**

In `leak.rs` tests (add `defer_call_args` — and `defer_call_via_closure`, which Task 4 needs — to the `crate::testfix` use list at the top of `mod tests`):

```rust
/// G(p1): defer t.H(p1); H(p1): <-p1 — the errgroup (*Group).done
/// shape. The defer instruction is the anchor (spec §5.2's documented
/// over-approximation).
#[test]
fn defer_static_hop_recv_yields_hop_candidate() {
    let param_c = || gvir::Param {
        id: 1,
        name: "c".into(),
        r#type: 0,
    };
    let p = Program::from_packages(vec![pkg(
        "t",
        vec![
            func_with_aux("t.F", vec![const_int_aux(1, 0)], hop_f_blocks()),
            func_with_params(
                "t.G",
                vec![param_c()],
                vec![block(
                    0,
                    vec![defer_call_args("t.H", vec![1]), ret(vec![])],
                    vec![],
                )],
            ),
            func_with_params(
                "t.H",
                vec![param_c()],
                vec![block(0, vec![recv(2, 1), ret(vec![])], vec![])],
            ),
        ],
    )]);
    let f = p.func(p.lookup_func("t.F").unwrap()).unwrap();
    let cands = candidates(&p, f);
    assert_eq!(cands.len(), 1, "one defer hop candidate: {cands:?}");
    assert_eq!(cands[0].kind, CandKind::Recv);
    assert_eq!(
        cands[0].hop.as_ref().map(|h| h.kind),
        Some(HopKind::Defer)
    );
    assert_eq!(cands[0].alloc_value, ValueId(2));
}
```

- [ ] **Step 3: Run to verify it fails**

Run: `mise x -- cargo test -p goverify-checkers defer_static_hop`
Expected: compile FAIL — `HopKind` has no variant `Defer`.

- [ ] **Step 4: Add `Defer` to `HopKind` and the scan match**

Add `Defer,` to `HopKind`. In the hop-site match, add the arm:

```rust
                        Op::Defer {
                            callee: Callee::Static(h),
                            args,
                        } => (HopKind::Defer, *h, args),
```

- [ ] **Step 5: Run to verify it passes**

Run: `mise x -- cargo test -p goverify-checkers --lib`
Expected: PASS.

- [ ] **Step 6: Format, lint, commit**

```bash
mise run fmt && mise run lint
git add crates/goverify-checkers/src/leak.rs crates/goverify-checkers/src/testfix.rs
git commit -m "hop: Defer edges as hop sites (errgroup-done shape)"
```

---

### Task 4: Single-store cell bridge (`map_through_hop`)

**Files:**
- Modify: `crates/goverify-checkers/src/leak.rs`

**Interfaces:**
- Consumes: `arg_loc`, `fv_loc`, `resolve_loc`, `canonical_value` (from `crate::shared`), `MAX_LOC_DEPTH`.
- Produces: `fn map_through_hop(callee_loc: &Loc, g: &Function, args: &[ValueId], cb: &HashMap<FuncId, Option<Vec<ValueId>>>, h: FuncId) -> Loc` and `fn bridge_cell(g: &Function, cell: ValueId, tail: Vec<u32>) -> Loc`; the hop scan's first mapping switches from `map_through_site` to `map_through_hop`.

**Why:** go/ssa spills a closure-captured param to an `Alloc` cell (the closure binds the cell, not the param), so the deferred-closure shape (`doCall$1`) resolves its channel to a *g-local* Alloc root, which plain `map_through_site` degrades to `Unknown`. The bridge re-roots a single-store cell through its stored value when that value resolves to a `Param`/`FreeVar` of `g` — mirroring `cap_of`'s cell form, one level only. This amends spec §3 (documented in Task 9).

- [ ] **Step 1: Write the failing tests**

```rust
/// F: v2 = make(chan,0); go t.G(v2). G(p1): cell v3 = alloc;
/// store v3 <- p1; v4 = make_closure t.H [v3]; defer via v4.
/// H(fv1): v2 = load fv1; send v2 <- v2 — the singleflight doCall$1
/// shape: the captured param spills to a g-local cell; the hop
/// mapping's single-store bridge re-roots it back to Param(0), then
/// through the go site to F's alloc.
#[test]
fn deferred_closure_capturing_param_bridges_cell_to_hop_candidate() {
    let p = Program::from_packages(vec![pkg(
        "t",
        vec![
            func_with_aux("t.F", vec![const_int_aux(1, 0)], hop_f_blocks()),
            gvir::Function {
                id: "t.G".into(),
                params: vec![gvir::Param {
                    id: 1,
                    name: "c".into(),
                    r#type: 0,
                }],
                aux: vec![fn_aux(2, "t.H")],
                blocks: vec![block(
                    0,
                    vec![
                        alloc_instr(3),
                        store(3, 1),
                        make_closure(4, 2, vec![3]),
                        defer_call_via_closure("t.H", 4),
                        ret(vec![]),
                    ],
                    vec![],
                )],
                ..Default::default()
            },
            func_with_free_vars(
                "t.H",
                vec![1],
                vec![block(
                    0,
                    vec![load_instr(2, 1), send(2, 2), ret(vec![])],
                    vec![],
                )],
            ),
        ],
    )]);
    let f = p.func(p.lookup_func("t.F").unwrap()).unwrap();
    let cands = candidates(&p, f);
    assert_eq!(cands.len(), 1, "cell-bridged hop candidate: {cands:?}");
    let c = &cands[0];
    assert_eq!(c.kind, CandKind::Send);
    assert_eq!(c.hop.as_ref().map(|h| h.kind), Some(HopKind::Defer));
    assert_eq!(c.alloc_value, ValueId(2));
    assert_eq!(
        c.spawner_loc,
        Loc {
            root: Root::Alloc(2),
            path: vec![]
        }
    );
}

/// Same shape but the cell is stored twice: content is unresolvable
/// (mirrors cap_of's rule), so the bridge degrades and no candidate
/// survives.
#[test]
fn double_stored_cell_yields_no_hop_candidate() {
    let p = Program::from_packages(vec![pkg(
        "t",
        vec![
            func_with_aux("t.F", vec![const_int_aux(1, 0)], hop_f_blocks()),
            gvir::Function {
                id: "t.G".into(),
                params: vec![gvir::Param {
                    id: 1,
                    name: "c".into(),
                    r#type: 0,
                }],
                aux: vec![fn_aux(2, "t.H")],
                blocks: vec![block(
                    0,
                    vec![
                        alloc_instr(3),
                        store(3, 1),
                        store(3, 1),
                        make_closure(4, 2, vec![3]),
                        defer_call_via_closure("t.H", 4),
                        ret(vec![]),
                    ],
                    vec![],
                )],
                ..Default::default()
            },
            func_with_free_vars(
                "t.H",
                vec![1],
                vec![block(
                    0,
                    vec![load_instr(2, 1), send(2, 2), ret(vec![])],
                    vec![],
                )],
            ),
        ],
    )]);
    let f = p.func(p.lookup_func("t.F").unwrap()).unwrap();
    assert!(
        candidates(&p, f).is_empty(),
        "double-stored cell must not bridge"
    );
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `mise x -- cargo test -p goverify-checkers --lib cell`
Expected: first test FAILS (0 candidates — the cell root degrades to Unknown today); second PASSES vacuously (that's fine — it pins the negative once the bridge exists).

- [ ] **Step 3: Implement the bridge**

Below `map_through_site`, add:

```rust
/// `map_through_site` for the g→h hop, with one extra rule the go-site
/// mapping doesn't need: a base rooted at a g-LOCAL `Alloc` cell is
/// bridged through the cell's single stored value (go/ssa spills a
/// closure-captured param to a cell and binds the cell, so the
/// deferred-closure shape — spec §1's doCall$1 — otherwise dead-ends at
/// an Alloc-in-g root). One bridge application only: a cell whose
/// content is another cell degrades to Unknown — the observed real
/// shape is one cell deep, and each extra level multiplies the
/// unsoundness surface (spec §3 amendment, 2026-07-27).
fn map_through_hop(
    callee_loc: &Loc,
    g: &Function,
    args: &[ValueId],
    cb: &HashMap<FuncId, Option<Vec<ValueId>>>,
    h: FuncId,
) -> Loc {
    let mut base = match &callee_loc.root {
        Root::Param(i) => arg_loc(g, args, *i),
        Root::FreeVar(i) => fv_loc(g, cb, h, *i),
        _ => return Loc::unknown(),
    };
    if let Root::Alloc(a) = base.root {
        base = bridge_cell(g, ValueId(a), base.path);
    }
    if base.root == Root::Unknown {
        return Loc::unknown();
    }
    let mut path = base.path;
    path.extend(callee_loc.path.iter().copied());
    if path.len() > MAX_LOC_DEPTH {
        return Loc::unknown();
    }
    Loc {
        root: base.root,
        path,
    }
}

/// The single-store cell bridge (mirrors `cap_of`'s cell form, one
/// level only): `cell` must be defined by an `Op::Alloc` in `g`, have
/// exactly one `Store{addr == cell}`, and that store's value must
/// canonically resolve to a `Param`/`FreeVar`-rooted `Loc` of `g` —
/// returned with `tail` appended (the path the caller had accumulated
/// past the cell). Anything else — no alloc def, zero or two stores,
/// non-Param/FreeVar content, a path past `MAX_LOC_DEPTH` — degrades to
/// `Loc::unknown()`. Total and panic-free.
fn bridge_cell(g: &Function, cell: ValueId, tail: Vec<u32>) -> Loc {
    let is_alloc = g
        .blocks
        .iter()
        .flat_map(|b| &b.instrs)
        .any(|ins| matches!(&ins.op, Op::Alloc { dst, .. } if *dst == cell));
    if !is_alloc {
        return Loc::unknown();
    }
    let mut stores = g
        .blocks
        .iter()
        .flat_map(|b| &b.instrs)
        .filter_map(|ins| match &ins.op {
            Op::Store { addr, val } if *addr == cell => Some(*val),
            _ => None,
        });
    let Some(first) = stores.next() else {
        return Loc::unknown();
    };
    if stores.next().is_some() {
        return Loc::unknown(); // two stores: content unresolvable
    }
    let content = resolve_loc(g, canonical_value(g, first));
    if !matches!(content.root, Root::Param(_) | Root::FreeVar(_)) {
        return Loc::unknown();
    }
    let mut path = content.path;
    path.extend(tail);
    if path.len() > MAX_LOC_DEPTH {
        return Loc::unknown();
    }
    Loc {
        root: content.root,
        path,
    }
}
```

In the hop scan's `hop_map`, replace the first mapping:

```rust
                    let hop_map = |l: &Loc| {
                        let g_loc = map_through_hop(l, callee_f, hargs, &cb_g, h);
                        map_through_site(&g_loc, f, args, &cb, c)
                    };
```

(`canonical_value` is already imported from `crate::shared` at the top of leak.rs.)

- [ ] **Step 4: Run to verify all tests pass**

Run: `mise x -- cargo test -p goverify-checkers --lib`
Expected: PASS, both new tests green, no regressions.

- [ ] **Step 5: Format, lint, commit**

```bash
mise run fmt && mise run lint
git add crates/goverify-checkers/src/leak.rs
git commit -m "hop: single-store cell bridge for captured-param closures (doCall\$1 shape)"
```

---

### Task 5: `cap_class` hop gate

**Files:**
- Modify: `crates/goverify-checkers/src/leak.rs` (the `cap_class` function)

**Interfaces:**
- Consumes: `Candidate.hop` (Task 2), `cap_class`, `hop_send_pkg` (Task 2).
- Produces: hop + buffered-const **send** → `CapClass::Silent`; everything else unchanged. (This is what lets Task 6 skip the ordinal conjunct on the hop path entirely — the ordinal's frame bookkeeping is wrong for hops: the fill count would span two frames, and a `Defer` hop runs after ALL of `g`, not after the defer site. Additionally `cap_class`'s cyclic check indexes `cand.op_block` against `cand.callee`'s blocks, which for a hop candidate is the WRONG function — the gate must fire before that lookup.)

- [ ] **Step 1: Write the failing tests**

```rust
/// Buffered (cap 1) send via a hop: the ordinal fill-count would span
/// two frames (and is wrong outright for Defer hops), so
/// hop+BufferedConst degrades to Silent (spec §5.1).
#[test]
fn hop_buffered_send_is_silent() {
    let p = hop_send_pkg(hop_f_blocks(), 1);
    let f = p.func(p.lookup_func("t.F").unwrap()).unwrap();
    let cands = candidates(&p, f);
    assert_eq!(cands.len(), 1, "one hop candidate: {cands:?}");
    assert!(cands[0].hop.is_some());
    assert_eq!(cap_class(&p, f, &cands[0]), CapClass::Silent);
}

/// Buffered recv via a hop stays reachability-only (Unbuffered): a
/// recv with zero senders blocks regardless of buffer size — no
/// ordinal is ever involved, so the hop changes nothing.
#[test]
fn hop_buffered_recv_stays_unbuffered() {
    let p = Program::from_packages(vec![pkg(
        "t",
        vec![
            func_with_aux("t.F", vec![const_int_aux(1, 1)], hop_f_blocks()),
            func_with_params(
                "t.G",
                vec![gvir::Param {
                    id: 1,
                    name: "c".into(),
                    r#type: 0,
                }],
                vec![block(
                    0,
                    vec![call_static("t.H", 2, 0, vec![1]), ret(vec![])],
                    vec![],
                )],
            ),
            func_with_params(
                "t.H",
                vec![gvir::Param {
                    id: 1,
                    name: "c".into(),
                    r#type: 0,
                }],
                vec![block(0, vec![recv(2, 1), ret(vec![])], vec![])],
            ),
        ],
    )]);
    let f = p.func(p.lookup_func("t.F").unwrap()).unwrap();
    let cands = candidates(&p, f);
    assert_eq!(cands.len(), 1, "one hop recv candidate: {cands:?}");
    assert_eq!(cap_class(&p, f, &cands[0]), CapClass::Unbuffered);
}
```

- [ ] **Step 2: Run to verify the first fails**

Run: `mise x -- cargo test -p goverify-checkers hop_buffered`
Expected: `hop_buffered_send_is_silent` FAILS (returns `BufferedConst(1)` or hits the wrong-frame cyclic lookup); `hop_buffered_recv_stays_unbuffered` PASSES (recv never reaches the send arm — it pins that the gate below doesn't overreach).

- [ ] **Step 3: Add the gate**

In `cap_class`, at the top of the `Some(n) =>` arm (before the `p.func(cand.callee)` lookup):

```rust
        Some(n) => {
            // Only `Send` remains: `Select` returned above, `Recv`
            // matched the prior arm regardless of its capacity value.
            if cand.hop.is_some() {
                // Spec §5.1: a hop candidate's fill count would span
                // two frames, and for a Defer hop the DAG-ancestor rule
                // is wrong outright (a deferred helper runs after ALL
                // of g, not after the defer site). Never emit a wrong
                // ordinal — buffered hop sends are Silent. This also
                // guards the cyclic check below, whose `cand.op_block`
                // indexes the helper's frame for a hop candidate, not
                // `cand.callee`'s.
                return CapClass::Silent;
            }
```

- [ ] **Step 4: Run to verify both pass**

Run: `mise x -- cargo test -p goverify-checkers --lib`
Expected: PASS.

- [ ] **Step 5: Format, lint, commit**

```bash
mise run fmt && mise run lint
git add crates/goverify-checkers/src/leak.rs
git commit -m "hop: buffered-const hop sends degrade to Silent (spec 5.1)"
```

---

### Task 6: Three-frame obligation query, hop messages, version 2

**Files:**
- Modify: `crates/goverify-checkers/src/leak.rs` (`obligations`, `tag_and_message`, `version`)

**Interfaces:**
- Consumes: `prefixed`, `conjoin`, `EncodedFunc::reach_query(block, extra: Vec<Term>)`, `own_preconditions`, Task 5's cap gate (hop candidates are never `BufferedConst` here).
- Produces: hop obligations with query `f-frame ∧ "s_"-prefixed g-frame ∧ "t_"-prefixed h-frame`; hop messages of the exact form `goroutine {g} may block forever: send in helper {h} on a spawner-created channel with no receive, close, or select in the spawning environment` (recv: `receive in helper {h} … with no send, close, or select`; select: `blocking select in helper {h} whose channels have no counterpart operations`); `LeakChecker::version() == 2`.

- [ ] **Step 1: Write the failing tests**

Add `no_summaries` to the `crate::testfix` use list in `mod tests`, then:

```rust
/// obligations() on the plain-call hop program: one obligation whose
/// message names the helper and whose query carries three disjoint
/// const namespaces (unprefixed f, s_ g, t_ h).
#[test]
fn hop_obligation_query_has_three_disjoint_frames() {
    let p = hop_send_pkg(hop_f_blocks(), 0);
    let f = p.lookup_func("t.F").unwrap();
    let obs = LeakChecker.obligations(&p, f, &no_summaries);
    assert_eq!(obs.len(), 1, "one hop obligation: {obs:?}");
    assert_eq!(obs[0].tag, "chan-send-leak");
    assert_eq!(
        obs[0].message,
        "goroutine t.G may block forever: send in helper t.H on a spawner-created channel with no receive, close, or select in the spawning environment"
    );
    let (mut plain, mut s, mut t) = (0u32, 0u32, 0u32);
    for (n, _) in &obs[0].query.consts {
        if n.starts_with("s_") {
            s += 1;
        } else if n.starts_with("t_") {
            t += 1;
        } else {
            plain += 1;
        }
    }
    assert!(
        plain > 0 && s > 0 && t > 0,
        "three frames expected: {:?}",
        obs[0].query.consts
    );
}

/// An f-side escape (the channel is returned) silences a hop candidate
/// exactly as it silences a direct one — the escape walk is untouched.
#[test]
fn hop_obligation_silenced_by_f_side_escape() {
    let p = hop_send_pkg(
        vec![block(
            0,
            vec![
                gvir_make_chan(2, 1),
                go_call_args("t.G", vec![2]),
                ret(vec![2]),
            ],
            vec![],
        )],
        0,
    );
    let f = p.lookup_func("t.F").unwrap();
    assert!(
        LeakChecker.obligations(&p, f, &no_summaries).is_empty(),
        "returned channel must silence the hop candidate"
    );
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `mise x -- cargo test -p goverify-checkers hop_obligation`
Expected: `hop_obligation_query_has_three_disjoint_frames` FAILS — 0 obligations (Task 2's transitional guard skips hop candidates). The escape test may already pass (pins the invariant).

- [ ] **Step 3: Implement**

1. Bump the version (comment stays):

```rust
    // bump on any semantic change to this checker's obligations.
    // 2 (2026-07-27): one-hop nested-helper anchoring — checker
    // (name, version) salts the SCC cache, so this rotates every cached
    // findings entry; no SCC_CACHE_VERSION bump (codec unchanged).
    fn version(&self) -> u32 {
        2
    }
```

2. `tag_and_message` grows a helper-name clause (direct messages byte-identical: `via` is empty for `hop: None`):

```rust
fn tag_and_message(p: &Program, cand: &Candidate) -> (&'static str, String) {
    let callee = p.func_name(cand.callee);
    let via = match &cand.hop {
        Some(h) => format!(" in helper {}", p.func_name(h.helper)),
        None => String::new(),
    };
    match cand.kind {
        CandKind::Send => (
            "chan-send-leak",
            format!(
                "goroutine {callee} may block forever: send{via} on a spawner-created channel with no receive, close, or select in the spawning environment"
            ),
        ),
        CandKind::Recv => (
            "chan-recv-leak",
            format!(
                "goroutine {callee} may block forever: receive{via} on a spawner-created channel with no send, close, or select"
            ),
        ),
        CandKind::Select => (
            "chan-select-leak",
            format!(
                "goroutine {callee} may block forever: blocking select{via} whose channels have no counterpart operations"
            ),
        ),
    }
}
```

3. In `obligations`: delete Task 2's transitional guard, then replace the encoding + query construction (everything from the `enc_cache.entry(cand.callee)` line through the `Obligation` push's `query:` field) with:

```rust
            // Populate the needed per-function encodings first, then
            // re-borrow immutably: the hop case needs g's AND h's
            // EncodedFunc at once, and two live `entry()` results would
            // be two simultaneous mutable borrows.
            let helper = cand.hop.as_ref().map(|h| h.helper);
            for id in std::iter::once(cand.callee).chain(helper) {
                enc_cache
                    .entry(id)
                    .or_insert_with(|| encode_func_with(p, id, summary_of).ok());
            }
            let Some(enc_c) = enc_cache.get(&cand.callee).and_then(Option::as_ref) else {
                continue; // callee encoding failed: skip, never report
            };
            let query = match &cand.hop {
                None => {
                    let mut extra = own_preconditions(&summary_of(cand.callee));
                    if let CapClass::BufferedConst(cap) = class {
                        let Some(full) = ordinal_conjunct(callee_f, enc_c, &cand, cap) else {
                            continue;
                        };
                        extra.push(full);
                    }
                    let Some(cq) = prefixed(&enc_c.reach_query(cand.op_block, extra), "s_")
                    else {
                        continue;
                    };
                    conjoin(enc_f.reach_query(cand.go_block, pre_f.clone()), cq)
                }
                Some(hop) => {
                    // Three-frame conjunction (spec §4): f reaches the
                    // go site ∧ g reaches the call/defer site ∧ h
                    // reaches the op, each frame prefix-renamed apart.
                    // cap_class never hands a hop candidate
                    // BufferedConst (spec §5.1), so no ordinal conjunct
                    // on this path.
                    let Some(enc_h) = enc_cache.get(&hop.helper).and_then(Option::as_ref)
                    else {
                        continue; // helper encoding failed: skip, never report
                    };
                    let pre_g = own_preconditions(&summary_of(cand.callee));
                    let pre_h = own_preconditions(&summary_of(hop.helper));
                    let Some(gq) = prefixed(&enc_c.reach_query(hop.call_block, pre_g), "s_")
                    else {
                        continue;
                    };
                    let Some(hq) = prefixed(&enc_h.reach_query(cand.op_block, pre_h), "t_")
                    else {
                        continue;
                    };
                    conjoin(
                        conjoin(enc_f.reach_query(cand.go_block, pre_f.clone()), gq),
                        hq,
                    )
                }
            };
            let (tag, message) = tag_and_message(p, &cand);
            out.push(Obligation {
                tag: tag.into(),
                message,
                pos: cand.go_pos.clone(),
                query,
            });
```

(The `let Some(callee_f) = p.func(cand.callee)` lookup above this stays — the `None` arm's `ordinal_conjunct` still needs it. Update the per-candidate doc comment on the enc_cache block if it references "the callee-side query" — it is now "the g- and h-side queries".)

- [ ] **Step 4: Run to verify everything passes**

Run: `mise x -- cargo test -p goverify-checkers --lib && mise x -- cargo test -p goverify-checkers --test leak_corpus && mise x -- cargo test -p goverify-checkers --test scc_cache_invalidation`
Expected: PASS (corpus + invalidation suites confirm no direct-path regression; both runs inside one process share version 2, so the salt change is invisible to them).

- [ ] **Step 5: Format, lint, commit**

```bash
mise run fmt && mise run lint
git add crates/goverify-checkers/src/leak.rs
git commit -m "hop: three-frame conjoined obligation, helper-naming messages, checker version 2"
```

---

### Task 7: End-to-end Z3 tests

**Files:**
- Modify: `crates/goverify-checkers/src/leak.rs` (tests only, in the e2e section next to `leak_end_to_end_reports_via_z3`)

**Interfaces:**
- Consumes: `run_leak`, `hop_send_pkg`, `hop_f_blocks` (Tasks 2/6).

- [ ] **Step 1: Write the tests**

```rust
/// The three-frame conjoined query end to end: F spawns G, G calls H,
/// H sends on F's channel — a real Z3 must find the composed path Sat.
#[test]
fn hop_send_reports_via_z3() {
    let p = hop_send_pkg(hop_f_blocks(), 0);
    let findings = run_leak(&p);
    assert_eq!(findings.len(), 1, "exactly one finding: {findings:?}");
    assert_eq!(findings[0].checker, "goroutine-leak");
    assert_eq!(findings[0].tag, "chan-send-leak");
    assert_eq!(findings[0].severity, Severity::Error);
    assert!(
        findings[0].message.contains("in helper t.H"),
        "message names the helper: {}",
        findings[0].message
    );
}

/// Rule 3 through the hop: a receive in the spawning environment
/// unblocks the helper's send — silent (the counterpart env already
/// contains helper ops via effects rebasing; nothing hop-specific).
#[test]
fn hop_send_suppressed_when_spawner_recvs() {
    let p = hop_send_pkg(
        vec![block(
            0,
            vec![
                gvir_make_chan(2, 1),
                go_call_args("t.G", vec![2]),
                recv(3, 2),
                ret(vec![]),
            ],
            vec![],
        )],
        0,
    );
    let findings = run_leak(&p);
    assert!(
        findings.is_empty(),
        "spawner recv unblocks the helper send: {findings:?}"
    );
}
```

- [ ] **Step 2: Run to verify they pass**

Run: `mise x -- cargo test -p goverify-checkers hop_send`
Expected: PASS (these validate Tasks 2–6 against a real solver; a failure here means the conjoined query is malformed — debug with `Query`'s printer before touching the scan).

- [ ] **Step 3: Format, lint, commit**

```bash
mise run fmt && mise run lint
git add crates/goverify-checkers/src/leak.rs
git commit -m "hop: end-to-end z3 report + rule-3 suppression tests"
```

---

### Task 8: Corpus fixtures + want pins

**Files:**
- Modify: `testdata/corpus/leak/leak.go` (append; the corpus module imports nothing — keep it that way, stdlib imports blow corpus runtime)

**Interfaces:**
- Consumes: the full checker path (extractor → `.gvir` → engine → Z3); `leak_corpus.rs`'s want-comment matcher picks up `// want: <tag>` trailing comments automatically — no harness change.

- [ ] **Step 1: Append the fixtures**

```go
// Reported (one hop, plain call): the bbolt (*Tx).check shape — every
// send lives in a helper the goroutine calls.
func LeakHelperSend() {
	ch := make(chan int)
	go spawnHelperSend(ch) // want: chan-send-leak
}

func spawnHelperSend(c chan int) { helperSend(c) }

func helperSend(c chan int) { c <- 1 }

// Reported (one hop, deferred call): the errgroup (*Group).done shape —
// the recv is reached through a defer in the spawned callee.
func LeakDeferHelperRecv() {
	ch := make(chan int)
	go spawnDeferRecv(ch) // want: chan-recv-leak
}

func spawnDeferRecv(c chan int) { defer helperRecv(c) }

func helperRecv(c chan int) { <-c }

// Reported (one hop, deferred closure): the singleflight doCall$1
// shape — the send lives in a closure the spawned callee defers; the
// captured param spills to a cell the hop mapping's single-store
// bridge resolves.
func LeakDeferClosureSend() {
	ch := make(chan int)
	go spawnDeferClosure(ch) // want: chan-send-leak
}

func spawnDeferClosure(c chan int) {
	defer func() { c <- 1 }()
}

// Silent: the hop candidate's counterpart recv exists in the spawner
// (rule 3 consults summarized helper ops — nothing hop-specific).
func NoLeakHelperPaired() int {
	ch := make(chan int)
	go spawnHelperSend(ch)
	return <-ch
}

// Silent (v1 boundary pin — a REAL leak the one-hop rule cannot see):
// the blocking send is two calls below the spawned callee. Tripwire
// for any future depth change (spec §10 "depth ≥ 2 anchoring").
func SilentHelperDepth2() {
	ch := make(chan int)
	go depth2Spawnee(ch)
}

func depth2Spawnee(c chan int) { depth2Mid(c) }

func depth2Mid(c chan int) { helperSend(c) }

// Silent (v1 boundary pin — a REAL leak, documented): a buffered-const
// send via a hop never gets the ordinal fill-count argument (spec
// §5.1), so the second helper call's send — genuinely blocked, cap 1,
// two sends, no drain — stays silent.
func SilentHelperBuffered() {
	ch := make(chan int, 1)
	go bufSpawnee(ch)
}

func bufSpawnee(c chan int) {
	bufHelper(c)
	bufHelper(c)
}

func bufHelper(c chan int) { c <- 1 }
```

- [ ] **Step 2: Run the corpus suite**

Run: `mise x -- cargo test -p goverify-checkers --test leak_corpus`
Expected: PASS — `leak_corpus_findings_match_want_comments` proves exactly the three `want:` lines fire (and none of the Silent/NoLeak ones); `cold_and_warm_cache_runs_are_byte_identical` proves determinism holds with the new shapes.

If `LeakDeferClosureSend` does NOT fire: dump the IR (`mise x -- cargo run -p goverify-cli -- debug ir` from a scratch module containing just that fixture, or inspect with the corpus loader) and check whether the closure capture produced the expected single-store cell — the bridge (Task 4) only handles `Alloc` + one `Store`. Do not weaken the fixture to make it pass; diagnose first (superpowers:systematic-debugging).

- [ ] **Step 3: Run the full determinism suite**

Run: `mise run corpus`
Expected: PASS (extractor→.gvir byte-identity across the whole corpus, new fixtures included).

- [ ] **Step 4: Format (gofmt runs via mise), commit**

```bash
mise run fmt
git add testdata/corpus/leak/leak.go
git commit -m "hop: leak corpus fixtures for the three real one-hop shapes + boundary pins"
```

---

### Task 9: Docs — spec amendments, README, queue tick

**Files:**
- Modify: `docs/superpowers/specs/2026-07-26-phase7-goroutine-leaks-design.md` (§2 rule 1; §10)
- Modify: `docs/superpowers/specs/2026-07-27-nested-helper-anchoring-design.md` (§3 cell-bridge amendment)
- Modify: `docs/superpowers/plans/2026-07-26-phase7-goroutine-leaks.md` (queue tick)
- Modify: `README.md` (checker paragraph)

**Interfaces:** none (documentation).

- [ ] **Step 1: Amend phase-7 spec §2 rule 1**

Replace rule 1's sentence block (it currently reads "**Blocking op on an identified channel.** A `Send`, `Recv`, or blocking `Select` **syntactically present in the spawned callee's own body** — never a blocking op reached only through a helper…" through "…(§10: 'nested-helper blocking ops (cross-function obligation anchoring)').") with:

> 1. **Blocking op on an identified channel.** A `Send`, `Recv`, or
>    blocking `Select` syntactically present either **in the spawned
>    callee's own body**, or — one-hop anchoring, added 2026-07-27, see
>    `2026-07-27-nested-helper-anchoring-design.md` — **in the own body
>    of a helper reached by a single static `Call`/`Defer` instruction
>    in the spawned callee's body**, whose channel operand, rebased
>    through the hop site's arguments/closure bindings (with a
>    single-store cell bridge for captured params) and then through the
>    go site's, resolves to a `Loc` rooted at an **`Alloc` in `f`** (a
>    `make(chan …)` in the spawner reaching the goroutine via argument
>    or capture). Blocking ops two or more call levels down still flow
>    into the goroutine's *summarized* `chan_ops` (rebased in by
>    `effects::collect`) and can suppress via rule 3's counterpart
>    match, but are never themselves the subject of a finding (§10:
>    "depth ≥ 2 anchoring").

- [ ] **Step 2: Amend phase-7 spec §10**

Find the §10 deferred item "nested-helper blocking ops (cross-function obligation anchoring)" and replace it with:

> - depth ≥ 2 anchoring (one-hop shipped 2026-07-27; see that wave's
>   spec §10 for its own residuals: cross-frame buffered fill counting,
>   Defer-hop exit-reachability, nested-spawn anchoring).

- [ ] **Step 3: Amend the new spec's §3 with the cell bridge**

In `2026-07-27-nested-helper-anchoring-design.md` §3, after the sentence ending "…so goroutine-internal channels drop out, as today.", append this paragraph:

> **Implementation amendment (Task 4):** the hop-site mapping is
> `map_through_hop`, which adds one rule `map_through_site` doesn't
> have: a base rooted at a *g-local* `Alloc` cell is bridged through
> the cell's single stored value when that value canonically resolves
> to a `Param`/`FreeVar` of `g` (mirroring `cap_of`'s cell form, one
> level only — nested cells degrade to Unknown). Without it the
> deferred-closure shape (§1's `doCall$1`) dead-ends: go/ssa spills a
> closure-captured param to a cell and binds the cell, so the naive
> mapping roots at Alloc-in-`g` and degrades. Two stores into the cell,
> or non-Param/FreeVar content, degrade to Unknown as before.

- [ ] **Step 4: Tick the consumed queue item in the phase-7 plan**

In `docs/superpowers/plans/2026-07-26-phase7-goroutine-leaks.md`'s "Follow-up queue" section, change:

`- Nested-helper blocking ops (cross-function obligation anchoring — Task 10 amendment 2).`

to:

`- ~~Nested-helper blocking ops (cross-function obligation anchoring — Task 10 amendment 2).~~ SHIPPED 2026-07-27 (one hop): docs/superpowers/specs/2026-07-27-nested-helper-anchoring-design.md.`

- [ ] **Step 5: README**

In the `goroutine-leak` checker bullet, after "…spawns a goroutine which, on some satisfiable execution, blocks forever on a channel send/receive/select that nothing reachable from the spawning function can ever unblock." insert:

> A blocking op sitting one static call or `defer` below the spawned
> callee is anchored too (one hop); deeper nesting is deliberately
> silent.

And update the trailing spec pointer sentence to reference both specs: "…see the [design spec](docs/superpowers/specs/2026-07-26-phase7-goroutine-leaks-design.md) and the [one-hop anchoring spec](docs/superpowers/specs/2026-07-27-nested-helper-anchoring-design.md) for the full scope boundary."

- [ ] **Step 6: Self-check and commit**

Grep-check no stale text remains:

```bash
grep -n "never a blocking op reached only through a helper" docs/superpowers/specs/2026-07-26-phase7-goroutine-leaks-design.md   # expect: no match
grep -n "nested-helper op is never itself the subject" docs/superpowers/specs/2026-07-26-phase7-goroutine-leaks-design.md        # expect: no match in §2 rule 1 (may remain in historical shakeout quotes elsewhere — those are records, leave them)
git add docs/superpowers/specs/2026-07-26-phase7-goroutine-leaks-design.md docs/superpowers/specs/2026-07-27-nested-helper-anchoring-design.md docs/superpowers/plans/2026-07-26-phase7-goroutine-leaks.md README.md
git commit -m "hop: spec amendments (rule 1 one-hop form, cell bridge), README, queue tick"
```

---

### Task 10: Full gates + shakeout regression re-runs + record

**Files:**
- Modify: `docs/superpowers/specs/2026-07-26-phase7-shakeout.md` (append a re-run section)

**Interfaces:** none (verification + record).

This is a **regression gate, not a recall gate** (spec §1): expected bbolt count is **457, byte-identical**, expected x/sync leak count is **0**. Any delta STOPS the wave for per-finding adjudication — do not update baselines unilaterally.

- [ ] **Step 1: Blocking CI tier**

Run: `mise run fmt && mise run lint && mise run test && mise run corpus && mise run secrets && mise run audit`
Expected: all green. (`mise run test` includes the leak unit/e2e/corpus/invalidation suites.)

- [ ] **Step 2: bbolt shakeout**

Run (release build + pinned clone; if the freshly built binary's first exec stalls for minutes, that is the known EDR new-file verdict delay — wait it out or retry, do not rebuild in a loop):

```bash
mise x -- bash -c './scripts/shakeout.sh > /tmp/hop-bbolt.txt 2>/tmp/hop-bbolt.err; echo "exit=$?"'
grep -cE '^[^ ].*: (nil-deref|bounds|div-zero|overflow): ' /tmp/hop-bbolt.txt   # expect 457
grep -cE '^[^ ].*: chan-(send|recv|select)-leak: ' /tmp/hop-bbolt.txt          # expect 0
```

Expected: 457 nil/bounds findings, 0 leak findings (bbolt's real leak, `(*Tx).Check`, becomes a hop *candidate* this wave but is still rule-2-silenced by `return ch` — that trace is worth spot-confirming with `goverify debug` if quick). If either count differs: STOP, hand-trace every delta against spec §2's rules, and surface the adjudication to the user before any further step.

- [ ] **Step 3: x/sync shakeout**

```bash
mise x -- bash -c './scripts/shakeout_conc.sh > /tmp/hop-sync.txt 2>/tmp/hop-sync.err; echo "exit=$?"'
grep -cE '^[^ ].*: chan-(send|recv|select)-leak: ' /tmp/hop-sync.txt           # expect 0
```

Expected: 0 leak findings. The `done`/`doCall$1` shapes now construct candidates and are silenced by rule 3 / SCC-widening saturation — that is the evidence the hop machinery engages on real code even at count 0.

- [ ] **Step 4: Append the re-run record**

Append a section to `docs/superpowers/specs/2026-07-26-phase7-shakeout.md` titled "Re-run after one-hop nested-helper anchoring (2026-07-27)" recording: the two count greps and their values; confirmation the bbolt output is unchanged vs. the wave's base (re-run the script from the pre-wave commit if a byte-level `cmp` is wanted, or record counts + spot traces); one short trace each for `(*Tx).Check` (hop candidate → rule-2 escape) and `errgroup done` (hop candidate → rule-3/saturation) as the machinery-engaged evidence; wall-clock of a warm bbolt run vs. the 3.41 s phase-7 figure (<15 % budget — hop candidates on bbolt are silenced before encoding, so expect noise-level change).

- [ ] **Step 5: Commit**

```bash
git add docs/superpowers/specs/2026-07-26-phase7-shakeout.md
git commit -m "hop: shakeout regression re-run record (bbolt 457/0, x/sync 0)"
```

---

## Follow-up queue (plan owner; do NOT implement)

Carried from the spec (§10):

- Depth ≥ 2 anchoring (generalize the hop to a capped chain; `SilentHelperDepth2` is the tripwire pin).
- Cross-frame buffered fill counting for plain-`Call` hops (`SilentHelperBuffered` is the pin).
- `Defer`-hop exit-reachability conjunct (close the "g never returns" over-approximation).
- Nested-spawn anchoring (`Go` edges inside the spawned callee).
- Multi-level cell bridging (the Task 4 bridge is deliberately one cell deep).
