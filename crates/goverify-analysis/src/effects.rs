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
