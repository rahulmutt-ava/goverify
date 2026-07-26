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

use goverify_analysis::{
    ChanOp, Effects, Loc, MAX_LOC_DEPTH, Root, closure_bindings, cyclic_blocks, fv_loc, resolve_loc,
};
use goverify_ir::{
    Callee, ConstVal, FuncId, Function, MakeKind, Op, Pos, Program, ValueId, ValueKind,
};

use crate::shared::canonical_value;

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
/// just this much of its logic here is cheaper than exposing it). The
/// caller-side and callee-side paths are each individually capped at
/// `MAX_LOC_DEPTH` by their own `resolve_loc` call, but concatenating two
/// depth-2 paths can still produce a depth-3/4 result — degrade to
/// `Loc::unknown()` in that case too (matching `rebase_loc`'s own
/// `.capped()` call), since every `Loc` key an `Effects` map can ever
/// hold is itself capped: an uncapped deep `Loc` here would be a key that
/// can never match anything, silently dropping a real counterpart.
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
    if path.len() > MAX_LOC_DEPTH {
        return Loc::unknown();
    }
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
                    // Alias-extender ops grow *both* sets independently:
                    // an op whose source is a tracked channel extends
                    // `chans`; one whose source is a tracked closure
                    // extends `closures`. Growing only `chans` here would
                    // let a closure value get laundered through a bare
                    // `Assign`/`Convert`/`Phi`/`Load` (e.g. Go's
                    // `type H func(); return H(f)` lowers to
                    // `Op::Assign`) and escape undetected, since
                    // `instr_escapes` wholesale-whitelists these ops on
                    // the assumption that aliasing already tracked the
                    // def through them.
                    Op::Load { dst, addr } => {
                        if chans.contains(addr) && chans.insert(*dst) {
                            changed = true;
                        }
                        if closures.contains(addr) && closures.insert(*dst) {
                            changed = true;
                        }
                    }
                    Op::Assign { dst, src } | Op::Convert { dst, src } => {
                        if chans.contains(src) && chans.insert(*dst) {
                            changed = true;
                        }
                        if closures.contains(src) && closures.insert(*dst) {
                            changed = true;
                        }
                    }
                    Op::Phi { dst, edges } => {
                        if edges.iter().any(|e| chans.contains(e)) && chans.insert(*dst) {
                            changed = true;
                        }
                        if edges.iter().any(|e| closures.contains(e)) && closures.insert(*dst) {
                            changed = true;
                        }
                    }
                    Op::Store { addr, val } if chans.contains(addr) && chans.insert(*val) => {
                        changed = true;
                    }
                    // A binding may itself be a tracked closure (nested
                    // capture), not just a tracked channel directly —
                    // check both sets, not `chans` alone.
                    Op::MakeClosure { dst, bindings, .. }
                        if bindings
                            .iter()
                            .any(|b| chans.contains(b) || closures.contains(b))
                            && closures.insert(*dst) =>
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

/// The channel-op kinds able to unblock a candidate stuck at a blocked
/// `Send`/`Recv`: a blocked send needs someone to receive (or a select
/// that can serve as a receiver, or a close — a closed-channel send
/// panics rather than blocking forever, so it still counts as "not
/// blocked forever"); a blocked recv is the mirror image. `Select`
/// itself is dispatched per-arm by `has_counterpart`'s caller (Task 7),
/// never passed to this table.
fn unblockers(kind: CandKind) -> &'static [ChanOp] {
    match kind {
        CandKind::Send => &[ChanOp::Recv, ChanOp::Select, ChanOp::Close],
        CandKind::Recv => &[ChanOp::Send, ChanOp::Select, ChanOp::Close],
        CandKind::Select => &[],
    }
}

/// Does anything in the spawner's own converged effects (`env` —
/// `summary_of(f).effects`, covering every callee's and every spawned
/// goroutine's channel ops) ever unblock a candidate stuck at `loc`?
/// `Send`/`Recv` only: the caller dispatches a `Select` candidate's own
/// per-arm re-check (each arm behaves like a `Send` or `Recv` in its own
/// right) rather than asking this function to judge a whole select at
/// once — passed `CandKind::Select` directly, this defensively returns
/// `true` (suppress), never risking a false positive for a shape it
/// wasn't built to evaluate. Checked at `loc` AND `Loc::unknown()`
/// (spec's may-alias rule): an op this pass could only resolve to
/// "somewhere unknown" might still be the very counterpart this
/// candidate needs — folding it in can only erase a finding, never
/// invent one.
pub(crate) fn has_counterpart(env: &Effects, loc: &Loc, kind: CandKind) -> bool {
    if kind == CandKind::Select {
        return true;
    }
    let set = unblockers(kind);
    let unblocked_at = |l: &Loc| {
        env.chan_ops
            .get(l)
            .is_some_and(|ops| set.iter().any(|u| ops.contains(u)))
    };
    unblocked_at(loc) || unblocked_at(&Loc::unknown())
}

