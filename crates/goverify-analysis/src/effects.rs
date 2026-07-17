//! Concurrency effects (parent spec §5). Unlike requires/ensures these are
//! NOT placeholders: they are syntactic facts, fully functional in phase 2.

use std::collections::BTreeSet;

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
}

/// Ordered: None < Bounded < Unbounded (join = max).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum Spawns {
    #[default]
    None,
    Bounded,
    Unbounded,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Effects {
    pub spawns: Spawns,
    pub chan_ops: BTreeSet<ChanOp>,
    pub lock_ops: BTreeSet<LockOp>,
}

impl Effects {
    pub fn empty() -> Effects {
        Effects::default()
    }

    pub fn top() -> Effects {
        Effects {
            spawns: Spawns::Unbounded,
            chan_ops: [
                ChanOp::Make,
                ChanOp::Send,
                ChanOp::Recv,
                ChanOp::Close,
                ChanOp::Select,
            ]
            .into(),
            lock_ops: [LockOp::Lock, LockOp::Unlock, LockOp::RLock, LockOp::RUnlock].into(),
        }
    }

    pub fn is_empty(&self) -> bool {
        *self == Effects::empty()
    }

    pub fn join(&mut self, other: &Effects) {
        self.spawns = self.spawns.max(other.spawns);
        self.chan_ops.extend(other.chan_ops.iter().copied());
        self.lock_ops.extend(other.lock_ops.iter().copied());
    }
}

use goverify_ir::{Callee, FuncId, LockKind, Op, Program, lock_kind};

/// Blocks that sit on a CFG cycle: reachable from themselves. O(B²) DFS —
/// fine for phase 2 (functions are small; revisit if profiling says so).
fn cyclic_blocks(f: &goverify_ir::Function) -> Vec<bool> {
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

/// Own concurrency ops + join of all call-graph callees' effects.
/// Callee resolution (static, invoke, dynamic) already happened in the
/// call graph, so `callee_effects` is simply the effects of every callee
/// edge — call-site precision is unnecessary for set-union effects.
pub fn collect(p: &Program, id: FuncId, callee_effects: &[&Effects]) -> Effects {
    let Some(f) = p.func(id) else {
        return Effects::top();
    };
    let cyclic = cyclic_blocks(f);
    let mut e = Effects::empty();
    for ce in callee_effects {
        e.join(ce);
    }
    for (bi, b) in f.blocks.iter().enumerate() {
        for ins in &b.instrs {
            match &ins.op {
                Op::Make {
                    kind: goverify_ir::MakeKind::Chan,
                    ..
                } => {
                    e.chan_ops.insert(ChanOp::Make);
                }
                Op::Send { .. } => {
                    e.chan_ops.insert(ChanOp::Send);
                }
                Op::Recv { .. } => {
                    e.chan_ops.insert(ChanOp::Recv);
                }
                Op::CloseChan { .. } => {
                    e.chan_ops.insert(ChanOp::Close);
                }
                Op::Select { .. } => {
                    e.chan_ops.insert(ChanOp::Select);
                }
                Op::Lock { kind, .. } => {
                    e.lock_ops.insert(lock_op(*kind));
                }
                Op::Go { callee, .. } => {
                    let s = if cyclic[bi] {
                        Spawns::Unbounded
                    } else {
                        Spawns::Bounded
                    };
                    e.spawns = e.spawns.max(s);
                    record_intrinsic_callee(p, &mut e, callee);
                }
                Op::Defer { callee, .. } => {
                    record_intrinsic_callee(p, &mut e, callee);
                }
                _ => {}
            }
        }
    }
    e
}

fn lock_op(kind: LockKind) -> LockOp {
    match kind {
        LockKind::Lock => LockOp::Lock,
        LockKind::Unlock => LockOp::Unlock,
        LockKind::RLock => LockOp::RLock,
        LockKind::RUnlock => LockOp::RUnlock,
    }
}

/// `defer`/`go` callees don't go through `lower.rs`'s plain-call
/// intrinsic rewrite (`lower_plain_call` only rewrites a direct
/// `Op::Call`), so `defer close(ch)` and `go mu.Unlock()`-shaped code
/// stays `Op::Defer`/`Op::Go` with a raw `Callee::Builtin("close")` or
/// `Callee::Static` lock-method callee instead of the `Op::CloseChan`/
/// `Op::Lock` intrinsic ops the plain-call path would produce
/// (final-review C2). Without this, those effects vanish entirely: the
/// callee is a builtin (no call-graph edge, nothing to join effects
/// from) or a body-less stdlib function (an edge exists, but there's no
/// lowered body to analyze), so the *only* place the effect can be
/// recorded is here, mirroring the plain-`Op::CloseChan`/`Op::Lock` arms
/// above.
fn record_intrinsic_callee(p: &Program, e: &mut Effects, callee: &Callee) {
    match callee {
        Callee::Builtin(name) if name == "close" => {
            e.chan_ops.insert(ChanOp::Close);
        }
        Callee::Static(f) => {
            if let Some(kind) = lock_kind(p.func_name(*f)) {
                e.lock_ops.insert(lock_op(kind));
            }
        }
        _ => {}
    }
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
        let e = collect(&p, p.lookup_func("t.F").unwrap(), &[]);
        assert!(
            e.chan_ops.contains(&ChanOp::Close),
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
        let e = collect(&p, p.lookup_func("t.F").unwrap(), &[]);
        assert!(
            e.chan_ops.contains(&ChanOp::Close),
            "go close(ch) must record ChanOp::Close: {e:?}"
        );
        assert_eq!(e.spawns, Spawns::Bounded);
    }

    /// Regression (final-review C2): `defer mu.Unlock()` — a `Defer`
    /// whose `Callee::Static` names one of the four `sync.Mutex`/
    /// `sync.RWMutex` lock-intrinsic methods — must record the matching
    /// `LockOp`, the same way a plain (non-deferred) call to that method
    /// would via `Op::Lock`.
    #[test]
    fn defer_lock_method_records_lockop() {
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
        let e = collect(&p, p.lookup_func("t.F").unwrap(), &[]);
        assert!(
            e.lock_ops.contains(&LockOp::Unlock),
            "defer (*sync.Mutex).Unlock() must record LockOp::Unlock: {e:?}"
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
        let e = collect(&p, p.lookup_func("t.F").unwrap(), &[]);
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
        let e = collect(&p, p.lookup_func("t.F").unwrap(), &[]);
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
        let mut callee = Effects::empty();
        callee.lock_ops.insert(LockOp::Lock);
        let e = collect(&p, p.lookup_func("t.F").unwrap(), &[&callee]);
        assert!(e.lock_ops.contains(&LockOp::Lock));
    }
}
