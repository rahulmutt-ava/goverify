//! Concurrency effects (parent spec §5), phase-3 location-precise form
//! (final-review I2): unlike requires/ensures these are NOT placeholders
//! — they are syntactic facts, fully functional, now keyed by an
//! access-path `Loc` instead of a single flat set. There is no global
//! collapse to a single "top" bucket anymore; `Unknown` is the finite top
//! *location* (spec §9), used only when identity genuinely can't be
//! established.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use goverify_ir::{
    CallGraph, Callee, FuncId, Function, LockKind, MakeKind, Op, Program, ValueId, ValueKind,
    lock_kind,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ChanOp {
    Make,
    Send,
    Recv,
    Close,
    Select,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LockOp {
    Lock,
    Unlock,
    RLock,
    RUnlock,
    /// `defer mu.Unlock()` — runs at function exit, not program point
    /// (phase-2 Task 7 watch item; phase-C balance analysis needs the
    /// distinction natively).
    DeferredUnlock,
    DeferredRUnlock,
}

/// Ordered: None < Bounded < Unbounded (join = max).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum Spawns {
    #[default]
    None,
    Bounded,
    Unbounded,
}

/// Where an effect happens (phase-3 spec §9): an access-path root plus a
/// bounded field path. `Unknown` is the finite top bucket — there is no
/// global collapse anymore.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Root {
    Param(u32),
    Global(String),
    /// Alloc-site identity: the allocating instruction's dst ValueId —
    /// only meaningful within its own function; `rebase` maps it to
    /// Unknown when crossing a call boundary.
    Alloc(u32),
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Loc {
    pub root: Root,
    pub path: Vec<u32>,
}

/// Field-path depth cap. Deeper access paths degrade to `Unknown` (spec
/// §9: overflow degrades the root, it never grows the path unbounded).
pub const MAX_LOC_DEPTH: usize = 2;

impl Loc {
    pub fn unknown() -> Loc {
        Loc {
            root: Root::Unknown,
            path: Vec::new(),
        }
    }

