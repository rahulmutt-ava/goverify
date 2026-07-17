//! Pins the sync-intrinsic name table (final-review deferred T7): the
//! RWMutex branches were previously untested.

use goverify_ir::{LockKind, lock_kind};

#[test]
fn all_sync_lock_methods_map() {
    for (name, want) in [
        ("(*sync.Mutex).Lock", LockKind::Lock),
        ("(*sync.Mutex).Unlock", LockKind::Unlock),
        ("(*sync.RWMutex).Lock", LockKind::Lock),
        ("(*sync.RWMutex).Unlock", LockKind::Unlock),
        ("(*sync.RWMutex).RLock", LockKind::RLock),
        ("(*sync.RWMutex).RUnlock", LockKind::RUnlock),
    ] {
        assert_eq!(lock_kind(name), Some(want), "lock_kind({name})");
    }
}

#[test]
fn non_lock_names_do_not_map() {
    for name in ["fmt.Println", "(*sync.WaitGroup).Wait", "", "Lock"] {
        assert_eq!(lock_kind(name), None, "lock_kind({name})");
    }
}
