//! Function summaries (parent spec §5), phase-3 form: clause formulas are
//! real terms over the function's symbolic interface. Free variables use
//! the fixed naming p<i> (params) / r<i> (results) — `iface_var_name` is
//! the single source of that convention.

use std::collections::BTreeMap;

use goverify_solver::Term;

use crate::effects::Effects;

/// A variable of the function's symbolic interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IfaceVar {
    Param(u32),
    Result(u32),
}

/// THE naming convention for interface variables in formulas. Checkers
/// (Task 11) must build vars with exactly these names.
pub fn iface_var_name(v: &IfaceVar) -> String {
    match v {
        IfaceVar::Param(i) => format!("p{i}"),
        IfaceVar::Result(i) => format!("r{i}"),
    }
}

/// A clause formula: a Bool-sorted term whose free variables are all
/// p<i>/r<i>-named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Formula {
    pub term: Term,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Clause {
    /// Which checker/fact this clause states, e.g. "nil-deref".
    pub tag: String,
    pub formula: Formula,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provenance {
    Inferred,
    Havoc,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    pub requires: Vec<Clause>,
    pub ensures: Vec<Clause>,
    pub effects: Effects,
    pub provenance: Provenance,
}

impl Default for Summary {
    fn default() -> Self {
        Summary {
            requires: Vec::new(),
            ensures: Vec::new(),
            effects: Effects::empty(),
            provenance: Provenance::Inferred,
        }
    }
}

impl Summary {
    /// The unknown-function summary: no requires (missing info must never
    /// create false positives), top effects (assume the worst).
    pub fn havoc() -> Summary {
        Summary {
            requires: Vec::new(),
            ensures: Vec::new(),
            effects: Effects::top(),
            provenance: Provenance::Havoc,
        }
    }
}

/// A callee requires-clause instantiated at a call site: `bound` is the
/// clause with p<i> := arg_terms[i] substituted (NOT negated); `violation`
/// is ¬bound. Both None = some needed variable had no caller term (unknown
/// arg, Result var, sort mismatch, arity overflow) — callers MUST treat
/// None as "cannot evaluate; do not report".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundClause {
    pub tag: String,
    pub bound: Option<Term>,
    pub violation: Option<Term>,
}

pub fn instantiate_requires(callee: &Summary, arg_terms: &[Option<Term>]) -> Vec<BoundClause> {
    callee
        .requires
        .iter()
        .map(|c| match bind(&c.formula, arg_terms) {
            Some((b, v)) => BoundClause {
                tag: c.tag.clone(),
                bound: Some(b),
                violation: Some(v),
            },
            None => BoundClause {
                tag: c.tag.clone(),
                bound: None,
                violation: None,
            },
        })
        .collect()
}

fn bind(f: &Formula, arg_terms: &[Option<Term>]) -> Option<(Term, Term)> {
    bind_with(f, arg_terms, &[])
}

/// The general binder: p<i> free vars map to arg_terms[i], r<i> free vars
/// to result_terms[i]. Any var that is neither, or whose slot is
/// missing/None, makes the clause unevaluable (None).
fn bind_with(
    f: &Formula,
    arg_terms: &[Option<Term>],
    result_terms: &[Option<Term>],
) -> Option<(Term, Term)> {
    let mut map = BTreeMap::new();
    for (name, _sort) in f.term.free_vars() {
        let t = if let Some(rest) = name.strip_prefix('p') {
            let idx: u32 = rest.parse().ok()?;
            arg_terms.get(idx as usize)?.clone()?
        } else {
            let rest = name.strip_prefix('r')?;
            let idx: u32 = rest.parse().ok()?;
            result_terms.get(idx as usize)?.clone()?
        };
        map.insert(name, t);
    }
    let bound = f.term.substitute(&map).ok()?;
    let violation = Term::not(bound.clone()).ok()?;
    Some((bound, violation))
}

