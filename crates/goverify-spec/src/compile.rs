//! Resolve + lower parsed annotations against a function's signature
//! (phase-6 spec §3). Total: every failure is a bad-annotation error
//! finding; the function is analyzed as if unannotated.

use std::collections::BTreeMap;

use goverify_analysis::annotations::{
    AnnClause, Annotations, BAD_ANNOTATION, CONTRACT, FuncAnnotations,
};
use goverify_analysis::{
    Clause, Finding, Formula, IfaceVar, Provenance, Severity, iface_var_name, int_repr,
    seq_datatype, sort_of,
};
use goverify_ir::{FuncId, Program};
use goverify_solver::{BvCmpOp, Sort, Term, ptr_is_nil};

use crate::ast::{CmpOp, Directive, Expr};
use crate::parse::parse_pragma;

/// Compile every //goverify: pragma in the program. `known_checkers` is
/// the valid `ignore` name set (checker names + the annotation finding
/// classes) — the CLI owns the list.
pub fn compile_program(p: &Program, known_checkers: &[&str]) -> Annotations {
    let mut out = Annotations::default();
    for f in p.func_ids() {
        let prs = p.pragmas(f);
        if prs.is_empty() {
            continue;
        }
        let mut fa = FuncAnnotations::default();
        for pr in prs {
            match compile_one(p, f, &pr.text, known_checkers) {
                Ok(Compiled::Requires(c)) => fa.requires.push(AnnClause {
                    clause: c,
                    text: expr_text(&pr.text),
                    pos: pr.pos.clone(),
                }),
                Ok(Compiled::Ensures(c)) => fa.ensures.push(AnnClause {
                    clause: c,
                    text: expr_text(&pr.text),
                    pos: pr.pos.clone(),
                }),
                Ok(Compiled::Ignore(name)) => fa.ignores.push(name),
                Err(msg) => out.findings.push(bad(p.func_name(f), pr.pos.clone(), &msg)),
            }
        }
        if fa != FuncAnnotations::default() {
            out.funcs.insert(f, fa);
        }
    }
    for pr in p.unmatched_pragmas() {
        out.findings.push(bad(
            "-",
            pr.pos.clone(),
            "annotation is not attached to a function declaration",
        ));
    }
    out
}

/// The expression/payload part of the pragma line, for messages: the
/// `//goverify:` prefix AND the directive keyword (`requires`/`ensures`)
/// are both stripped, leaving just the expression text (mirrors
/// `parse_pragma`'s own directive/payload split). Control characters are
/// stripped: this string is quoted into finding messages, and the human
/// renderer sanitizes file paths but NOT messages — untrusted bytes must
/// not reach the terminal raw.
fn expr_text(text: &str) -> String {
    let rest = text.strip_prefix("//goverify:").unwrap_or(text);
    let rest = match rest.find("//") {
        Some(i) => &rest[..i],
        None => rest,
    };
    let rest = rest.trim();
    let payload = match rest.find(char::is_whitespace) {
        Some(i) => rest[i..].trim(),
        None => "",
    };
    payload.chars().filter(|c| !c.is_control()).collect()
}

fn bad(func: &str, pos: Option<goverify_ir::Pos>, msg: &str) -> Finding {
    Finding {
        checker: BAD_ANNOTATION.to_string(),
        tag: BAD_ANNOTATION.to_string(),
        func: func.to_string(),
        pos,
        message: format!("invalid annotation: {msg}"),
        trace: Vec::new(),
        model: Vec::new(),
        severity: Severity::Error,
    }
}

#[cfg_attr(test, derive(Debug))]
enum Compiled {
    Requires(Clause),
    Ensures(Clause),
    Ignore(String),
}

