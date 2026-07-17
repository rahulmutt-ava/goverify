//! Whole-DAG program: all loaded packages, functions interned by their
//! stable ssa id string, sorted for determinism.

use std::collections::HashMap;
use std::path::Path;

use goverify_extract::{gvir, load_package};

use crate::func::Function;
use crate::types::TypeTable;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FuncId(pub u32);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MethodInfo {
    pub name: String,
    pub sig: crate::types::TypeId,
    pub func: Option<FuncId>, // None = abstract (interface) method
}

#[derive(Debug, Default)]
pub struct Program {
    types: TypeTable,
    func_names: Vec<String>, // FuncId → ssa id string
    by_name: HashMap<String, FuncId>,
    funcs: Vec<Option<Function>>, // FuncId → lowered body (None = external)
    /// Method sets of named types, keyed by the type's global TypeId,
    /// sorted entries. Used by Task 9's invoke resolution.
    pub method_sets: std::collections::BTreeMap<crate::types::TypeId, Vec<MethodInfo>>,
    diagnostics: Vec<String>,
}

impl Program {
    /// Build from decoded packages. Infallible: malformed content degrades
    /// to diagnostics + havoc (fuzz target decodes arbitrary bytes into
    /// packages and calls this).
    pub fn from_packages(mut pkgs: Vec<gvir::Package>) -> Program {
        // Deterministic global order regardless of input order.
        pkgs.sort_by(|a, b| a.import_path.cmp(&b.import_path));
        let mut p = Program::default();
        // Pass 1: intern every function name (sorted per package already;
        // sort globally for FuncId stability).
        let mut names: Vec<&str> = pkgs
            .iter()
            .flat_map(|pkg| pkg.functions.iter().map(|f| f.id.as_str()))
            .collect();
        names.sort_unstable();
        names.dedup();
        for n in names {
            p.intern_func(n);
        }
        // Pass 2: types, method sets, bodies (bodies land in Task 6).
        for pkg in &pkgs {
            let tmap = p.types.import_package(&pkg.types);
            p.import_method_sets(pkg, &tmap);
            p.lower_package(pkg, &tmap);
        }
        p
    }

