//! Filesystem content-addressed store: `<root>/<layer>/<hh>/<hex>`.
//! Writes are tempfile+rename (atomic); reads treat every failure as a
//! miss; an advisory exclusive lock per layer serializes writers from
//! concurrent runs (parent spec §9).

use std::fs;
use std::io;
use std::path::PathBuf;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

#[derive(Debug)]
pub struct Store {
    root: PathBuf,
}

fn hex(key: &[u8; 32]) -> String {
    key.iter().map(|b| format!("{b:02x}")).collect()
}

impl Store {
    /// Creates the root directory with 0700 permissions (best-effort; any
    /// failure is ignored). Never fails — read-only consumers of a
    /// nonexistent cache just miss.
    pub fn open(root: PathBuf) -> Store {
        // Best-effort: create root and set 0o700 permissions on Unix.
        let _ = fs::create_dir_all(&root);
        #[cfg(unix)]
        {
            let _ = fs::set_permissions(&root, fs::Permissions::from_mode(0o700));
        }
        Store { root }
    }

    fn entry_path(&self, layer: &str, key: &[u8; 32]) -> PathBuf {
        let h = hex(key);
        self.root.join(layer).join(&h[..2]).join(h)
    }

    /// Any failure — missing, unreadable, permission — is a miss.
    pub fn get(&self, layer: &str, key: &[u8; 32]) -> Option<Vec<u8>> {
        fs::read(self.entry_path(layer, key)).ok()
    }

    pub fn put(&self, layer: &str, key: &[u8; 32], value: &[u8]) -> io::Result<()> {
        let layer_dir = self.root.join(layer);
        let dest = self.entry_path(layer, key);
        fs::create_dir_all(dest.parent().expect("entry path has parent"))?;
        // Advisory lock (spec §7): serializes concurrent runs' writes.
        let lock_path = self.root.join(format!("{layer}.lock"));
        let lock = fs::File::create(&lock_path)?;
        lock.lock()?;
        let tmp = layer_dir.join(format!("tmp-{}-{}", &hex(key)[..8], std::process::id()));
        // Clean up tmp file on failure (best-effort).
        if let Err(e) = fs::write(&tmp, value) {
            let _ = fs::remove_file(&tmp);
            let _ = lock.unlock();
            return Err(e);
        }
        let renamed = fs::rename(&tmp, &dest);
        let _ = lock.unlock();
        if renamed.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        renamed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_then_get_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path().to_path_buf());
        let key = [7u8; 32];
        s.put("query", &key, b"hello").unwrap();
        assert_eq!(s.get("query", &key), Some(b"hello".to_vec()));
    }

    #[test]
    fn missing_key_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path().to_path_buf());
        assert_eq!(s.get("query", &[0u8; 32]), None);
    }

    #[test]
    fn layers_are_disjoint() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path().to_path_buf());
        let key = [1u8; 32];
        s.put("query", &key, b"q").unwrap();
        assert_eq!(s.get("summary", &key), None);
    }

    #[test]
    fn concurrent_puts_same_key_are_safe() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path().to_path_buf());
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    let s2 = Store::open(dir.path().to_path_buf());
                    s2.put("query", &[9u8; 32], b"same-bytes").unwrap();
                });
            }
        });
        assert_eq!(s.get("query", &[9u8; 32]), Some(b"same-bytes".to_vec()));
    }

    #[test]
    #[cfg(unix)]
    fn root_created_with_0700_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let root_path = dir.path().to_path_buf();
        let _s = Store::open(root_path.clone());
        let metadata = fs::metadata(&root_path).unwrap();
        let mode = metadata.permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "root directory should have 0o700 permissions");
    }
}
