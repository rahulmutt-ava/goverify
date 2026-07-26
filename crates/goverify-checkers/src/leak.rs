//! Goroutine-leak checker (phase 7, spec §2): a spawned goroutine that
//! blocks on a channel operation whose channel is spawner-owned (rooted
//! at an `Op::Make`/`Op::Alloc` site back in the spawning function `f`,
//! per `goverify_analysis::resolve_loc`) and never escapes `f`'s control
//! is a leak candidate — nothing outside `f` can ever unblock it once
//! `f` stops referencing the channel. This module owns the two pure,
//! syntactic passes: `candidates` (v1: a syntactically-direct scan of a
//! spawned callee's own body for blocking ops) and `escapes` (a
//! whitelist-based alias/use walk, spec §2 rule 2's strict form). Task 6
//! adds counterpart/capacity reasoning (does anything else ever unblock
//! this op?); Task 7 wires both into the `Checker` impl.
//!
//! `#![allow(dead_code)]`: every item below `candidates`/`escapes`
//! themselves is exercised only by this module's own `#[cfg(test)]`
//! block until Task 7 wires them into a `Checker` impl — the same
//! "prefer `#[allow(dead_code)]` + a note over a tautological test"
//! call the phase-7 plan's Task 4 carry-forward already made for this
//! exact staged-wiring situation. Drop this once Task 7 lands.
#![allow(dead_code)]

use std::collections::{HashMap, HashSet};

use goverify_analysis::{Loc, Root, closure_bindings, fv_loc, resolve_loc};
use goverify_ir::{Callee, FuncId, Function, Op, Pos, Program, ValueId};

/// The blocking-op shape a [`Candidate`] was found at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CandKind {
    Send,
    Recv,
    Select,
}

/// A syntactically-direct blocking op, found in a spawned goroutine's own
/// body, whose channel is rooted at an `Alloc` site back in the spawner
/// (`f`). Task 6 re-checks each one against the spawner's own channel-op
/// effects (does anything else ever unblock it?) and the escape walk
/// (does the channel leave `f`'s control?) before promoting it to a
/// finding.
#[derive(Debug, Clone)]
pub(crate) struct Candidate {
    /// Block index of the `Op::Go` in `f`.
    pub go_block: usize,
    pub go_pos: Option<Pos>,
    /// The spawned function (has a body).
    pub callee: FuncId,
    /// Block index of the blocking op in `callee`.
    pub op_block: usize,
    pub kind: CandKind,
    /// `Root::Alloc` id in `f` — the escape/capacity subject.
    pub alloc_value: ValueId,
    /// Alloc-rooted `Loc` in `f` — the counterpart key.
    pub spawner_loc: Loc,
    /// Select candidates only: every arm's spawner-side `Loc`, paired
    /// with the arm's dir (1 = send, 2 = recv). Empty for Send/Recv
    /// candidates.
    pub arm_locs: Vec<(u32, Loc)>,
}

/// `resolve_loc(f, args[i])` — a `Go` call site's `i`th argument,
/// resolved in the *spawner*'s own frame; `Loc::unknown()` for an
/// out-of-range `i` (malformed/fuzzed input, never a panic).
fn arg_loc(f: &Function, args: &[ValueId], i: u32) -> Loc {
    args.get(i as usize)
        .map_or(Loc::unknown(), |&a| resolve_loc(f, a))
}

