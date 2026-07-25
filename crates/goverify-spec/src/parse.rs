//! Pragma-line parser (phase-6 spec §2). Untrusted input (parent spec
//! §11): total, never panics, depth/node caps. Errors are one-line
//! human strings quoted into bad-annotation findings.

use crate::ast::{CmpOp, Directive, Expr};

const PREFIX: &str = "//goverify:";
/// Recursion cap: expressions deeper than this are rejected.
const MAX_DEPTH: u32 = 32;
/// Token cap: pragma lines are single source lines; anything past this
/// is hostile or generated.
const MAX_TOKENS: usize = 256;

/// Parse a full pragma line (including the `//goverify:` prefix).
/// A trailing `// comment` on the payload is stripped before parsing.
pub fn parse_pragma(text: &str) -> Result<Directive, String> {
    let rest = text
        .strip_prefix(PREFIX)
        .ok_or_else(|| "not a //goverify: pragma".to_string())?;
    // Strip a trailing //-comment; expressions cannot contain "//"
    // (no division, no strings in the grammar).
    let rest = match rest.find("//") {
        Some(i) => &rest[..i],
        None => rest,
    };
    let rest = rest.trim();
    let (directive, payload) = match rest.find(char::is_whitespace) {
        Some(i) => (&rest[..i], rest[i..].trim()),
        None => (rest, ""),
    };
    match directive {
        "requires" | "ensures" => {
            if payload.is_empty() {
                return Err(format!("`{directive}` needs an expression"));
            }
            let e = parse_expr_str(payload)?;
            Ok(if directive == "requires" {
                Directive::Requires(e)
            } else {
                Directive::Ensures(e)
            })
        }
        "ignore" => {
            let name = payload;
            let valid = !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
            if !valid {
                return Err("`ignore` needs a checker name ([a-z0-9-]+)".to_string());
            }
            Ok(Directive::Ignore(name.to_string()))
        }
        "effects" | "pure" => Err(format!(
            "directive `{directive}` is not supported in this version"
        )),
        "" => Err("empty pragma".to_string()),
        other => Err(format!("unknown directive `{other}`")),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    Ident(String),
    Int(i128),
    Punct(&'static str), // one of: ( ) . ! == != <= >= < > && || ==> -
}

fn lex(s: &str) -> Result<Vec<Tok>, String> {
    let mut out = Vec::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let c = b[i] as char;
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if out.len() >= MAX_TOKENS {
            return Err("expression too long".to_string());
        }
        if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            while i < b.len() && ((b[i] as char).is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            out.push(Tok::Ident(s[start..i].to_string()));
            continue;
        }
        if c.is_ascii_digit() {
            let start = i;
            while i < b.len() && (b[i] as char).is_ascii_digit() {
                i += 1;
            }
            let n: i128 = s[start..i]
                .parse()
                .map_err(|_| format!("integer literal `{}` out of range", &s[start..i]))?;
            out.push(Tok::Int(n));
            continue;
        }
        // Longest-match punctuation. "==>" before "==".
        let rest = &s[i..];
        let p = [
            "==>", "==", "!=", "<=", ">=", "&&", "||", "(", ")", ".", "!", "<", ">", "-",
        ]
        .into_iter()
        .find(|p| rest.starts_with(p));
        match p {
            Some(p) => {
                out.push(Tok::Punct(p));
                i += p.len();
            }
            None => return Err(format!("unexpected character `{c}`")),
        }
    }
    Ok(out)
}

struct P {
    toks: Vec<Tok>,
    at: usize,
}

impl P {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.at)
    }
    fn eat_punct(&mut self, p: &str) -> bool {
        if let Some(Tok::Punct(q)) = self.peek()
            && *q == p
        {
            self.at += 1;
            return true;
        }
        false
    }
}

fn parse_expr_str(s: &str) -> Result<Expr, String> {
    let toks = lex(s)?;
    let mut p = P { toks, at: 0 };
    let e = expr(&mut p, 0)?;
    if p.at != p.toks.len() {
        return Err("trailing tokens after expression".to_string());
    }
    Ok(e)
}

// expr := or ("==>" expr)?   (right-assoc, lowest precedence)
fn expr(p: &mut P, d: u32) -> Result<Expr, String> {
    let d = depth(d)?;
    let lhs = or(p, d)?;
    if p.eat_punct("==>") {
        let rhs = expr(p, d)?;
        return Ok(Expr::Implies(Box::new(lhs), Box::new(rhs)));
    }
    Ok(lhs)
}

fn or(p: &mut P, d: u32) -> Result<Expr, String> {
    let d = depth(d)?;
    let mut lhs = and(p, d)?;
    while p.eat_punct("||") {
        let rhs = and(p, d)?;
        lhs = Expr::Or(Box::new(lhs), Box::new(rhs));
    }
    Ok(lhs)
}

fn and(p: &mut P, d: u32) -> Result<Expr, String> {
    let d = depth(d)?;
    let mut lhs = cmp(p, d)?;
    while p.eat_punct("&&") {
        let rhs = cmp(p, d)?;
        lhs = Expr::And(Box::new(lhs), Box::new(rhs));
    }
    Ok(lhs)
}

