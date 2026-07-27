//! Goroutine-leak checker (phase 7, spec §2): a spawned goroutine that
//! blocks on a channel operation whose channel is spawner-owned (rooted
//! at an `Op::Make`/`Op::Alloc` site back in the spawning function `f`,
//! per `goverify_analysis::resolve_loc`) and never escapes `f`'s control
//! is a leak candidate — nothing outside `f` can ever unblock it once
//! `f` stops referencing the channel. This module owns the two pure,
//! syntactic passes: `candidates` (v1: a syntactically-direct scan of a
//! spawned callee's own body for blocking ops) and `escapes` (a
//! whitelist-based alias/use walk, spec §2 rule 2's strict form), plus
//! counterpart/capacity reasoning (does anything else ever unblock this
//! op?) and the `Checker` impl that turns a surviving candidate into a
//! conjoined SMT query: the spawn site is reachable in `f` AND the
//! blocking op is reachable in the spawned callee (whose consts are
//! prefix-renamed so the two encodings share no names), AND — for a
//! buffered send — the buffer is already full when it fires.

use std::collections::{HashMap, HashSet, VecDeque};

use goverify_analysis::{
    ChanOp, Checker, Clause, Effects, EncodedFunc, Loc, MAX_LOC_DEPTH, Obligation, Root, Summary,
    closure_bindings, cyclic_blocks, encode_func_with, fv_loc, resolve_loc,
};
use goverify_ir::{
    Callee, ConstVal, FuncId, Function, MakeKind, Op, Pos, Program, ValueId, ValueKind,
};
use goverify_solver::{BvBinOp, BvCmpOp, Query, SatResult, Term};

use crate::shared::{canonical_value, own_preconditions};

/// The blocking-op shape a [`Candidate`] was found at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CandKind {
    Send,
    Recv,
    Select,
}

/// How a hop candidate's helper is reached from the spawned callee's
/// body. Documentation/message value only — the obligation anchors on
/// `call_block` reachability either way (a Defer hop's over-approximation
/// is a documented spec §5.2 boundary).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HopKind {
    Call,
    Defer,
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
    /// Block index of the blocking op in the op's host frame (the spawned
    /// callee, or `hop.helper` for a hop candidate).
    pub op_block: usize,
    /// Instruction index of the blocking op within `op_block` — the
    /// buffered-send ordinal conjunct counts the sends that precede the
    /// candidate inside its own block, so it needs the offset, not just
    /// the block.
    pub op_instr: usize,
    pub kind: CandKind,
    /// The blocking op's channel resolved in the op's host frame (the
    /// spawned callee, or `hop.helper` for a hop candidate), before
    /// `map_through_site` re-roots it into `f` — the key the ordinal
    /// conjunct counts host-side sends against. `Loc::unknown()` for a
    /// `Select` candidate (whose arms each have their own, and which
    /// never gets an ordinal conjunct: `cap_class` classifies a select as
    /// `Unbuffered` or `Silent`, never `BufferedConst`).
    pub callee_loc: Loc,
    /// `Root::Alloc` id in `f` — the escape/capacity subject.
    pub alloc_value: ValueId,
    /// Alloc-rooted `Loc` in `f` — the counterpart key.
    pub spawner_loc: Loc,
    /// Select candidates only: every arm's spawner-side `Loc`, paired
    /// with the arm's dir (1 = send, 2 = recv). Empty for Send/Recv
    /// candidates.
    pub arm_locs: Vec<(u32, Loc)>,
    /// `None` for a direct candidate (op in the spawned callee's own
    /// body); `Some` when the op sits one static Call/Defer below it,
    /// in `hop.helper`'s body — `op_block`/`op_instr`/`callee_loc` then
    /// index/resolve in the HELPER's frame, and the obligation becomes
    /// a three-frame conjunction (2026-07-27 wave).
    pub hop: Option<Hop>,
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
    hop: Option<Hop>,
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
        hop,
    })
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
/// The per-op scan now lives in `scan_body`, parameterized over the
/// loc-mapping function.
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
                out.extend(candidate_from(bi, ins.pos.clone(), c, hit, None));
            }
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
                        Op::Defer {
                            callee: Callee::Static(h),
                            args,
                        } => (HopKind::Defer, *h, args),
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