/// Re-roots a callee-local `Loc` (as returned by `resolve_loc` on a value
/// inside the spawned function's own body) through this `Go` call site's
/// argument/closure-binding mappings, back into the spawner `f`'s frame:
/// a `Param(i)`-rooted `Loc` re-roots through `arg_loc(f, args, i)`; a
/// `FreeVar(i)`-rooted `Loc` re-roots through the closure binding
/// `fv_loc(f, cb, c, i)` (phase 7); anything else (a callee-local
/// `Alloc`, a `Global`, or an already-`Unknown` root) has no
/// caller-visible identity here and degrades to `Loc::unknown()` —
/// mirrors `goverify_analysis::effects`'s private per-call-site
/// `rebase_loc`, specialized to a single `Loc` (that helper works over a
/// whole `Effects` map and is private to goverify-analysis; duplicating
/// just this much of its logic here is cheaper than exposing it).
fn map_through_site(
    callee_loc: &Loc,
    f: &Function,
    args: &[ValueId],
    cb: &HashMap<FuncId, Option<Vec<ValueId>>>,
    c: FuncId,
) -> Loc {
    let base = match &callee_loc.root {
        Root::Param(i) => arg_loc(f, args, *i),
        Root::FreeVar(i) => fv_loc(f, cb, c, *i),
        _ => return Loc::unknown(),
    };
    if base.root == Root::Unknown {
        return Loc::unknown();
    }
    let mut path = base.path;
    path.extend(callee_loc.path.iter().copied());
    Loc {
        root: base.root,
        path,
    }
}

