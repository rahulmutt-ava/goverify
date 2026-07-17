//! Global type table: structured Go types interned across packages by
//! canonical repr string. Per-package .gvir type ids are local; importing
//! a package returns the local→global mapping.

use std::collections::HashMap;

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
        let max_local = types.iter().map(|t| t.id).max().unwrap_or(0) as usize;
        let mut map = vec![unknown; max_local + 1];
        // Pass 1: intern all reprs so cycles resolve.
        for t in types {
            if t.id != 0 {
                map[t.id as usize] = self.intern(&t.repr);
            }
        }
        // Pass 2: translate kinds. First writer for a repr wins; identical
        // sources produce identical structures, so later writers agree.
        for t in types {
            if t.id == 0 {
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
}
