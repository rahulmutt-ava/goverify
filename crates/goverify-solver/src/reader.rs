//! S-expression reader for solver responses + the canonical-query parser
//! used by the round-trip property suite and the fuzz target. Parses
//! bytes the analyzer didn't write: rejects, never panics (parent §11,
//! §12.4). NOT a general SMT-LIB2 parser — it understands exactly the
//! subset printer.rs emits, plus solver response lines.

use std::collections::BTreeMap;

use crate::SatResult;
use crate::printer::{Logic, Query};
use crate::sort::{CtorDecl, DatatypeDecl, Sort};
use crate::term::{BvBinOp, BvCmpOp, Term};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadError(pub String);

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "smt read error: {}", self.0)
    }
}

impl std::error::Error for ReadError {}

fn err(m: impl Into<String>) -> ReadError {
    ReadError(m.into())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SExpr {
    Atom(String),
    List(Vec<SExpr>),
}

const MAX_INPUT: usize = 1 << 20; // 1 MiB
const MAX_DEPTH: usize = 64;

/// Parse one s-expression from the front of `input`; returns it plus the
/// number of bytes consumed. Iterative (explicit stack), depth-capped.
pub fn parse_sexpr(input: &str) -> Result<(SExpr, usize), ReadError> {
    if input.len() > MAX_INPUT {
        return Err(err("input too large"));
    }
    let b = input.as_bytes();
    let mut i = 0usize;
    let mut stack: Vec<Vec<SExpr>> = Vec::new();
    loop {
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= b.len() {
            return Err(err("unexpected end of input"));
        }
        match b[i] {
            b'(' => {
                if stack.len() >= MAX_DEPTH {
                    return Err(err("nesting too deep"));
                }
                stack.push(Vec::new());
                i += 1;
            }
            b')' => {
                let done = stack.pop().ok_or_else(|| err("unbalanced ')'"))?;
                i += 1;
                let e = SExpr::List(done);
                match stack.last_mut() {
                    Some(parent) => parent.push(e),
                    None => {
                        // A stray ')' directly following the completed
                        // top-level form (e.g. "(a))") is never a
                        // legitimate next command — every real command
                        // begins with '(' — so treat it as unbalanced
                        // rather than silently returning success and
                        // stranding it for a later caller to (maybe)
                        // catch.
                        let mut j = i;
                        while j < b.len() && b[j].is_ascii_whitespace() {
                            j += 1;
                        }
                        if j < b.len() && b[j] == b')' {
                            return Err(err("unbalanced ')'"));
                        }
                        return Ok((e, i));
                    }
                }
            }
            b'"' => {
                // quoted string atom (models contain them); keep quotes.
                let start = i;
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    i += 1;
                }
                if i >= b.len() {
                    return Err(err("unterminated string"));
                }
                i += 1;
                let e = SExpr::Atom(input[start..i].to_string());
                match stack.last_mut() {
                    Some(parent) => parent.push(e),
                    None => return Ok((e, i)),
                }
            }
            _ => {
                let start = i;
                while i < b.len() && !b[i].is_ascii_whitespace() && b[i] != b'(' && b[i] != b')' {
                    i += 1;
                }
                let e = SExpr::Atom(input[start..i].to_string());
                match stack.last_mut() {
                    Some(parent) => parent.push(e),
                    None => return Ok((e, i)),
                }
            }
        }
    }
}

/// First response line → SatResult. ANYTHING unrecognized is Unknown
/// (bug-finder semantics: garbage output must never become a report).
pub fn parse_response(first_line: &str) -> SatResult {
    match first_line.trim() {
        "sat" => SatResult::Sat,
        "unsat" => SatResult::Unsat,
        _ => SatResult::Unknown,
    }
}