/// A callee ensures-clause instantiated at a call site: p<i> := the
/// caller's arg terms, r<i> := the call's result terms (the dst for a
/// single-value call; the Extract dsts for a tuple call). Same None
/// contract as `instantiate_requires`.
pub fn instantiate_ensures(
    callee: &Summary,
    arg_terms: &[Option<Term>],
    result_terms: &[Option<Term>],
) -> Vec<BoundClause> {
    callee
        .ensures
        .iter()
        .map(|c| match bind_with(&c.formula, arg_terms, result_terms) {
            Some((b, v)) => BoundClause {
                tag: c.tag.clone(),
                bound: Some(b),
                violation: Some(v),
            },
            None => BoundClause {
                tag: c.tag.clone(),
                bound: None,
                violation: None,
            },
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use goverify_solver::{Term, ptr_is_nil, ptr_nil, ptr_sort};

    use super::*;

    fn nonnil_clause(param: u32) -> Clause {
        let v = IfaceVar::Param(param);
        let p = Term::var(&iface_var_name(&v), ptr_sort());
        Clause {
            tag: "nil-deref".into(),
            formula: Formula {
                term: Term::not(ptr_is_nil(p).unwrap()).unwrap(),
            },
        }
    }

    fn callee_with(requires: Vec<Clause>) -> Summary {
        Summary {
            requires,
            ..Summary::default()
        }
    }

    #[test]
    fn nil_arg_binds_to_violation_term() {
        let callee = callee_with(vec![nonnil_clause(0)]);
        let bound = instantiate_requires(&callee, &[Some(ptr_nil())]);
        assert_eq!(bound.len(), 1);
        let v = bound[0].violation.as_ref().expect("bindable");
        // violation = ¬¬(is-nil nil): no free vars left.
        assert!(v.free_vars().is_empty(), "fully ground violation");
    }

    #[test]
    fn unknown_arg_means_no_violation() {
        let callee = callee_with(vec![nonnil_clause(0)]);
        assert_eq!(
            instantiate_requires(&callee, &[None])[0].violation,
            None,
            "unknown arg: cannot evaluate; do not report"
        );
    }

    #[test]
    fn out_of_range_param_means_no_violation() {
        let callee = callee_with(vec![nonnil_clause(5)]);
        assert_eq!(instantiate_requires(&callee, &[])[0].violation, None);
    }

    /// Folded fast-follow T12: a Result-var clause can never be bound at
    /// a call site — violation must be None, not a bogus term.
    #[test]
    fn result_var_clause_means_no_violation() {
        let r = Term::var(&iface_var_name(&IfaceVar::Result(0)), ptr_sort());
        let callee = callee_with(vec![Clause {
            tag: "t".into(),
            formula: Formula {
                term: ptr_is_nil(r).unwrap(),
            },
        }]);
        assert_eq!(
            instantiate_requires(&callee, &[Some(ptr_nil())])[0].violation,
            None
        );
    }

    #[test]
    fn sort_mismatched_arg_means_no_violation() {
        let callee = callee_with(vec![nonnil_clause(0)]);
        assert_eq!(
            instantiate_requires(&callee, &[Some(Term::bool_lit(true))])[0].violation,
            None,
            "substitute() sort check must degrade, not report"
        );
    }

    #[test]
    fn havoc_summary_has_no_requires() {
        // Missing info must never create false positives (parent spec §11).
        let h = Summary::havoc();
        assert!(h.requires.is_empty());
        assert_eq!(h.provenance, Provenance::Havoc);
        assert_eq!(h.effects, crate::effects::Effects::top());
    }

    fn nonnil_result_clause(result: u32) -> Clause {
        let v = IfaceVar::Result(result);
        let r = Term::var(&iface_var_name(&v), ptr_sort());
        Clause {
            tag: "nil-deref".into(),
            formula: Formula {
                term: Term::not(ptr_is_nil(r).unwrap()).unwrap(),
            },
        }
    }

    fn callee_with_ensures(ensures: Vec<Clause>) -> Summary {
        Summary {
            ensures,
            ..Summary::default()
        }
    }

    #[test]
    fn ensures_binds_result_terms() {
        let callee = callee_with_ensures(vec![nonnil_result_clause(0)]);
        let dst = Term::var("v7", ptr_sort());
        let bound = instantiate_ensures(&callee, &[], &[Some(dst)]);
        assert_eq!(bound.len(), 1, "one ensures clause in, one bound clause out");
        let b = bound[0]
            .bound
            .as_ref()
            .expect("r0 must bind to the dst term");
        let vars = b.free_vars();
        let free: Vec<&String> = vars.keys().collect();
        assert_eq!(free, vec!["v7"], "bound clause is over the caller's dst");
    }

    #[test]
    fn ensures_missing_result_term_means_unbindable() {
        // Discarded component (`b, _ := f()`): no Extract, no term — the
        // clause must be skipped, never mis-bound.
        let callee = callee_with_ensures(vec![nonnil_result_clause(1)]);
        assert_eq!(
            instantiate_ensures(&callee, &[], &[Some(ptr_nil()), None])[0].bound,
            None,
            "missing result term: cannot evaluate; do not assert"
        );
    }

    #[test]
    fn ensures_mixed_param_and_result_vars_bind_both() {
        // Clause over p0 and r0 (future-proofing: arg-dependent ensures).
        let p0 = Term::var("p0", ptr_sort());
        let r0 = Term::var("r0", ptr_sort());
        let both = Clause {
            tag: "nil-deref".into(),
            formula: Formula {
                term: Term::or(vec![
                    Term::not(ptr_is_nil(p0).unwrap()).unwrap(),
                    Term::not(ptr_is_nil(r0).unwrap()).unwrap(),
                ])
                .unwrap(),
            },
        };
        let callee = callee_with_ensures(vec![both]);
        let out = instantiate_ensures(
            &callee,
            &[Some(Term::var("va", ptr_sort()))],
            &[Some(Term::var("vd", ptr_sort()))],
        );
        let b = out[0].bound.as_ref().expect("both vars bindable");
        let vars = b.free_vars();
        let mut free: Vec<&String> = vars.keys().collect();
        free.sort();
        assert_eq!(free, vec!["va", "vd"], "mixed p0/r0 clause binds both caller terms");
    }

    #[test]
    fn requires_binding_still_rejects_result_vars() {
        // Regression guard on the existing behavior: instantiate_requires
        // must keep refusing r<i> vars (they have no meaning pre-call).
        let callee = callee_with(vec![nonnil_result_clause(0)]);
        assert_eq!(
            instantiate_requires(&callee, &[Some(ptr_nil())])[0].violation,
            None,
            "instantiate_requires must keep rejecting r<i> vars"
        );
    }
}