/// A blocking `Select` candidate's PER-ARM counterpart check (`dir` 1 =
/// send arm, anything else = recv arm). Deliberately NOT
/// `has_counterpart(env, loc, Send/Recv)`: that table lists `ChanOp::Select`
/// as an unblocker, and the candidate's OWN select is folded into `env`
/// (`summary_of(f).effects` covers every spawned goroutine's ops, and the
/// spawned closure's select rebases through its bindings to precisely
/// these arm locs). Asking the generic table therefore made every
/// blocking-select candidate find *itself* at every arm and
/// self-suppress — `chan-select-leak` was structurally unfireable for any
/// input (found by the phase-7 corpus fixture `LeakSelectAllBlocked`).
///
/// So at the ARM's own loc the unblocker set drops `Select`: a send arm
/// needs a `Recv`/`Close`, a recv arm a `Send`/`Close`. At
/// `Loc::unknown()` `Select` is KEPT as a suppressor — the candidate's own
/// select always rebases to the known `Root::Alloc` arm locs, never to
/// unknown, so unknown-loc select evidence is necessarily foreign, and the
/// may-alias rule still applies to it.
///
/// The cost, stated plainly: same-loc `Select` evidence is ambiguous
/// between this candidate's own select and a genuine second select that
/// could pair with it, and this resolves the ambiguity toward reporting,
/// because the alternative makes the tag vacuous. A real
/// select-pairing-with-another-select shape can therefore false-positive
/// in v1; accepted, and gated by shakeout G2.
fn arm_has_counterpart(env: &Effects, loc: &Loc, dir: u32) -> bool {
    let (at_arm, at_unknown): (&[ChanOp], &[ChanOp]) = if dir == 1 {
        (
            &[ChanOp::Recv, ChanOp::Close],
            &[ChanOp::Recv, ChanOp::Select, ChanOp::Close],
        )
    } else {
        (
            &[ChanOp::Send, ChanOp::Close],
            &[ChanOp::Send, ChanOp::Select, ChanOp::Close],
        )
    };
    let any_of = |l: &Loc, set: &[ChanOp]| {
        env.chan_ops
            .get(l)
            .is_some_and(|ops| set.iter().any(|u| ops.contains(u)))
    };
    any_of(loc, at_arm) || any_of(&Loc::unknown(), at_unknown)
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

/// Renames every const in `q` with `prefix`, substituting in all asserts.
/// Returns `None` if substitution fails (a sort mismatch can't happen for
/// a pure rename, but degrade rather than unwrap on adversarial input).
/// The point: the spawner's and the spawned callee's encodings are two
/// independent frames that both name their blocks `g0`, `g1`, … and their
/// params `p0`, `p1`, … — conjoining them unrenamed would silently
/// identify the two functions' guards and values.
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
        consts: q
            .consts
            .iter()
            .map(|(n, s)| (format!("{prefix}{n}"), s.clone()))
            .collect(),
        asserts,
    })
}

/// f-side query ∧ prefix-renamed callee-side query. Both encodings share
/// the same two datatype decls (Ptr, GoSeq — `encode_func_with` installs
/// them unconditionally), so `fq`'s set is kept as-is (`cq`'s would be a
/// duplicate declaration, which the printer would emit twice). The
/// `debug_assert` is a tripwire on exactly that assumption: if the encoder
/// ever declares datatypes conditionally, dropping `cq`'s set would
/// silently produce a query referencing an undeclared sort, so fail loudly
/// in debug builds rather than hand the solver a malformed script.
/// Consts are sorted and deduped for canonical determinism.
fn conjoin(mut fq: Query, cq: Query) -> Query {
    debug_assert_eq!(
        fq.datatypes, cq.datatypes,
        "conjoin assumes identical datatype decls on both sides"
    );
    fq.consts.extend(cq.consts);
    fq.consts.sort();
    fq.consts.dedup();
    fq.asserts.extend(cq.asserts);
    fq
}

