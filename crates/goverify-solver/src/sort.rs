//! SMT sorts (phase-3 spec §3): quantifier-free, four theories.

/// A sort. `Datatype` names a declared algebraic datatype (v1 ships only
/// `Ptr`, but the type is general).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Sort {
    Bool,
    BitVec(u32),
    Array(Box<Sort>, Box<Sort>),
    Datatype(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CtorDecl {
    pub name: String,
    /// (accessor name, field sort) pairs.
    pub fields: Vec<(String, Sort)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatatypeDecl {
    pub name: String,
    pub ctors: Vec<CtorDecl>,
}

impl DatatypeDecl {
    pub fn sort(&self) -> Sort {
        Sort::Datatype(self.name.clone())
    }

    pub fn ctor(&self, name: &str) -> Option<&CtorDecl> {
        self.ctors.iter().find(|c| c.name == name)
    }
}

/// Ill-sorted construction. Analyzer-internal: callers degrade to
/// "no obligation" (never a finding) on Err.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SortError(pub String);

impl std::fmt::Display for SortError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "sort error: {}", self.0)
    }
}

impl std::error::Error for SortError {}

/// The one datatype v1 needs: pointers as nil | 64-bit address.
pub fn ptr_datatype() -> DatatypeDecl {
    DatatypeDecl {
        name: "Ptr".into(),
        ctors: vec![
            CtorDecl {
                name: "ptr-nil".into(),
                fields: vec![],
            },
            CtorDecl {
                name: "ptr-addr".into(),
                fields: vec![("ptr-addr-val".into(), Sort::BitVec(64))],
            },
        ],
    }
}

pub fn ptr_sort() -> Sort {
    Sort::Datatype("Ptr".into())
}