fn compile_one(
    p: &Program,
    f: FuncId,
    text: &str,
    known_checkers: &[&str],
) -> Result<Compiled, String> {
    let d = parse_pragma(text)?;
    match d {
        Directive::Ignore(name) => {
            if !known_checkers.contains(&name.as_str()) {
                return Err(format!(
                    "`ignore` names unknown checker `{name}` (known: {})",
                    known_checkers.join(", ")
                ));
            }
            Ok(Compiled::Ignore(name))
        }
        Directive::Requires(e) => {
            let term = lower_bool(p, f, &e, /*allow_results=*/ false)?;
            Ok(Compiled::Requires(Clause {
                tag: CONTRACT.to_string(),
                formula: Formula { term },
                provenance: Provenance::Annotated,
            }))
        }
        Directive::Ensures(e) => {
            let term = lower_bool(p, f, &e, /*allow_results=*/ true)?;
            Ok(Compiled::Ensures(Clause {
                tag: CONTRACT.to_string(),
                formula: Formula { term },
                provenance: Provenance::Annotated,
            }))
        }
    }
}

/// A resolved name: its interface var and Go type.
struct Binding {
    var: IfaceVar,
    ty: goverify_ir::TypeId,
}

/// Name table for one function: receiver+params by declared name,
/// results by declared name or ret/ret<i>. Declared names win.
fn bindings(p: &Program, f: FuncId) -> Result<BTreeMap<String, Binding>, String> {
    let func = p.func(f).ok_or("annotated function has no body in .gvir")?;
    let mut map: BTreeMap<String, Binding> = BTreeMap::new();
    for (i, name) in func.param_names.iter().enumerate() {
        if name.is_empty() || name == "_" {
            continue;
        }
        let ty = func.value(func.params[i]).ty;
        map.insert(
            name.clone(),
            Binding {
                var: IfaceVar::Param(i as u32),
                ty,
            },
        );
    }
    let goverify_ir::TypeKind::Signature { results, .. } = p.types().kind(func.sig) else {
        return Err("function signature not available".to_string());
    };
    let results = results.clone();
    if results.len() != func.result_names.len() {
        return Err("signature/result_names arity mismatch (stale .gvir?)".to_string());
    }
    for (i, (name, ty)) in func.result_names.iter().zip(&results).enumerate() {
        let keys: Vec<String> = if !name.is_empty() && name != "_" {
            vec![name.clone()]
        } else if results.len() == 1 {
            vec!["ret".to_string(), "ret0".to_string()]
        } else {
            vec![format!("ret{i}")]
        };
        for k in keys {
            // Declared names win: don't clobber an existing entry.
            map.entry(k).or_insert(Binding {
                var: IfaceVar::Result(i as u32),
                ty: *ty,
            });
        }
    }
    Ok(map)
}

fn lower_bool(p: &Program, f: FuncId, e: &Expr, allow_results: bool) -> Result<Term, String> {
    let names = bindings(p, f)?;
    let t = lower(p, &names, e, allow_results)?;
    match t.sort() {
        Sort::Bool => Ok(t),
        s => Err(format!("expression is {s:?}, expected a boolean condition")),
    }
}