/// Parse text in the exact canonical shape printer.rs emits.
pub fn parse_query(text: &str) -> Result<Query, ReadError> {
    if text.len() > MAX_INPUT {
        return Err(err("input too large"));
    }
    let mut logic = None;
    let mut datatypes: Vec<DatatypeDecl> = Vec::new();
    let mut consts: Vec<(String, Sort)> = Vec::new();
    let mut asserts: Vec<Term> = Vec::new();
    let mut saw_check_sat = false;
    let mut rest = text;
    while !rest.trim().is_empty() {
        if saw_check_sat {
            return Err(err("content after (check-sat)"));
        }
        let (e, n) = parse_sexpr(rest)?;
        rest = &rest[n..];
        let SExpr::List(items) = &e else {
            return Err(err("top level must be command lists"));
        };
        match items.first() {
            Some(SExpr::Atom(a)) if a == "set-logic" => {
                let [_, SExpr::Atom(l)] = items.as_slice() else {
                    return Err(err("malformed set-logic"));
                };
                logic = Some(match l.as_str() {
                    "QF_BV" => Logic::QfBv,
                    "QF_ABV" => Logic::QfAbv,
                    "ALL" => Logic::All,
                    other => return Err(err(format!("unknown logic {other}"))),
                });
            }
            Some(SExpr::Atom(a)) if a == "declare-datatypes" => {
                datatypes.push(parse_datatype(items)?);
            }
            Some(SExpr::Atom(a)) if a == "declare-const" => {
                let [_, SExpr::Atom(name), sort] = items.as_slice() else {
                    return Err(err("malformed declare-const"));
                };
                consts.push((name.clone(), parse_sort(sort)?));
            }
            Some(SExpr::Atom(a)) if a == "assert" => {
                let [_, body] = items.as_slice() else {
                    return Err(err("malformed assert"));
                };
                let env: BTreeMap<String, Sort> = consts.iter().cloned().collect();
                asserts.push(parse_term(body, &env, &datatypes)?);
            }
            Some(SExpr::Atom(a)) if a == "check-sat" => {
                saw_check_sat = true;
            }
            _ => return Err(err("unknown command")),
        }
    }
    if !saw_check_sat {
        return Err(err("missing (check-sat)"));
    }
    Ok(Query {
        logic: logic.ok_or_else(|| err("missing (set-logic)"))?,
        datatypes,
        consts,
        asserts,
    })
}

fn parse_sort(e: &SExpr) -> Result<Sort, ReadError> {
    match e {
        SExpr::Atom(a) if a == "Bool" => Ok(Sort::Bool),
        SExpr::Atom(a) => Ok(Sort::Datatype(a.clone())),
        SExpr::List(items) => match items.as_slice() {
            [SExpr::Atom(u), SExpr::Atom(bv), SExpr::Atom(w)] if u == "_" && bv == "BitVec" => {
                Ok(Sort::BitVec(w.parse().map_err(|_| err("bad width"))?))
            }
            [SExpr::Atom(arr), k, v] if arr == "Array" => Ok(Sort::Array(
                Box::new(parse_sort(k)?),
                Box::new(parse_sort(v)?),
            )),
            _ => Err(err("unknown sort")),
        },
    }
}

fn parse_datatype(items: &[SExpr]) -> Result<DatatypeDecl, ReadError> {
    // ((N 0)) (((ctor (acc sort)...) ...))
    let [_, SExpr::List(names), SExpr::List(bodies)] = items else {
        return Err(err("malformed declare-datatypes"));
    };
    let [SExpr::List(nv)] = names.as_slice() else {
        return Err(err("expect one datatype"));
    };
    let [SExpr::Atom(name), SExpr::Atom(zero)] = nv.as_slice() else {
        return Err(err("expect (Name 0)"));
    };
    if zero != "0" {
        return Err(err("parametric datatypes unsupported"));
    }
    let [SExpr::List(ctors)] = bodies.as_slice() else {
        return Err(err("expect one ctor list"));
    };
    let mut out = Vec::new();
    for c in ctors {
        let SExpr::List(cv) = c else {
            return Err(err("ctor must be a list"));
        };
        let Some((SExpr::Atom(cname), fields)) = cv.split_first() else {
            return Err(err("empty ctor"));
        };
        let mut fs = Vec::new();
        for f in fields {
            let SExpr::List(fv) = f else {
                return Err(err("field must be a list"));
            };
            let [SExpr::Atom(fname), fsort] = fv.as_slice() else {
                return Err(err("malformed field"));
            };
            fs.push((fname.clone(), parse_sort(fsort)?));
        }
        out.push(CtorDecl {
            name: cname.clone(),
            fields: fs,
        });
    }
    Ok(DatatypeDecl {
        name: name.clone(),
        ctors: out,
    })
}

