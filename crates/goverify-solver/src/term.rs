//! Typed first-order terms (phase-3 spec §3). Immutable AST; every Term
//! carries its Sort; ill-sorted construction is unrepresentable through
//! the public API (constructors return Err). The ONLY lowering to
//! SMT-LIB2 is printer.rs (single-lowering rule, spec §4).

use std::collections::BTreeMap;

use crate::sort::{CtorDecl, DatatypeDecl, Sort, SortError, ptr_datatype};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BvBinOp {
    Add,
    Sub,
    Mul,
    Udiv,
    Sdiv,
    Urem,
    Srem,
    And,
    Or,
    Xor,
    Shl,
    Lshr,
    Ashr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BvCmpOp {
    Ult,
    Ule,
    Slt,
    Sle,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Node {
    BoolLit(bool),
    BvLit {
        width: u32,
        value: u128,
    },
    Var(String),
    Not(Box<Term>),
    And(Vec<Term>),
    Or(Vec<Term>),
    Implies(Box<Term>, Box<Term>),
    Eq(Box<Term>, Box<Term>),
    Ite(Box<Term>, Box<Term>, Box<Term>),
    BvBin {
        op: BvBinOp,
        lhs: Box<Term>,
        rhs: Box<Term>,
    },
    BvCmp {
        op: BvCmpOp,
        lhs: Box<Term>,
        rhs: Box<Term>,
    },
    Select(Box<Term>, Box<Term>),
    Store(Box<Term>, Box<Term>, Box<Term>),
    /// Constructor application; dt/ctor names resolved at build time.
    DtCtor {
        dt: String,
        ctor: String,
        args: Vec<Term>,
    },
    /// `(_ is <ctor>)` tester.
    DtIs {
        ctor: String,
        arg: Box<Term>,
    },
    /// Field accessor.
    DtGet {
        field: String,
        arg: Box<Term>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Term {
    pub(crate) node: Node,
    sort: Sort,
}

fn err(msg: impl Into<String>) -> SortError {
    SortError(msg.into())
}

/// Bare SMT-LIB2 simple symbol — keeps the printer quoting-free.
fn valid_symbol(s: &str) -> bool {
    const EXTRA: &[char] = &[
        '~', '!', '@', '$', '%', '^', '&', '*', '_', '-', '+', '=', '<', '>', '.', '?', '/',
    ];
    let ok = |c: char| c.is_ascii_alphanumeric() || EXTRA.contains(&c);
    !s.is_empty() && !s.starts_with(|c: char| c.is_ascii_digit()) && s.chars().all(ok)
}

impl Term {
    pub fn sort(&self) -> &Sort {
        &self.sort
    }

    pub fn bool_lit(b: bool) -> Term {
        Term {
            node: Node::BoolLit(b),
            sort: Sort::Bool,
        }
    }

    /// `value` must fit in `width` bits (analyzer-internal invariant).
    pub fn bv_lit(width: u32, value: u128) -> Term {
        assert!((1..=128).contains(&width), "bv_lit width {width}");
        assert!(
            width == 128 || value < (1u128 << width),
            "bv_lit: {value} does not fit in {width} bits"
        );
        Term {
            node: Node::BvLit { width, value },
            sort: Sort::BitVec(width),
        }
    }

    /// `name` must be a bare SMT-LIB2 symbol (analyzer-internal invariant;
    /// callers only ever pass p<i>/r<i>-shaped names).
    pub fn var(name: &str, sort: Sort) -> Term {
        assert!(valid_symbol(name), "invalid SMT symbol: {name:?}");
        Term {
            node: Node::Var(name.to_string()),
            sort,
        }
    }

    /// Named `not` (not `std::ops::Not`) to match the SMT-LIB2 connective;
    /// signature is a cross-task contract (phase-3 spec §3).
    #[allow(clippy::should_implement_trait)]
    pub fn not(t: Term) -> Result<Term, SortError> {
        if t.sort != Sort::Bool {
            return Err(err(format!("not: expected Bool, got {:?}", t.sort)));
        }
        Ok(Term {
            node: Node::Not(Box::new(t)),
            sort: Sort::Bool,
        })
    }

    pub fn and(ts: Vec<Term>) -> Result<Term, SortError> {
        Self::nary("and", ts, Node::And)
    }

    pub fn or(ts: Vec<Term>) -> Result<Term, SortError> {
        Self::nary("or", ts, Node::Or)
    }

    fn nary(what: &str, ts: Vec<Term>, mk: fn(Vec<Term>) -> Node) -> Result<Term, SortError> {
        if ts.is_empty() {
            return Err(err(format!("{what}: empty operand list")));
        }
        if let Some(t) = ts.iter().find(|t| t.sort != Sort::Bool) {
            return Err(err(format!("{what}: expected Bool, got {:?}", t.sort)));
        }
        Ok(Term {
            node: mk(ts),
            sort: Sort::Bool,
        })
    }

    pub fn implies(a: Term, b: Term) -> Result<Term, SortError> {
        if a.sort != Sort::Bool || b.sort != Sort::Bool {
            return Err(err("implies: both operands must be Bool"));
        }
        Ok(Term {
            node: Node::Implies(Box::new(a), Box::new(b)),
            sort: Sort::Bool,
        })
    }

    pub fn eq(a: Term, b: Term) -> Result<Term, SortError> {
        if a.sort != b.sort {
            return Err(err(format!("eq: {:?} vs {:?}", a.sort, b.sort)));
        }
        Ok(Term {
            node: Node::Eq(Box::new(a), Box::new(b)),
            sort: Sort::Bool,
        })
    }

    pub fn ite(c: Term, t: Term, e: Term) -> Result<Term, SortError> {
        if c.sort != Sort::Bool {
            return Err(err("ite: condition must be Bool"));
        }
        if t.sort != e.sort {
            return Err(err(format!(
                "ite: branch sorts {:?} vs {:?}",
                t.sort, e.sort
            )));
        }
        let sort = t.sort.clone();
        Ok(Term {
            node: Node::Ite(Box::new(c), Box::new(t), Box::new(e)),
            sort,
        })
    }

    pub fn bv_bin(op: BvBinOp, lhs: Term, rhs: Term) -> Result<Term, SortError> {
        let sort = Self::same_bv(&lhs, &rhs)?;
        Ok(Term {
            node: Node::BvBin {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            },
            sort,
        })
    }

    pub fn bv_cmp(op: BvCmpOp, lhs: Term, rhs: Term) -> Result<Term, SortError> {
        Self::same_bv(&lhs, &rhs)?;
        Ok(Term {
            node: Node::BvCmp {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            },
            sort: Sort::Bool,
        })
    }

    fn same_bv(lhs: &Term, rhs: &Term) -> Result<Sort, SortError> {
        match (&lhs.sort, &rhs.sort) {
            (Sort::BitVec(a), Sort::BitVec(b)) if a == b => Ok(lhs.sort.clone()),
            _ => Err(err(format!("bitvec op: {:?} vs {:?}", lhs.sort, rhs.sort))),
        }
    }

    pub fn select(arr: Term, idx: Term) -> Result<Term, SortError> {
        let Sort::Array(k, v) = arr.sort.clone() else {
            return Err(err(format!("select: expected Array, got {:?}", arr.sort)));
        };
        if *k != idx.sort {
            return Err(err(format!("select: index {:?} vs key {:?}", idx.sort, k)));
        }
        Ok(Term {
            node: Node::Select(Box::new(arr), Box::new(idx)),
            sort: *v,
        })
    }

    pub fn store(arr: Term, idx: Term, val: Term) -> Result<Term, SortError> {
        let Sort::Array(k, v) = arr.sort.clone() else {
            return Err(err(format!("store: expected Array, got {:?}", arr.sort)));
        };
        if *k != idx.sort || *v != val.sort {
            return Err(err("store: index/value sort mismatch"));
        }
        let sort = arr.sort.clone();
        Ok(Term {
            node: Node::Store(Box::new(arr), Box::new(idx), Box::new(val)),
            sort,
        })
    }

    fn resolve_ctor<'a>(dt: &'a DatatypeDecl, ctor: &str) -> Result<&'a CtorDecl, SortError> {
        dt.ctor(ctor)
            .ok_or_else(|| err(format!("datatype {}: no constructor {ctor}", dt.name)))
    }

    pub fn dt_ctor(dt: &DatatypeDecl, ctor: &str, args: Vec<Term>) -> Result<Term, SortError> {
        let c = Self::resolve_ctor(dt, ctor)?;
        if c.fields.len() != args.len() {
            return Err(err(format!(
                "{ctor}: arity {} vs {}",
                c.fields.len(),
                args.len()
            )));
        }
        for ((fname, fsort), a) in c.fields.iter().zip(&args) {
            if fsort != &a.sort {
                return Err(err(format!("{ctor}.{fname}: {:?} vs {:?}", fsort, a.sort)));
            }
        }
        Ok(Term {
            node: Node::DtCtor {
                dt: dt.name.clone(),
                ctor: ctor.to_string(),
                args,
            },
            sort: dt.sort(),
        })
    }

    pub fn dt_is(dt: &DatatypeDecl, ctor: &str, arg: Term) -> Result<Term, SortError> {
        Self::resolve_ctor(dt, ctor)?;
        if arg.sort != dt.sort() {
            return Err(err(format!(
                "(_ is {ctor}): arg is {:?}, want {:?}",
                arg.sort,
                dt.sort()
            )));
        }
        Ok(Term {
            node: Node::DtIs {
                ctor: ctor.to_string(),
                arg: Box::new(arg),
            },
            sort: Sort::Bool,
        })
    }

    pub fn dt_get(
        dt: &DatatypeDecl,
        ctor: &str,
        field: &str,
        arg: Term,
    ) -> Result<Term, SortError> {
        let c = Self::resolve_ctor(dt, ctor)?;
        let Some((_, fsort)) = c.fields.iter().find(|(n, _)| n == field) else {
            return Err(err(format!("{ctor}: no field {field}")));
        };
        if arg.sort != dt.sort() {
            return Err(err(format!(
                "{field}: arg is {:?}, want {:?}",
                arg.sort,
                dt.sort()
            )));
        }
        let sort = fsort.clone();
        Ok(Term {
            node: Node::DtGet {
                field: field.to_string(),
                arg: Box::new(arg),
            },
            sort,
        })
    }

    /// Capture-free substitution of free variables by name. Sort-checked:
    /// a replacement whose sort differs from the variable's is an error.
    pub fn substitute(&self, map: &BTreeMap<String, Term>) -> Result<Term, SortError> {
        if let Node::Var(name) = &self.node {
            return match map.get(name) {
                Some(r) if r.sort == self.sort => Ok(r.clone()),
                Some(r) => Err(err(format!(
                    "substitute {name}: {:?} vs {:?}",
                    r.sort, self.sort
                ))),
                None => Ok(self.clone()),
            };
        }
        let mut t = self.clone();
        t.node = match t.node {
            n @ (Node::BoolLit(_) | Node::BvLit { .. } | Node::Var(_)) => n,
            Node::Not(a) => Node::Not(Box::new(a.substitute(map)?)),
            Node::And(ts) => Node::And(Self::subst_all(ts, map)?),
            Node::Or(ts) => Node::Or(Self::subst_all(ts, map)?),
            Node::Implies(a, b) => {
                Node::Implies(Box::new(a.substitute(map)?), Box::new(b.substitute(map)?))
            }
            Node::Eq(a, b) => Node::Eq(Box::new(a.substitute(map)?), Box::new(b.substitute(map)?)),
            Node::Ite(c, a, b) => Node::Ite(
                Box::new(c.substitute(map)?),
                Box::new(a.substitute(map)?),
                Box::new(b.substitute(map)?),
            ),
            Node::BvBin { op, lhs, rhs } => Node::BvBin {
                op,
                lhs: Box::new(lhs.substitute(map)?),
                rhs: Box::new(rhs.substitute(map)?),
            },
            Node::BvCmp { op, lhs, rhs } => Node::BvCmp {
                op,
                lhs: Box::new(lhs.substitute(map)?),
                rhs: Box::new(rhs.substitute(map)?),
            },
            Node::Select(a, i) => {
                Node::Select(Box::new(a.substitute(map)?), Box::new(i.substitute(map)?))
            }
            Node::Store(a, i, v) => Node::Store(
                Box::new(a.substitute(map)?),
                Box::new(i.substitute(map)?),
                Box::new(v.substitute(map)?),
            ),
            Node::DtCtor { dt, ctor, args } => Node::DtCtor {
                dt,
                ctor,
                args: Self::subst_all(args, map)?,
            },
            Node::DtIs { ctor, arg } => Node::DtIs {
                ctor,
                arg: Box::new(arg.substitute(map)?),
            },
            Node::DtGet { field, arg } => Node::DtGet {
                field,
                arg: Box::new(arg.substitute(map)?),
            },
        };
        Ok(t)
    }

    fn subst_all(ts: Vec<Term>, map: &BTreeMap<String, Term>) -> Result<Vec<Term>, SortError> {
        ts.into_iter().map(|t| t.substitute(map)).collect()
    }

    /// Free variables (there are no binders in the QF language, so "free"
    /// = "all"). Sorted by name — feeds the printer's declaration order.
    pub fn free_vars(&self) -> BTreeMap<String, Sort> {
        let mut out = BTreeMap::new();
        self.collect_vars(&mut out);
        out
    }

    fn collect_vars(&self, out: &mut BTreeMap<String, Sort>) {
        match &self.node {
            Node::Var(name) => {
                out.insert(name.clone(), self.sort.clone());
            }
            Node::BoolLit(_) | Node::BvLit { .. } => {}
            Node::Not(a) | Node::DtIs { arg: a, .. } | Node::DtGet { arg: a, .. } => {
                a.collect_vars(out);
            }
            Node::And(ts) | Node::Or(ts) | Node::DtCtor { args: ts, .. } => {
                for t in ts {
                    t.collect_vars(out);
                }
            }
            Node::Implies(a, b) | Node::Eq(a, b) => {
                a.collect_vars(out);
                b.collect_vars(out);
            }
            Node::BvBin { lhs, rhs, .. } | Node::BvCmp { lhs, rhs, .. } => {
                lhs.collect_vars(out);
                rhs.collect_vars(out);
            }
            Node::Select(a, b) => {
                a.collect_vars(out);
                b.collect_vars(out);
            }
            Node::Ite(a, b, c) | Node::Store(a, b, c) => {
                a.collect_vars(out);
                b.collect_vars(out);
                c.collect_vars(out);
            }
        }
    }

    /// True if any subterm has Datatype sort or is a Dt* node — tells
    /// `Query::for_asserts` whether the Ptr declaration is needed.
    pub(crate) fn uses_datatype(&self) -> bool {
        if matches!(self.sort, Sort::Datatype(_))
            || matches!(
                self.node,
                Node::DtCtor { .. } | Node::DtIs { .. } | Node::DtGet { .. }
            )
        {
            return true;
        }
        match &self.node {
            Node::BoolLit(_) | Node::BvLit { .. } | Node::Var(_) => false,
            Node::Not(a) | Node::DtIs { arg: a, .. } | Node::DtGet { arg: a, .. } => {
                a.uses_datatype()
            }
            Node::And(ts) | Node::Or(ts) | Node::DtCtor { args: ts, .. } => {
                ts.iter().any(Term::uses_datatype)
            }
            Node::Implies(a, b) | Node::Eq(a, b) => a.uses_datatype() || b.uses_datatype(),
            Node::BvBin { lhs, rhs, .. } | Node::BvCmp { lhs, rhs, .. } => {
                lhs.uses_datatype() || rhs.uses_datatype()
            }
            Node::Select(a, b) => a.uses_datatype() || b.uses_datatype(),
            Node::Ite(a, b, c) | Node::Store(a, b, c) => {
                a.uses_datatype() || b.uses_datatype() || c.uses_datatype()
            }
        }
    }
}

pub fn ptr_nil() -> Term {
    Term::dt_ctor(&ptr_datatype(), "ptr-nil", vec![])
        .expect("ptr-nil is a valid nullary constructor")
}

pub fn ptr_is_nil(t: Term) -> Result<Term, SortError> {
    Term::dt_is(&ptr_datatype(), "ptr-nil", t)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::sort::{Sort, ptr_datatype, ptr_sort};

    #[test]
    fn constructors_carry_sorts() {
        let x = Term::var("x", Sort::BitVec(32));
        let y = Term::var("y", Sort::BitVec(32));
        let add = Term::bv_bin(BvBinOp::Add, x.clone(), y.clone()).unwrap();
        assert_eq!(add.sort(), &Sort::BitVec(32), "bv_bin result sort");
        let lt = Term::bv_cmp(BvCmpOp::Ult, x, y).unwrap();
        assert_eq!(lt.sort(), &Sort::Bool, "bv_cmp result sort");
    }

    #[test]
    fn ill_sorted_construction_is_rejected() {
        let b = Term::bool_lit(true);
        let bv = Term::bv_lit(8, 7);
        assert!(
            Term::bv_bin(BvBinOp::Add, b.clone(), bv.clone()).is_err(),
            "bool + bv8"
        );
        assert!(Term::and(vec![bv.clone()]).is_err(), "and over non-bool");
        assert!(
            Term::ite(bv.clone(), b.clone(), b.clone()).is_err(),
            "non-bool cond"
        );
        assert!(Term::eq(b, bv).is_err(), "eq across sorts");
        let w32 = Term::bv_lit(32, 1);
        let w8 = Term::bv_lit(8, 1);
        assert!(
            Term::bv_bin(BvBinOp::Add, w32, w8).is_err(),
            "width mismatch"
        );
    }

    #[test]
    fn bv_lit_value_must_fit_width() {
        assert!(
            std::panic::catch_unwind(|| Term::bv_lit(4, 16)).is_err(),
            "bv_lit(4, 16): 16 needs 5 bits — internal misuse, assert fires"
        );
        let _ = Term::bv_lit(4, 15); // fits
    }

    #[test]
    fn ptr_datatype_helpers() {
        let dt = ptr_datatype();
        let p = Term::var("p0", ptr_sort());
        let is_nil = ptr_is_nil(p.clone()).unwrap();
        assert_eq!(is_nil.sort(), &Sort::Bool);
        let addr = Term::dt_get(&dt, "ptr-addr", "ptr-addr-val", p.clone()).unwrap();
        assert_eq!(addr.sort(), &Sort::BitVec(64));
        assert!(Term::dt_ctor(&dt, "no-such-ctor", vec![]).is_err());
        assert!(
            Term::dt_is(&dt, "ptr-nil", Term::bool_lit(true)).is_err(),
            "tester on non-Ptr arg"
        );
    }

    #[test]
    fn substitute_replaces_free_vars_sort_checked() {
        let p = Term::var("p0", ptr_sort());
        let f = Term::not(ptr_is_nil(p).unwrap()).unwrap();
        let mut m = BTreeMap::new();
        m.insert("p0".to_string(), ptr_nil());
        let g = f.substitute(&m).unwrap();
        assert!(g.free_vars().is_empty(), "p0 fully substituted");
        // sort-mismatched substitution is rejected
        let mut bad = BTreeMap::new();
        bad.insert("p0".to_string(), Term::bool_lit(true));
        assert!(f.substitute(&bad).is_err());
    }

    #[test]
    fn free_vars_collects_names_and_sorts() {
        let x = Term::var("x", Sort::Bool);
        let p = Term::var("p0", ptr_sort());
        let t = Term::and(vec![x, ptr_is_nil(p).unwrap()]).unwrap();
        let fv = t.free_vars();
        assert_eq!(fv.get("x"), Some(&Sort::Bool));
        assert_eq!(fv.get("p0"), Some(&ptr_sort()));
        assert_eq!(fv.len(), 2);
    }
}