fn lower(
    p: &Program,
    names: &BTreeMap<String, Binding>,
    e: &Expr,
    allow_results: bool,
) -> Result<Term, String> {
    match e {
        Expr::Old(inner) => lower(p, names, inner, allow_results),
        Expr::Select(..) => Err("field selection is not supported in v1".to_string()),
        Expr::Nil => Err("`nil` is only usable in == / != comparisons".to_string()),
        Expr::Bool(b) => Ok(Term::bool_lit(*b)),
        Expr::Int(_) => Err("bare integer literal is not a condition".to_string()),
        Expr::Ident(name) => {
            let b = names
                .get(name)
                .ok_or_else(|| format!("unknown name `{name}` (params/results only)"))?;
            if !allow_results && matches!(b.var, IfaceVar::Result(_)) {
                return Err(format!(
                    "`{name}` is a result; `requires` may only reference parameters"
                ));
            }
            let sort = sort_of(p.types(), b.ty)
                .ok_or_else(|| format!("type of `{name}` is not modeled by the analyzer"))?;
            Ok(Term::var(&iface_var_name(&b.var), sort))
        }
        Expr::Len(inner) | Expr::Cap(inner) => {
            let base = lower(p, names, inner, allow_results)?;
            if base.sort() != &seq_datatype().sort() {
                return Err("len/cap need a slice or string operand".to_string());
            }
            let field = if matches!(e, Expr::Len(_)) {
                "seq-len"
            } else {
                "seq-cap"
            };
            Term::dt_get(&seq_datatype(), "seq-val", field, base)
                .map_err(|e| format!("internal sort error: {e}"))
        }
        Expr::Not(inner) => {
            let t = lower(p, names, inner, allow_results)?;
            Term::not(t).map_err(|_| "`!` needs a boolean operand".to_string())
        }
        Expr::And(a, b) => {
            let (a, b) = (
                lower(p, names, a, allow_results)?,
                lower(p, names, b, allow_results)?,
            );
            Term::and(vec![a, b]).map_err(|_| "`&&` needs boolean operands".to_string())
        }
        Expr::Or(a, b) => {
            let (a, b) = (
                lower(p, names, a, allow_results)?,
                lower(p, names, b, allow_results)?,
            );
            Term::or(vec![a, b]).map_err(|_| "`||` needs boolean operands".to_string())
        }
        Expr::Implies(a, b) => {
            let (a, b) = (
                lower(p, names, a, allow_results)?,
                lower(p, names, b, allow_results)?,
            );
            Term::implies(a, b).map_err(|_| "`==>` needs boolean operands".to_string())
        }
        Expr::Cmp(op, a, b) => lower_cmp(p, names, *op, a, b, allow_results),
    }
}

fn lower_cmp(
    p: &Program,
    names: &BTreeMap<String, Binding>,
    op: CmpOp,
    a: &Expr,
    b: &Expr,
    allow_results: bool,
) -> Result<Term, String> {
    // nil comparisons: ptr tester, either side.
    let nil_side = match (a, b) {
        (Expr::Nil, Expr::Nil) => return Err("`nil == nil` is not a useful condition".to_string()),
        (Expr::Nil, other) | (other, Expr::Nil) => Some(other),
        _ => None,
    };
    if let Some(other) = nil_side {
        if !matches!(op, CmpOp::Eq | CmpOp::Ne) {
            return Err("`nil` only supports == and !=".to_string());
        }
        let t = lower(p, names, other, allow_results)?;
        if t.sort() != &goverify_solver::ptr_sort() {
            return Err("nil comparison needs a pointer/interface operand".to_string());
        }
        let is_nil = ptr_is_nil(t).map_err(|e| format!("internal sort error: {e}"))?;
        return match op {
            CmpOp::Eq => Ok(is_nil),
            _ => Term::not(is_nil).map_err(|e| format!("internal sort error: {e}")),
        };
    }
    // Integer-literal sides adopt the other side's width+signedness.
    let (lt, rt, signed) = match (a, b) {
        (Expr::Int(_), Expr::Int(_)) => {
            return Err("comparing two integer literals is not a useful condition".to_string());
        }
        (Expr::Int(n), other) => {
            let (t, w, s) = int_operand(p, names, other, allow_results)?;
            (lit(*n, w, s)?, t, s)
        }
        (other, Expr::Int(n)) => {
            let (t, w, s) = int_operand(p, names, other, allow_results)?;
            (t, lit(*n, w, s)?, s)
        }
        _ => {
            let ta = lower(p, names, a, allow_results)?;
            let tb = lower(p, names, b, allow_results)?;
            if ta.sort() != tb.sort() {
                return Err("comparison operands have different types".to_string());
            }
            match (ta.sort(), op) {
                (_, CmpOp::Eq) => {
                    return Term::eq(ta, tb).map_err(|e| format!("internal sort error: {e}"));
                }
                (_, CmpOp::Ne) => {
                    let eq = Term::eq(ta, tb).map_err(|e| format!("internal sort error: {e}"))?;
                    return Term::not(eq).map_err(|e| format!("internal sort error: {e}"));
                }
                (Sort::BitVec(_), _) => {
                    // Ordered compare: need signedness from the Go type.
                    let s = expr_signedness(p, names, a)
                        .or_else(|| expr_signedness(p, names, b))
                        .ok_or("cannot determine signedness of comparison")?;
                    (ta, tb, s)
                }
                _ => return Err("ordered comparison needs integer operands".to_string()),
            }
        }
    };
    // == / != on the literal paths:
    match op {
        CmpOp::Eq => return Term::eq(lt, rt).map_err(|e| format!("internal sort error: {e}")),
        CmpOp::Ne => {
            let eq = Term::eq(lt, rt).map_err(|e| format!("internal sort error: {e}"))?;
            return Term::not(eq).map_err(|e| format!("internal sort error: {e}"));
        }
        _ => {}
    }
    // No Ugt/Sgt in BvCmpOp: > and >= swap operands (house convention,
    // encode.rs binop_term).
    let (cmp, l, r) = match (op, signed) {
        (CmpOp::Lt, true) => (BvCmpOp::Slt, lt, rt),
        (CmpOp::Lt, false) => (BvCmpOp::Ult, lt, rt),
        (CmpOp::Le, true) => (BvCmpOp::Sle, lt, rt),
        (CmpOp::Le, false) => (BvCmpOp::Ule, lt, rt),
        (CmpOp::Gt, true) => (BvCmpOp::Slt, rt, lt),
        (CmpOp::Gt, false) => (BvCmpOp::Ult, rt, lt),
        (CmpOp::Ge, true) => (BvCmpOp::Sle, rt, lt),
        (CmpOp::Ge, false) => (BvCmpOp::Ule, rt, lt),
        (CmpOp::Eq | CmpOp::Ne, _) => unreachable!("handled above"),
    };
    Term::bv_cmp(cmp, l, r).map_err(|e| format!("internal sort error: {e}"))
}

