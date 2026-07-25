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

/// Strip control characters from untrusted text before it reaches a
/// message a renderer will print. Shared by `expr_text` (the quoted
/// expression) and `bad` (the parse/resolve error string) — both can
/// carry raw pragma bytes verbatim (`parse_pragma`'s "unknown directive
/// `{other}`"/"unexpected character `{c}`" arms embed the offending
/// bytes directly), and the human renderer sanitizes file paths but NOT
/// finding messages.
fn strip_control(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).collect()
}

/// The expression/payload part of the pragma line, for messages: the
/// `//goverify:` prefix AND the directive keyword (`requires`/`ensures`)
/// are both stripped, leaving just the expression text (mirrors
/// `parse_pragma`'s own directive/payload split).
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
    strip_control(payload)
}

fn bad(func: &str, pos: Option<goverify_ir::Pos>, msg: &str) -> Finding {
    Finding {
        checker: BAD_ANNOTATION.to_string(),
        tag: BAD_ANNOTATION.to_string(),
        func: func.to_string(),
        pos,
        message: format!("invalid annotation: {}", strip_control(msg)),
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
    fn compile_program_over_hello_corpus_has_no_bad_annotations() {
        let p = load_corpus("hello");
        let ann = compile_program(&p, &["nil"]);
        // hello.go's real pragma is well-formed; findings must stay empty
        // (regression guard: a bug here would silently swallow every
        // future corpus fixture's bad-annotation coverage).
        assert!(ann.findings.is_empty());
    }

    // ---- bad() itself: Finding shape + control-char sanitization -----

    #[test]
    fn bad_produces_error_finding_with_invalid_annotation_prefix() {
        let f = bad("f", None, "boom");
        assert_eq!(f.checker, BAD_ANNOTATION);
        assert_eq!(f.tag, BAD_ANNOTATION);
        assert_eq!(f.func, "f");
        assert_eq!(f.severity, Severity::Error);
        assert_eq!(f.message, "invalid annotation: boom");
        assert!(f.trace.is_empty());
        assert!(f.model.is_empty());
    }

    #[test]
    fn unmatched_pragma_message_matches_compile_program_wording() {
        // Pins the exact literal `compile_program`'s unmatched-pragma arm
        // passes to `bad` with func "-" (no corpus fixture currently
        // carries a genuinely unmatched pragma to round-trip through
        // `Program::unmatched_pragmas` end-to-end).
        let f = bad(
            "-",
            None,
            "annotation is not attached to a function declaration",
        );
        assert_eq!(f.func, "-");
        assert_eq!(
            f.message,
            "invalid annotation: annotation is not attached to a function declaration"
        );
    }

    #[test]
    fn compile_program_bad_annotation_findings_have_expected_shape() {
        let p = load_corpus("hello");
        let f = p.lookup_func("example.com/hello.Deref").unwrap();
        let err = compile_one(&p, f, "//goverify:requires q != nil", &["nil"]).unwrap_err();
        let finding = bad(p.func_name(f), None, &err);
        assert_eq!(finding.checker, BAD_ANNOTATION);
        assert_eq!(finding.tag, BAD_ANNOTATION);
        assert_eq!(finding.severity, Severity::Error);
        assert!(
            finding.message.starts_with("invalid annotation: "),
            "{}",
            finding.message
        );
        assert!(
            finding.message.contains("unknown name"),
            "{}",
            finding.message
        );
    }

    #[test]
    fn bad_annotation_message_strips_control_chars_from_untrusted_pragma_bytes() {
        let p = load_corpus("hello");
        let f = p.lookup_func("example.com/hello.Deref").unwrap();
        // `parse_pragma`'s "unknown directive" arm embeds the raw
        // directive token verbatim; an ESC[2J clear-screen sequence here
        // models a hostile pragma line the extractor captured byte-exact
        // from a Go comment.
        let raw = "//goverify:\u{1b}[2Jfoo bar";
        let err = compile_one(&p, f, raw, &["nil"]).expect_err("unknown directive");
        assert!(
            err.chars().any(|c| c.is_control()),
            "sanity check: compile_one's raw error must still carry the control byte \
             (compile_one itself does not sanitize — only `bad` does)"
        );
        let finding = bad(p.func_name(f), None, &err);
        assert!(
            !finding.message.chars().any(|c| c.is_control()),
            "bad() must strip control chars before they reach a finding message: {:?}",
            finding.message
        );
    }

    // ---- riskiest lowering rules: swap, signedness, literal masking, --
    // ---- len/cap accessors, unnamed/named result binding, name -------
    // ---- precedence, top-level sort, nil/int edge cases ---------------

    fn canonical(t: Term) -> String {
        goverify_solver::Query::for_asserts(goverify_solver::Logic::All, vec![t]).canonical_text()
    }

    /// `bounds` corpus: `get(s []int, i int) int` — one slice operand, one
    /// signed-int operand, loaded once for every assertion below.
    #[test]
    fn swap_signedness_and_seq_accessors_over_bounds_corpus() {
        let p = load_corpus("bounds");
        let f = p.lookup_func("example.com/bounds.get").expect("get");
        let requires = |text: &str| -> Term {
            let Compiled::Requires(c) = compile_one(&p, f, text, &["nil", "bounds"])
                .unwrap_or_else(|e| panic!("{text}: {e}"))
            else {
                panic!("expected Requires")
            };
            c.formula.term
        };

        // `>` swaps to `0 < i` (Slt, signed: i is a plain `int`).
        let text = canonical(requires("//goverify:requires i > 0"));
        assert!(
            text.contains("(bvslt (_ bv0 64) p1)"),
            "`i > 0` must lower via operand swap to `0 < i`, signed:\n{text}"
        );

        // `>=` swaps to `0 <= i` (Sle).
        let text = canonical(requires("//goverify:requires i >= 0"));
        assert!(
            text.contains("(bvsle (_ bv0 64) p1)"),
            "`i >= 0` must lower via operand swap to `0 <= i`:\n{text}"
        );

        // `len(s)` accessor + forced-signed comparison (documented quirk:
        // len/cap are unsigned 64-bit values but compared SIGNED).
        let text = canonical(requires("//goverify:requires len(s) > 0"));
        assert!(
            text.contains("(seq-len p0)"),
            "len must use seq-len:\n{text}"
        );
        assert!(
            text.contains("(bvslt (_ bv0 64) (seq-len p0))"),
            "len() comparisons stay signed even though seq-len is unsigned:\n{text}"
        );

        // `cap(s)` accessor.
        let text = canonical(requires("//goverify:requires cap(s) > 0"));
        assert!(
            text.contains("(seq-cap p0)"),
            "cap must use seq-cap:\n{text}"
        );

        // Negative-literal masking: -1 at width 64 is all-ones.
        let text = canonical(requires("//goverify:requires i == -1"));
        assert!(
            text.contains("(_ bv18446744073709551615 64)"),
            "negative literal must two's-complement-mask to all-ones at width 64:\n{text}"
        );

        // Sort mismatch: Seq vs BitVec.
        let err = compile_one(&p, f, "//goverify:requires s == i", &["nil", "bounds"])
            .expect_err("s (slice) and i (int) have different sorts");
        assert!(err.contains("different types"), "got: {err}");

        // Out-of-range literal: 2^63 does not fit a 64-bit signed int.
        let err = compile_one(
            &p,
            f,
            "//goverify:requires i == 9223372036854775808",
            &["nil", "bounds"],
        )
        .expect_err("2^63 overflows a signed 64-bit int");
        assert!(err.contains("does not fit"), "got: {err}");
    }

    /// `knownfp` corpus: `BranchElemOffset(base uintptr, idx uint16)
    /// uintptr` — a genuinely unsigned-typed operand, which `bounds`/
    /// `hello`/`ensures` don't have; needed to exercise the Ult/Ule (not
    /// Slt/Sle) side of the signed/unsigned dispatch.
    #[test]
    fn unsigned_operand_selection_over_knownfp_corpus() {
        let p = load_corpus("knownfp");
        let f = p
            .lookup_func("example.com/knownfp.BranchElemOffset")
            .expect("BranchElemOffset");
        let text = canonical(
            match compile_one(&p, f, "//goverify:requires idx > 0", &["nil"]).unwrap() {
                Compiled::Requires(c) => c.formula.term,
                _ => panic!("expected Requires"),
            },
        );
        assert!(
            text.contains("(bvult (_ bv0 16) p1)"),
            "unsigned `idx > 0` must swap to unsigned Ult, not Slt:\n{text}"
        );

        // Negative literal on an unsigned operand: out of range.
        let err = compile_one(&p, f, "//goverify:requires idx == -1", &["nil"])
            .expect_err("-1 does not fit a uint16 operand");
        assert!(err.contains("does not fit"), "got: {err}");
    }

    /// `ensures` corpus: `NewT(fail bool) (*T, error)` (two UNNAMED
    /// results: ret0/ret1) and `NewTNamed(fail bool) (t *T, err error)`
    /// (two NAMED results: t/err) — the unnamed-fallback and
    /// declared-name paths of `bindings()`'s per-result loop.
    #[test]
    fn unnamed_and_named_result_binding_over_ensures_corpus() {
        let p = load_corpus("ensures");

        let new_t = p.lookup_func("example.com/ensures.NewT").expect("NewT");
        let ensures = |f: goverify_ir::FuncId, text: &str| -> Term {
            let Compiled::Ensures(c) =
                compile_one(&p, f, text, &["nil"]).unwrap_or_else(|e| panic!("{text}: {e}"))
            else {
                panic!("expected Ensures")
            };
            c.formula.term
        };

        // Unnamed results: ret0/ret1 bind to r0/r1.
        let vars = ensures(new_t, "//goverify:ensures ret0 != nil").free_vars();
        assert_eq!(vars.keys().collect::<Vec<_>>(), vec!["r0"], "ret0 -> r0");
        let vars = ensures(new_t, "//goverify:ensures ret1 == nil").free_vars();
        assert_eq!(vars.keys().collect::<Vec<_>>(), vec!["r1"], "ret1 -> r1");

        let new_t_named = p
            .lookup_func("example.com/ensures.NewTNamed")
            .expect("NewTNamed");

        // Named results: declared names (t, err) bind to r0/r1 directly.
        let vars = ensures(new_t_named, "//goverify:ensures t != nil").free_vars();
        assert_eq!(vars.keys().collect::<Vec<_>>(), vec!["r0"], "t -> r0");
        let vars = ensures(new_t_named, "//goverify:ensures err == nil").free_vars();
        assert_eq!(vars.keys().collect::<Vec<_>>(), vec!["r1"], "err -> r1");

        // Declared-name precedence: once results are NAMED, the synthetic
        // ret0/ret1 aliases do not exist at all — only the declared names
        // resolve (spec §3 "declared names win").
        let err = compile_one(&p, new_t_named, "//goverify:ensures ret0 != nil", &["nil"])
            .expect_err("NewTNamed's results are named; ret0 must not resolve");
        assert!(err.contains("unknown name"), "got: {err}");
    }

    /// A hand-built temp module with a genuine name COLLISION: a param
    /// literally named `ret` on a function whose single result is
    /// UNNAMED (whose synthetic aliases are exactly "ret"/"ret0", spec
    /// §3). No existing corpus fixture has this shape, so this
    /// constructs one directly via `testutil::load_module` rather than
    /// relying on indirect reasoning from named-vs-unnamed functions.
    #[test]
    fn declared_name_wins_over_synthetic_ret_alias() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("go.mod"),
            "module example.com/collide\n\ngo 1.25.10\n",
        )
        .expect("write go.mod");
        std::fs::write(
            dir.path().join("collide.go"),
            "package collide\n\nfunc F(ret int) int { return ret }\n",
        )
        .expect("write collide.go");
        let p = goverify_ir::testutil::load_module(dir.path());
        let f = p.lookup_func("example.com/collide.F").expect("F");
        let Compiled::Requires(c) = compile_one(&p, f, "//goverify:requires ret > 0", &["nil"])
            .expect("declared param `ret` must resolve")
        else {
            panic!("expected Requires")
        };
        let vars = c.formula.term.free_vars();
        assert_eq!(
            vars.keys().collect::<Vec<_>>(),
            vec!["p0"],
            "`ret` must resolve to the declared PARAM (p0), never the synthetic \
             unnamed-result alias (r0) — declared names win (spec §3)"
        );
    }

    /// `hello` corpus (`Deref(p *int) int`): top-level sort enforcement
    /// and every nil/literal edge case that doesn't need a slice/result/
    /// unsigned operand.
    #[test]
    fn top_level_sort_and_nil_literal_edge_cases_over_hello_corpus() {
        let p = load_corpus("hello");
        let f = p.lookup_func("example.com/hello.Deref").expect("Deref");
        let reject = |text: &str| -> String { compile_one(&p, f, text, &["nil"]).expect_err(text) };

        // Top-level non-Bool: `p` alone is Ptr-sorted, not Bool.
        let err = reject("//goverify:requires p");
        assert!(err.contains("expected a boolean condition"), "got: {err}");

        // Bare `nil`.
        let err = reject("//goverify:requires nil");
        assert!(err.contains("only usable in == / !="), "got: {err}");

        // Bare integer literal.
        let err = reject("//goverify:requires 5");
        assert!(err.contains("not a condition"), "got: {err}");

        // `nil == nil`.
        let err = reject("//goverify:requires nil == nil");
        assert!(err.contains("not a useful condition"), "got: {err}");

        // `nil` with an ordered comparator.
        let err = reject("//goverify:requires p < nil");
        assert!(err.contains("only supports == and !="), "got: {err}");

        // Two integer literals compared.
        let err = reject("//goverify:requires 1 == 2");
        assert!(err.contains("comparing two integer literals"), "got: {err}");
    }
}