/// Every block that can precede `block` on a DAG path — reverse
/// reachability over `dag_succs` (back edges already cut by the encoder),
/// STRICT: `block` itself is never included. Ascending order, so the
/// ordinal sum below is built in a deterministic sequence. Total: an
/// out-of-range successor id (crafted `.gvir`) is ignored, and `block >=
/// dag_succs.len()` yields an empty ancestor set.
fn strict_ancestors(dag_succs: &[Vec<u32>], block: usize) -> Vec<usize> {
    let n = dag_succs.len();
    let mut preds: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (b, succs) in dag_succs.iter().enumerate() {
        for &s in succs {
            if (s as usize) < n {
                preds[s as usize].push(b);
            }
        }
    }
    let mut seen = vec![false; n];
    let mut queue: VecDeque<usize> = VecDeque::new();
    if block < n {
        seen[block] = true;
        queue.push_back(block);
    }
    while let Some(b) = queue.pop_front() {
        for &pb in &preds[b] {
            if !seen[pb] {
                seen[pb] = true;
                queue.push_back(pb);
            }
        }
    }
    (0..n).filter(|&b| seen[b] && b != block).collect()
}

/// How many `Op::Send` instrs in callee block `b` send on `loc` (resolved
/// in the callee's own frame). `upto` bounds the scan to instrs strictly
/// before that index — used for the candidate's own block, where only the
/// sends that precede it have already filled the buffer; `None` counts the
/// whole block.
fn sends_on_loc_in(callee_f: &Function, b: usize, loc: &Loc, upto: Option<usize>) -> u64 {
    let Some(blk) = callee_f.blocks.get(b) else {
        return 0;
    };
    blk.instrs
        .iter()
        .enumerate()
        .filter(|(i, _)| upto.is_none_or(|u| *i < u))
        .filter(|(_, ins)| {
            matches!(&ins.op, Op::Send { chan, .. } if resolve_loc(callee_f, *chan) == *loc)
        })
        .count() as u64
}

/// The buffered-send ordinal conjunct (spec §2): a send on a channel with
/// const capacity `cap` blocks forever only if the buffer is ALREADY full
/// when it fires and nothing ever drains it (the counterpart check has
/// already established there is no receive/close/select anywhere). The
/// fill count at the candidate is
///
/// ```text
/// pending = <sends on this loc before the candidate in its own block>
///         + Σ over strict ancestors b: ite(g_b, <sends on this loc in b>, 0)
/// ```
///
/// and the candidate is blocked iff `pending >= cap`. `BvCmpOp` has no
/// unsigned-≥ variant, so that is encoded as `¬(pending <u cap)`. All
/// arithmetic is 32-bit: a capacity or a send count that doesn't fit
/// (crafted `.gvir`), a sum that would wrap the counter, a missing guard
/// term, or any term-construction error yields `None` — the caller skips
/// the candidate rather than emitting a query built on a wrapped count.
fn ordinal_conjunct(
    callee_f: &Function,
    enc_c: &EncodedFunc,
    cand: &Candidate,
    cap: u64,
) -> Option<Term> {
    let cap = u32::try_from(cap).ok()?;
    let mut total = sends_on_loc_in(
        callee_f,
        cand.op_block,
        &cand.callee_loc,
        Some(cand.op_instr),
    );
    let mut pending = Term::bv_lit(32, u128::from(u32::try_from(total).ok()?));
    for b in strict_ancestors(&enc_c.dag_succs, cand.op_block) {
        let n = sends_on_loc_in(callee_f, b, &cand.callee_loc, None);
        if n == 0 {
            continue;
        }
        total = total.checked_add(n)?;
        let n = u32::try_from(n).ok()?;
        u32::try_from(total).ok()?; // the running sum must not wrap
        let inc = Term::ite(
            enc_c.guards.get(b)?.clone(),
            Term::bv_lit(32, u128::from(n)),
            Term::bv_lit(32, 0),
        )
        .ok()?;
        pending = Term::bv_bin(BvBinOp::Add, pending, inc).ok()?;
    }
    let below = Term::bv_cmp(BvCmpOp::Ult, pending, Term::bv_lit(32, u128::from(cap))).ok()?;
    Term::not(below).ok()
}