/// Lower a non-literal integer operand; return (term, width, signed).
/// len()/cap() are unsigned 64-bit by construction.
fn int_operand(
    p: &Program,
    names: &BTreeMap<String, Binding>,
    e: &Expr,
    allow_results: bool,
) -> Result<(Term, u32, bool), String> {
    let t = lower(p, names, e, allow_results)?;
    let Sort::BitVec(w) = t.sort() else {
        return Err("ordered comparison needs an integer operand".to_string());
    };
    let w = *w;
    let signed = expr_signedness(p, names, e).unwrap_or(true);
    Ok((t, w, signed))
}

/// Signedness of an expression from its Go type: idents via int_repr;
/// len/cap are non-negative 64-bit values, compared signed (they fit in
/// i64 — Go len() is int) which matches bounds.rs's Slt/Sle use. No
/// `allow_results` parameter: unlike `lower`, signedness lookup never
/// distinguishes requires/ensures context — clippy's
/// `only_used_in_recursion` correctly flagged the brief's version, which
/// threaded the flag through purely to forward it to its own recursive
/// calls without ever consulting it.
fn expr_signedness(p: &Program, names: &BTreeMap<String, Binding>, e: &Expr) -> Option<bool> {
    match e {
        Expr::Old(inner) | Expr::Not(inner) => expr_signedness(p, names, inner),
        Expr::Len(_) | Expr::Cap(_) => Some(true),
        Expr::Ident(name) => {
            let b = names.get(name)?;
            int_repr(p.types(), b.ty).map(|(_, s)| s)
        }
        _ => None,
    }
}