fn bv_bin_of(s: &str) -> Option<BvBinOp> {
    Some(match s {
        "bvadd" => BvBinOp::Add,
        "bvsub" => BvBinOp::Sub,
        "bvmul" => BvBinOp::Mul,
        "bvudiv" => BvBinOp::Udiv,
        "bvsdiv" => BvBinOp::Sdiv,
        "bvurem" => BvBinOp::Urem,
        "bvsrem" => BvBinOp::Srem,
        "bvand" => BvBinOp::And,
        "bvor" => BvBinOp::Or,
        "bvxor" => BvBinOp::Xor,
        "bvshl" => BvBinOp::Shl,
        "bvlshr" => BvBinOp::Lshr,
        "bvashr" => BvBinOp::Ashr,
        _ => return None,
    })
}

fn bv_cmp_of(s: &str) -> Option<BvCmpOp> {
    Some(match s {
        "bvult" => BvCmpOp::Ult,
        "bvule" => BvCmpOp::Ule,
        "bvslt" => BvCmpOp::Slt,
        "bvsle" => BvCmpOp::Sle,
        _ => return None,
    })
}

fn parse_term(
    e: &SExpr,
    env: &BTreeMap<String, Sort>,
    dts: &[DatatypeDecl],
) -> Result<Term, ReadError> {
    let sub = |e: &SExpr| parse_term(e, env, dts);
    let ill = |se: crate::sort::SortError| err(format!("ill-sorted: {se}"));
    match e {
        SExpr::Atom(a) if a == "true" => Ok(Term::bool_lit(true)),
        SExpr::Atom(a) if a == "false" => Ok(Term::bool_lit(false)),
        SExpr::Atom(a) => {
            if let Some(sort) = env.get(a) {
                return Ok(Term::var(a, sort.clone()));
            }
            // nullary datatype constructor?
            for dt in dts {
                if dt.ctor(a).is_some() {
                    return Term::dt_ctor(dt, a, vec![]).map_err(ill);
                }
            }
            Err(err(format!("unknown atom {a}")))
        }
        SExpr::List(items) => match items.as_slice() {
            [SExpr::Atom(u), SExpr::Atom(bv), SExpr::Atom(w)]
                if u == "_" && bv.starts_with("bv") =>
            {
                let value: u128 = bv[2..].parse().map_err(|_| err("bad bv literal"))?;
                let width: u32 = w.parse().map_err(|_| err("bad bv width"))?;
                if width == 0 || width > 128 || (width < 128 && value >= (1u128 << width)) {
                    return Err(err("bv literal out of range"));
                }
                Ok(Term::bv_lit(width, value))
            }
            [SExpr::List(tester), arg] => {
                // ((_ is ctor) arg)
                let [SExpr::Atom(u), SExpr::Atom(is), SExpr::Atom(ctor)] = tester.as_slice() else {
                    return Err(err("unknown applied form"));
                };
                if u != "_" || is != "is" {
                    return Err(err("unknown applied form"));
                }
                let dt = dts
                    .iter()
                    .find(|d| d.ctor(ctor).is_some())
                    .ok_or_else(|| err(format!("tester for unknown ctor {ctor}")))?;
                Term::dt_is(dt, ctor, sub(arg)?).map_err(ill)
            }
            [SExpr::Atom(head), rest @ ..] => {
                let args: Vec<Term> = rest.iter().map(sub).collect::<Result<_, _>>()?;
                let one = |args: &[Term]| args[0].clone();
                match (head.as_str(), args.len()) {
                    ("not", 1) => Term::not(one(&args)).map_err(ill),
                    ("and", n) if n >= 1 => Term::and(args).map_err(ill),
                    ("or", n) if n >= 1 => Term::or(args).map_err(ill),
                    ("=>", 2) => Term::implies(args[0].clone(), args[1].clone()).map_err(ill),
                    ("=", 2) => Term::eq(args[0].clone(), args[1].clone()).map_err(ill),
                    ("ite", 3) => {
                        Term::ite(args[0].clone(), args[1].clone(), args[2].clone()).map_err(ill)
                    }
                    ("select", 2) => Term::select(args[0].clone(), args[1].clone()).map_err(ill),
                    ("store", 3) => {
                        Term::store(args[0].clone(), args[1].clone(), args[2].clone()).map_err(ill)
                    }
                    (h, 2) if bv_bin_of(h).is_some() => {
                        Term::bv_bin(bv_bin_of(h).unwrap(), args[0].clone(), args[1].clone())
                            .map_err(ill)
                    }
                    (h, 2) if bv_cmp_of(h).is_some() => {
                        Term::bv_cmp(bv_cmp_of(h).unwrap(), args[0].clone(), args[1].clone())
                            .map_err(ill)
                    }
                    (h, _) => {
                        // ctor application or accessor
                        for dt in dts {
                            if dt.ctor(h).is_some() {
                                return Term::dt_ctor(dt, h, args).map_err(ill);
                            }
                            for c in &dt.ctors {
                                if c.fields.iter().any(|(f, _)| f == h) && args.len() == 1 {
                                    return Term::dt_get(dt, &c.name, h, one(&args)).map_err(ill);
                                }
                            }
                        }
                        Err(err(format!("unknown head {h}")))
                    }
                }
            }
            _ => Err(err("unknown term shape")),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SatResult;
    use crate::printer::{Logic, Query};
    use crate::sort::{Sort, ptr_sort};
    use crate::term::{Term, ptr_is_nil};

    #[test]
    fn response_lines() {
        assert_eq!(parse_response("sat"), SatResult::Sat);
        assert_eq!(parse_response("unsat"), SatResult::Unsat);
        assert_eq!(parse_response("unknown"), SatResult::Unknown);
        assert_eq!(parse_response("timeout"), SatResult::Unknown);
        assert_eq!(parse_response(""), SatResult::Unknown);
        assert_eq!(parse_response("(error \"boom\")"), SatResult::Unknown);
    }

    #[test]
    fn sexpr_basic() {
        let (e, n) = parse_sexpr("(a (b c) d)").unwrap();
        assert_eq!(n, 11);
        let SExpr::List(items) = e else {
            panic!("want list")
        };
        assert_eq!(items.len(), 3);
    }

    #[test]
    fn sexpr_rejects_garbage_without_panicking() {
        for bad in ["", "(", ")", "(a", "((((((", "(a))"] {
            assert!(parse_sexpr(bad).is_err(), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn sexpr_depth_cap() {
        let deep = format!("{}{}{}", "(".repeat(100), "x", ")".repeat(100));
        assert!(parse_sexpr(&deep).is_err(), "depth > 64 must be rejected");
    }

    #[test]
    fn query_round_trips() {
        let p = Term::var("p0", ptr_sort());
        let q = Query::for_asserts(Logic::All, vec![ptr_is_nil(p).unwrap()]);
        let text = q.canonical_text();
        let parsed = parse_query(&text).expect("canonical text must parse");
        assert_eq!(
            parsed.canonical_text(),
            text,
            "print∘parse must be a fixpoint"
        );
    }

    /// Deterministic (non-random) coverage for the five `parse_term` arms
    /// the round-trip property's generators previously never reached:
    /// `or`, `=>`, `ite`, a constructor applied to non-empty args
    /// (`ptr-addr`), and a field accessor (`ptr-addr-val`). Random
    /// sampling can still miss rare branches by luck; this pins each one
    /// down explicitly so none of them silently regress.
    #[test]
    fn query_round_trips_covers_or_implies_ite_ctor_and_accessor() {
        use crate::sort::ptr_datatype;

        let b = Term::var("b", Sort::Bool);
        let addr = Term::dt_ctor(&ptr_datatype(), "ptr-addr", vec![Term::bv_lit(64, 7)])
            .expect("ptr-addr is a valid unary constructor");
        let val = Term::dt_get(&ptr_datatype(), "ptr-addr", "ptr-addr-val", addr.clone())
            .expect("ptr-addr-val is a valid accessor");
        let asserts = vec![
            Term::or(vec![b.clone(), Term::bool_lit(false)]).unwrap(),
            Term::implies(b.clone(), Term::bool_lit(true)).unwrap(),
            Term::ite(b, Term::bv_lit(8, 1), Term::bv_lit(8, 2)).unwrap(),
            Term::eq(val, Term::bv_lit(64, 7)).unwrap(),
            Term::not(ptr_is_nil(addr).unwrap()).unwrap(),
        ];
        for a in asserts {
            let q = Query::for_asserts(Logic::All, vec![a]);
            let text = q.canonical_text();
            let parsed = parse_query(&text)
                .unwrap_or_else(|e| panic!("canonical text must parse: {e}\n{text}"));
            assert_eq!(
                parsed.canonical_text(),
                text,
                "print∘parse must be a fixpoint:\n{text}"
            );
        }
    }
}

#[cfg(test)]
mod props {
    use proptest::prelude::*;

    use super::*;
    use crate::printer::{Logic, Query};
    use crate::sort::Sort;
    use crate::term::Term;
    use crate::testgen::arb_term;

    /// Not a property test: a fixed-sample-count check that `arb_term()`
    /// actually produces every node kind `parse_term` understands,
    /// somewhere in the tree — not just theoretically, per the review
    /// finding that the previous generator never constructed `or`,
    /// `=>`, `ite`, a non-empty-args constructor, or a field accessor.
    /// (`print_parse_print_fixpoint` only proves round-tripping for
    /// whatever the sampled terms happen to contain; this proves they
    /// actually contain everything.)
    #[test]
    fn generator_reaches_every_node_kind() {
        use std::collections::HashSet;

        use crate::term::Node;
        use proptest::strategy::ValueTree;
        use proptest::test_runner::TestRunner;

        fn walk(t: &Term, tags: &mut HashSet<&'static str>) {
            match &t.node {
                Node::Or(ts) => {
                    tags.insert("or");
                    ts.iter().for_each(|s| walk(s, tags));
                }
                Node::Implies(a, b) => {
                    tags.insert("implies");
                    walk(a, tags);
                    walk(b, tags);
                }
                Node::Ite(c, a, b) => {
                    tags.insert("ite");
                    walk(c, tags);
                    walk(a, tags);
                    walk(b, tags);
                }
                Node::DtCtor { args, .. } => {
                    if !args.is_empty() {
                        tags.insert("ctor_with_args");
                    }
                    args.iter().for_each(|a| walk(a, tags));
                }
                Node::DtGet { arg, .. } => {
                    tags.insert("dt_get");
                    walk(arg, tags);
                }
                Node::Not(a) | Node::DtIs { arg: a, .. } => walk(a, tags),
                Node::And(ts) => ts.iter().for_each(|s| walk(s, tags)),
                Node::Eq(a, b)
                | Node::BvBin { lhs: a, rhs: b, .. }
                | Node::BvCmp { lhs: a, rhs: b, .. }
                | Node::Select(a, b) => {
                    walk(a, tags);
                    walk(b, tags);
                }
                Node::Store(a, b, c) => {
                    walk(a, tags);
                    walk(b, tags);
                    walk(c, tags);
                }
                Node::BoolLit(_) | Node::BvLit { .. } | Node::Var(_) => {}
            }
        }

        let mut runner = TestRunner::default();
        let strat = arb_term();
        let mut tags = HashSet::new();
        for _ in 0..500 {
            let t = strat
                .new_tree(&mut runner)
                .expect("arb_term() must always produce a value")
                .current();
            walk(&t, &mut tags);
        }
        for want in ["or", "implies", "ite", "ctor_with_args", "dt_get"] {
            assert!(
                tags.contains(want),
                "generator never produced a `{want}` node across 500 samples: {tags:?}"
            );
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 128, ..ProptestConfig::default() })]

        /// print → parse → print is a fixpoint (phase-3 spec §12): this,
        /// not the differential harness, is the canonical printer's guard.
        #[test]
        fn print_parse_print_fixpoint(t in arb_term()) {
            // Only Bool terms can be asserted; wrap non-bool t in an eq.
            let a = if t.sort() == &Sort::Bool { t.clone() }
                    else { Term::eq(t.clone(), t).unwrap() };
            let q = Query::for_asserts(Logic::All, vec![a]);
            let text = q.canonical_text();
            let parsed = parse_query(&text).expect("canonical text must parse");
            prop_assert_eq!(parsed.canonical_text(), text);
        }

        /// The reader never panics on arbitrary input (cheap in-process
        /// complement to the fuzz target).
        #[test]
        fn reader_never_panics(s in "\\PC{0,256}") {
            let _ = parse_sexpr(&s);
            let _ = parse_query(&s);
            let _ = parse_response(&s);
        }
    }
}
