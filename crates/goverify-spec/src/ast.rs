//! Annotation expression AST (phase-6 spec §2). The grammar is a small
//! Go-syntax subset; field selection is PARSED but rejected at
//! resolution (reserved syntax, v1 has no interface-level heap terms).

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Directive {
    Requires(Expr),
    Ensures(Expr),
    /// Checker name to suppress within the annotated function.
    Ignore(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmpOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    Ident(String),
    /// base.field — parsed, rejected at resolution (spec §2).
    Select(Box<Expr>, String),
    Int(i128),
    Bool(bool),
    Nil,
    Len(Box<Expr>),
    Cap(Box<Expr>),
    /// old(e) — accepted synonym for entry values; resolution unwraps it.
    Old(Box<Expr>),
    Not(Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Implies(Box<Expr>, Box<Expr>),
    Cmp(CmpOp, Box<Expr>, Box<Expr>),
}
