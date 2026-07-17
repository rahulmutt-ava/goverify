//! Canonical SMT-LIB2 printer (phase-3 spec §4): the ONLY Term→SMT-LIB2
//! lowering in the codebase. Both backends consume these exact bytes and
//! blake3(bytes) is the query-cache identity, so any format change
//! invalidates every cache in the world — the golden test pins it.

use std::fmt::Write;

use crate::sort::{DatatypeDecl, Sort, ptr_datatype};
use crate::term::{BvBinOp, BvCmpOp, Node, Term};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Logic {
    QfBv,
    QfAbv,
    All,
}

impl Logic {
    fn as_str(self) -> &'static str {
        match self {
            Logic::QfBv => "QF_BV",
            Logic::QfAbv => "QF_ABV",
            Logic::All => "ALL",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    pub logic: Logic,
    pub datatypes: Vec<DatatypeDecl>,
    pub consts: Vec<(String, Sort)>,
    pub asserts: Vec<Term>,
}

impl Query {
    /// Build a query from bare assertions: free variables become
    /// declarations automatically; the Ptr datatype is declared iff any
    /// assert mentions a datatype (v1 has only Ptr).
    pub fn for_asserts(logic: Logic, asserts: Vec<Term>) -> Query {
        let mut consts = std::collections::BTreeMap::new();
        let mut needs_ptr = false;
        for a in &asserts {
            consts.append(&mut a.free_vars());
            needs_ptr = needs_ptr || a.uses_datatype();
        }
        Query {
            logic,
            datatypes: if needs_ptr {
                vec![ptr_datatype()]
            } else {
                vec![]
            },
            consts: consts.into_iter().collect(),
            asserts,
        }
    }

    pub fn canonical_text(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "(set-logic {})", self.logic.as_str());
        let mut dts = self.datatypes.clone();
        dts.sort_by(|a, b| a.name.cmp(&b.name));
        dts.dedup_by(|a, b| a.name == b.name);
        for dt in &dts {
            let _ = write!(out, "(declare-datatypes (({} 0)) ((", dt.name);
            for (i, c) in dt.ctors.iter().enumerate() {
                if i > 0 {
                    out.push(' ');
                }
                let _ = write!(out, "({}", c.name);
                for (fname, fsort) in &c.fields {
                    let _ = write!(out, " ({fname} {})", sort_str(fsort));
                }
                out.push(')');
            }
            out.push_str(")))\n");
        }
        let mut consts = self.consts.clone();
        consts.sort();
        consts.dedup();
        for (name, sort) in &consts {
            let _ = writeln!(out, "(declare-const {name} {})", sort_str(sort));
        }
        for a in &self.asserts {
            let mut t = String::new();
            term_str(a, &mut t);
            let _ = writeln!(out, "(assert {t})");
        }
        out.push_str("(check-sat)\n");
        out
    }
}

fn sort_str(s: &Sort) -> String {
    match s {
        Sort::Bool => "Bool".to_string(),
        Sort::BitVec(w) => format!("(_ BitVec {w})"),
        Sort::Array(k, v) => format!("(Array {} {})", sort_str(k), sort_str(v)),
        Sort::Datatype(n) => n.clone(),
    }
}

fn bv_bin_str(op: BvBinOp) -> &'static str {
    match op {
        BvBinOp::Add => "bvadd",
        BvBinOp::Sub => "bvsub",
        BvBinOp::Mul => "bvmul",
        BvBinOp::Udiv => "bvudiv",
        BvBinOp::Sdiv => "bvsdiv",
        BvBinOp::Urem => "bvurem",
        BvBinOp::Srem => "bvsrem",
        BvBinOp::And => "bvand",
        BvBinOp::Or => "bvor",
        BvBinOp::Xor => "bvxor",
        BvBinOp::Shl => "bvshl",
        BvBinOp::Lshr => "bvlshr",
        BvBinOp::Ashr => "bvashr",
    }
}

fn bv_cmp_str(op: BvCmpOp) -> &'static str {
    match op {
        BvCmpOp::Ult => "bvult",
        BvCmpOp::Ule => "bvule",
        BvCmpOp::Slt => "bvslt",
        BvCmpOp::Sle => "bvsle",
    }
}

fn app(out: &mut String, head: &str, args: &[&Term]) {
    out.push('(');
    out.push_str(head);
    for a in args {
        out.push(' ');
        term_str(a, out);
    }
    out.push(')');
}

