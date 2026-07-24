//! Extraction-cache orchestration (phase-5a spec §3): manifest ->
//! recursive import-DAG keys -> store hits + dirty-set extraction.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use goverify_cache::Store;

use crate::gvir;
use crate::load::load_package_bytes;
use crate::sidecar::{ManifestPkg, Sidecar, SidecarError};

/// Bump on any change to the key preimage or stored-value semantics.
/// v2: the per-package file set now includes the module's go.mod (its
/// `go` directive changes emitted SSA per-module), and the sidecar
/// `content_key` now folds `GOOS`/`GOARCH`/`GOEXPERIMENT` — both new
/// preimage inputs.
const EXTRACT_CACHE_VERSION: u32 = 2;
const LAYER: &str = "extract";

pub struct ExtractStats {
    pub cached: usize,
    pub extracted: usize,
}

fn file_hash(path: &Path) -> std::io::Result<[u8; 32]> {
    Ok(*blake3::Hasher::new()
        .update(&std::fs::read(path)?)
        .finalize()
        .as_bytes())
}

/// Recursive package keys over the manifest's import DAG (memoized
/// DFS). Missing deps or cycles are errors -> caller falls back.
///
/// Key preimage: `blake3("goverify-extract-key\0" ⊕
/// EXTRACT_CACHE_VERSION(u32-LE) ⊕ lp(content_key) ⊕ lp(import_path) ⊕
/// per file, sorted: lp(file-content-blake3) ⊕ per dep, sorted:
/// dep-key)`, where `lp` is a u64-LE length prefix followed by the
/// bytes. `content_key` already covers extractor sources and the Go
/// toolchain version (sidecar.rs), so both key components ride in. File
/// PATHS are never key material — only their content hashes — since
/// paths are machine-specific absolutes.
fn package_keys(
    sc_key: &str,
    pkgs: &[ManifestPkg],
) -> Result<HashMap<String, [u8; 32]>, SidecarError> {
    let by_path: HashMap<&str, &ManifestPkg> =
        pkgs.iter().map(|p| (p.import_path.as_str(), p)).collect();
    let mut keys: HashMap<String, [u8; 32]> = HashMap::new();
    // Memoized DFS with an explicit on-stack `visiting` marker (import
    // graphs are acyclic in valid Go; a crafted cycle must degrade, not
    // recurse/panic).
    fn key_of<'a>(
        path: &'a str,
        sc_key: &str,
        by_path: &HashMap<&str, &'a ManifestPkg>,
        keys: &mut HashMap<String, [u8; 32]>,
        visiting: &mut Vec<&'a str>,
    ) -> Result<[u8; 32], SidecarError> {
        if let Some(k) = keys.get(path) {
            return Ok(*k);
        }
        if visiting.contains(&path) {
            return Err(SidecarError::Extractor(format!(
                "manifest: import cycle through {path}"
            )));
        }
        let pkg = by_path
            .get(path)
            .ok_or_else(|| SidecarError::Extractor(format!("manifest: missing dep {path}")))?;
        visiting.push(path);
        let mut dep_keys: Vec<[u8; 32]> = Vec::with_capacity(pkg.deps.len());
        for d in &pkg.deps {
            dep_keys.push(key_of(d, sc_key, by_path, keys, visiting)?);
        }
        visiting.pop();
        dep_keys.sort_unstable();
        let mut h = blake3::Hasher::new();
        h.update(b"goverify-extract-key\0");
        h.update(&EXTRACT_CACHE_VERSION.to_le_bytes());
        let mut field = |b: &[u8]| {
            h.update(&(b.len() as u64).to_le_bytes());
            h.update(b);
        };
        field(sc_key.as_bytes());
        field(path.as_bytes());
        // Files are already sorted by the manifest; hash CONTENT only
        // (paths are machine-specific absolutes — never key material).
        for f in &pkg.files {
            let fh = file_hash(f).map_err(SidecarError::Io)?;
            h.update(&(fh.len() as u64).to_le_bytes());
            h.update(&fh);
        }
        for dk in &dep_keys {
            h.update(dk);
        }
        let k = *h.finalize().as_bytes();
        keys.insert(path.to_string(), k);
        Ok(k)
    }
    for p in pkgs {
        let mut visiting = Vec::new();
        key_of(&p.import_path, sc_key, &by_path, &mut keys, &mut visiting)?;
    }
    Ok(keys)
}

/// Full pipeline: manifest -> recursive keys -> store hits + dirty-set
/// extraction -> decoded packages, sorted by import path. Any manifest/
/// key-computation failure is an Err — the caller falls back to plain
/// uncached extraction (degrade, never die).
pub fn load_packages_cached(
    sc: &Sidecar,
    module_dir: &Path,
    patterns: &[&str],
    cache_root: &Path,
) -> Result<(Vec<gvir::Package>, ExtractStats), SidecarError> {
    let manifest = sc.manifest(module_dir, patterns)?;
    let keys = package_keys(sc.content_key(), &manifest)?;
    let store = Store::open(cache_root.to_path_buf());

    let mut packages: Vec<gvir::Package> = Vec::with_capacity(manifest.len());
    let mut dirty: Vec<&str> = Vec::new();
    let mut stats = ExtractStats {
        cached: 0,
        extracted: 0,
    };
    for p in &manifest {
        let key = &keys[&p.import_path];
        match store
            .get(LAYER, key)
            .and_then(|b| load_package_bytes(&b).ok())
        {
            Some(pkg) => {
                stats.cached += 1;
                packages.push(pkg);
            }
            None => dirty.push(&p.import_path),
        }
    }
    if !dirty.is_empty() {
        // Consumed as fresh artifacts are matched to their requested
        // import path below: a `remove` that returns `false` catches
        // both "the extractor emitted a package we never asked for"
        // and "the extractor emitted the same package twice" in one
        // check, since either way the path is no longer in the set the
        // second time it's seen.
        let mut dirty_set: HashSet<&str> = dirty.iter().copied().collect();
        let out = tempfile::tempdir().map_err(SidecarError::Io)?;
        let files = sc.extract_only(module_dir, &dirty, out.path())?;
        for f in &files {
            let bytes = std::fs::read(f).map_err(SidecarError::Io)?;
            let pkg = match load_package_bytes(&bytes) {
                Ok(pkg) => pkg,
                Err(e) => {
                    // Skip WITH a diagnostic (spec §11: degrade, never
                    // die — silently is not the same as silently
                    // dropping a manifest entry without a trace).
                    eprintln!(
                        "goverify: extract cache: skipping undecodable artifact {}: {e}",
                        f.display()
                    );
                    continue;
                }
            };
            if !dirty_set.remove(pkg.import_path.as_str()) {
                eprintln!(
                    "goverify: extract cache: skipping unexpected package {} (not in the requested dirty set, or a duplicate)",
                    pkg.import_path
                );
                continue;
            }
            if let Some(key) = keys.get(&pkg.import_path) {
                // Write failure degrades to slower, never wrong.
                let _ = store.put(LAYER, key, &bytes);
            }
            stats.extracted += 1;
            packages.push(pkg);
        }
    }
    packages.sort_by(|a, b| a.import_path.cmp(&b.import_path));
    Ok((packages, stats))
}