    fn capped(self) -> Loc {
        if self.path.len() > MAX_LOC_DEPTH || self.root == Root::Unknown {
            Loc::unknown()
        } else {
            self
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Effects {
    pub spawns: Spawns,
    pub chan_ops: BTreeMap<Loc, BTreeSet<ChanOp>>,
    pub lock_ops: BTreeMap<Loc, BTreeSet<LockOp>>,
}

impl Effects {
    pub fn empty() -> Effects {
        Effects::default()
    }

    /// The finite top: every op, all at the single `Unknown` location.
    pub fn top() -> Effects {
        let mut chan_ops = BTreeMap::new();
        chan_ops.insert(
            Loc::unknown(),
            BTreeSet::from([
                ChanOp::Make,
                ChanOp::Send,
                ChanOp::Recv,
                ChanOp::Close,
                ChanOp::Select,
            ]),
        );
        let mut lock_ops = BTreeMap::new();
        lock_ops.insert(
            Loc::unknown(),
            BTreeSet::from([
                LockOp::Lock,
                LockOp::Unlock,
                LockOp::RLock,
                LockOp::RUnlock,
                LockOp::DeferredUnlock,
                LockOp::DeferredRUnlock,
            ]),
        );
        Effects {
            spawns: Spawns::Unbounded,
            chan_ops,
            lock_ops,
        }
    }

    pub fn is_empty(&self) -> bool {
        *self == Effects::empty()
    }

    /// Pointwise `BTreeSet` union under `BTreeMap` key merge; `spawns` is
    /// max. Commutative, idempotent, monotone (property-tested below).
    pub fn join(&mut self, other: &Effects) {
        self.spawns = self.spawns.max(other.spawns);
        for (loc, ops) in &other.chan_ops {
            self.chan_ops
                .entry(loc.clone())
                .or_default()
                .extend(ops.iter().copied());
        }
        for (loc, ops) in &other.lock_ops {
            self.lock_ops
                .entry(loc.clone())
                .or_default()
                .extend(ops.iter().copied());
        }
    }
}

/// Every def-bearing `Op` variant's destination `ValueId`, for building
/// the def-map `resolve_loc` walks. `None` for ops with no dst (or an
/// absent dst, e.g. a discarded `Call`/`Havoc`).
fn op_dst(op: &Op) -> Option<ValueId> {
    match op {
        Op::Assign { dst, .. }
        | Op::Alloc { dst, .. }
        | Op::Load { dst, .. }
        | Op::FieldAddr { dst, .. }
        | Op::Field { dst, .. }
        | Op::IndexAddr { dst, .. }
        | Op::Index { dst, .. }
        | Op::Lookup { dst, .. }
        | Op::Slice { dst, .. }
        | Op::BinOp { dst, .. }
        | Op::UnOp { dst, .. }
        | Op::Convert { dst, .. }
        | Op::Extract { dst, .. }
        | Op::Phi { dst, .. }
        | Op::MakeClosure { dst, .. }
        | Op::MakeInterface { dst, .. }
        | Op::Make { dst, .. }
        | Op::Recv { dst, .. }
        | Op::Select { dst, .. }
        | Op::TypeAssert { dst, .. } => Some(*dst),
        Op::Call { dst, .. } | Op::Havoc { dst } => *dst,
        _ => None,
    }
}

/// `ValueId -> &Op` for every def-bearing instruction in `f`. Rebuilt
/// fresh per `resolve_loc` call: functions are small at phase-3 scale
/// (same tradeoff `cyclic_blocks` makes) — revisit (e.g. a shared
/// `pub(crate) DefMap` built once per `collect` call) if profiling ever
/// says so.
fn def_map(f: &Function) -> HashMap<ValueId, &Op> {
    let mut m = HashMap::new();
    for b in &f.blocks {
        for ins in &b.instrs {
            if let Some(dst) = op_dst(&ins.op) {
                m.insert(dst, &ins.op);
            }
        }
    }
    m
}

/// Walks `v`'s def chain to find its access-path root (spec §9):
/// `FieldAddr{base, field}` prepends `field` to the path and recurses on
/// `base`; a `Param` value roots at its position in `f.params`; a
/// `Global(name)` value roots at `Global(name)`; an `Alloc{dst}` def
/// roots at `Alloc(dst.0)`; everything else (loads, phis, calls, opaque,
/// missing defs) is `Loc::unknown()`. A path deeper than `MAX_LOC_DEPTH`
/// degrades the whole `Loc` to `Unknown` (never just truncates the
/// path) — checked every hop, which also bounds the walk against a
/// malformed/cyclic def chain (fuzzed input).
pub fn resolve_loc(f: &Function, v: ValueId) -> Loc {
    let defs = def_map(f);
    let mut cur = v;
    let mut path: Vec<u32> = Vec::new();
    loop {
        if path.len() > MAX_LOC_DEPTH {
            return Loc::unknown();
        }
        match &f.value(cur).kind {
            ValueKind::Param => {
                return match f.params.iter().position(|&p| p == cur) {
                    Some(i) => Loc {
                        root: Root::Param(i as u32),
                        path,
                    },
                    None => Loc::unknown(),
                };
            }
            ValueKind::Global(name) => {
                return Loc {
                    root: Root::Global(name.clone()),
                    path,
                };
            }
            _ => {}
        }
        match defs.get(&cur) {
            Some(Op::FieldAddr { base, field, .. }) => {
                path.insert(0, *field);
                cur = *base;
            }
            Some(Op::Alloc { dst, .. }) => {
                return Loc {
                    root: Root::Alloc(dst.0),
                    path,
                };
            }
            _ => return Loc::unknown(),
        }
    }
}

/// Re-roots a callee's location-keyed effects through the caller's
/// arguments at a call site (spec §9): a `Param(i)`-rooted entry
/// re-roots through `caller_arg_loc(i)`, concatenating paths
/// caller-first (depth-capped ⇒ Unknown); `Global` entries pass through
/// unchanged (a package-level variable has the same identity regardless
/// of caller); a callee-local `Alloc` or already-`Unknown` entry becomes
/// `Unknown` — a callee's local allocation has no caller-visible
/// identity (phase 7 refines if needed). `spawns` isn't location-scoped,
/// so it passes through unchanged.
pub fn rebase(callee: &Effects, caller_arg_loc: &dyn Fn(u32) -> Loc) -> Effects {
    Effects {
        spawns: callee.spawns,
        chan_ops: rebase_map(&callee.chan_ops, caller_arg_loc),
        lock_ops: rebase_map(&callee.lock_ops, caller_arg_loc),
    }
}

fn rebase_map<T: Ord + Copy>(
    m: &BTreeMap<Loc, BTreeSet<T>>,
    caller_arg_loc: &dyn Fn(u32) -> Loc,
) -> BTreeMap<Loc, BTreeSet<T>> {
    let mut out: BTreeMap<Loc, BTreeSet<T>> = BTreeMap::new();
    for (loc, ops) in m {
        let new_loc = rebase_loc(loc, caller_arg_loc);
        out.entry(new_loc).or_default().extend(ops.iter().copied());
    }
    out
}

fn rebase_loc(loc: &Loc, caller_arg_loc: &dyn Fn(u32) -> Loc) -> Loc {
    match &loc.root {
        Root::Param(i) => {
            let caller = caller_arg_loc(*i);
            let mut path = caller.path.clone();
            path.extend(loc.path.iter().copied());
            Loc {
                root: caller.root,
                path,
            }
            .capped()
        }
        Root::Global(_) => loc.clone(),
        Root::Alloc(_) | Root::Unknown => Loc::unknown(),
    }
}

/// Blocks that sit on a CFG cycle: reachable from themselves. O(B²) DFS —
/// fine for phase 2 (functions are small; revisit if profiling says so).
fn cyclic_blocks(f: &Function) -> Vec<bool> {
    let n = f.blocks.len();
    let mut cyclic = vec![false; n];
    for (start, block) in f.blocks.iter().enumerate() {
        let mut seen = vec![false; n];
        let mut stack: Vec<usize> = block
            .succs
            .iter()
            .map(|&s| s as usize)
            .filter(|&s| s < n)
            .collect();
        while let Some(b) = stack.pop() {
            if b == start {
                cyclic[start] = true;
                break;
            }
            if !seen[b] {
                seen[b] = true;
                stack.extend(
                    f.blocks[b]
                        .succs
                        .iter()
                        .map(|&s| s as usize)
                        .filter(|&s| s < n),
                );
            }
        }
    }
    cyclic
}

fn lock_op(kind: LockKind) -> LockOp {
    match kind {
        LockKind::Lock => LockOp::Lock,
        LockKind::Unlock => LockOp::Unlock,
        LockKind::RLock => LockOp::RLock,
        LockKind::RUnlock => LockOp::RUnlock,
    }
}

/// `defer mu.Unlock()`/`defer mu.RUnlock()` run at function exit, not at
/// the defer statement's program point (phase-2 Task 7 watch item) — the
/// matching `LockOp` variant records that distinction. `go`'s deferred
/// method call has no such delay (the new goroutine runs it whenever it
/// runs), so only `Defer` gets this mapping; a plain `Lock`/`RLock`
/// deferred call is left as-is (weird code, but not a balance-relevant
/// unlock).
fn deferred(op: LockOp) -> LockOp {
    match op {
        LockOp::Unlock => LockOp::DeferredUnlock,
        LockOp::RUnlock => LockOp::DeferredRUnlock,
        other => other,
    }
}

fn arg_loc(f: &Function, args: &[ValueId], i: u32) -> Loc {
    args.get(i as usize)
        .map_or(Loc::unknown(), |&a| resolve_loc(f, a))
}

/// `defer`/`go` callees don't go through `lower.rs`'s plain-call
/// intrinsic rewrite (`lower_plain_call` only rewrites a direct
/// `Op::Call`), so `defer close(ch)` and `go mu.Unlock()`-shaped code
/// stays `Op::Defer`/`Op::Go` with a raw `Callee::Builtin("close")` or
/// `Callee::Static` lock-method callee instead of the `Op::CloseChan`/
/// `Op::Lock` intrinsic ops the plain-call path would produce
/// (final-review C2). Those two shapes are recorded directly, at the
/// chan/receiver argument's `Loc`. Any other static callee — including a
/// plain, non-lock deferred/spawned function — "still contributes via
/// the call-graph edge exactly as before": joined here via the same
/// arg-precise `rebase` a plain `Call` gets. Returns the callee's
/// `FuncId` whenever `callee` is `Callee::Static`, regardless of which
/// branch fired, so `collect` can add it to the static-site set (the
/// call-graph double-count guard: this callee must NOT also be joined a
/// second time, all-Unknown, by the invoke/dynamic diff pass).
fn record_go_defer_callee(
    p: &Program,
    f: &Function,
    e: &mut Effects,
    effects_of: &dyn Fn(FuncId) -> Effects,
    callee: &Callee,
    args: &[ValueId],
    is_defer: bool,
) -> Option<FuncId> {
    match callee {
        Callee::Builtin(name) if name == "close" => {
            let loc = arg_loc(f, args, 0);
            e.chan_ops.entry(loc).or_default().insert(ChanOp::Close);
            None
        }
        Callee::Static(c) => {
            if let Some(kind) = lock_kind(p.func_name(*c)) {
                let op = lock_op(kind);
                let op = if is_defer { deferred(op) } else { op };
                let loc = arg_loc(f, args, 0);
                e.lock_ops.entry(loc).or_default().insert(op);
            } else {
                e.join(&rebase(&effects_of(*c), &|i| arg_loc(f, args, i)));
            }
            Some(*c)
        }
        _ => None,
    }
}

/// Own concurrency ops, recorded at the access-path `Loc` of each op's
/// subject, plus every callee's effects: static callees (`Call`/`Go`/
/// `Defer` with `Callee::Static`) are rebased precisely through this
/// call site's arguments; every other call-graph callee (invoke/dynamic
/// dispatch — resolved by `graph`) is joined all-Unknown, UNLESS it
/// already has a static instruction site in `f` (the double-count guard:
/// a static callee is always also an edge in `CallGraph::callees`, so
/// joining it again via the graph would double-count it — worse, would
/// inject that callee's *unrelated* top/havoc effects on top of a
/// precisely-recorded intrinsic op, e.g. `defer mu.Unlock()`'s stdlib
/// `Unlock` callee).
///
/// Takes the caller's already-built `graph` rather than calling
/// `CallGraph::build(p)` itself: `collect` runs once per function per
/// fixpoint round, and `CallGraph::build` is O(whole program) — building
/// it afresh here would be O(n) *per function analyzed*, i.e. O(n²)
/// overall. On the real corpora this analyzer runs over (whole-DAG
/// extraction pulls in the full transitively-reachable stdlib), that
/// blows up to tens of minutes; `engine.rs` already builds the graph
/// once up front for `Sccs::compute` and threads the same reference
/// through every `analyze_function`/`collect` call.
pub fn collect(
    p: &Program,
    id: FuncId,
    graph: &CallGraph,
    effects_of: &dyn Fn(FuncId) -> Effects,
) -> Effects {
    let Some(f) = p.func(id) else {
        return Effects::top();
    };
    let cyclic = cyclic_blocks(f);
    let mut e = Effects::empty();
    let mut static_sites: BTreeSet<FuncId> = BTreeSet::new();

    for (bi, b) in f.blocks.iter().enumerate() {
        for ins in &b.instrs {
            match &ins.op {
                Op::Make {
                    kind: MakeKind::Chan,
                    dst,
                    ..
                } => {
                    e.chan_ops
                        .entry(resolve_loc(f, *dst))
                        .or_default()
                        .insert(ChanOp::Make);
                }
                Op::Send { chan, .. } => {
                    e.chan_ops
                        .entry(resolve_loc(f, *chan))
                        .or_default()
                        .insert(ChanOp::Send);
                }
                Op::Recv { chan, .. } => {
                    e.chan_ops
                        .entry(resolve_loc(f, *chan))
                        .or_default()
                        .insert(ChanOp::Recv);
                }
                Op::CloseChan { chan } => {
                    e.chan_ops
                        .entry(resolve_loc(f, *chan))
                        .or_default()
                        .insert(ChanOp::Close);
                }
                Op::Select { arms, .. } => {
                    for arm in arms {
                        e.chan_ops
                            .entry(resolve_loc(f, arm.chan))
                            .or_default()
                            .insert(ChanOp::Select);
                    }
                }
                Op::Lock { kind, mu } => {
                    e.lock_ops
                        .entry(resolve_loc(f, *mu))
                        .or_default()
                        .insert(lock_op(*kind));
                }
                Op::Call {
                    callee: Callee::Static(c),
                    args,
                    ..
                } => {
                    static_sites.insert(*c);
                    e.join(&rebase(&effects_of(*c), &|i| arg_loc(f, args, i)));
                }
                Op::Go { callee, args } => {
                    let s = if cyclic[bi] {
                        Spawns::Unbounded
                    } else {
                        Spawns::Bounded
                    };
                    e.spawns = e.spawns.max(s);
                    if let Some(c) =
                        record_go_defer_callee(p, f, &mut e, effects_of, callee, args, false)
                    {
                        static_sites.insert(c);
                    }
                }
                Op::Defer { callee, args } => {
                    if let Some(c) =
                        record_go_defer_callee(p, f, &mut e, effects_of, callee, args, true)
                    {
                        static_sites.insert(c);
                    }
                }
                _ => {}
            }
        }
    }

    // Call-graph double-count guard (final-review I2): every callee
    // reached only through an invoke/dynamic edge (never a static
    // instruction site in `f`) still needs joining — all-Unknown, since
    // there's no caller-visible argument mapping for a dispatch that
    // could target any implementer.
    for &c in graph.callees(id) {
        if !static_sites.contains(&c) {
            e.join(&rebase(&effects_of(c), &|_| Loc::unknown()));
        }
    }

    e
}

#[cfg(test)]
mod tests {
    use goverify_ir::Program;

    use super::*;
    use crate::testpkg::{
        block, call, defer_call, defer_close, func, go_call, go_close, instr, pkg,
    };

    /// Regression (final-review C2): `defer close(ch)` doesn't go through
    /// `lower.rs`'s plain-call intrinsic rewrite, so it stays
    /// `Op::Defer{callee: Callee::Builtin("close")}` — `collect` must
    /// still record the `ChanOp::Close` effect from that callee.
    #[test]
    fn defer_close_builtin_records_chanop_close() {
        let p = Program::from_packages(vec![pkg(
            "t",
            vec![func(
                "t.F",
                vec![block(0, vec![defer_close(1), instr("Return")], vec![])],
            )],
        )]);
        let g = CallGraph::build(&p);
        let e = collect(&p, p.lookup_func("t.F").unwrap(), &g, &|_| Effects::empty());
        let ops: BTreeSet<ChanOp> = e.chan_ops.values().flatten().copied().collect();
        assert!(
            ops.contains(&ChanOp::Close),
            "defer close(ch) must record ChanOp::Close: {e:?}"
        );
    }

    /// Same as above but for `go close(ch)` — also exercises that the
    /// existing spawn-recording logic keeps working alongside the new
    /// callee inspection.
    #[test]
    fn go_close_builtin_records_chanop_close_and_spawn() {
        let p = Program::from_packages(vec![pkg(
            "t",
            vec![func(
                "t.F",
                vec![block(0, vec![go_close(1), instr("Return")], vec![])],
            )],
        )]);
        let g = CallGraph::build(&p);
        let e = collect(&p, p.lookup_func("t.F").unwrap(), &g, &|_| Effects::empty());
        let ops: BTreeSet<ChanOp> = e.chan_ops.values().flatten().copied().collect();
        assert!(
            ops.contains(&ChanOp::Close),
            "go close(ch) must record ChanOp::Close: {e:?}"
        );
        assert_eq!(e.spawns, Spawns::Bounded);
    }

    /// Regression (final-review C2, updated for the location-precise
    /// lattice): `defer mu.Unlock()` — a `Defer` whose `Callee::Static`
    /// names one of the `sync.Mutex`/`sync.RWMutex` lock-intrinsic
    /// methods — must record `LockOp::DeferredUnlock`, not a plain
    /// `Unlock` (phase-2 Task 7 watch item; phase-C balance analysis
    /// needs the distinction natively). Also exercises the call-graph
    /// double-count guard: `(*sync.Mutex).Unlock` is external (havoc,
    /// i.e. `Effects::top()`), and is ALSO an edge in the call graph
    /// (every `Defer{Callee::Static}` is) — without the guard, `collect`
    /// would additionally join that havoc top() in, and `Unlock` would
    /// reappear via the `Unknown` bucket.
    #[test]
    fn defer_lock_method_records_deferred_unlock() {
        let p = Program::from_packages(vec![pkg(
            "t",
            vec![func(
                "t.F",
                vec![block(
                    0,
                    vec![defer_call("(*sync.Mutex).Unlock"), instr("Return")],
                    vec![],
                )],
            )],
        )]);
        let g = CallGraph::build(&p);
        let e = collect(&p, p.lookup_func("t.F").unwrap(), &g, &|_| Effects::empty());
        let ops: BTreeSet<LockOp> = e.lock_ops.values().flatten().copied().collect();
        assert!(
            ops.contains(&LockOp::DeferredUnlock),
            "defer mu.Unlock() must be DeferredUnlock, not Unlock: {e:?}"
        );
        assert!(
            !ops.contains(&LockOp::Unlock),
            "not double-recorded as plain Unlock"
        );
    }

    #[test]
    fn lock_on_param_gets_param_rooted_loc() {
        use goverify_extract::gvir;
        use goverify_extract::gvir::instruction::Sem;

        use crate::testpkg::func_with_params;

        // t.F(p) { p.Lock() } — a Call with static_callee "(*sync.Mutex).Lock"
        // and operand 1 (the param value id) lowers to Op::Lock{mu: param}.
        // Operand layout mirrors lower.rs's [callee slot, args…]: index 0
        // is the (unused-for-static) callee slot, index 1 is the receiver.
        let mut lock = instr("Call");
        lock.operands = vec![0, 1];
        lock.sem = Some(Sem::Call(gvir::CallSem {
            static_callee: "(*sync.Mutex).Lock".into(),
            ..Default::default()
        }));
        let p = Program::from_packages(vec![pkg(
            "t",
            vec![func_with_params(
                "t.F",
                vec![gvir::Param {
                    id: 1,
                    name: "p".into(),
                    r#type: 0,
                }],
                vec![block(0, vec![lock, instr("Return")], vec![])],
            )],
        )]);
        let g = CallGraph::build(&p);
        let e = collect(&p, p.lookup_func("t.F").unwrap(), &g, &|_| Effects::empty());
        let want = Loc {
            root: Root::Param(0),
            path: vec![],
        };
        assert!(
            e.lock_ops
                .get(&want)
                .is_some_and(|ops| ops.contains(&LockOp::Lock)),
            "Lock must be recorded at the param-rooted Loc: {e:?}"
        );
    }

    #[test]
    fn effects_top_is_unknown_bucket() {
        let t = Effects::top();
        assert_eq!(t.lock_ops.len(), 1);
        assert!(t.lock_ops.contains_key(&Loc::unknown()));
        assert_eq!(
            t.lock_ops[&Loc::unknown()].len(),
            6,
            "all six LockOps incl. deferred variants"
        );
    }

    #[test]
    fn rebase_reroots_param_entries_through_caller_args() {
        let mut callee = Effects::empty();
        callee
            .lock_ops
            .entry(Loc {
                root: Root::Param(0),
                path: vec![1],
            })
            .or_default()
            .insert(LockOp::Lock);
        let rebased = rebase(&callee, &|i| {
            assert_eq!(i, 0);
            Loc {
                root: Root::Param(3),
                path: vec![2],
            }
        });
        let want = Loc {
            root: Root::Param(3),
            path: vec![2, 1],
        };
        assert!(rebased.lock_ops.contains_key(&want), "{rebased:?}");
    }

    #[test]
    fn rebase_depth_overflow_degrades_to_unknown() {
        let mut callee = Effects::empty();
        callee
            .lock_ops
            .entry(Loc {
                root: Root::Param(0),
                path: vec![1, 2],
            })
            .or_default()
            .insert(LockOp::Lock);
        let rebased = rebase(&callee, &|_| Loc {
            root: Root::Param(0),
            path: vec![3],
        });
        assert!(
            rebased.lock_ops.contains_key(&Loc::unknown()),
            "3-deep path exceeds MAX_LOC_DEPTH=2: {rebased:?}"
        );
    }

    #[test]
    fn rebase_callee_alloc_becomes_unknown() {
        let mut callee = Effects::empty();
        callee
            .lock_ops
            .entry(Loc {
                root: Root::Alloc(7),
                path: vec![],
            })
            .or_default()
            .insert(LockOp::Lock);
        let rebased = rebase(&callee, &|_| Loc::unknown());
        assert_eq!(
            rebased.lock_ops.keys().collect::<Vec<_>>(),
            vec![&Loc::unknown()]
        );
    }

    #[test]
    fn go_in_loop_is_unbounded_spawn() {
        // CFG: b0 -> b1; b1 contains Go and loops to itself; b1 -> b2.
        let p = Program::from_packages(vec![pkg(
            "t",
            vec![func(
                "t.F",
                vec![
                    block(0, vec![instr("Jump")], vec![1]),
                    block(1, vec![go_call("t.G"), instr("Jump")], vec![1, 2]),
                    block(2, vec![instr("Return")], vec![]),
                ],
            )],
        )]);
        let g = CallGraph::build(&p);
        let e = collect(&p, p.lookup_func("t.F").unwrap(), &g, &|_| Effects::empty());
        assert_eq!(e.spawns, Spawns::Unbounded);
    }

    #[test]
    fn straight_line_go_is_bounded() {
        let p = Program::from_packages(vec![pkg(
            "t",
            vec![func(
                "t.F",
                vec![block(0, vec![go_call("t.G"), instr("Return")], vec![])],
            )],
        )]);
        let g = CallGraph::build(&p);
        let e = collect(&p, p.lookup_func("t.F").unwrap(), &g, &|_| Effects::empty());
        assert_eq!(e.spawns, Spawns::Bounded);
    }

    #[test]
    fn callee_effects_join_in() {
        let p = Program::from_packages(vec![pkg(
            "t",
            vec![func(
                "t.F",
                vec![block(0, vec![call("t.G"), instr("Return")], vec![])],
            )],
        )]);
        let g_id = p.lookup_func("t.G").unwrap();
        let mut callee = Effects::empty();
        callee
            .lock_ops
            .entry(Loc::unknown())
            .or_default()
            .insert(LockOp::Lock);
        let graph = CallGraph::build(&p);
        let e = collect(&p, p.lookup_func("t.F").unwrap(), &graph, &|c| {
            if c == g_id {
                callee.clone()
            } else {
                Effects::empty()
            }
        });
        let ops: BTreeSet<LockOp> = e.lock_ops.values().flatten().copied().collect();
        assert!(ops.contains(&LockOp::Lock));
    }
}

#[cfg(test)]
mod props {
    use proptest::prelude::*;

    use super::*;

    fn arb_loc() -> impl Strategy<Value = Loc> {
        (
            prop_oneof![
                (0u32..4).prop_map(Root::Param),
                Just(Root::Global("t.G".into())),
                (0u32..8).prop_map(Root::Alloc),
                Just(Root::Unknown),
            ],
            prop::collection::vec(0u32..4, 0..=2),
        )
            .prop_map(|(root, path)| Loc { root, path })
    }

    fn arb_effects() -> impl Strategy<Value = Effects> {
        (
            prop::collection::btree_map(
                arb_loc(),
                prop::collection::btree_set(
                    prop::sample::select(vec![
                        LockOp::Lock,
                        LockOp::Unlock,
                        LockOp::RLock,
                        LockOp::RUnlock,
                        LockOp::DeferredUnlock,
                        LockOp::DeferredRUnlock,
                    ]),
                    1..3,
                ),
                0..3,
            ),
            prop::collection::btree_map(
                arb_loc(),
                prop::collection::btree_set(
                    prop::sample::select(vec![
                        ChanOp::Make,
                        ChanOp::Send,
                        ChanOp::Recv,
                        ChanOp::Close,
                        ChanOp::Select,
                    ]),
                    1..3,
                ),
                0..3,
            ),
            prop::sample::select(vec![Spawns::None, Spawns::Bounded, Spawns::Unbounded]),
        )
            .prop_map(|(lock_ops, chan_ops, spawns)| Effects {
                spawns,
                chan_ops,
                lock_ops,
            })
    }

    proptest! {
        #[test]
        fn join_is_commutative_idempotent_monotone(a in arb_effects(), b in arb_effects()) {
            let mut ab = a.clone(); ab.join(&b);
            let mut ba = b.clone(); ba.join(&a);
            prop_assert_eq!(&ab, &ba, "commutative");
            let mut aa = a.clone(); aa.join(&a);
            prop_assert_eq!(&aa, &a, "idempotent");
            let mut a_top = a.clone(); a_top.join(&Effects::top());
            prop_assert_eq!(&a_top, &{ let mut t = Effects::top(); t.join(&a); t }, "top absorbs symmetrically");
            // Monotone: a ⊑ a∨b. Joining `b` into `a` yields `ab`; if `a`
            // is already ⊑ `ab` (i.e. `a` contributed nothing `ab` didn't
            // already have), then joining `a` into `ab` must leave `ab`
            // unchanged.
            let mut ab_join_a = ab.clone(); ab_join_a.join(&a);
            prop_assert_eq!(&ab_join_a, &ab, "monotone: a joined into a∨b changes nothing (a ⊑ a∨b)");
        }
    }
}
