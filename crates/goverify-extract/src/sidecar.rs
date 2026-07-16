use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A built extractor binary, addressed by the content hash of its
/// sources folded with the Go toolchain version: the same sources
/// built by the same toolchain always reuse the same binary, and any
/// source change or toolchain upgrade forces a rebuild (spec §3: the
/// extraction cache key is `hash(source files ⊕ Go version ⊕ extractor
/// version)`).
pub struct Sidecar {
    bin: PathBuf,
}

#[derive(Debug)]
pub enum SidecarError {
    Io(io::Error),
    GoBuild(String),
    Extractor(String),
}

impl fmt::Display for SidecarError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SidecarError::Io(e) => write!(f, "sidecar io: {e}"),
            SidecarError::GoBuild(stderr) => {
                write!(f, "building extractor (is `go` installed?): {stderr}")
            }
            SidecarError::Extractor(stderr) => write!(f, "extractor failed: {stderr}"),
        }
    }
}

impl std::error::Error for SidecarError {}

impl From<io::Error> for SidecarError {
    fn from(e: io::Error) -> Self {
        SidecarError::Io(e)
    }
}

impl Sidecar {
    pub fn build(extractor_src: &Path, build_dir: &Path) -> Result<Sidecar, SidecarError> {
        let go_version = go_version()?;
        let hash = cache_key(extractor_src, &go_version)?;
        let bin = build_dir.join(format!("goverify-extractor-{}", &hash[..16]));
        if !bin.exists() {
            fs::create_dir_all(build_dir)?;
            // Build to a temp name, then rename: concurrent builders race
            // benignly to an identical artifact, across both processes AND
            // threads. std::process::id() alone isn't enough — cargo test
            // runs multiple Sidecar::build calls as threads within one
            // process, so they'd share a pid and collide on the same tmp
            // path; add a per-thread/per-call counter to disambiguate.
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let tmp = build_dir.join(format!(
                "goverify-extractor-{}.tmp{}.{n}",
                &hash[..16],
                std::process::id()
            ));
            let output = Command::new("go")
                .args(["build", "-trimpath", "-o"])
                .arg(&tmp)
                .arg(".")
                .env("CGO_ENABLED", "0")
                .current_dir(extractor_src)
                .output()?;
            if !output.status.success() {
                return Err(SidecarError::GoBuild(
                    String::from_utf8_lossy(&output.stderr).into_owned(),
                ));
            }
            fs::rename(&tmp, &bin)?;
        }
        Ok(Sidecar { bin })
    }

    pub fn extract(
        &self,
        module_dir: &Path,
        patterns: &[&str],
        out_dir: &Path,
    ) -> Result<Vec<PathBuf>, SidecarError> {
        fs::create_dir_all(out_dir)?;
        let out_abs = out_dir.canonicalize()?;
        let output = Command::new(&self.bin)
            .arg("-out")
            .arg(&out_abs)
            .args(patterns)
            .current_dir(module_dir)
            .output()?;
        if !output.status.success() {
            return Err(SidecarError::Extractor(
                String::from_utf8_lossy(&output.stderr).into_owned(),
            ));
        }
        // Forward the extractor's degrade diagnostics ("goverify: skipping
        // <pkg>: <err>") to our stderr; otherwise they vanish even though
        // extraction succeeded (spec §11: degrade, never die — silently).
        if !output.stderr.is_empty() {
            eprint!("{}", String::from_utf8_lossy(&output.stderr));
        }
        let mut files: Vec<PathBuf> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(PathBuf::from)
            .collect();
        files.sort();
        Ok(files)
    }
}

/// Resolves the Go toolchain version via `go env GOVERSION` (output like
/// `go1.25.1`), trimmed of surrounding whitespace. Called at most once
/// per `Sidecar::build`. A failure to spawn `go` surfaces as
/// `SidecarError::Io` via `?`; a non-zero exit or empty output is
/// reported as `SidecarError::GoBuild`, the same error class used when
/// `go` is missing or broken elsewhere in this file.
fn go_version() -> Result<String, SidecarError> {
    let output = Command::new("go").args(["env", "GOVERSION"]).output()?;
    if !output.status.success() {
        return Err(SidecarError::GoBuild(
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ));
    }
    let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if version.is_empty() {
        return Err(SidecarError::GoBuild(
            "`go env GOVERSION` printed no output".to_string(),
        ));
    }
    Ok(version)
}

/// The sidecar binary's cache key: blake3 over the Go toolchain version
/// (domain-separated from the file entries below by a distinct tag and
/// its own length prefix, so it can never collide with file content or
/// path bytes) followed by (relative path, length, bytes) of every
/// non-hidden file in `dir`, in sorted path order.
///
/// Takes `go_version` as a parameter rather than resolving it internally
/// so the hashing logic stays unit-testable without invoking `go`.
fn cache_key(dir: &Path, go_version: &str) -> Result<String, SidecarError> {
    let mut files = Vec::new();
    collect_files(dir, dir, &mut files)?;
    files.sort();
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"goverify-sidecar-cache-key/go-version\0");
    hasher.update(&(go_version.len() as u64).to_le_bytes());
    hasher.update(go_version.as_bytes());
    for rel in &files {
        hasher.update(rel.as_bytes());
        let bytes = fs::read(dir.join(rel))?;
        hasher.update(&(bytes.len() as u64).to_le_bytes());
        hasher.update(&bytes);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

fn collect_files(root: &Path, dir: &Path, out: &mut Vec<String>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            collect_files(root, &path, out)?;
        } else {
            out.push(
                path.strip_prefix(root)
                    .expect("path is under root")
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const GO_VERSION: &str = "go1.25.1";

    #[test]
    fn cache_key_is_stable_and_content_sensitive() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.go"), "package a\n").unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/b.go"), "package sub\n").unwrap();

        let h1 = cache_key(dir.path(), GO_VERSION).unwrap();
        let h2 = cache_key(dir.path(), GO_VERSION).unwrap();
        assert_eq!(h1, h2, "cache_key must be deterministic");

        std::fs::write(dir.path().join("a.go"), "package a // changed\n").unwrap();
        assert_ne!(
            h1,
            cache_key(dir.path(), GO_VERSION).unwrap(),
            "content change must change the hash"
        );
    }

    #[test]
    fn cache_key_ignores_hidden_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.go"), "package a\n").unwrap();
        let h1 = cache_key(dir.path(), GO_VERSION).unwrap();
        std::fs::write(dir.path().join(".DS_Store"), "junk").unwrap();
        assert_eq!(h1, cache_key(dir.path(), GO_VERSION).unwrap());
    }

    #[test]
    fn cache_key_is_stable_across_calls_with_same_go_version() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.go"), "package a\n").unwrap();

        let h1 = cache_key(dir.path(), GO_VERSION).unwrap();
        let h2 = cache_key(dir.path(), GO_VERSION).unwrap();
        assert_eq!(
            h1, h2,
            "same directory and same Go version must produce identical keys"
        );
    }

    #[test]
    fn cache_key_changes_with_go_version() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.go"), "package a\n").unwrap();

        let h1 = cache_key(dir.path(), "go1.25.1").unwrap();
        let h2 = cache_key(dir.path(), "go1.26.0").unwrap();
        assert_ne!(
            h1, h2,
            "same directory with a different Go version must produce a different key"
        );
    }
}
