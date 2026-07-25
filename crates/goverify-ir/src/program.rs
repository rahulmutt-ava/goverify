//! Whole-DAG program: all loaded packages, functions interned by their
//! stable ssa id string, sorted for determinism.

use std::collections::HashMap;
use std::path::Path;

use goverify_extract::{gvir, load_package};
use prost::Message;

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
    func_hashes: Vec<[u8; 32]>,   // FuncId → content hash (see func_ir_hash)
    func_sem_hashes: Vec<[u8; 32]>, // FuncId → position-blind hash (see func_semantic_hash)
    /// Method sets of named types, keyed by the type's global TypeId,
    /// sorted entries. Used by Task 9's invoke resolution.
    pub method_sets: std::collections::BTreeMap<crate::types::TypeId, Vec<MethodInfo>>,
    diagnostics: Vec<String>,
}

/// blake3 hash over a package's non-function sections: `schema_version`,
/// `go_version`, `extractor_version`, `import_path`, the re-encoded
/// `types`/`method_sets`/`pragmas` tables, and each file's `path` (NOT
/// its `sha256` — see below). Folded into every function hash in the
/// package because a function's own encoding indexes into the package's
/// type table — a types/method-sets change must invalidate every
/// function in the package even though their own message bytes are
/// unchanged.
///
/// Files are keyed by path (and path order), not by `gvir::File.sha256`
/// (proto/gvir/v1/gvir.proto), deliberately: a `Position.file` index and
/// a printed finding's path are both about *which file*, so a path
/// rename/reorder is context that must invalidate. But file *content*
/// is not extra context here — every content change that affects
/// analysis already surfaces in some hash this function separately
/// folds in (the edited functions' own re-encoded messages, including
/// their positions; `types`; `method_sets`; `pragmas`; or the synthetic
/// `init` function for global initializers). Hashing `sha256` here would
/// make every function in a package (including comment-only edits)
/// invalidate on any byte in any file changing — the wave's G3
/// acceptance gate ("edit one function → exactly its SCC + callers
/// re-analyze") requires exactly this to not happen.
fn ctx_hash(pkg: &gvir::Package) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(b"goverify-func-ctx\0");
    let mut field = |bytes: &[u8]| {
        h.update(&(bytes.len() as u64).to_le_bytes());
        h.update(bytes);
    };
    field(pkg.schema_version.as_bytes());
    field(pkg.go_version.as_bytes());
    field(pkg.extractor_version.as_bytes());
    field(pkg.import_path.as_bytes());
    for t in &pkg.types {
        field(&t.encode_to_vec());
    }
    for m in &pkg.method_sets {
        field(&m.encode_to_vec());
    }
    for f in &pkg.files {
        field(f.path.as_bytes());
    }
    for pr in &pkg.pragmas {
        field(&pr.encode_to_vec());
    }
    *h.finalize().as_bytes()
}

/// blake3 hash of a function that is present as an entry in some
/// package's `pkg.functions` (whether or not that entry has a body —
/// `blocks` may be empty for a declared-but-bodyless function): the
/// domain tag, its package's `ctx_hash`, and the length-prefixed
/// `encode_to_vec()` of its own `gvir::Function` message. Function
/// messages embed their positions, so a line shift invalidates exactly
/// the shifted functions. Only a function that never appears as an
/// entry in any package (purely referenced, e.g. from a call site or
/// method set) falls through to `external_hash` instead.
fn func_hash(ctx: &[u8; 32], f: &gvir::Function) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(b"goverify-func-ir\0");
    h.update(ctx);
    let bytes = f.encode_to_vec();
    h.update(&(bytes.len() as u64).to_le_bytes());
    h.update(&bytes);
    *h.finalize().as_bytes()
}

/// blake3 hash of an interned-but-absent (external) function: the domain
/// tag plus its length-prefixed name only. Externals are havoc; this
/// only pins identity.
fn external_hash(name: &str) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(b"goverify-func-ext\0");
    h.update(&(name.len() as u64).to_le_bytes());
    h.update(name.as_bytes());
    *h.finalize().as_bytes()
}

