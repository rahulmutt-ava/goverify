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
    /// Identity-only: analyses key globals by name, never by
    /// initializer value. If that changes, the SCC cache's ctx-hash
    /// (goverify-ir func_ir_hash) must start keying file content —
    /// see its INVARIANT note.
    Global(String),
    /// Alloc-site identity: the allocating instruction's dst ValueId —
    /// only meaningful within its own function; `rebase` maps it to
    /// Unknown when crossing a call boundary.
    Alloc(u32),
    /// Ordinal into the enclosing function's `free_vars` (phase 7): a
    /// captured closure variable's identity, meaningful only inside that
    /// function. `rebase` maps it through the caller's closure-binding
    /// closure (`caller_fv_loc`) or degrades to `Unknown` when no
    /// mapping is supplied.
    FreeVar(u32),
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

/// Field-path/def-chain hop cap: `resolve_loc`'s walk (both `FieldAddr`
/// path-growing hops and the transparent `Load`/`Assign` deref hops)
/// degrades to `Unknown` past this many iterations, bounding the loop
/// against a malformed/cyclic def chain (fuzzed input) independent of
/// `MAX_LOC_DEPTH`, which only bounds path *growth*.
pub const MAX_LOC_HOPS: usize = 32;

/// Walks `v`'s def chain to find its access-path root (spec §9):
/// `FieldAddr{base, field}` prepends `field` to the path and recurses on
/// `base`; a `Param` value roots at its position in `f.params`; a
/// `FreeVar` value roots at its position in `f.free_vars` (phase 7); a
/// `Global(name)` value roots at `Global(name)`; an `Alloc{dst}` or
/// `Make{dst}` def roots at `Alloc(dst.0)` (alloc-site identity — a
/// `make(chan)`/`make(map)`/`make(slice)` register is exactly as much an
/// allocation site as `Op::Alloc`); `Load{addr}`/`Assign{src}` are
/// transparent hops (deref-collapsing: a cell and its loaded/assigned
/// content share a Loc — sound for effects matching, since over-merging
/// can only suppress leak findings, never manufacture them); everything
/// else (phis, calls, opaque, missing defs) is `Loc::unknown()`. A path
/// deeper than `MAX_LOC_DEPTH` degrades the whole `Loc` to `Unknown`
/// (never just truncates the path); a walk longer than `MAX_LOC_HOPS`
/// hops (path-growing or transparent) does too.
pub fn resolve_loc(f: &Function, v: ValueId) -> Loc {
    let defs = def_map(f);
    let mut cur = v;
    let mut path: Vec<u32> = Vec::new();
    let mut hops: usize = 0;
    loop {
        if path.len() > MAX_LOC_DEPTH || hops > MAX_LOC_HOPS {
            return Loc::unknown();
        }
        hops += 1;
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
            ValueKind::FreeVar => {
                return match f.free_vars.iter().position(|&fv| fv == cur) {
                    Some(i) => Loc {
                        root: Root::FreeVar(i as u32),
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
            Some(Op::Alloc { dst, .. }) | Some(Op::Make { dst, .. }) => {
                return Loc {
                    root: Root::Alloc(dst.0),
                    path,
                };
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
    }
}

/// Re-roots a callee's location-keyed effects through the caller's
/// arguments (and closure bindings) at a call site (spec §9): a
/// `Param(i)`-rooted entry re-roots through `caller_arg_loc(i)`, and a
/// `FreeVar(i)`-rooted entry re-roots through `caller_fv_loc(i)`
/// (phase 7), both concatenating paths caller-first (depth-capped ⇒
/// Unknown); `Global` entries pass through unchanged (a package-level
/// variable has the same identity regardless of caller); a callee-local
/// `Alloc` or already-`Unknown` entry becomes `Unknown` — a callee's
/// local allocation has no caller-visible identity (phase 7 refines if
/// needed). `spawns` isn't location-scoped, so it passes through
/// unchanged.
pub fn rebase(
    callee: &Effects,
    caller_arg_loc: &dyn Fn(u32) -> Loc,
    caller_fv_loc: &dyn Fn(u32) -> Loc,
) -> Effects {
    Effects {
        spawns: callee.spawns,
        chan_ops: rebase_map(&callee.chan_ops, caller_arg_loc, caller_fv_loc),
        lock_ops: rebase_map(&callee.lock_ops, caller_arg_loc, caller_fv_loc),
    }
}

fn rebase_map<T: Ord + Copy>(
    m: &BTreeMap<Loc, BTreeSet<T>>,
    caller_arg_loc: &dyn Fn(u32) -> Loc,
    caller_fv_loc: &dyn Fn(u32) -> Loc,
) -> BTreeMap<Loc, BTreeSet<T>> {
    let mut out: BTreeMap<Loc, BTreeSet<T>> = BTreeMap::new();
    for (loc, ops) in m {
        let new_loc = rebase_loc(loc, caller_arg_loc, caller_fv_loc);
        out.entry(new_loc).or_default().extend(ops.iter().copied());
    }
    out
}

fn rebase_loc(
    loc: &Loc,
    caller_arg_loc: &dyn Fn(u32) -> Loc,
    caller_fv_loc: &dyn Fn(u32) -> Loc,
) -> Loc {
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
        Root::FreeVar(i) => {
            let caller = caller_fv_loc(*i);
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

/// A callee's `FreeVar(i)`-rooted `Loc` re-roots through its `i`th
/// `MakeClosure` binding at this call site (phase 7): absent, ambiguous
/// (≥2 `MakeClosure` sites for `c`), or out-of-range `i` all degrade to
/// `Loc::unknown()` — never a panic on malformed/fuzzed input.
///
/// `pub` (not `pub(crate)`): phase 7's goroutine-leak checker
/// (`goverify-checkers`'s `leak.rs`) needs this exact re-rooting logic
/// for its own call-site mapping (a `Go` site's free-var argument),
/// rather than duplicating `closure_bindings`' bindings lookup a second
/// time in a sibling crate.
pub fn fv_loc(f: &Function, cb: &HashMap<FuncId, Option<Vec<ValueId>>>, c: FuncId, i: u32) -> Loc {
    match cb.get(&c) {
        Some(Some(bindings)) => bindings
            .get(i as usize)
            .map_or(Loc::unknown(), |&bv| resolve_loc(f, bv)),
        _ => Loc::unknown(),
    }
}

/// Blocks that sit on a CFG cycle: reachable from themselves. O(B²) DFS —
/// fine for phase 2 (functions are small; revisit if profiling says so).
///
/// `pub` (not `pub(crate)`): phase 7's buffered-cyclic-goroutine checker
/// (Task 6) reuses this same cycle test standalone, without re-running
/// all of `collect`.
pub fn cyclic_blocks(f: &Function) -> Vec<bool> {
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
#[allow(clippy::too_many_arguments)]
fn record_go_defer_callee(
    p: &Program,
    f: &Function,
    e: &mut Effects,
    effects_of: &dyn Fn(FuncId) -> Effects,
    cb: &HashMap<FuncId, Option<Vec<ValueId>>>,
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
                e.join(&rebase(&effects_of(*c), &|i| arg_loc(f, args, i), &|i| {
                    fv_loc(f, cb, *c, i)
                }));
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
    let cb = closure_bindings(f);
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
                    e.join(&rebase(&effects_of(*c), &|i| arg_loc(f, args, i), &|i| {
                        fv_loc(f, &cb, *c, i)
                    }));
                }
                Op::Go { callee, args } => {
                    let s = if cyclic[bi] {
                        Spawns::Unbounded
                    } else {
                        Spawns::Bounded
                    };
                    e.spawns = e.spawns.max(s);
                    if let Some(c) =
                        record_go_defer_callee(p, f, &mut e, effects_of, &cb, callee, args, false)
                    {
                        static_sites.insert(c);
                    }
                }
                Op::Defer { callee, args } => {
                    if let Some(c) =
                        record_go_defer_callee(p, f, &mut e, effects_of, &cb, callee, args, true)
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
            e.join(&rebase(&effects_of(c), &|_| Loc::unknown(), &|_| {
                Loc::unknown()
            }));
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
        let rebased = rebase(
            &callee,
            &|i| {
                assert_eq!(i, 0);
                Loc {
                    root: Root::Param(3),
                    path: vec![2],
                }
            },
            &|_| Loc::unknown(),
        );
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
        let rebased = rebase(
            &callee,
            &|_| Loc {
                root: Root::Param(0),
                path: vec![3],
            },
            &|_| Loc::unknown(),
        );
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
        let rebased = rebase(&callee, &|_| Loc::unknown(), &|_| Loc::unknown());
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
    fn make_chan_dst_roots_at_alloc_site() {
        // t.F: v2 = MakeChan; Send v2 — the send's Loc must root at
        // Alloc(2), not Unknown (pre-phase-7 behavior).
        let mk = crate::testpkg::gvir_make_chan(2, 1); // register 2, cap operand id 1 (helper below)
        let mut send = instr("Send");
        send.operands = vec![2, 1];
        let p = Program::from_packages(vec![pkg(
            "t",
            vec![func(
                "t.F",
                vec![block(0, vec![mk, send, instr("Return")], vec![])],
            )],
        )]);
        let g = CallGraph::build(&p);
        let e = collect(&p, p.lookup_func("t.F").unwrap(), &g, &|_| Effects::empty());
        let want = Loc {
            root: Root::Alloc(2),
            path: vec![],
        };
        assert!(
            e.chan_ops
                .get(&want)
                .is_some_and(|ops| ops.contains(&ChanOp::Send)),
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
            vec![crate::testpkg::func_with_free_vars(
                "t.F$1",
                vec![1],
                vec![block(0, vec![send, instr("Return")], vec![])],
            )],
        )]);
        let g = CallGraph::build(&p);
        let e = collect(&p, p.lookup_func("t.F$1").unwrap(), &g, &|_| {
            Effects::empty()
        });
        let want = Loc {
            root: Root::FreeVar(0),
            path: vec![],
        };
        assert!(
            e.chan_ops
                .get(&want)
                .is_some_and(|ops| ops.contains(&ChanOp::Send)),
            "{e:?}"
        );
    }

    #[test]
    fn load_hop_collapses_cell_and_content() {
        // v2 = Alloc (cell); v3 = Load v2; Send v3 — the loaded channel
        // shares the cell's Loc: Root::Alloc(2).
        use goverify_extract::gvir;
        use goverify_extract::gvir::instruction::Sem;

        let mut alloc = instr("Alloc");
        alloc.register = 2;
        alloc.sem = Some(Sem::Alloc(gvir::AllocSem { heap: true }));
        // gvir has no standalone "Load" wire kind: a dereference load
        // lowers from "UnOp" with op "*" (lower.rs: `"*" => Op::Load{..}`).
        let mut load = instr("UnOp");
        load.register = 3;
        load.operands = vec![2];
        load.sem = Some(Sem::Unop(gvir::UnOpSem {
            op: "*".into(),
            comma_ok: false,
        }));
        let mut send = instr("Send");
        send.operands = vec![3, 1];
        let p = Program::from_packages(vec![pkg(
            "t",
            vec![func(
                "t.F",
                vec![block(0, vec![alloc, load, send, instr("Return")], vec![])],
            )],
        )]);
        let g = CallGraph::build(&p);
        let e = collect(&p, p.lookup_func("t.F").unwrap(), &g, &|_| Effects::empty());
        let want = Loc {
            root: Root::Alloc(2),
            path: vec![],
        };
        assert!(
            e.chan_ops
                .get(&want)
                .is_some_and(|ops| ops.contains(&ChanOp::Send)),
            "{e:?}"
        );
    }

    #[test]
    fn rebase_free_var_reroots_through_fv_loc() {
        let mut callee = Effects::empty();
        callee
            .chan_ops
            .entry(Loc {
                root: Root::FreeVar(0),
                path: vec![],
            })
            .or_default()
            .insert(ChanOp::Send);
        let rebased = rebase(&callee, &|_| Loc::unknown(), &|i| {
            assert_eq!(i, 0);
            Loc {
                root: Root::Alloc(7),
                path: vec![],
            }
        });
        assert!(rebased.chan_ops.contains_key(&Loc {
            root: Root::Alloc(7),
            path: vec![]
        }));
    }

    #[test]
    fn rebase_free_var_without_mapping_degrades_to_unknown() {
        let mut callee = Effects::empty();
        callee
            .chan_ops
            .entry(Loc {
                root: Root::FreeVar(3),
                path: vec![],
            })
            .or_default()
            .insert(ChanOp::Send);
        let rebased = rebase(&callee, &|_| Loc::unknown(), &|_| Loc::unknown());
        assert_eq!(
            rebased.chan_ops.keys().collect::<Vec<_>>(),
            vec![&Loc::unknown()]
        );
    }

    #[test]
    fn go_closure_send_rebases_free_var_through_bindings() {
        // t.F: v2 = make(chan); v3 = MakeClosure t.F$1 [v2]; go t.F$1()
        // t.F$1: FreeVar fv1; Send fv1
        // F's effects must contain Send at Alloc(2) — the closure's
        // FreeVar(0) rebased through binding v2.
        use goverify_extract::gvir;
        use goverify_extract::gvir::instruction::Sem;

        use crate::testpkg::{fn_aux, func_with_aux, make_closure};

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
                            crate::testpkg::gvir_make_chan(2, 1),
                            make_closure(3, 1, vec![2]),
                            go,
                            instr("Return"),
                        ],
                        vec![],
                    )],
                ),
                crate::testpkg::func_with_free_vars(
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
            if c == f1 {
                f1_effects.clone()
            } else {
                Effects::empty()
            }
        });
        let want = Loc {
            root: Root::Alloc(2),
            path: vec![],
        };
        assert!(
            e.chan_ops
                .get(&want)
                .is_some_and(|ops| ops.contains(&ChanOp::Send)),
            "closure-captured send must rebase to the spawner's alloc site: {e:?}"
        );
        assert_eq!(e.spawns, Spawns::Bounded);
    }

    /// Two MakeClosure sites for the same target fn (fuzz shape) —
    /// bindings are ambiguous, FreeVar-rooted effects degrade to Unknown.
    #[test]
    fn ambiguous_make_closure_degrades_free_var_to_unknown() {
        use goverify_extract::gvir;
        use goverify_extract::gvir::instruction::Sem;

        use crate::testpkg::{fn_aux, func_with_aux, make_closure};

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
                            crate::testpkg::gvir_make_chan(2, 1),
                            crate::testpkg::gvir_make_chan(5, 1),
                            make_closure(3, 1, vec![2]),
                            make_closure(4, 1, vec![5]),
                            go,
                            instr("Return"),
                        ],
                        vec![],
                    )],
                ),
                crate::testpkg::func_with_free_vars(
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
            if c == f1 {
                f1_effects.clone()
            } else {
                Effects::empty()
            }
        });
        let send_at = |loc: &Loc| {
            e.chan_ops
                .get(loc)
                .is_some_and(|o| o.contains(&ChanOp::Send))
        };
        assert!(
            send_at(&Loc::unknown()),
            "ambiguous bindings must degrade: {e:?}"
        );
        assert!(!send_at(&Loc {
            root: Root::Alloc(2),
            path: vec![]
        }));
        assert!(!send_at(&Loc {
            root: Root::Alloc(5),
            path: vec![]
        }));
    }

    /// `testpkg::const_int_aux` isn't exercised by the two closure-rebase
    /// tests above (their `MakeChan` cap operand doesn't need a defined
    /// aux value) but is part of the testpkg interface this task adds —
    /// pin its wire shape directly so it isn't dead code.
    #[test]
    fn const_int_aux_builds_expected_wire_shape() {
        use goverify_extract::gvir;

        use crate::testpkg::const_int_aux;

        let a = const_int_aux(9, 42);
        assert_eq!(a.id, 9);
        assert_eq!(a.kind, "Const");
        assert!(matches!(
            a.r#const,
            Some(gvir::ConstValue {
                value: Some(gvir::const_value::Value::Int(42)),
            })
        ));
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
                (0u32..4).prop_map(Root::FreeVar),
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