/// The obligation tag/message pair for a candidate's shape. Position-free
/// by construction (`Obligation::message`'s invariant): the callee's name
/// is deterministic and independent of where it sits in the file.
fn tag_and_message(p: &Program, cand: &Candidate) -> (&'static str, String) {
    let callee = p.func_name(cand.callee);
    match cand.kind {
        CandKind::Send => (
            "chan-send-leak",
            format!(
                "goroutine {callee} may block forever: send on a spawner-created channel with no receive, close, or select in the spawning environment"
            ),
        ),
        CandKind::Recv => (
            "chan-recv-leak",
            format!(
                "goroutine {callee} may block forever: receive on a spawner-created channel with no send, close, or select"
            ),
        ),
        CandKind::Select => (
            "chan-select-leak",
            format!(
                "goroutine {callee} may block forever: blocking select whose channels have no counterpart operations"
            ),
        ),
    }
}

/// The goroutine-leak checker (phase-7 spec §2). Obligations only: a leak
/// is a property of a spawn site, not a contract, so nothing lifts to a
/// requires/ensures clause.
pub struct LeakChecker;

impl Checker for LeakChecker {
    fn name(&self) -> &'static str {
        "goroutine-leak"
    }

    // bump on any semantic change to this checker's obligations.
    fn version(&self) -> u32 {
        1
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
        let Some(func) = p.func(f) else {
            return Vec::new();
        };
        // Encode `f` once, before the candidate loop: an encoding failure
        // (e.g. past the assertion cap) means no obligations at all — the
        // engine's own probe already emitted the diagnostic.
        let Ok(enc_f) = encode_func_with(p, f, summary_of) else {
            return Vec::new();
        };
        let own = summary_of(f);
        let env = &own.effects;
        let pre_f = own_preconditions(&own);
        // Several `go` sites often share a callee; encode each at most
        // once. Lookup-only (never iterated), so no map order can reach
        // the output.
        let mut enc_cache: HashMap<FuncId, Option<EncodedFunc>> = HashMap::new();
        // One obligation per blocking op, siblings included: two blocking
        // ops on the same channel in one goroutine (`go func() { ch <- 1;
        // ch <- 2 }()`) are two candidates that can produce two findings
        // with identical checker/func/tag/message/pos. That is a SUPPORTED
        // case downstream, not a fingerprint collision — the fingerprint
        // scheme assigns each finding an ordinal among identical siblings
        // in position order (goverify-cli fingerprint.rs), so siblings key
        // distinctly and baselines stay stable. Deduping here cannot be
        // done correctly either: the only sound key would have to include
        // the query (two candidates in different callee blocks differ only
        // by which guard is asserted), which makes the dedup fire on
        // incidental block structure, while any tighter key drops a
        // Sat candidate in favour of an Unsat sibling and loses the
        // finding outright. Report-level collapsing, if ever wanted,
        // belongs post-discharge at the findings layer.
        let mut out = Vec::new();
        for cand in candidates(p, func) {
            if cand.hop.is_some() {
                // Transitional (this wave's obligations task replaces it
                // with the three-frame query): a hop candidate's
                // op_block indexes the HELPER's frame, and the two-frame
                // query below would anchor it against g's encoding.
                continue;
            }
            // Select: every arm's channel must pass the escape check, and
            // no arm may have a counterpart — per arm, via
            // `arm_has_counterpart` (dir 1 = send arm, 2 = recv arm),
            // whose same-loc set excludes `Select` so the candidate's own
            // select can't suppress it. Send/Recv use the candidate's
            // single loc and the generic table (an op is never in its own
            // unblocker set there, so no self-match is possible).
            let (escaped, suppressed) = match cand.kind {
                CandKind::Select => (
                    cand.arm_locs.iter().any(|(_, l)| match &l.root {
                        Root::Alloc(a) => escapes(func, ValueId(*a)),
                        _ => true,
                    }),
                    cand.arm_locs
                        .iter()
                        .any(|(dir, l)| arm_has_counterpart(env, l, *dir)),
                ),
                k => (
                    escapes(func, cand.alloc_value),
                    has_counterpart(env, &cand.spawner_loc, k),
                ),
            };
            if escaped || suppressed {
                continue;
            }
            let class = cap_class(p, func, &cand);
            if class == CapClass::Silent {
                continue;
            }
            let Some(callee_f) = p.func(cand.callee) else {
                continue;
            };
            let enc_c = enc_cache
                .entry(cand.callee)
                .or_insert_with(|| encode_func_with(p, cand.callee, summary_of).ok());
            let Some(enc_c) = enc_c.as_ref() else {
                continue; // callee encoding failed: skip, never report
            };
            let mut extra = own_preconditions(&summary_of(cand.callee));
            if let CapClass::BufferedConst(cap) = class {
                let Some(full) = ordinal_conjunct(callee_f, enc_c, &cand, cap) else {
                    continue;
                };
                extra.push(full);
            }
            let Some(cq) = prefixed(&enc_c.reach_query(cand.op_block, extra), "s_") else {
                continue;
            };
            let (tag, message) = tag_and_message(p, &cand);
            out.push(Obligation {
                tag: tag.into(),
                message,
                pos: cand.go_pos.clone(),
                query: conjoin(enc_f.reach_query(cand.go_block, pre_f.clone()), cq),
            });
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use goverify_analysis::{EngineConfig, Finding, Severity, analyze_full};
    use goverify_extract::gvir;
    use goverify_ir::Program;
    use goverify_solver::{Logic, SolverLimits, Sort, Z3Native};

    use super::*;
    #[allow(unused_imports)] // wired for Task 4 (deferred-closure hop test)
    use crate::testfix::defer_call_via_closure;
    use crate::testfix::{
        alloc_instr, block, branch_on, call_builtin, call_static, change_type_instr, const_int_aux,
        defer_call_args, field_addr_on, fn_aux, func_with_aux, func_with_free_vars,
        func_with_params, go_call_args, go_call_dynamic, go_call_via_closure, gvir_make_chan,
        instr, load_instr, make_closure, make_interface_instr, pkg, recv, ret, select, send, store,
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
        assert_eq!(cands[0].hop.as_ref().map(|h| h.kind), Some(HopKind::Defer));
        assert_eq!(cands[0].alloc_value, ValueId(2));
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
                        vec![call_static("t.G", 2, 0, vec![1]), send(1, 1), ret(vec![])],
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

    /// The per-arm select dispatch (`arm_has_counterpart`, dir 1 = send
    /// arm, 2 = recv arm): "any arm pairs" is true when at least one arm's
    /// own direction-appropriate counterpart exists in the env, false for
    /// an empty env. Updated in the select-self-suppression fix — the
    /// dispatch used to call `has_counterpart(.., Send/Recv)`, whose
    /// unblocker sets include `Select`.
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
            arm_locs
                .iter()
                .any(|(dir, loc)| arm_has_counterpart(env, loc, *dir))
        };
        let env = env_with(la.clone(), &[ChanOp::Recv]);
        assert!(dispatch(&env), "the send arm can pair via Recv on la");
        assert!(
            !dispatch(&Effects::empty()),
            "an empty env leaves no arm able to pair"
        );
    }

    /// The self-suppression regression: the candidate's OWN select is in
    /// `env` at every arm loc (the spawned closure's ops rebase there), so
    /// a same-loc `Select` must NOT count as an unblocker — otherwise
    /// `chan-select-leak` is unfireable for every input.
    #[test]
    fn select_arm_not_suppressed_by_its_own_select_at_arm_locs() {
        let la = Loc {
            root: Root::Alloc(2),
            path: vec![],
        };
        let lb = Loc {
            root: Root::Alloc(3),
            path: vec![],
        };
        let mut env = env_with(la.clone(), &[ChanOp::Select, ChanOp::Make]);
        env.chan_ops
            .entry(lb.clone())
            .or_default()
            .extend([ChanOp::Select, ChanOp::Make]);
        assert!(
            !arm_has_counterpart(&env, &la, 1),
            "a send arm must not be unblocked by the candidate's own select"
        );
        assert!(
            !arm_has_counterpart(&env, &lb, 2),
            "a recv arm must not be unblocked by the candidate's own select"
        );
    }

    /// A `Select` at `Loc::unknown()` is necessarily FOREIGN (the
    /// candidate's own select rebases to its known `Alloc` arm locs), so
    /// the may-alias rule still lets it suppress.
    #[test]
    fn select_arm_suppressed_by_unknown_loc_select() {
        let la = Loc {
            root: Root::Alloc(2),
            path: vec![],
        };
        let env = env_with(Loc::unknown(), &[ChanOp::Select]);
        assert!(
            arm_has_counterpart(&env, &la, 1),
            "an unresolvable-elsewhere Select must still count (may-alias rule)"
        );
        assert!(arm_has_counterpart(&env, &la, 2), "same for a recv arm");
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
            op_instr: 0,
            kind: CandKind::Send,
            callee_loc: Loc::unknown(),
            alloc_value: ValueId(4),
            spawner_loc: Loc {
                root: Root::Alloc(4),
                path: vec![],
            },
            arm_locs: vec![],
            hop: None,
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
            op_instr: 0,
            kind: CandKind::Send,
            callee_loc: Loc::unknown(),
            alloc_value: ValueId(4),
            spawner_loc: Loc {
                root: Root::Alloc(4),
                path: vec![],
            },
            arm_locs: vec![],
            hop: None,
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

    // -- query construction ----------------------------------------------

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

    // -- end-to-end (engine + Z3) -----------------------------------------

    /// The full engine path: the leak checker is the only registered
    /// checker, backed by a real Z3, so a finding here means the conjoined
    /// spawn/block query actually came back Sat.
    fn run_leak(p: &Program) -> Vec<Finding> {
        let cfg = EngineConfig::default();
        let checkers: Vec<&dyn Checker> = vec![&LeakChecker];
        analyze_full(p, &cfg, &checkers, &|_role| {
            Box::new(Z3Native::new(SolverLimits {
                timeout_ms: 5_000,
                mem_mb: 1024,
            }))
        })
        .findings
    }

    /// F: v2 = make(chan, 0); go t.G(v2); return. G(p1): p1 <- p1.
    /// Nothing in F ever receives, closes or selects on v2, and v2 never
    /// escapes F — the spawned send blocks forever.
    fn spawn_and_send_pkg(f_blocks: Vec<gvir::BasicBlock>, cap: i64) -> Program {
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
                    vec![block(0, vec![send(1, 1), ret(vec![])], vec![])],
                ),
            ],
        )])
    }

    #[test]
    fn leak_end_to_end_reports_via_z3() {
        let p = spawn_and_send_pkg(
            vec![block(
                0,
                vec![
                    gvir_make_chan(2, 1),
                    go_call_args("t.G", vec![2]),
                    ret(vec![]),
                ],
                vec![],
            )],
            0,
        );
        let findings = run_leak(&p);
        assert_eq!(findings.len(), 1, "exactly one finding: {findings:?}");
        assert_eq!(findings[0].checker, "goroutine-leak");
        assert_eq!(findings[0].tag, "chan-send-leak");
        assert_eq!(findings[0].severity, Severity::Error);
    }

    #[test]
    fn leak_suppressed_when_recv_follows_spawn() {
        let p = spawn_and_send_pkg(
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
            "a receive in the spawning environment unblocks the send: {findings:?}"
        );
    }

    /// The buffered-capacity ordinal conjunct: cap 3, four sends on the
    /// channel in one acyclic callee block. The first three fit in the
    /// buffer (pending < cap ⇒ the conjunct is false ⇒ Unsat ⇒ silent);
    /// only the fourth is blocked, so exactly one finding survives.
    #[test]
    fn buffered_send_reports_only_past_capacity() {
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
                    vec![block(
                        0,
                        vec![send(1, 1), send(1, 1), send(1, 1), send(1, 1), ret(vec![])],
                        vec![],
                    )],
                ),
            ],
        )]);
        let findings = run_leak(&p);
        assert_eq!(
            findings.len(),
            1,
            "only the send past capacity blocks: {findings:?}"
        );
        assert_eq!(findings[0].tag, "chan-send-leak");
    }

    /// One finding per blocking op, siblings included: two unbuffered
    /// sends on the same channel in one goroutine are two leak sites, and
    /// the checker reports both even though they carry identical
    /// tag/message/`go`-site pos. Deliberate — the fingerprint scheme
    /// ordinals identical siblings (goverify-cli fingerprint.rs), so this
    /// costs no baseline stability, and no dedup available at this layer
    /// is both sound and complete (see `obligations`' comment).
    #[test]
    fn identical_blocking_ops_each_report() {
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
                    vec![block(0, vec![send(1, 1), send(1, 1), ret(vec![])], vec![])],
                ),
            ],
        )]);
        let findings = run_leak(&p);
        assert_eq!(
            findings.len(),
            2,
            "each blocking op is its own leak site: {findings:?}"
        );
        assert!(
            findings.iter().all(|f| f.tag == "chan-send-leak"),
            "{findings:?}"
        );
    }

    /// A MULTI-block callee, so the ordinal conjunct's ancestor summation
    /// (`strict_ancestors` + the guarded `ite` sum) actually runs — every
    /// other fixture here has a single-block callee, where the ancestor set
    /// is empty and only the in-block `op_instr` half is exercised.
    ///
    /// `t.G(c, cond)`: block 0 branches on `cond` to blocks 1 and 2; block
    /// 1 sends once on `c` and falls through to block 2; block 2 is the
    /// join AND holds the candidate's blocking send (first instruction, so
    /// its own block contributes 0). So
    /// `pending = 0 + ite(g1, 1, 0)` — the ancestor's send is the ONLY
    /// contribution, and the finding can fire only if it is counted.
    ///
    /// Also pins `op_block` vs `go_block`: the candidate sits at callee
    /// block 2 while the `go` site is spawner block 0, so confusing the two
    /// collapses `pending` to 0 and this test fails (Task 6 left that
    /// discrimination untested — every callee there was single-block).
    /// `conditional == false` makes block 0 an unconditional `Jump` to
    /// block 1, so block 1 DOMINATES the candidate: `g1` is forced true on
    /// every path reaching block 2. That variant is what discriminates the
    /// `ite`'s polarity (see
    /// `buffered_send_dominating_ancestor_send_is_forced`) — in the
    /// conditional/diamond variant both polarities stay satisfiable,
    /// because reaching the join via the ¬cond edge makes `¬g1` true.
    fn ancestor_send_pkg(cap: i64, conditional: bool) -> Program {
        let (entry_instrs, entry_succs) = if conditional {
            (vec![branch_on(2)], vec![1, 2])
        } else {
            (vec![instr("Jump")], vec![1])
        };
        Program::from_packages(vec![pkg(
            "t",
            vec![
                func_with_aux(
                    "t.F",
                    vec![const_int_aux(1, cap)],
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
                    vec![
                        gvir::Param {
                            id: 1,
                            name: "c".into(),
                            r#type: 0,
                        },
                        gvir::Param {
                            id: 2,
                            name: "cond".into(),
                            r#type: 0,
                        },
                    ],
                    vec![
                        block(0, entry_instrs, entry_succs),
                        block(1, vec![send(1, 1)], vec![2]),
                        block(2, vec![send(1, 1), ret(vec![])], vec![]),
                    ],
                ),
            ],
        )])
    }

    /// cap 1: `pending = ite(g1, 1, 0) >= 1` is satisfiable (take the
    /// branch through block 1), so the join's send is blocked and reports.
    /// The sibling candidate (block 1's own send) has an empty ancestor set
    /// and `pending = 0 >= 1`, i.e. Unsat — hence exactly one finding.
    #[test]
    fn buffered_send_counts_guarded_ancestor_block_sends() {
        let findings = run_leak(&ancestor_send_pkg(1, true));
        assert_eq!(
            findings.len(),
            1,
            "the guarded ancestor's send fills the 1-slot buffer: {findings:?}"
        );
        assert_eq!(findings[0].tag, "chan-send-leak");
    }

    /// The chain variant (block 1 dominates the candidate, so `g1` is
    /// forced true): `pending` is then unconditionally 1, and cap 1 fires.
    /// This is the polarity pin — an `ite(g_b, 0, n)` mix-up evaluates to 0
    /// here and reports nothing, whereas in the diamond variant above both
    /// polarities stay satisfiable via the two different paths to the join.
    #[test]
    fn buffered_send_dominating_ancestor_send_is_forced() {
        let findings = run_leak(&ancestor_send_pkg(1, false));
        assert_eq!(
            findings.len(),
            1,
            "a dominating ancestor's send always counts: {findings:?}"
        );
        assert_eq!(findings[0].tag, "chan-send-leak");
    }

    /// cap 2, same shape: `pending` can reach at most 1, so
    /// `pending >= 2` is Unsat on every path and NOTHING reports. The
    /// negative direction — an ancestor summation that over-counts (e.g.
    /// summing unguarded, or counting the candidate's own send) would fire
    /// here.
    #[test]
    fn buffered_send_below_capacity_with_ancestors_stays_silent() {
        let findings = run_leak(&ancestor_send_pkg(2, true));
        assert!(
            findings.is_empty(),
            "one ancestor send cannot fill a 2-slot buffer: {findings:?}"
        );
    }

    /// The `LeakSelectAllBlocked` shape, end to end: two unbuffered makes
    /// bound into a closure, the closure blocking-selects (recv arms) over
    /// both, and the spawner does nothing else with either channel. Pins
    /// the self-suppression fix at the engine layer — before it, the
    /// candidate's own select (folded into `f`'s converged effects at both
    /// arm locs) suppressed every arm and this reported nothing.
    #[test]
    fn select_all_blocked_reports_via_z3() {
        let p = Program::from_packages(vec![pkg(
            "t",
            vec![
                func_with_aux(
                    "t.F",
                    vec![const_int_aux(1, 0), fn_aux(2, "t.F$1")],
                    vec![block(
                        0,
                        vec![
                            gvir_make_chan(3, 1),
                            gvir_make_chan(4, 1),
                            make_closure(5, 2, vec![3, 4]),
                            go_call_via_closure("t.F$1", 5),
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
        let findings = run_leak(&p);
        assert_eq!(findings.len(), 1, "exactly one finding: {findings:?}");
        assert_eq!(findings[0].checker, "goroutine-leak");
        assert_eq!(findings[0].tag, "chan-select-leak");
        assert_eq!(findings[0].severity, Severity::Error);
    }

    #[test]
    fn ignore_name_is_valid() {
        assert!(
            crate::default_checkers()
                .iter()
                .any(|c| c.name() == "goroutine-leak"),
            "goroutine-leak must be a registered checker name"
        );
    }
}