/// An integer literal at the operand's width; range-checked, negative
/// values two's-complement-masked (bounds.rs lit_sext convention). w <=
/// 64 always holds (int_repr caps at 64, len/cap are 64-bit); the guard
/// keeps the shifts below well-defined regardless.
fn lit(n: i128, w: u32, signed: bool) -> Result<Term, String> {
    if w == 0 || w > 64 {
        return Err(format!("unsupported operand width {w}"));
    }
    let fits = if signed {
        let min = -(1i128 << (w - 1));
        let max = (1i128 << (w - 1)) - 1;
        n >= min && n <= max
    } else {
        n >= 0 && (w == 128 || n < (1i128 << w))
    };
    if !fits {
        return Err(format!(
            "literal {n} does not fit the operand's {w}-bit type"
        ));
    }
    let mask = if w == 128 {
        u128::MAX
    } else {
        (1u128 << w) - 1
    };
    Ok(Term::bv_lit(w, (n as u128) & mask))
}

#[cfg(test)]
mod tests {
    use goverify_ir::testutil::load_corpus;

    use super::*;

    #[test]
    fn hello_requires_compiles_to_nonnil_clause() {
        let p = load_corpus("hello");
        let ann = compile_program(&p, &["nil", "bounds"]);
        let f = p.lookup_func("example.com/hello.Deref").unwrap();
        let fa = ann.funcs.get(&f).expect("Deref annotations");
        assert_eq!(fa.requires.len(), 1);
        assert_eq!(fa.requires[0].clause.tag, CONTRACT);
        assert_eq!(fa.requires[0].text, "p != nil");
        // Free vars: exactly p0.
        let vars = fa.requires[0].clause.formula.term.free_vars();
        assert_eq!(vars.keys().collect::<Vec<_>>(), vec!["p0"]);
        assert!(ann.findings.is_empty(), "hello has no bad annotations");
    }

    #[test]
    fn unknown_name_is_a_bad_annotation() {
        let p = load_corpus("hello");
        let f = p.lookup_func("example.com/hello.Deref").unwrap();
        let err = compile_one(&p, f, "//goverify:requires q != nil", &["nil"])
            .expect_err("q is not a param of Deref");
        assert!(err.contains("unknown name"), "got: {err}");
    }

    #[test]
    fn requires_referencing_result_is_rejected() {
        let p = load_corpus("hello");
        let f = p.lookup_func("example.com/hello.Deref").unwrap();
        let err = compile_one(&p, f, "//goverify:requires ret != 0", &["nil"])
            .expect_err("requires may only reference params");
        assert!(err.contains("may only reference parameters"), "got: {err}");
    }

    #[test]
    fn field_selection_is_rejected() {
        let p = load_corpus("hello");
        let f = p.lookup_func("example.com/hello.Deref").unwrap();
        let err = compile_one(&p, f, "//goverify:requires p.buf != nil", &["nil"])
            .expect_err("field selection is reserved");
        assert!(err.contains("field selection"), "got: {err}");
    }

    #[test]
    fn nil_on_int_param_is_rejected() {
        let p = load_corpus("hello");
        let f = p.lookup_func("example.com/hello.Add").unwrap();
        let err = compile_one(&p, f, "//goverify:requires a != nil", &["nil"])
            .expect_err("a is an int, not a pointer");
        assert!(err.contains("pointer/interface operand"), "got: {err}");
    }

    #[test]
    fn unknown_ignore_checker_is_rejected() {
        let p = load_corpus("hello");
        let f = p.lookup_func("example.com/hello.Deref").unwrap();
        let err = compile_one(
            &p,
            f,
            "//goverify:ignore made-up-checker",
            &["nil", "bounds"],
        )
        .expect_err("made-up-checker is not in known_checkers");
        assert!(err.contains("unknown checker"), "got: {err}");
    }

    #[test]
    fn compile_program_reports_bad_annotations_as_findings() {
        let p = load_corpus("hello");
        let ann = compile_program(&p, &["nil"]);
        // hello.go's real pragma is well-formed; findings must stay empty
        // (regression guard: a bug here would silently swallow every
        // future corpus fixture's bad-annotation coverage).
        assert!(ann.findings.is_empty());
    }
}
