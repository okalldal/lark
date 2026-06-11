//! Phase 2 output — the raw grammar AST the recursive-descent parser builds
//! and the compiler phases consume.

use crate::grammar::symbol::Symbol;

#[derive(Debug, Clone)]
pub(super) enum Item {
    RuleItem(RawRule),
    TermItem(RawTerm),
    /// Each element is one `%ignore` expansion (list of Exprs).
    IgnoreItem(Vec<Vec<Expr>>),
    ImportItem(ImportSpec),
    DeclareItem(Vec<Symbol>),
}

#[derive(Debug, Clone)]
pub(super) struct RawRule {
    pub(super) name: String,
    pub(super) modifiers: String,
    pub(super) params: Vec<String>,
    pub(super) priority: i32,
    pub(super) expansions: Vec<AliasedExpansion>,
}

#[derive(Debug, Clone)]
pub(super) struct AliasedExpansion {
    pub(super) expansion: Vec<Expr>,
    pub(super) alias: Option<String>,
}

#[derive(Debug, Clone)]
pub(super) enum Expr {
    Value(Value),
    Repeat {
        inner: Box<Expr>,
        min: usize,
        max: Option<usize>,
    },
    Group(Vec<AliasedExpansion>),
    Maybe(Vec<AliasedExpansion>),
}

#[derive(Debug, Clone)]
pub(super) enum Value {
    Terminal(String),
    Rule(String),
    Literal(LiteralVal),
    Range(String, String),
    TemplateUsage { name: String, args: Vec<Value> },
}

#[derive(Debug, Clone)]
pub(super) enum LiteralVal {
    Str(String, bool), // value, case-insensitive
    Re(String, u32),   // pattern, flags
}

#[derive(Debug, Clone)]
pub(super) struct RawTerm {
    pub(super) name: String,
    pub(super) priority: i32,
    pub(super) expansions: Vec<AliasedExpansion>,
}

#[derive(Debug, Clone)]
pub(super) struct ImportSpec {
    pub(super) path: Vec<String>, // e.g. ["common"] or [".", "mylib"]
    pub(super) relative: bool,
    pub(super) names: Option<Vec<String>>,
    pub(super) alias: Option<String>,
}
