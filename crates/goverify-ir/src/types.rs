//! Global type table: structured Go types interned across packages by
//! canonical repr string. Per-package .gvir type ids are local; importing
//! a package returns the local→global mapping.

use std::collections::{HashMap, HashSet};

use goverify_extract::gvir;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TypeId(pub u32);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldInfo {
    pub name: String,
    pub ty: TypeId,
    pub embedded: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeKind {
    Basic {
        name: String,
    },
    Named {
        name: String,
        underlying: TypeId,
    },
    Pointer {
        elem: TypeId,
    },
    Slice {
        elem: TypeId,
    },
    Array {
        elem: TypeId,
        len: u64,
    },
    Map {
        key: TypeId,
        value: TypeId,
    },
    Chan {
        elem: TypeId,
        dir: u32,
    },
    Struct {
        fields: Vec<FieldInfo>,
    },
    Interface,
    Signature {
        params: Vec<TypeId>,
        results: Vec<TypeId>,
        variadic: bool,
    },
    Tuple {
        elems: Vec<TypeId>,
    },
    TypeParam,
    Unknown,
}

#[derive(Debug, Default)]
pub struct TypeTable {
    by_repr: HashMap<String, TypeId>,
    reprs: Vec<String>,
    kinds: Vec<TypeKind>,
}

impl TypeTable {
    pub fn kind(&self, id: TypeId) -> &TypeKind {
        self.kinds.get(id.0 as usize).unwrap_or(&TypeKind::Unknown)
    }

    pub fn repr(&self, id: TypeId) -> &str {
        self.reprs.get(id.0 as usize).map_or("<unknown>", |s| s)
    }

    /// The shared Unknown type (index 0 is reserved for it).
    pub fn unknown(&mut self) -> TypeId {
        self.intern("<unknown>")
    }

    fn intern(&mut self, repr: &str) -> TypeId {
        if let Some(&id) = self.by_repr.get(repr) {
            return id;
        }
        let id = TypeId(self.reprs.len() as u32);
        self.by_repr.insert(repr.to_string(), id);
        self.reprs.push(repr.to_string());
        self.kinds.push(TypeKind::Unknown);
        id
    }

    /// Import one package's type list; returns local-id → global TypeId.
    /// Index 0 of the returned map is the Unknown type (local id 0 means
    /// "absent" in .gvir). Malformed component references degrade to
    /// Unknown — never panic (fuzzed input).
    pub fn import_package(&mut self, types: &[gvir::Type]) -> Vec<TypeId> {
        let unknown = self.unknown();
        // Cap the map size at types.len() to prevent allocation bombs from
        // untrusted ids. Legitimate extractor output assigns ids densely 1..=N,
        // so any id > types.len() is malformed and degrades to Unknown.
        let cap = types.len();
        let mut map = vec![unknown; cap + 1];
        // Pass 1: intern all reprs so cycles resolve.
        for t in types {
            if t.id != 0 && (t.id as usize) <= cap {
                map[t.id as usize] = self.intern(&t.repr);
            }
        }
        // Pass 2: translate kinds. First writer for a repr wins; identical
        // sources produce identical structures, so later writers agree.
        for t in types {
            if t.id == 0 || (t.id as usize) > cap {
                continue;
            }
            let gid = map[t.id as usize];
            if !matches!(self.kinds[gid.0 as usize], TypeKind::Unknown) {
                continue; // already populated by an earlier package
            }
            let r = |local: u32| -> TypeId { map.get(local as usize).copied().unwrap_or(unknown) };
            let kind = match gvir::TypeKind::try_from(t.kind).unwrap_or(gvir::TypeKind::Unspecified)
            {
                gvir::TypeKind::Basic => TypeKind::Basic {
                    name: t.name.clone(),
                },
                gvir::TypeKind::Named => TypeKind::Named {
                    name: t.name.clone(),
                    underlying: r(t.elem),
                },
                gvir::TypeKind::Pointer => TypeKind::Pointer { elem: r(t.elem) },
                gvir::TypeKind::Slice => TypeKind::Slice { elem: r(t.elem) },
                gvir::TypeKind::Array => TypeKind::Array {
                    elem: r(t.elem),
                    len: t.array_len,
                },
                gvir::TypeKind::Map => TypeKind::Map {
                    key: r(t.key),
                    value: r(t.elem),
                },
                gvir::TypeKind::Chan => TypeKind::Chan {
                    elem: r(t.elem),
                    dir: t.chan_dir,
                },
                gvir::TypeKind::Struct => TypeKind::Struct {
                    fields: t
                        .fields
                        .iter()
                        .map(|f| FieldInfo {
                            name: f.name.clone(),
                            ty: r(f.r#type),
                            embedded: f.embedded,
                        })
                        .collect(),
                },
                gvir::TypeKind::Interface => TypeKind::Interface,
                gvir::TypeKind::Signature => TypeKind::Signature {
                    params: t.params.iter().map(|&p| r(p)).collect(),
                    results: t.results.iter().map(|&p| r(p)).collect(),
                    variadic: t.variadic,
                },
                gvir::TypeKind::Tuple => TypeKind::Tuple {
                    elems: t.params.iter().map(|&p| r(p)).collect(),
                },
                gvir::TypeKind::TypeParam => TypeKind::TypeParam,
                gvir::TypeKind::Unspecified => TypeKind::Unknown,
            };
            self.kinds[gid.0 as usize] = kind;
        }
        map
    }
}

/// Name-free structural key for a `TypeId` (final-review C1): two
/// `TypeId`s that are textually distinct only because of parameter/result
/// *names* on a `Signature` (or on any type nested inside one) — e.g. the
/// interface method `Write(p []byte) (n int, err error)` vs the
/// implementer's `Write(b []byte) (int, error)` — must still compare
/// equal here, because Go signature identity never includes parameter or
/// result names. `emit.go`'s `typeID` interns by the *full* canonical
/// repr string (`types.TypeString`), which does include those names, so
/// two structurally-identical signatures land on different `TypeId`s;
/// this key recovers the structural equivalence by walking each type's
/// `TypeKind` components (which are already name-free `TypeId`s, or in
/// the one case that legitimately carries semantic names — `Named`'s own
/// name, and `Struct`'s field names, both part of real Go type identity —
/// preserved verbatim) instead of ever re-reading a `repr` string.
///
/// Over-merging structurally-identical-but-distinct types is the safe
/// direction (it can only widen a call-graph edge set, never narrow it);
/// under-matching is not, so this must never fall back to per-repr
/// comparison.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum StructuralKey {
    Basic(String),
    Named(String, Box<StructuralKey>),
    Pointer(Box<StructuralKey>),
    Slice(Box<StructuralKey>),
    Array(Box<StructuralKey>, u64),
    Map(Box<StructuralKey>, Box<StructuralKey>),
    Chan(Box<StructuralKey>, u32),
    Struct(Vec<(String, StructuralKey, bool)>),
    Interface,
    Signature(Vec<StructuralKey>, Vec<StructuralKey>, bool),
    Tuple(Vec<StructuralKey>),
    TypeParam,
    Unknown,
    /// A cycle was detected on the current recursion path. Legitimate
    /// recursive Go types reach this too (`type node struct{ next *node }`
    /// routes Named→Struct→Pointer→Named), not just malformed/fuzzed
    /// `.gvir` — either way the walk must degrade rather than recurse
    /// forever. The enclosing `Named` key's name still anchors identity,
    /// so distinct recursive types keep distinct keys; types that collapse
    /// to the same key over-merge — the safe direction. Cached keys inside
    /// a cycle are entry-point-dependent and deterministic only because
    /// `CallGraph::build`'s traversal order is fixed; do not "optimize"
    /// the on-path check away on the premise that cycles are illegitimate.
    Cyclic,
}

/// Memoizing computer for `StructuralKey`s, shared across the many lookups
/// a single `CallGraph::build` performs (one whole-program computation, not
/// re-derived from scratch per call site).
#[derive(Debug, Default)]
pub struct StructuralKeyCache {
    cache: HashMap<TypeId, StructuralKey>,
}

impl StructuralKeyCache {
    pub fn new() -> StructuralKeyCache {
        StructuralKeyCache::default()
    }

    pub fn key(&mut self, types: &TypeTable, id: TypeId) -> StructuralKey {
        let mut on_path: HashSet<TypeId> = HashSet::new();
        Self::compute(types, id, &mut self.cache, &mut on_path)
    }

    /// `on_path` tracks `TypeId`s currently being expanded on *this*
    /// recursion path (not "ever visited") — a proper cycle detector, as
    /// opposed to a depth cap, that never mistakes a DAG's legitimate
    /// diamond-shaped sharing (the same component type reached twice via
    /// different branches, which is common and must key identically) for
    /// a cycle. `cache` is keyed by plain `TypeId` and shared across the
    /// whole `StructuralKeyCache`, so repeated sub-structure is computed
    /// once regardless of how many times it's reached.
    fn compute(
        types: &TypeTable,
        id: TypeId,
        cache: &mut HashMap<TypeId, StructuralKey>,
        on_path: &mut HashSet<TypeId>,
    ) -> StructuralKey {
        if let Some(k) = cache.get(&id) {
            return k.clone();
        }
        if !on_path.insert(id) {
            return StructuralKey::Cyclic; // not cached: path-dependent
        }
        let key = match types.kind(id) {
            TypeKind::Basic { name } => StructuralKey::Basic(name.clone()),
            TypeKind::Named { name, underlying } => StructuralKey::Named(
                name.clone(),
                Box::new(Self::compute(types, *underlying, cache, on_path)),
            ),
            TypeKind::Pointer { elem } => {
                StructuralKey::Pointer(Box::new(Self::compute(types, *elem, cache, on_path)))
            }
            TypeKind::Slice { elem } => {
                StructuralKey::Slice(Box::new(Self::compute(types, *elem, cache, on_path)))
            }
            TypeKind::Array { elem, len } => {
                StructuralKey::Array(Box::new(Self::compute(types, *elem, cache, on_path)), *len)
            }
            TypeKind::Map { key, value } => StructuralKey::Map(
                Box::new(Self::compute(types, *key, cache, on_path)),
                Box::new(Self::compute(types, *value, cache, on_path)),
            ),
            TypeKind::Chan { elem, dir } => {
                StructuralKey::Chan(Box::new(Self::compute(types, *elem, cache, on_path)), *dir)
            }
            TypeKind::Struct { fields } => StructuralKey::Struct(
                fields
                    .iter()
                    .map(|f| {
                        (
                            f.name.clone(),
                            Self::compute(types, f.ty, cache, on_path),
                            f.embedded,
                        )
                    })
                    .collect(),
            ),
            TypeKind::Interface => StructuralKey::Interface,
            TypeKind::Signature {
                params,
                results,
                variadic,
            } => StructuralKey::Signature(
                params
                    .iter()
                    .map(|&t| Self::compute(types, t, cache, on_path))
                    .collect(),
                results
                    .iter()
                    .map(|&t| Self::compute(types, t, cache, on_path))
                    .collect(),
                *variadic,
            ),
            TypeKind::Tuple { elems } => StructuralKey::Tuple(
                elems
                    .iter()
                    .map(|&t| Self::compute(types, t, cache, on_path))
                    .collect(),
            ),
            TypeKind::TypeParam => StructuralKey::TypeParam,
            TypeKind::Unknown => StructuralKey::Unknown,
        };
        on_path.remove(&id);
        cache.insert(id, key.clone());
        key
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interns_across_packages_by_repr() {
        use goverify_extract::gvir;
        let mk = |id, repr: &str, kind| gvir::Type {
            id,
            repr: repr.into(),
            kind: kind as i32,
            ..Default::default()
        };
        // Two packages both describe `*int`, with different local ids.
        let pkg_a = vec![
            mk(1, "int", gvir::TypeKind::Basic),
            gvir::Type {
                id: 2,
                repr: "*int".into(),
                kind: gvir::TypeKind::Pointer as i32,
                elem: 1,
                ..Default::default()
            },
        ];
        let pkg_b = vec![
            gvir::Type {
                id: 1,
                repr: "*int".into(),
                kind: gvir::TypeKind::Pointer as i32,
                elem: 2,
                ..Default::default()
            },
            mk(2, "int", gvir::TypeKind::Basic),
        ];
        let mut table = TypeTable::default();
        let map_a = table.import_package(&pkg_a);
        let map_b = table.import_package(&pkg_b);
        assert_eq!(map_a[2], map_b[1], "*int must intern to one global id");
        let TypeKind::Pointer { elem } = *table.kind(map_b[1]) else {
            panic!("expected pointer kind");
        };
        assert_eq!(table.repr(elem), "int");
    }

    #[test]
    fn out_of_range_component_degrades_to_unknown() {
        use goverify_extract::gvir;
        let pkg = vec![gvir::Type {
            id: 1,
            repr: "*bad".into(),
            kind: gvir::TypeKind::Pointer as i32,
            elem: 99,
            ..Default::default()
        }];
        let mut table = TypeTable::default();
        let map = table.import_package(&pkg); // must not panic
        let TypeKind::Pointer { elem } = *table.kind(map[1]) else {
            panic!()
        };
        assert!(matches!(table.kind(elem), TypeKind::Unknown));
    }

    #[test]
    fn huge_id_avoids_allocation_bomb() {
        use goverify_extract::gvir;
        let mk = |id, repr: &str, kind| gvir::Type {
            id,
            repr: repr.into(),
            kind: kind as i32,
            ..Default::default()
        };
        // Package with u32::MAX id and a valid sibling type.
        // Allocation should cap at types.len(), not allocate ~17 GB.
        let pkg = vec![
            mk(1, "int", gvir::TypeKind::Basic),
            gvir::Type {
                id: u32::MAX,
                repr: "overflow".into(),
                kind: gvir::TypeKind::Basic as i32,
                ..Default::default()
            },
        ];
        let mut table = TypeTable::default();
        let map = table.import_package(&pkg); // must not panic or hang
        // Valid type 1 is accessible
        assert!(matches!(table.kind(map[1]), TypeKind::Basic { .. }));
        // Overflow id (out of range) degrades to Unknown when accessed
        let overflow_id = u32::MAX as usize;
        if overflow_id < map.len() {
            assert!(matches!(table.kind(map[overflow_id]), TypeKind::Unknown));
        }
    }

    /// Regression (final-review C1): two `Signature` `TypeId`s interned
    /// from different top-level reprs — as `emit.go`'s `typeID` would for
    /// `func(p []byte) (n int, err error)` vs `func(b []byte) (int,
    /// error)` — but with identical structural components (same
    /// param/result component `TypeId`s) must produce the same
    /// `StructuralKey`.
    #[test]
    fn structural_key_ignores_signature_param_names() {
        use goverify_extract::gvir;
        let pkg = vec![
            gvir::Type {
                id: 1,
                repr: "[]byte".into(),
                kind: gvir::TypeKind::Slice as i32,
                elem: 2,
                ..Default::default()
            },
            gvir::Type {
                id: 2,
                repr: "byte".into(),
                kind: gvir::TypeKind::Basic as i32,
                name: "byte".into(),
                ..Default::default()
            },
            gvir::Type {
                id: 3,
                repr: "int".into(),
                kind: gvir::TypeKind::Basic as i32,
                name: "int".into(),
                ..Default::default()
            },
            gvir::Type {
                id: 4,
                repr: "error".into(),
                kind: gvir::TypeKind::Basic as i32,
                name: "error".into(),
                ..Default::default()
            },
            gvir::Type {
                id: 5,
                repr: "func(p []byte) (n int, err error)".into(),
                kind: gvir::TypeKind::Signature as i32,
                params: vec![1],
                results: vec![3, 4],
                ..Default::default()
            },
            gvir::Type {
                id: 6,
                repr: "func(b []byte) (int, error)".into(),
                kind: gvir::TypeKind::Signature as i32,
                params: vec![1],
                results: vec![3, 4],
                ..Default::default()
            },
        ];
        let mut table = TypeTable::default();
        let map = table.import_package(&pkg);
        assert_ne!(
            map[5], map[6],
            "the two reprs must intern to different TypeIds (that's the bug's precondition)"
        );
        let mut keys = StructuralKeyCache::new();
        assert_eq!(
            keys.key(&table, map[5]),
            keys.key(&table, map[6]),
            "signatures differing only in param/result names must key identically"
        );
    }

    #[test]
    fn structural_key_distinguishes_genuinely_different_signatures() {
        use goverify_extract::gvir;
        let pkg = vec![
            gvir::Type {
                id: 1,
                repr: "int".into(),
                kind: gvir::TypeKind::Basic as i32,
                name: "int".into(),
                ..Default::default()
            },
            gvir::Type {
                id: 2,
                repr: "string".into(),
                kind: gvir::TypeKind::Basic as i32,
                name: "string".into(),
                ..Default::default()
            },
            gvir::Type {
                id: 3,
                repr: "func(int) int".into(),
                kind: gvir::TypeKind::Signature as i32,
                params: vec![1],
                results: vec![1],
                ..Default::default()
            },
            gvir::Type {
                id: 4,
                repr: "func(string) int".into(),
                kind: gvir::TypeKind::Signature as i32,
                params: vec![2],
                results: vec![1],
                ..Default::default()
            },
        ];
        let mut table = TypeTable::default();
        let map = table.import_package(&pkg);
        let mut keys = StructuralKeyCache::new();
        assert_ne!(
            keys.key(&table, map[3]),
            keys.key(&table, map[4]),
            "genuinely different param types must not collapse to the same key"
        );
    }

    /// A self-referential `Named` type (malformed/fuzzed `.gvir` — never
    /// legitimate Go) must degrade to `StructuralKey::Cyclic`, not recurse
    /// forever.
    #[test]
    fn structural_key_handles_cyclic_type_without_hanging() {
        use goverify_extract::gvir;
        let pkg = vec![gvir::Type {
            id: 1,
            repr: "t.Self".into(),
            kind: gvir::TypeKind::Named as i32,
            name: "t.Self".into(),
            elem: 1, // self-referential
            ..Default::default()
        }];
        let mut table = TypeTable::default();
        let map = table.import_package(&pkg);
        let mut keys = StructuralKeyCache::new();
        let k = keys.key(&table, map[1]); // must return, not hang
        assert_eq!(
            k,
            StructuralKey::Named("t.Self".into(), Box::new(StructuralKey::Cyclic))
        );
    }
}