/// Scans `f`'s blocks in index order for `Op::Go { callee: Callee::Static
/// }` sites whose target has a body, then scans that target's *own* body
/// (v1: syntactically-direct only — a blocking op reached only through a
/// helper function is never reported here, though it may still matter to
/// Task 6's suppression logic) for `Send`/`Recv`/blocking-`Select` ops
/// whose channel is rooted at one of `f`'s own `Alloc` sites (via the
/// call site's param/free-var mappings). Dynamic/invoke/builtin go
/// callees, callees without a body, and non-blocking selects contribute
/// nothing. A blocking `Select` needs *every* arm mapped to `Root::Alloc`
/// (the first arm's alloc/loc becomes the `Candidate`'s own fields;
/// `arm_locs` carries all of them, for Task 6 to re-check each one).
/// Total and panic-free: every id lookup is bounds-checked; an
/// unrecognized or unmapped shape simply yields no candidate.
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
            for (op_block, cblk) in callee_f.blocks.iter().enumerate() {
                for cins in &cblk.instrs {
                    match &cins.op {
                        Op::Send { chan, .. } => {
                            let callee_loc = resolve_loc(callee_f, *chan);
                            let mapped = map_through_site(&callee_loc, f, args, &cb, c);
                            if let Root::Alloc(a) = mapped.root {
                                out.push(Candidate {
                                    go_block: bi,
                                    go_pos: ins.pos.clone(),
                                    callee: c,
                                    op_block,
                                    kind: CandKind::Send,
                                    alloc_value: ValueId(a),
                                    spawner_loc: mapped,
                                    arm_locs: Vec::new(),
                                });
                            }
                        }
                        Op::Recv { chan, .. } => {
                            let callee_loc = resolve_loc(callee_f, *chan);
                            let mapped = map_through_site(&callee_loc, f, args, &cb, c);
                            if let Root::Alloc(a) = mapped.root {
                                out.push(Candidate {
                                    go_block: bi,
                                    go_pos: ins.pos.clone(),
                                    callee: c,
                                    op_block,
                                    kind: CandKind::Recv,
                                    alloc_value: ValueId(a),
                                    spawner_loc: mapped,
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
                                let callee_loc = resolve_loc(callee_f, arm.chan);
                                let mapped = map_through_site(&callee_loc, f, args, &cb, c);
                                if !matches!(mapped.root, Root::Alloc(_)) {
                                    all_alloc = false;
                                    break;
                                }
                                mapped_arms.push((arm.dir, mapped));
                            }
                            if !all_alloc {
                                continue;
                            }
                            let Some(Root::Alloc(a)) =
                                mapped_arms.first().map(|(_, l)| l.root.clone())
                            else {
                                continue;
                            };
                            out.push(Candidate {
                                go_block: bi,
                                go_pos: ins.pos.clone(),
                                callee: c,
                                op_block,
                                kind: CandKind::Select,
                                alloc_value: ValueId(a),
                                spawner_loc: mapped_arms[0].1.clone(),
                                arm_locs: mapped_arms,
                            });
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    out
}

/// A `Callee::Dynamic`'s function-value operand, if any — `Static`,
/// `Invoke` (whose receiver already arrives via `args`, per `lower.rs`'s
/// operand-layout comment) and `Builtin` carry no `ValueId` of their own.
fn callee_reads(callee: &Callee) -> Vec<ValueId> {
    match callee {
        Callee::Dynamic { value } => vec![*value],
        _ => vec![],
    }
}

/// Every "read" `ValueId` an `Op` consumes (never a `dst`/def slot): the
/// exhaustive-by-default enumeration `instr_escapes`'s fallback arm uses,
/// so a newly added `Op` variant defaults to being scanned (and,
/// therefore, if it touches a tracked value, to escaping) the moment it's
/// added here — matched exhaustively over every `Op` variant so the
/// compiler itself forces this function to be updated when the enum
/// grows.
fn op_reads(op: &Op) -> Vec<ValueId> {
    match op {
        Op::Assign { src, .. } | Op::Convert { src, .. } => vec![*src],
        Op::Alloc { .. } => vec![],
        Op::Load { addr, .. } => vec![*addr],
        Op::Store { addr, val } => vec![*addr, *val],
        Op::FieldAddr { base, .. } | Op::Field { base, .. } => vec![*base],
        Op::IndexAddr { base, index, .. } | Op::Index { base, index, .. } => {
            vec![*base, *index]
        }
        Op::Lookup { map, key, .. } => vec![*map, *key],
        Op::Slice {
            base,
            low,
            high,
            max,
            ..
        } => {
            let mut v = vec![*base];
            v.extend(low.iter().copied());
            v.extend(high.iter().copied());
            v.extend(max.iter().copied());
            v
        }
        Op::BinOp { lhs, rhs, .. } => vec![*lhs, *rhs],
        Op::UnOp { operand, .. } => vec![*operand],
        Op::Extract { tuple, .. } => vec![*tuple],
        Op::Phi { edges, .. } => edges.clone(),
        Op::Call { callee, args, .. } => callee_reads(callee)
            .into_iter()
            .chain(args.iter().copied())
            .collect(),
        Op::MakeClosure { bindings, .. } => bindings.clone(),
        Op::MakeInterface { src, .. } => vec![*src],
        Op::Make { args, .. } => args.clone(),
        Op::Send { chan, val } => vec![*chan, *val],
        Op::Recv { chan, .. } => vec![*chan],
        Op::CloseChan { chan } => vec![*chan],
        Op::Select { arms, .. } => arms
            .iter()
            .flat_map(|a| std::iter::once(a.chan).chain(a.send))
            .collect(),
        Op::Go { callee, args } | Op::Defer { callee, args } => callee_reads(callee)
            .into_iter()
            .chain(args.iter().copied())
            .collect(),
        Op::Return { vals } => vals.clone(),
        Op::Jump => vec![],
        Op::Branch { cond } => vec![*cond],
        Op::Panic { val } => vec![*val],
        Op::TypeAssert { src, .. } => vec![*src],
        Op::Lock { mu, .. } => vec![*mu],
        Op::Havoc { .. } => vec![],
    }
}

/// One instruction's contribution to the whitelist scan (spec §2 rule 2,
/// strict form) — see each arm's comment for the whitelisted/escaping
/// bullet it implements.
fn instr_escapes(op: &Op, is_tracked: &dyn Fn(&ValueId) -> bool) -> bool {
    match op {
        // Alias-extender ops themselves: using a tracked value as their
        // own def-chain source is exactly how aliasing propagates (see
        // `escapes`'s fixpoint loop below), not a use that escapes it.
        Op::Load { .. } | Op::Assign { .. } | Op::Convert { .. } | Op::Phi { .. } => false,
        // Defining/refilling a tracked cell is bookkeeping; storing a
        // tracked value into an untracked address escapes it
        // ("Store.val where addr∉chans").
        Op::Store { addr, val } => !is_tracked(addr) && is_tracked(val),
        // Bindings are tracked separately via the `closures` alias set
        // (`escapes`'s fixpoint loop); the capturing op itself is
        // whitelisted.
        Op::MakeClosure { .. } => false,
        // `Send.chan`/`Recv.chan`/`CloseChan.chan` are whitelisted; only
        // the value actually sent ("Send.val") can escape.
        Op::Send { val, .. } => is_tracked(val),
        Op::Recv { .. } | Op::CloseChan { .. } => false,
        // `SelectArm.chan` is whitelisted; `SelectArm.send` (the channel
        // sent as a value) can escape, same as `Send.val`.
        Op::Select { arms, .. } => arms.iter().any(|a| a.send.is_some_and(|s| is_tracked(&s))),
        // Plain `Call` args always escape (strict rule: even a static
        // callee could stash the channel somewhere effects don't model),
        // except the three builtins that only observe the channel.
        Op::Call { callee, args, .. } => {
            let callee_escapes = matches!(callee, Callee::Dynamic { value } if is_tracked(value));
            let whitelisted_builtin = matches!(
                callee,
                Callee::Builtin(name) if matches!(name.as_str(), "close" | "len" | "cap")
            );
            callee_escapes || (!whitelisted_builtin && args.iter().any(is_tracked))
        }
        // `Go`/`Defer` args are whitelisted only for a static callee or
        // the `close` builtin; a dynamic/invoke callee's args (and a
        // tracked dynamic callee value itself) escape.
        Op::Go { callee, args } | Op::Defer { callee, args } => {
            let callee_escapes = matches!(callee, Callee::Dynamic { value } if is_tracked(value));
            let whitelisted = matches!(callee, Callee::Static(_))
                || matches!(callee, Callee::Builtin(name) if name == "close");
            callee_escapes || (!whitelisted && args.iter().any(is_tracked))
        }
        // Everything else (`Return.vals`, `MakeInterface.src`, `BinOp`,
        // `Index`, `Lookup`, `Make` args, `TypeAssert`, `Extract`,
        // `Panic`, …): exhaustive-by-default fallback, so a new `Op`
        // variant defaults to escaping rather than being silently
        // exempted.
        _ => op_reads(op).iter().any(is_tracked),
    }
}

/// Whitelist-based use-walk (spec §2 rule 2, strict form): `ch` escapes
/// `f` iff it (or anything it, or a closure over it, aliases) is used in
/// a role other than plain channel-op subject, own-cell store, or a
/// same-function static spawn/defer/close argument. Two alias sets are
/// grown to fixpoint first — `chans` (seeded with `ch`, grown through
/// `Load`/`Assign`/`Convert`/`Phi`'s def edges and `Store`'s
/// cell-content rule) and `closures` (`MakeClosure` sites whose bindings
/// include a `chans` member) — then a single whitelist scan decides.
/// Total and panic-free: an op kind with no special case falls to the
/// exhaustive `op_reads` fallback, which conservatively treats any use
/// as an escape.
pub(crate) fn escapes(f: &Function, ch: ValueId) -> bool {
    let mut chans: HashSet<ValueId> = HashSet::new();
    chans.insert(ch);
    let mut closures: HashSet<ValueId> = HashSet::new();

    loop {
        let mut changed = false;
        for b in &f.blocks {
            for ins in &b.instrs {
                match &ins.op {
                    Op::Load { dst, addr } if chans.contains(addr) && chans.insert(*dst) => {
                        changed = true;
                    }
                    Op::Assign { dst, src } | Op::Convert { dst, src }
                        if chans.contains(src) && chans.insert(*dst) =>
                    {
                        changed = true;
                    }
                    Op::Phi { dst, edges }
                        if edges.iter().any(|e| chans.contains(e)) && chans.insert(*dst) =>
                    {
                        changed = true;
                    }
                    Op::Store { addr, val } if chans.contains(addr) && chans.insert(*val) => {
                        changed = true;
                    }
                    Op::MakeClosure { dst, bindings, .. }
                        if bindings.iter().any(|b| chans.contains(b)) && closures.insert(*dst) =>
                    {
                        changed = true;
                    }
                    _ => {}
                }
            }
        }
        if !changed {
            break;
        }
    }

    let is_tracked = |v: &ValueId| chans.contains(v) || closures.contains(v);
    for b in &f.blocks {
        for ins in &b.instrs {
            if instr_escapes(&ins.op, &is_tracked) {
                return true;
            }
        }
    }
    false
}

/// Placeholder for Task 7's `Checker` impl: declared now (with no
/// `Checker` impl yet) purely so `pub use leak::LeakChecker;` in lib.rs
/// has something to re-export. `default_checkers()` deliberately does
/// NOT include it yet — that wiring, plus the trait impl itself, is
/// Task 7's job.
pub struct LeakChecker;

#[cfg(test)]
mod tests {
    use goverify_extract::gvir;
    use goverify_ir::Program;

    use super::*;
    use crate::testfix::{
        alloc_instr, block, call_builtin, call_static, const_int_aux, fn_aux, func_with_aux,
        func_with_free_vars, func_with_params, go_call_args, go_call_dynamic, go_call_via_closure,
        gvir_make_chan, make_closure, make_interface_instr, pkg, recv, ret, select, send, store,
    };

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
                        vec![
                            gvir_make_chan(2, 1),
                            go_call_args("t.G", vec![2]),
                            ret(vec![]),
                        ],
                        vec![],
                    )],
                ),
                func_with_params(
                    "t.G",
                    vec![gvir::Param {
                        id: 1,
                        name: "c".into(),
                        r#type: 0,
                    }],
                    vec![block(0, vec![send(1, 1), ret(vec![])], vec![])],
                ),
            ],
        )]);
        let f = p.func(p.lookup_func("t.F").unwrap()).unwrap();
        let cands = candidates(&p, f);
        assert_eq!(cands.len(), 1, "exactly one candidate: {cands:?}");
        assert_eq!(cands[0].kind, CandKind::Send);
        assert_eq!(cands[0].alloc_value, ValueId(2));
        assert_eq!(
            cands[0].spawner_loc,
            Loc {
                root: Root::Alloc(2),
                path: vec![]
            }
        );
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

    /// F: make_chan(2,1); make_closure(3, fn_aux, [2]); go t.F$1(closure).
    /// t.F$1: FreeVar aux 1; recv(2, 1). Expect Recv at Alloc(2).
    #[test]
    fn closure_free_var_recv_yields_recv_candidate() {
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
                            go_call_via_closure("t.F$1", 3),
                            ret(vec![]),
                        ],
                        vec![],
                    )],
                ),
                func_with_free_vars(
                    "t.F$1",
                    vec![1],
                    vec![block(0, vec![recv(2, 1), ret(vec![])], vec![])],
                ),
            ],
        )]);
        let f = p.func(p.lookup_func("t.F").unwrap()).unwrap();
        let cands = candidates(&p, f);
        assert_eq!(cands.len(), 1, "exactly one candidate: {cands:?}");
        assert_eq!(cands[0].kind, CandKind::Recv);
        assert_eq!(cands[0].alloc_value, ValueId(2));
        assert_eq!(
            cands[0].spawner_loc,
            Loc {
                root: Root::Alloc(2),
                path: vec![]
            }
        );
    }

    /// F has param id 1 (no make); go_call_args("t.G", vec![1]) — the
    /// forwarded value is F's own param, never rooted at an Alloc site in
    /// F, so nothing is a candidate.
    #[test]
    fn param_rooted_channel_yields_no_candidate() {
        let p = Program::from_packages(vec![pkg(
            "t",
            vec![
                func_with_params(
                    "t.F",
                    vec![gvir::Param {
                        id: 1,
                        name: "c".into(),
                        r#type: 0,
                    }],
                    vec![block(
                        0,
                        vec![go_call_args("t.G", vec![1]), ret(vec![])],
                        vec![],
                    )],
                ),
                func_with_params(
                    "t.G",
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
        assert!(
            candidates(&p, f).is_empty(),
            "param-rooted channel must not be a candidate"
        );
    }

    /// `go` with a dynamic callee (empty static_callee/builtin) matches no
    /// `Callee::Static` site, so nothing is scanned.
    #[test]
    fn dynamic_go_callee_yields_no_candidate() {
        let p = Program::from_packages(vec![pkg(
            "t",
            vec![func_with_aux(
                "t.F",
                vec![const_int_aux(1, 0)],
                vec![block(
                    0,
                    vec![gvir_make_chan(2, 1), go_call_dynamic(), ret(vec![])],
                    vec![],
                )],
            )],
        )]);
        let f = p.func(p.lookup_func("t.F").unwrap()).unwrap();
        assert!(candidates(&p, f).is_empty());
    }

    /// `go ext.NoBody(ch)` where `ext.NoBody` is never declared as a
    /// `gvir::Function`: `p.func(c)` is `None`, so the go site is
    /// skipped entirely.
    #[test]
    fn bodyless_go_callee_yields_no_candidate() {
        let p = Program::from_packages(vec![pkg(
            "t",
            vec![func_with_aux(
                "t.F",
                vec![const_int_aux(1, 0)],
                vec![block(
                    0,
                    vec![
                        gvir_make_chan(2, 1),
                        go_call_args("ext.NoBody", vec![2]),
                        ret(vec![]),
                    ],
                    vec![],
                )],
            )],
        )]);
        let f = p.func(p.lookup_func("t.F").unwrap()).unwrap();
        assert!(candidates(&p, f).is_empty());
    }

    /// A non-blocking `select` never blocks, so it is never a candidate
    /// regardless of whether its arms map to an `Alloc` site.
    #[test]
    fn nonblocking_select_yields_no_candidate() {
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
                            go_call_args("t.G", vec![2]),
                            ret(vec![]),
                        ],
                        vec![],
                    )],
                ),
                func_with_params(
                    "t.G",
                    vec![gvir::Param {
                        id: 1,
                        name: "c".into(),
                        r#type: 0,
                    }],
                    vec![block(
                        0,
                        vec![select(2, vec![(1, 1, 1)], false), ret(vec![])],
                        vec![],
                    )],
                ),
            ],
        )]);
        let f = p.func(p.lookup_func("t.F").unwrap()).unwrap();
        assert!(candidates(&p, f).is_empty());
    }

    /// Two makes in F, both bound into the closure; the callee selects
    /// (blocking) over both FreeVars — every arm maps to an Alloc site,
    /// so one Select candidate carrying both arm locs is produced.
    #[test]
    fn blocking_select_all_alloc_arms_yields_select_candidate() {
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
                            gvir_make_chan(3, 1),
                            make_closure(4, 1, vec![2, 3]),
                            go_call_via_closure("t.F$1", 4),
                            ret(vec![]),
                        ],
                        vec![],
                    )],
                ),
                func_with_free_vars(
                    "t.F$1",
                    vec![1, 2],
                    vec![block(
                        0,
                        vec![select(3, vec![(2, 1, 0), (2, 2, 0)], true), ret(vec![])],
                        vec![],
                    )],
                ),
            ],
        )]);
        let f = p.func(p.lookup_func("t.F").unwrap()).unwrap();
        let cands = candidates(&p, f);
        assert_eq!(cands.len(), 1, "exactly one candidate: {cands:?}");
        assert_eq!(cands[0].kind, CandKind::Select);
        assert_eq!(cands[0].arm_locs.len(), 2);
        assert_eq!(
            cands[0].spawner_loc,
            Loc {
                root: Root::Alloc(2),
                path: vec![]
            }
        );
    }

    /// Plain channel-op uses (send/recv/close via the `close` builtin)
    /// never escape.
    #[test]
    fn plain_chan_op_uses_do_not_escape() {
        let p = Program::from_packages(vec![pkg(
            "t",
            vec![func_with_aux(
                "t.F",
                vec![const_int_aux(1, 0)],
                vec![block(
                    0,
                    vec![
                        gvir_make_chan(2, 1),
                        send(2, 1),
                        recv(3, 2),
                        call_builtin("close", 0, 0, vec![2]),
                        ret(vec![]),
                    ],
                    vec![],
                )],
            )],
        )]);
        let f = p.func(p.lookup_func("t.F").unwrap()).unwrap();
        assert!(
            !escapes(f, ValueId(2)),
            "plain channel-op uses must not escape"
        );
    }

    /// Storing the channel into a foreign (untracked) cell escapes it.
    #[test]
    fn store_to_foreign_addr_escapes() {
        let p = Program::from_packages(vec![pkg(
            "t",
            vec![func_with_aux(
                "t.F",
                vec![const_int_aux(1, 0)],
                vec![block(
                    0,
                    vec![
                        gvir_make_chan(2, 1),
                        alloc_instr(3),
                        store(3, 2),
                        ret(vec![]),
                    ],
                    vec![],
                )],
            )],
        )]);
        let f = p.func(p.lookup_func("t.F").unwrap()).unwrap();
        assert!(
            escapes(f, ValueId(2)),
            "storing into a foreign cell must escape"
        );
    }

    /// Storing the channel into its own tracked cell is bookkeeping, not
    /// an escape — and grows the `chans` alias set to the stored content.
    #[test]
    fn store_into_own_cell_does_not_escape_and_content_aliases() {
        let p = Program::from_packages(vec![pkg(
            "t",
            vec![func_with_aux(
                "t.F",
                vec![const_int_aux(1, 0)],
                vec![block(
                    0,
                    vec![
                        alloc_instr(2),
                        gvir_make_chan(3, 1),
                        store(2, 3),
                        ret(vec![]),
                    ],
                    vec![],
                )],
            )],
        )]);
        let f = p.func(p.lookup_func("t.F").unwrap()).unwrap();
        assert!(
            !escapes(f, ValueId(2)),
            "storing into its own tracked cell must not escape"
        );
    }

    /// The strict rule: a plain call's argument escapes even for a
    /// static callee (a summarized callee could stash the channel
    /// somewhere effects don't model).
    #[test]
    fn plain_call_arg_escapes() {
        let p = Program::from_packages(vec![pkg(
            "t",
            vec![func_with_aux(
                "t.F",
                vec![const_int_aux(1, 0)],
                vec![block(
                    0,
                    vec![
                        gvir_make_chan(2, 1),
                        call_static("t.G", 0, 0, vec![2]),
                        ret(vec![]),
                    ],
                    vec![],
                )],
            )],
        )]);
        let f = p.func(p.lookup_func("t.F").unwrap()).unwrap();
        assert!(escapes(f, ValueId(2)), "plain call args escape (strict)");
    }

    /// A static `go`/`defer` argument is whitelisted (unlike a plain
    /// call's).
    #[test]
    fn static_go_arg_does_not_escape() {
        let p = Program::from_packages(vec![pkg(
            "t",
            vec![func_with_aux(
                "t.F",
                vec![const_int_aux(1, 0)],
                vec![block(
                    0,
                    vec![
                        gvir_make_chan(2, 1),
                        go_call_args("t.G", vec![2]),
                        ret(vec![]),
                    ],
                    vec![],
                )],
            )],
        )]);
        let f = p.func(p.lookup_func("t.F").unwrap()).unwrap();
        assert!(
            !escapes(f, ValueId(2)),
            "a static go argument must not escape"
        );
    }

    /// A closure capturing the channel, then returned, escapes.
    #[test]
    fn closure_value_returned_escapes() {
        let p = Program::from_packages(vec![pkg(
            "t",
            vec![func_with_aux(
                "t.F",
                vec![fn_aux(1, "t.F$1")],
                vec![block(
                    0,
                    vec![
                        gvir_make_chan(2, 1),
                        make_closure(3, 1, vec![2]),
                        ret(vec![3]),
                    ],
                    vec![],
                )],
            )],
        )]);
        let f = p.func(p.lookup_func("t.F").unwrap()).unwrap();
        assert!(
            escapes(f, ValueId(2)),
            "a returned closure over the channel must escape"
        );
    }

    /// Boxing the channel into an interface escapes it.
    #[test]
    fn make_interface_escapes() {
        let p = Program::from_packages(vec![pkg(
            "t",
            vec![func_with_aux(
                "t.F",
                vec![const_int_aux(1, 0)],
                vec![block(
                    0,
                    vec![
                        gvir_make_chan(2, 1),
                        make_interface_instr(3, 2),
                        ret(vec![]),
                    ],
                    vec![],
                )],
            )],
        )]);
        let f = p.func(p.lookup_func("t.F").unwrap()).unwrap();
        assert!(escapes(f, ValueId(2)), "MakeInterface.src must escape");
    }
}