/// Position-blind context hash (phase-5b spec §5): `ctx_hash` with
/// `Pragma.pos` cleared — a comment shift above a pragma must not mark
/// every function in the package as changed for `--diff-base`.
fn semantic_ctx_hash(pkg: &gvir::Package) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(b"goverify-func-semctx\0");
    let mut field = |bytes: &[u8]| {
        h.update(&(bytes.len() as u64).to_le_bytes());
        h.update(bytes);
    };
    field(pkg.schema_version.as_bytes());
    field(pkg.go_version.as_bytes());
    field(pkg.extractor_version.as_bytes());
    field(pkg.import_path.as_bytes());
    for t in &pkg.types {
        field(&t.encode_to_vec());
    }
    for m in &pkg.method_sets {
        field(&m.encode_to_vec());
    }
    for f in &pkg.files {
        field(f.path.as_bytes());
    }
    for pr in &pkg.pragmas {
        let mut pr = pr.clone();
        pr.pos = None;
        field(&pr.encode_to_vec());
    }
    *h.finalize().as_bytes()
}

/// Position-blind sibling of `func_hash` (phase-5b spec §5): the
/// function re-encoded with `Function.pos`, every `Instruction.pos`,
/// and every `Instruction.detail` cleared. `--diff-base` compares these
/// across git refs, so a comment-only edit (positions shift, semantics
/// don't) yields an empty changed set (gate G4). `detail` is debug-only
/// prose (gvir.proto) and is dropped for the same reason.
/// NEVER a cache-key input: cache keys stay position-sensitive
/// (`func_hash`) so warm replays render exact positions.
fn semantic_func_hash(ctx: &[u8; 32], f: &gvir::Function) -> [u8; 32] {
    let mut g = f.clone();
    g.pos = None;
    for b in &mut g.blocks {
        for i in &mut b.instrs {
            i.pos = None;
            i.detail = String::new();
        }
    }
    let mut h = blake3::Hasher::new();
    h.update(b"goverify-func-sem\0");
    h.update(ctx);
    let bytes = g.encode_to_vec();
    h.update(&(bytes.len() as u64).to_le_bytes());
    h.update(&bytes);
    *h.finalize().as_bytes()
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
        // Pass 2: types, method sets, bodies. This can lazily intern
        // further FuncIds (method-set entries and call/aux references
        // whose target is never declared as a `gvir::Function` in any
        // loaded package), so the hash table below is only sized once
        // all interning from this pass is done.
        for pkg in &pkgs {
            let tmap = p.types.import_package(&pkg.types);
            p.import_method_sets(pkg, &tmap);
            p.lower_package(pkg, &tmap);
        }
        // Pass 3: per-function content hashes (Task 7's SCC cache key
        // input). Every interned name defaults to a name-only external
        // hash; functions with a `gvir::Function` entry in some package
        // are then overwritten with a package-context + own-IR hash.
        //
        // Duplicate ids across packages must track `lower_package`'s
        // winner (lower.rs): a bodyless entry (`blocks` empty) is
        // skipped there — it never calls `set_func_body` and so can
        // never clobber an already-lowered body — while a bodied entry
        // always overwrites. Mirror that here with a `bodied` flag per
        // FuncId: a bodied entry always overwrites the hash (and wins
        // permanently, matching the body table); a bodyless entry only
        // fills the hash in if no bodied entry has won yet.
        p.func_hashes = p.func_names.iter().map(|n| external_hash(n)).collect();
        p.func_sem_hashes = p.func_hashes.clone(); // externals: same name-only hash
        let mut bodied = vec![false; p.func_names.len()];
        for pkg in &pkgs {
            let ctx = ctx_hash(pkg);
            let sem_ctx = semantic_ctx_hash(pkg);
            for f in &pkg.functions {
                if let Some(&id) = p.by_name.get(f.id.as_str()) {
                    let idx = id.0 as usize;
                    if f.blocks.is_empty() {
                        if !bodied[idx] {
                            p.func_hashes[idx] = func_hash(&ctx, f);
                            p.func_sem_hashes[idx] = semantic_func_hash(&sem_ctx, f);
                        }
                    } else {
                        p.func_hashes[idx] = func_hash(&ctx, f);
                        p.func_sem_hashes[idx] = semantic_func_hash(&sem_ctx, f);
                        bodied[idx] = true;
                    }
                }
            }
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

    /// Stable content hash of this function's IR + its package context
    /// (types/method-sets/pragmas, and files' paths — not file content;
    /// see `ctx_hash`'s doc comment). See phase-5a spec §2: this is the
    /// member-hash input to the SCC cache key. Externals hash their name
    /// only.
    ///
    /// INVARIANT (SCC-cache soundness): dropping file content from the
    /// package context is sound only while checkers treat globals and
    /// externals as identity-only (no checker reads a global's literal
    /// initializer value or an external's link target). A future checker
    /// that derives facts from global VALUES needs an invalidation edge
    /// the call-graph SCC key cannot express — revisit this hash and
    /// `Root::Global` (goverify-analysis effects.rs) together.
    pub fn func_ir_hash(&self, id: FuncId) -> [u8; 32] {
        self.func_hashes
            .get(id.0 as usize)
            .copied()
            .unwrap_or([0u8; 32])
    }

    /// Position-blind sibling of `func_ir_hash` (phase-5b spec §5) —
    /// `--diff-base`'s changed-function comparator. Not a cache key.
    pub fn func_semantic_hash(&self, id: FuncId) -> [u8; 32] {
        self.func_sem_hashes
            .get(id.0 as usize)
            .copied()
            .unwrap_or([0u8; 32])
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

    #[test]
    fn func_ir_hashes_are_stable_and_content_sensitive() {
        // Build two identical single-function packages via the same
        // constructor the fuzz seeds use (fuzz_seeds.rs pattern), then a
        // third with a mutated function body position.
        fn pkg(line: u32) -> goverify_extract::gvir::Package {
            use goverify_extract::gvir;
            gvir::Package {
                schema_version: goverify_extract::SCHEMA_VERSION.to_string(),
                go_version: "go1.25.10".to_string(),
                extractor_version: "0.1.0".to_string(),
                import_path: "example.com/h".to_string(),
                files: vec![],
                types: vec![],
                functions: vec![gvir::Function {
                    id: "example.com/h.F".to_string(),
                    name: "F".to_string(),
                    r#type: 0,
                    params: vec![],
                    aux: vec![],
                    blocks: vec![],
                    pos: Some(gvir::Position {
                        file: 0,
                        line,
                        col: 1,
                    }),
                    result_names: vec![],
                }],
                method_sets: vec![],
                pragmas: vec![],
            }
        }
        let p1 = Program::from_packages(vec![pkg(1)]);
        let p2 = Program::from_packages(vec![pkg(1)]);
        let p3 = Program::from_packages(vec![pkg(2)]);
        let f = p1.lookup_func("example.com/h.F").expect("lookup_func");
        assert_eq!(
            p1.func_ir_hash(f),
            p2.func_ir_hash(f),
            "identical packages hash identically"
        );
        assert_ne!(
            p1.func_ir_hash(f),
            p3.func_ir_hash(f),
            "a position change must change the hash"
        );
    }

    #[test]
    fn ctx_hash_keys_file_path_not_content() {
        // G3 acceptance gate: a source-file byte edit (which changes its
        // `sha256` but not its `path`) must not invalidate functions
        // whose own IR is unchanged. A path change (rename/reorder) is
        // real context — `Position.file` and printed findings reference
        // files by path — and must invalidate.
        fn pkg(path: &str, sha256: &str) -> goverify_extract::gvir::Package {
            use goverify_extract::gvir;
            gvir::Package {
                schema_version: goverify_extract::SCHEMA_VERSION.to_string(),
                go_version: "go1.25.10".to_string(),
                extractor_version: "0.1.0".to_string(),
                import_path: "example.com/h".to_string(),
                files: vec![gvir::File {
                    path: path.to_string(),
                    sha256: sha256.to_string(),
                }],
                types: vec![],
                functions: vec![gvir::Function {
                    id: "example.com/h.F".to_string(),
                    name: "F".to_string(),
                    r#type: 0,
                    params: vec![],
                    aux: vec![],
                    blocks: vec![],
                    pos: Some(gvir::Position {
                        file: 1,
                        line: 1,
                        col: 1,
                    }),
                    result_names: vec![],
                }],
                method_sets: vec![],
                pragmas: vec![],
            }
        }
        let same_sha = Program::from_packages(vec![pkg("h.go", "aaaa")]);
        let edited_content = Program::from_packages(vec![pkg("h.go", "bbbb")]);
        let renamed = Program::from_packages(vec![pkg("h2.go", "aaaa")]);

        let f1 = same_sha
            .lookup_func("example.com/h.F")
            .expect("lookup_func same_sha");
        let f2 = edited_content
            .lookup_func("example.com/h.F")
            .expect("lookup_func edited_content");
        let f3 = renamed
            .lookup_func("example.com/h.F")
            .expect("lookup_func renamed");

        assert_eq!(
            same_sha.func_ir_hash(f1),
            edited_content.func_ir_hash(f2),
            "a file content edit (sha256 change only) must not change \
             func_ir_hash (G3: unchanged functions must not invalidate)"
        );
        assert_ne!(
            same_sha.func_ir_hash(f1),
            renamed.func_ir_hash(f3),
            "a file path change is real context and must change func_ir_hash"
        );
    }

    #[test]
    fn duplicate_id_hash_follows_lowerings_bodied_winner() {
        // A duplicate function id spanning two packages: `a` (sorts
        // first) declares it with a real body, `b` redeclares it as a
        // bodyless stub. `lower_package` skips bodyless entries (they
        // never call `set_func_body`, so they can't clobber an
        // already-lowered body) — the hash winner must follow that same
        // rule, not just "last package processed".
        use goverify_extract::gvir;
        fn bodied(import_path: &str, line: u32) -> gvir::Package {
            gvir::Package {
                import_path: import_path.into(),
                functions: vec![gvir::Function {
                    id: "dup.F".into(),
                    blocks: vec![gvir::BasicBlock {
                        index: 0,
                        instrs: vec![gvir::Instruction {
                            kind: "Return".into(),
                            ..Default::default()
                        }],
                        ..Default::default()
                    }],
                    pos: Some(gvir::Position {
                        file: 0,
                        line,
                        col: 1,
                    }),
                    ..Default::default()
                }],
                ..Default::default()
            }
        }
        fn stub(import_path: &str, line: u32) -> gvir::Package {
            gvir::Package {
                import_path: import_path.into(),
                functions: vec![gvir::Function {
                    id: "dup.F".into(),
                    blocks: vec![],
                    pos: Some(gvir::Position {
                        file: 0,
                        line,
                        col: 1,
                    }),
                    ..Default::default()
                }],
                ..Default::default()
            }
        }

        let a_only = Program::from_packages(vec![bodied("a", 1)]);
        let combined = Program::from_packages(vec![bodied("a", 1), stub("b", 1)]);
        let stub_moved = Program::from_packages(vec![bodied("a", 1), stub("b", 2)]);
        let body_moved = Program::from_packages(vec![bodied("a", 2), stub("b", 1)]);

        let fa = a_only.lookup_func("dup.F").expect("lookup_func a_only");
        let fc = combined.lookup_func("dup.F").expect("lookup_func combined");
        let fs = stub_moved
            .lookup_func("dup.F")
            .expect("lookup_func stub_moved");
        let fb = body_moved
            .lookup_func("dup.F")
            .expect("lookup_func body_moved");

        assert_eq!(
            a_only.func_ir_hash(fa),
            combined.func_ir_hash(fc),
            "package a's bodied entry must win the hash over b's stub, \
             matching lower_package's body winner"
        );
        assert_eq!(
            combined.func_ir_hash(fc),
            stub_moved.func_ir_hash(fs),
            "moving the losing stub's position must not change the hash \
             once a bodied entry has won"
        );
        assert_ne!(
            combined.func_ir_hash(fc),
            body_moved.func_ir_hash(fb),
            "moving the winning bodied entry's position must change the \
             hash"
        );
    }

    #[test]
    fn semantic_hash_ignores_positions_and_detail_but_not_semantics() {
        use goverify_extract::gvir;
        fn pkg(line: u32, op: &str, detail: &str) -> gvir::Package {
            gvir::Package {
                schema_version: goverify_extract::SCHEMA_VERSION.to_string(),
                go_version: "go1.25.10".to_string(),
                extractor_version: "0.1.0".to_string(),
                import_path: "example.com/h".to_string(),
                files: vec![],
                types: vec![],
                functions: vec![gvir::Function {
                    id: "example.com/h.F".to_string(),
                    name: "F".to_string(),
                    r#type: 0,
                    params: vec![],
                    aux: vec![],
                    blocks: vec![gvir::BasicBlock {
                        index: 0,
                        instrs: vec![gvir::Instruction {
                            kind: "BinOp".to_string(),
                            register: 1,
                            r#type: 0,
                            operands: vec![],
                            pos: Some(gvir::Position {
                                file: 0,
                                line,
                                col: 1,
                            }),
                            detail: detail.to_string(),
                            sem: Some(gvir::instruction::Sem::Binop(gvir::BinOpSem {
                                op: op.to_string(),
                            })),
                        }],
                        succs: vec![],
                        preds: vec![],
                    }],
                    pos: Some(gvir::Position {
                        file: 0,
                        line,
                        col: 1,
                    }),
                    result_names: vec![],
                }],
                method_sets: vec![],
                pragmas: vec![],
            }
        }
        let base = Program::from_packages(vec![pkg(1, "+", "a + b")]);
        let shifted = Program::from_packages(vec![pkg(50, "+", "a + b")]);
        let redetailed = Program::from_packages(vec![pkg(1, "+", "x + y")]);
        let changed = Program::from_packages(vec![pkg(1, "*", "a + b")]);
        let f = base.lookup_func("example.com/h.F").expect("lookup_func");

        // Position shift: ir hash moves (asserted invariant), semantic
        // hash must NOT (G4: comment-only edits are diff-invisible).
        assert_ne!(base.func_ir_hash(f), shifted.func_ir_hash(f));
        assert_eq!(
            base.func_semantic_hash(f),
            shifted.func_semantic_hash(f),
            "positions must not reach the semantic hash"
        );
        // detail is debug-only prose: also excluded.
        assert_eq!(
            base.func_semantic_hash(f),
            redetailed.func_semantic_hash(f),
            "Instruction.detail must not reach the semantic hash"
        );
        // A real semantic change moves both.
        assert_ne!(
            base.func_semantic_hash(f),
            changed.func_semantic_hash(f),
            "operator change is a semantic change"
        );
    }

    #[test]
    fn semantic_hash_of_externals_pins_identity() {
        // A function only referenced, never declared (an `aux` call-site
        // reference to an undeclared id): externals hash by name only,
        // so two identically-built Programs must agree even though
        // `example.com/h.Ext` never appears as a `gvir::Function` entry.
        use goverify_extract::gvir;
        fn pkg() -> gvir::Package {
            gvir::Package {
                schema_version: goverify_extract::SCHEMA_VERSION.to_string(),
                go_version: "go1.25.10".to_string(),
                extractor_version: "0.1.0".to_string(),
                import_path: "example.com/h".to_string(),
                files: vec![],
                types: vec![],
                functions: vec![gvir::Function {
                    id: "example.com/h.F".to_string(),
                    name: "F".to_string(),
                    r#type: 0,
                    params: vec![],
                    aux: vec![gvir::AuxValue {
                        id: 1,
                        kind: "Function".to_string(),
                        repr: "example.com/h.Ext".to_string(),
                        r#type: 0,
                        r#const: None,
                    }],
                    blocks: vec![gvir::BasicBlock {
                        index: 0,
                        instrs: vec![gvir::Instruction {
                            kind: "Call".to_string(),
                            register: 1,
                            r#type: 0,
                            operands: vec![1],
                            pos: None,
                            detail: String::new(),
                            sem: Some(gvir::instruction::Sem::Call(gvir::CallSem {
                                static_callee: "example.com/h.Ext".to_string(),
                                method: String::new(),
                                iface_type: 0,
                                invoke: false,
                                builtin: String::new(),
                                method_sig: 0,
                            })),
                        }],
                        succs: vec![],
                        preds: vec![],
                    }],
                    pos: None,
                    result_names: vec![],
                }],
                method_sets: vec![],
                pragmas: vec![],
            }
        }
        // `lower_package` must actually intern the callee for this test to
        // exercise the external path; if it doesn't, `lookup_func` below
        // returns None and the test fails loudly rather than vacuously
        // passing.
        let p1 = Program::from_packages(vec![pkg()]);
        let p2 = Program::from_packages(vec![pkg()]);
        let ext = p1
            .lookup_func("example.com/h.Ext")
            .expect("callee must be interned as an external");
        assert_eq!(
            p1.func_semantic_hash(ext),
            p2.func_semantic_hash(ext),
            "external semantic hash is name-only and must be stable across \
             identically-built Programs"
        );
    }
}