// cmp := unary (op unary)?   — non-associative: a < b < c is an error.
fn cmp(p: &mut P, d: u32) -> Result<Expr, String> {
    let d = depth(d)?;
    let lhs = unary(p, d)?;
    let op = [
        ("==", CmpOp::Eq),
        ("!=", CmpOp::Ne),
        ("<=", CmpOp::Le),
        (">=", CmpOp::Ge),
        ("<", CmpOp::Lt),
        (">", CmpOp::Gt),
    ]
    .into_iter()
    .find(|(t, _)| p.eat_punct(t));
    match op {
        Some((_, op)) => {
            let rhs = unary(p, d)?;
            Ok(Expr::Cmp(op, Box::new(lhs), Box::new(rhs)))
        }
        None => Ok(lhs),
    }
}

fn unary(p: &mut P, d: u32) -> Result<Expr, String> {
    let d = depth(d)?;
    if p.eat_punct("!") {
        let e = unary(p, d)?;
        return Ok(Expr::Not(Box::new(e)));
    }
    if p.eat_punct("-") {
        return match p.peek().cloned() {
            Some(Tok::Int(n)) => {
                p.at += 1;
                Ok(Expr::Int(-n))
            }
            _ => Err("`-` is only supported on integer literals".to_string()),
        };
    }
    primary(p, d)
}

fn primary(p: &mut P, d: u32) -> Result<Expr, String> {
    let d = depth(d)?;
    let e = match p.peek().cloned() {
        Some(Tok::Int(n)) => {
            p.at += 1;
            Expr::Int(n)
        }
        Some(Tok::Punct("(")) => {
            p.at += 1;
            let e = expr(p, d)?;
            if !p.eat_punct(")") {
                return Err("missing `)`".to_string());
            }
            e
        }
        Some(Tok::Ident(id)) => {
            p.at += 1;
            match id.as_str() {
                "true" => Expr::Bool(true),
                "false" => Expr::Bool(false),
                "nil" => Expr::Nil,
                "len" | "cap" | "old" => {
                    if !p.eat_punct("(") {
                        return Err(format!("`{id}` needs `(`"));
                    }
                    let inner = expr(p, d)?;
                    if !p.eat_punct(")") {
                        return Err("missing `)`".to_string());
                    }
                    let b = Box::new(inner);
                    match id.as_str() {
                        "len" => Expr::Len(b),
                        "cap" => Expr::Cap(b),
                        _ => Expr::Old(b),
                    }
                }
                _ => Expr::Ident(id),
            }
        }
        _ => return Err("expected expression".to_string()),
    };
    // Postfix field selection (parsed; resolution rejects it).
    let mut e = e;
    while p.eat_punct(".") {
        match p.peek().cloned() {
            Some(Tok::Ident(f)) => {
                p.at += 1;
                e = Expr::Select(Box::new(e), f);
            }
            _ => return Err("expected field name after `.`".to_string()),
        }
    }
    Ok(e)
}

fn depth(d: u32) -> Result<u32, String> {
    if d >= MAX_DEPTH {
        return Err("expression too deeply nested".to_string());
    }
    Ok(d + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{Directive, Expr};

    #[test]
    fn accepts_spec_examples() {
        for text in [
            "//goverify:requires p != nil && n >= 0",
            "//goverify:ensures err == nil ==> ret != nil",
            "//goverify:requires len(p) > 0",
            "//goverify:requires old(n) >= -1",
            "//goverify:ignore nil-deref   // trailing comment ok",
            "//goverify:requires (a || b) && !c",
        ] {
            parse_pragma(text).unwrap_or_else(|e| panic!("{text}: {e}"));
        }
    }

    #[test]
    fn precedence_implies_lowest_right_assoc() {
        let Directive::Requires(e) = parse_pragma("//goverify:requires a ==> b ==> c").unwrap()
        else {
            panic!("directive")
        };
        // a ==> (b ==> c)
        assert!(matches!(e, Expr::Implies(_, ref r)
			if matches!(**r, Expr::Implies(_, _))));
    }

    #[test]
    fn cmp_non_associative() {
        assert!(parse_pragma("//goverify:requires a < b < c").is_err());
    }

    #[test]
    fn rejects_table() {
        for (text, needle) in [
            ("//goverify:effects locks(mu)", "not supported"),
            ("//goverify:pure", "not supported"),
            ("//goverify:frobnicate x", "unknown directive"),
            ("//goverify:requires", "needs an expression"),
            ("//goverify:ignore Nil_Deref", "checker name"),
            ("//goverify:ignore", "checker name"),
            ("//goverify:requires p +", "unexpected character"),
            ("//goverify:requires p != nil extra", "trailing tokens"),
            (
                "//goverify:requires 99999999999999999999999999999999999999999",
                "out of range",
            ),
        ] {
            let err = parse_pragma(text).expect_err(text);
            assert!(err.contains(needle), "{text}: got {err}");
        }
    }

    #[test]
    fn depth_cap_rejects_not_panics() {
        let deep = format!(
            "//goverify:requires {}p{}",
            "(".repeat(100),
            ")".repeat(100)
        );
        assert!(parse_pragma(&deep).is_err());
    }

    #[test]
    fn field_selection_parses() {
        // Reserved syntax: parse succeeds; Task 6 resolution rejects.
        let d = parse_pragma("//goverify:requires p.buf != nil").unwrap();
        assert!(matches!(d, Directive::Requires(_)));
    }
}