fn term_str(t: &Term, out: &mut String) {
    match &t.node {
        Node::BoolLit(true) => out.push_str("true"),
        Node::BoolLit(false) => out.push_str("false"),
        Node::BvLit { width, value } => {
            let _ = write!(out, "(_ bv{value} {width})");
        }
        Node::Var(n) => out.push_str(n),
        Node::Not(a) => app(out, "not", &[a]),
        Node::And(ts) => app(out, "and", &ts.iter().collect::<Vec<_>>()),
        Node::Or(ts) => app(out, "or", &ts.iter().collect::<Vec<_>>()),
        Node::Implies(a, b) => app(out, "=>", &[a, b]),
        Node::Eq(a, b) => app(out, "=", &[a, b]),
        Node::Ite(c, a, b) => app(out, "ite", &[c, a, b]),
        Node::BvBin { op, lhs, rhs } => app(out, bv_bin_str(*op), &[lhs, rhs]),
        Node::BvCmp { op, lhs, rhs } => app(out, bv_cmp_str(*op), &[lhs, rhs]),
        Node::Select(a, i) => app(out, "select", &[a, i]),
        Node::Store(a, i, v) => app(out, "store", &[a, i, v]),
        Node::DtCtor { ctor, args, .. } => {
            if args.is_empty() {
                out.push_str(ctor);
            } else {
                app(out, ctor, &args.iter().collect::<Vec<_>>());
            }
        }
        Node::DtIs { ctor, arg } => {
            let head = format!("(_ is {ctor})");
            app(out, &head, &[arg]);
        }
        Node::DtGet { field, arg } => app(out, field, &[arg]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sort::{Sort, ptr_sort};
    use crate::term::{BvCmpOp, Term, ptr_is_nil};

    /// The full canonical format, pinned byte-for-byte. If this golden
    /// ever changes, every query-cache entry in the world is invalidated —
    /// that is the point of pinning it.
    #[test]
    fn canonical_text_golden() {
        let p = Term::var("p0", ptr_sort());
        let q = Query::for_asserts(Logic::All, vec![ptr_is_nil(p).unwrap()]);
        assert_eq!(
            q.canonical_text(),
            "(set-logic ALL)\n\
             (declare-datatypes ((Ptr 0)) (((ptr-nil) (ptr-addr (ptr-addr-val (_ BitVec 64))))))\n\
             (declare-const p0 Ptr)\n\
             (assert ((_ is ptr-nil) p0))\n\
             (check-sat)\n"
        );
    }

    #[test]
    fn bv_and_bool_query_golden() {
        let x = Term::var("x", Sort::BitVec(8));
        let five = Term::bv_lit(8, 5);
        let cmp = Term::bv_cmp(BvCmpOp::Ult, x, five).unwrap();
        let q = Query::for_asserts(Logic::QfBv, vec![cmp]);
        assert_eq!(
            q.canonical_text(),
            "(set-logic QF_BV)\n\
             (declare-const x (_ BitVec 8))\n\
             (assert (bvult x (_ bv5 8)))\n\
             (check-sat)\n"
        );
    }

    #[test]
    fn decls_are_sorted_regardless_of_construction_order() {
        let b = Term::var("bbb", Sort::Bool);
        let a = Term::var("aaa", Sort::Bool);
        let q = Query::for_asserts(Logic::QfBv, vec![Term::and(vec![b, a]).unwrap()]);
        let text = q.canonical_text();
        let ai = text.find("declare-const aaa").unwrap();
        let bi = text.find("declare-const bbb").unwrap();
        assert!(ai < bi, "consts sorted by name:\n{text}");
    }

    #[test]
    fn array_sort_prints_smtlib2() {
        let arr = Term::var(
            "m",
            Sort::Array(Box::new(Sort::BitVec(64)), Box::new(Sort::Bool)),
        );
        let idx = Term::bv_lit(64, 0);
        let q = Query::for_asserts(Logic::QfAbv, vec![Term::select(arr, idx).unwrap()]);
        assert!(
            q.canonical_text()
                .contains("(declare-const m (Array (_ BitVec 64) Bool))"),
            "{}",
            q.canonical_text()
        );
    }

    #[test]
    fn printing_is_deterministic() {
        let mk = || {
            let p = Term::var("p0", ptr_sort());
            Query::for_asserts(Logic::All, vec![ptr_is_nil(p).unwrap()])
        };
        assert_eq!(mk().canonical_text(), mk().canonical_text());
    }
}