/// Blocking-op capacity classification (Task 6 refinement over the raw
/// syntactic candidate): distinguishes a genuinely reachability-only
/// block (`Unbuffered` — no buffer can ever save it) from a buffered
/// send a solver could still discharge via an ordinal fill-count
/// argument (`BufferedConst`, Task 7's job to actually use), from a
/// shape this syntactic pass can't safely reason about at all
/// (`Silent` — a non-const capacity, or a buffered send sitting on a
/// CFG cycle where "is the buffer ever full forever" isn't decidable
/// here). `Silent` never promotes to a finding on its own; it just means
/// this classifier has nothing further to add.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CapClass {
    /// Const capacity 0 (or a `Recv`, whose buffer never matters —
    /// reachability-only regardless of capacity).
    Unbuffered,
    /// Const capacity `N > 0`, and (for a `Send`) the op's own block
    /// isn't on a CFG cycle in its callee.
    BufferedConst(u64),
    /// Non-const capacity, an unresolvable def shape, or a buffered
    /// `Send` sitting on a CFG cycle.
    Silent,
}

/// `v`'s capacity in `f`: `v`'s def is `Op::Make{kind: Chan, args}` →
/// `args[0]`'s const value (direct form); `v`'s def is `Op::Alloc` (cell
/// form — a closure-captured channel's `alloc_value` is the capture
/// cell, per Task 4/5's semantic pointer) → exactly one `Store{addr ==
/// v}` in `f`, whose stored value resolves (through the same bridge,
/// recursively) to a `Make{Chan}` → that one's capacity. Anything else —
/// no def found, more than one store into the cell, a store whose value
/// never bottoms out at a `Make{Chan}` — is `None` (non-const/
/// unresolvable). Bounded to 64 hops through the cell bridge (mirrors
/// `canonical_value`'s own cap): crafted `.gvir` could fabricate an
/// `Alloc`/`Store` cycle that never resolves, and this must degrade
/// rather than loop forever.
fn cap_of(f: &Function, v: ValueId) -> Option<u64> {
    let mut cur = v;
    for _ in 0..64 {
        let def = f
            .blocks
            .iter()
            .flat_map(|b| &b.instrs)
            .find_map(|ins| match &ins.op {
                Op::Make {
                    dst,
                    kind: MakeKind::Chan,
                    args,
                } if *dst == cur => Some(Ok(args)),
                Op::Alloc { dst, .. } if *dst == cur => Some(Err(())),
                _ => None,
            });
        match def {
            Some(Ok(args)) => return const_cap(f, args.first().copied()),
            Some(Err(())) => {
                let mut stores =
                    f.blocks
                        .iter()
                        .flat_map(|b| &b.instrs)
                        .filter_map(|ins| match &ins.op {
                            Op::Store { addr, val } if *addr == cur => Some(*val),
                            _ => None,
                        });
                let first = stores.next()?;
                if stores.next().is_some() {
                    return None; // more than one store into the cell: unresolvable
                }
                let canon = canonical_value(f, first);
                if canon == cur {
                    return None; // degenerate self-store: avoid looping forever
                }
                cur = canon;
            }
            None => return None,
        }
    }
    None
}

/// `v`'s constant int value, walking same-function `Assign` chains
/// first (`crate::shared::canonical_value`) — `None` for a missing
/// operand, a non-const value, or a negative literal (a real channel
/// capacity is never negative; crafted negative `.gvir` degrades to
/// "non-const" here rather than becoming a nonsensical capacity via a
/// lossy cast).
fn const_cap(f: &Function, v: Option<ValueId>) -> Option<u64> {
    let canon = canonical_value(f, v?);
    match &f.value(canon).kind {
        ValueKind::Const(ConstVal::Int(n)) => u64::try_from(*n).ok(),
        _ => None,
    }
}

