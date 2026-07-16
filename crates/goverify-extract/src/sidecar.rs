use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A built extractor binary, addressed by the content hash of its
/// sources: the same sources always reuse the same binary, and any
/// source change forces a rebuild (spec §3: the extractor is itself
/// content-hashed).
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
        let hash = hash_dir(extractor_src)?;
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

/// Content hash of a source directory: blake3 over (relative path,
/// length, bytes) of every non-hidden file, in sorted path order.
fn hash_dir(dir: &Path) -> Result<String, SidecarError> {
    let mut files = Vec::new();
    collect_files(dir, dir, &mut files)?;
    files.sort();
    let mut hasher = blake3::Hasher::new();
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

    #[test]
    fn hash_dir_is_stable_and_content_sensitive() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.go"), "package a\n").unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/b.go"), "package sub\n").unwrap();

        let h1 = hash_dir(dir.path()).unwrap();
        let h2 = hash_dir(dir.path()).unwrap();
        assert_eq!(h1, h2, "hash_dir must be deterministic");

        std::fs::write(dir.path().join("a.go"), "package a // changed\n").unwrap();
        assert_ne!(
            h1,
            hash_dir(dir.path()).unwrap(),
            "content change must change the hash"
        );
    }

    #[test]
    fn hash_dir_ignores_hidden_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.go"), "package a\n").unwrap();
        let h1 = hash_dir(dir.path()).unwrap();
        std::fs::write(dir.path().join(".DS_Store"), "junk").unwrap();
        assert_eq!(h1, hash_dir(dir.path()).unwrap());
    }
}