    pub fn load_dir(dir: &Path) -> std::io::Result<Program> {
        let mut pkgs = Vec::new();
        let mut diags = Vec::new();
        let mut entries: Vec<_> = std::fs::read_dir(dir)?
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "gvir"))
            .collect();
        entries.sort();
        for path in entries {
            match load_package(&path) {
                Ok(pkg) => pkgs.push(pkg),
                Err(e) => diags.push(format!("skipping {}: {e}", path.display())),
            }
        }
        let mut p = Program::from_packages(pkgs);
        p.diagnostics.splice(0..0, diags);
        Ok(p)
    }

    pub(crate) fn intern_func(&mut self, name: &str) -> FuncId {
        if let Some(&id) = self.by_name.get(name) {
            return id;
        }
        let id = FuncId(self.func_names.len() as u32);
        self.by_name.insert(name.to_string(), id);
        self.func_names.push(name.to_string());
        self.funcs.push(None);
        id
    }

    fn import_method_sets(&mut self, pkg: &gvir::Package, tmap: &[crate::types::TypeId]) {
        for ms in &pkg.method_sets {
            let Some(&ty) = tmap.get(ms.r#type as usize) else {
                continue;
            };
            if self.method_sets.contains_key(&ty) {
                continue; // same named type seen from another package
            }
            let mut methods = Vec::new();
            for m in &ms.methods {
                let func = (!m.func_id.is_empty()).then(|| self.intern_func(&m.func_id));
                let sig = tmap
                    .get(m.sig as usize)
                    .copied()
                    .unwrap_or_else(|| self.types.unknown());
                methods.push(MethodInfo {
                    name: m.name.clone(),
                    sig,
                    func,
                });
            }
            self.method_sets.insert(ty, methods);
        }
    }

    pub fn func_ids(&self) -> impl Iterator<Item = FuncId> + '_ {
        (0..self.func_names.len() as u32).map(FuncId)
    }

    pub fn func(&self, id: FuncId) -> Option<&Function> {
        self.funcs.get(id.0 as usize).and_then(Option::as_ref)
    }

    pub fn func_name(&self, id: FuncId) -> &str {
        self.func_names
            .get(id.0 as usize)
            .map_or("<unknown>", |s| s)
    }

    pub fn lookup_func(&self, name: &str) -> Option<FuncId> {
        self.by_name.get(name).copied()
    }

    pub fn types(&self) -> &TypeTable {
        &self.types
    }

    pub fn diagnostics(&self) -> &[String] {
        &self.diagnostics
    }

    pub(crate) fn push_diagnostic(&mut self, d: String) {
        self.diagnostics.push(d);
    }

    /// The shared Unknown type. Exposed to `lower.rs` (a sibling module)
    /// without making the `types` field itself `pub(crate)`.
    pub(crate) fn types_unknown(&mut self) -> crate::types::TypeId {
        self.types.unknown()
    }

    /// Install a lowered body for a previously-interned function. Bounds
    /// checked even though `id` always comes from `intern_func` (and is
    /// therefore always in range) — cheap insurance against a future
    /// caller passing a stray id.
    pub(crate) fn set_func_body(&mut self, id: FuncId, f: Function) {
        if let Some(slot) = self.funcs.get_mut(id.0 as usize) {
            *slot = Some(f);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn func_ids_stable_under_package_order() {
        use goverify_extract::gvir;
        let f = |id: &str| gvir::Function {
            id: id.into(),
            ..Default::default()
        };
        let pkg = |path: &str, fs: Vec<gvir::Function>| gvir::Package {
            import_path: path.into(),
            functions: fs,
            ..Default::default()
        };
        let a = || pkg("a", vec![f("a.F"), f("a.G")]);
        let b = || pkg("b", vec![f("b.H")]);
        let p1 = Program::from_packages(vec![a(), b()]);
        let p2 = Program::from_packages(vec![b(), a()]);
        for name in ["a.F", "a.G", "b.H"] {
            assert_eq!(p1.lookup_func(name), p2.lookup_func(name), "{name}");
        }
        // Verify func_ids() yields them in ascending order
        let ids1: Vec<_> = p1.func_ids().collect();
        let ids2: Vec<_> = p2.func_ids().collect();
        assert_eq!(ids1, ids2);
    }

    #[test]
    fn load_dir_skips_malformed_and_ignores_non_gvir() {
        use std::fs;
        use std::io::Write;

        let dir = tempfile::tempdir().unwrap();
        let dir_path = dir.path();

        // Create a malformed .gvir file (garbage bytes)
        let malformed_path = dir_path.join("malformed.gvir");
        let mut f = fs::File::create(&malformed_path).unwrap();
        f.write_all(&[0xffu8; 64]).unwrap();
        drop(f);

        // Create a non-.gvir file (should be ignored)
        let non_gvir_path = dir_path.join("readme.txt");
        fs::write(&non_gvir_path, "not a gvir file").unwrap();

        // Load the directory
        let result = Program::load_dir(dir_path);
        assert!(result.is_ok(), "load_dir must not fail on malformed files");

        let p = result.unwrap();
        // The Program should exist but be empty (no valid packages loaded)
        assert_eq!(p.func_ids().count(), 0);

        // Diagnostics should mention the malformed file and appear first
        let diags = p.diagnostics();
        assert_eq!(
            diags.len(),
            1,
            "exactly one diagnostic (malformed.gvir); readme.txt must be \
             extension-filtered, not diagnosed: {diags:?}"
        );
        assert!(
            diags[0].contains("malformed.gvir"),
            "first diagnostic should mention malformed.gvir, got: {:?}",
            diags[0]
        );
    }
}