/// Capacity classification per Task 6's spec: a `Select` candidate is
/// `Unbuffered` iff EVERY arm's channel (each `arm_locs` entry's
/// `Root::Alloc`) has const capacity 0 — otherwise `Silent` (v1: no
/// ordinal reasoning across a select's multiple channels yet). A
/// `Send`/`Recv` candidate classifies `cand.alloc_value` itself: const 0
/// is always `Unbuffered`; a `Recv`'s non-zero const capacity is still
/// `Unbuffered`-equivalent (reachability-only: a recv with zero senders
/// anywhere blocks forever regardless of how large the buffer is); a
/// `Send`'s non-zero const capacity is `BufferedConst` only when its own
/// block (`cand.op_block`) doesn't sit on a CFG cycle in the callee
/// (`cyclic_blocks`) — a cyclic buffered send could refill the buffer
/// indefinitely, defeating the ordinal fill-count argument, so that
/// degrades to `Silent` too. A non-const capacity, or a callee id that
/// no longer resolves to a body, also degrades to `Silent`. Total and
/// panic-free: every id/index lookup above is bounds-checked by its own
/// callee (`Function::value`, `cyclic_blocks`'s `Vec<bool>`, `Program::func`).
pub(crate) fn cap_class(p: &Program, f: &Function, cand: &Candidate) -> CapClass {
    if cand.kind == CandKind::Select {
        let all_zero = !cand.arm_locs.is_empty()
            && cand.arm_locs.iter().all(|(_, loc)| match &loc.root {
                Root::Alloc(a) => cap_of(f, ValueId(*a)) == Some(0),
                _ => false,
            });
        return if all_zero {
            CapClass::Unbuffered
        } else {
            CapClass::Silent
        };
    }

    match cap_of(f, cand.alloc_value) {
        Some(0) => CapClass::Unbuffered,
        Some(_) if cand.kind == CandKind::Recv => CapClass::Unbuffered,
        Some(n) => {
            // Only `Send` remains: `Select` returned above, `Recv`
            // matched the prior arm regardless of its capacity value.
            let Some(callee_fn) = p.func(cand.callee) else {
                return CapClass::Silent;
            };
            let cyclic = cyclic_blocks(callee_fn);
            let is_cyclic = cyclic.get(cand.op_block).copied().unwrap_or(true);
            if is_cyclic {
                CapClass::Silent
            } else {
                CapClass::BufferedConst(n)
            }
        }
        None => CapClass::Silent,
    }
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
        alloc_instr, block, call_builtin, call_static, change_type_instr, const_int_aux,
        field_addr_on, fn_aux, func_with_aux, func_with_free_vars, func_with_params, go_call_args,
        go_call_dynamic, go_call_via_closure, gvir_make_chan, load_instr, make_closure,
        make_interface_instr, pkg, recv, ret, select, send, store,
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

    /// Review finding 1 (false-positive path): a closure captured over
    /// the channel, laundered through a bare `Op::Assign` (Go's
    /// `type H func(); return H(f)` lowers `ChangeType` to exactly this),
    /// then returned. The `closures` alias set must grow through
    /// alias-extender ops the same way `chans` does, or this escape goes
    /// undetected (the laundered value is untracked, so `Return`'s
    /// fallback check sees nothing).
    #[test]
    fn closure_alias_laundered_through_assign_and_returned_escapes() {
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
                        change_type_instr(4, 3),
                        ret(vec![4]),
                    ],
                    vec![],
                )],
            )],
        )]);
        let f = p.func(p.lookup_func("t.F").unwrap()).unwrap();
        assert!(
            escapes(f, ValueId(2)),
            "a closure alias laundered through Assign must still escape"
        );
    }

    /// Review finding 2 (false-positive path): a caller-side path already
    /// at depth 2 (two `FieldAddr` hops off an `Alloc`) composed with a
    /// callee-side path of depth 1 exceeds `MAX_LOC_DEPTH` — the combined
    /// `Loc` must degrade to `Unknown` (never surface an uncapped depth-3
    /// `Loc` that no `Effects` key could ever equal), so `candidates`
    /// finds nothing here.
    #[test]
    fn deep_composed_path_degrades_to_unknown_and_yields_no_candidate() {
        let p = Program::from_packages(vec![pkg(
            "t",
            vec![
                func_with_aux(
                    "t.F",
                    vec![const_int_aux(1, 0)],
                    vec![block(
                        0,
                        vec![
                            alloc_instr(2),
                            field_addr_on(3, 2),
                            field_addr_on(4, 3),
                            go_call_args("t.W", vec![4]),
                            ret(vec![]),
                        ],
                        vec![],
                    )],
                ),
                func_with_params(
                    "t.W",
                    vec![gvir::Param {
                        id: 1,
                        name: "s".into(),
                        r#type: 0,
                    }],
                    vec![block(
                        0,
                        vec![field_addr_on(2, 1), recv(3, 2), ret(vec![])],
                        vec![],
                    )],
                ),
            ],
        )]);
        let f = p.func(p.lookup_func("t.F").unwrap()).unwrap();
        assert!(
            candidates(&p, f).is_empty(),
            "a depth-3 composed path must degrade to Unknown, not yield a candidate"
        );
    }

    /// Review finding 3 (coverage gap): the real closure-capture shape,
    /// where `MakeClosure`'s binding is an `Alloc` CELL rather than the
    /// `MakeChan` register itself (`ch := make(chan int); go func(){
    /// <-ch }()` lowers to: alloc the cell, make the channel, store it
    /// into the cell, capture the cell). The callee reaches the channel
    /// through a `Load` of its `FreeVar` (the captured cell pointer);
    /// `resolve_loc` collapses that `Load` hop (deref-collapsing, per
    /// `resolve_loc`'s own doc comment), so the callee-side `Loc` stays
    /// `FreeVar(0)` with an empty path — the candidate must land on the
    /// CELL's `Alloc` site (register 2), not the `MakeChan` register
    /// (register 3). This is the shape Task 6's cell→Make bridge is
    /// built against.
    #[test]
    fn closure_capture_via_cell_yields_recv_candidate_at_cell_alloc() {
        let p = Program::from_packages(vec![pkg(
            "t",
            vec![
                func_with_aux(
                    "t.F",
                    vec![fn_aux(1, "t.F$1")],
                    vec![block(
                        0,
                        vec![
                            alloc_instr(2),
                            gvir_make_chan(3, 1),
                            store(2, 3),
                            make_closure(4, 1, vec![2]),
                            go_call_via_closure("t.F$1", 4),
                            ret(vec![]),
                        ],
                        vec![],
                    )],
                ),
                func_with_free_vars(
                    "t.F$1",
                    vec![1],
                    vec![block(
                        0,
                        vec![load_instr(2, 1), recv(3, 2), ret(vec![])],
                        vec![],
                    )],
                ),
            ],
        )]);
        let f = p.func(p.lookup_func("t.F").unwrap()).unwrap();
        let cands = candidates(&p, f);
        assert_eq!(cands.len(), 1, "exactly one candidate: {cands:?}");
        assert_eq!(cands[0].kind, CandKind::Recv);
        assert_eq!(
            cands[0].alloc_value,
            ValueId(2),
            "must land on the CELL's alloc site, not the MakeChan register"
        );
        assert_eq!(
            cands[0].spawner_loc,
            Loc {
                root: Root::Alloc(2),
                path: vec![]
            }
        );
    }

    // -- has_counterpart -----------------------------------------------

    fn env_with(loc: Loc, ops: &[ChanOp]) -> Effects {
        let mut e = Effects::empty();
        e.chan_ops
            .entry(loc)
            .or_default()
            .extend(ops.iter().copied());
        e
    }

    #[test]
    fn send_candidate_suppressed_by_recv_at_same_loc() {
        let l = Loc {
            root: Root::Alloc(2),
            path: vec![],
        };
        let env = env_with(l.clone(), &[ChanOp::Send, ChanOp::Recv]);
        assert!(has_counterpart(&env, &l, CandKind::Send));
    }

    #[test]
    fn send_candidate_not_suppressed_by_sibling_send() {
        let l = Loc {
            root: Root::Alloc(2),
            path: vec![],
        };
        let env = env_with(l.clone(), &[ChanOp::Send, ChanOp::Make]);
        assert!(
            !has_counterpart(&env, &l, CandKind::Send),
            "Send/Make are not unblockers for a blocked send"
        );
    }

    #[test]
    fn send_candidate_suppressed_by_any_unknown_loc_unblocker() {
        let l = Loc {
            root: Root::Alloc(2),
            path: vec![],
        };
        let env = env_with(Loc::unknown(), &[ChanOp::Recv]);
        assert!(
            has_counterpart(&env, &l, CandKind::Send),
            "an unresolvable-elsewhere Recv must still count (may-alias rule)"
        );
    }

    #[test]
    fn recv_candidate_suppressed_by_close() {
        let l = Loc {
            root: Root::Alloc(2),
            path: vec![],
        };
        let env = env_with(l.clone(), &[ChanOp::Close]);
        assert!(has_counterpart(&env, &l, CandKind::Recv));
    }

    #[test]
    fn recv_candidate_unsuppressed_by_recv() {
        let l = Loc {
            root: Root::Alloc(2),
            path: vec![],
        };
        let env = env_with(l.clone(), &[ChanOp::Recv]);
        assert!(
            !has_counterpart(&env, &l, CandKind::Recv),
            "a sibling Recv can never unblock another blocked Recv"
        );
    }

    /// The defensive default: `has_counterpart` isn't meant to judge a
    /// whole `Select` candidate on its own (Task 7 dispatches per arm
    /// instead), so passed `CandKind::Select` directly it always
    /// suppresses, even against an empty env.
    #[test]
    fn has_counterpart_select_kind_defaults_to_true() {
        let l = Loc {
            root: Root::Alloc(2),
            path: vec![],
        };
        assert!(has_counterpart(&Effects::empty(), &l, CandKind::Select));
    }

    /// Simulates Task 7's per-arm select dispatch (dir 1 = send arm →
    /// `CandKind::Send`, dir 2 = recv arm → `CandKind::Recv`): "any arm
    /// pairs" is true when at least one arm's own direction-appropriate
    /// counterpart exists in the env, false for an empty env.
    #[test]
    fn select_arm_dispatch_suppressed_if_any_arm_matched() {
        let la = Loc {
            root: Root::Alloc(2),
            path: vec![],
        };
        let lb = Loc {
            root: Root::Alloc(3),
            path: vec![],
        };
        let arm_locs: Vec<(u32, Loc)> = vec![(1, la.clone()), (2, lb.clone())];
        let dispatch = |env: &Effects| {
            arm_locs.iter().any(|(dir, loc)| {
                let kind = if *dir == 1 {
                    CandKind::Send
                } else {
                    CandKind::Recv
                };
                has_counterpart(env, loc, kind)
            })
        };
        let env = env_with(la.clone(), &[ChanOp::Recv]);
        assert!(dispatch(&env), "the send arm can pair via Recv on la");
        assert!(
            !dispatch(&Effects::empty()),
            "an empty env leaves no arm able to pair"
        );
    }

    // -- cap_class -------------------------------------------------------

    /// F: const_int_aux(1, 0); make_chan(2, 1); go t.G(2). G sends on its
    /// param. Cap 0 → Unbuffered.
    #[test]
    fn cap_class_unbuffered_from_const_zero() {
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
                    vec![block(0, vec![send(1, 1), ret(vec![])], vec![])],
                ),
            ],
        )]);
        let f = p.func(p.lookup_func("t.F").unwrap()).unwrap();
        let cands = candidates(&p, f);
        assert_eq!(cands.len(), 1, "exactly one candidate: {cands:?}");
        assert_eq!(cap_class(&p, f, &cands[0]), CapClass::Unbuffered);
    }

    /// Same shape, cap 3, callee's send sits in a straight-line (acyclic)
    /// block → BufferedConst(3).
    #[test]
    fn cap_class_buffered_const_acyclic_send() {
        let p = Program::from_packages(vec![pkg(
            "t",
            vec![
                func_with_aux(
                    "t.F",
                    vec![const_int_aux(1, 3)],
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
        assert_eq!(cap_class(&p, f, &cands[0]), CapClass::BufferedConst(3));
    }

    /// The cap operand is F's own param (not a const aux) → non-const →
    /// Silent.
    #[test]
    fn cap_class_silent_for_nonconst_cap() {
        let p = Program::from_packages(vec![pkg(
            "t",
            vec![
                func_with_params(
                    "t.F",
                    vec![gvir::Param {
                        id: 1,
                        name: "n".into(),
                        r#type: 0,
                    }],
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
        assert_eq!(cap_class(&p, f, &cands[0]), CapClass::Silent);
    }

    /// Cap 3, but the callee's send block loops to itself (`succs`
    /// includes its own index) → Silent (a cyclic buffered send can
    /// refill forever).
    #[test]
    fn cap_class_silent_for_cyclic_buffered_send() {
        let p = Program::from_packages(vec![pkg(
            "t",
            vec![
                func_with_aux(
                    "t.F",
                    vec![const_int_aux(1, 3)],
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
                    vec![block(0, vec![send(1, 1), ret(vec![])], vec![0])],
                ),
            ],
        )]);
        let f = p.func(p.lookup_func("t.F").unwrap()).unwrap();
        let cands = candidates(&p, f);
        assert_eq!(cands.len(), 1, "exactly one candidate: {cands:?}");
        assert_eq!(cap_class(&p, f, &cands[0]), CapClass::Silent);
    }

    /// Cell form: Alloc cell reg 4; make_chan(2, 1) cap const 3;
    /// store(4, 2). The candidate's alloc_value is the cell (4), not the
    /// MakeChan register (2) — the cell→Make bridge must resolve through
    /// the single store to find the cap.
    #[test]
    fn cap_class_cell_form_single_store_resolves() {
        let p = Program::from_packages(vec![pkg(
            "t",
            vec![
                func_with_aux(
                    "t.F",
                    vec![const_int_aux(1, 3)],
                    vec![block(
                        0,
                        vec![
                            alloc_instr(4),
                            gvir_make_chan(2, 1),
                            store(4, 2),
                            ret(vec![]),
                        ],
                        vec![],
                    )],
                ),
                func_with_params("t.G", vec![], vec![block(0, vec![ret(vec![])], vec![])]),
            ],
        )]);
        let f = p.func(p.lookup_func("t.F").unwrap()).unwrap();
        let g = p.lookup_func("t.G").unwrap();
        let cand = Candidate {
            go_block: 0,
            go_pos: None,
            callee: g,
            op_block: 0,
            kind: CandKind::Send,
            alloc_value: ValueId(4),
            spawner_loc: Loc {
                root: Root::Alloc(4),
                path: vec![],
            },
            arm_locs: vec![],
        };
        assert_eq!(cap_class(&p, f, &cand), CapClass::BufferedConst(3));
    }

    /// Same as above, plus a second `store(4, 5)` into the same cell:
    /// the exactly-one-store bridge no longer holds → Silent.
    #[test]
    fn cap_class_cell_form_two_stores_is_silent() {
        let p = Program::from_packages(vec![pkg(
            "t",
            vec![
                func_with_aux(
                    "t.F",
                    vec![const_int_aux(1, 3)],
                    vec![block(
                        0,
                        vec![
                            alloc_instr(4),
                            gvir_make_chan(2, 1),
                            store(4, 2),
                            store(4, 5),
                            ret(vec![]),
                        ],
                        vec![],
                    )],
                ),
                func_with_params("t.G", vec![], vec![block(0, vec![ret(vec![])], vec![])]),
            ],
        )]);
        let f = p.func(p.lookup_func("t.F").unwrap()).unwrap();
        let g = p.lookup_func("t.G").unwrap();
        let cand = Candidate {
            go_block: 0,
            go_pos: None,
            callee: g,
            op_block: 0,
            kind: CandKind::Send,
            alloc_value: ValueId(4),
            spawner_loc: Loc {
                root: Root::Alloc(4),
                path: vec![],
            },
            arm_locs: vec![],
        };
        assert_eq!(cap_class(&p, f, &cand), CapClass::Silent);
    }

    /// Cap 3, but the candidate is a Recv: buffering never matters for a
    /// recv — reachability only, so Unbuffered-equivalent.
    #[test]
    fn buffered_recv_is_reachability_only() {
        let p = Program::from_packages(vec![pkg(
            "t",
            vec![
                func_with_aux(
                    "t.F",
                    vec![const_int_aux(1, 3)],
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
                    vec![block(0, vec![recv(2, 1), ret(vec![])], vec![])],
                ),
            ],
        )]);
        let f = p.func(p.lookup_func("t.F").unwrap()).unwrap();
        let cands = candidates(&p, f);
        assert_eq!(cands.len(), 1, "exactly one candidate: {cands:?}");
        assert_eq!(cap_class(&p, f, &cands[0]), CapClass::Unbuffered);
    }
}
