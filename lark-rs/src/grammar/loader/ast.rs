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

/// The `%override` / `%extend` modifier a rule or terminal definition may carry.
/// `Plain` is an ordinary (first) definition; `Override` replaces a pre-existing
/// definition's body outright; `Extend` prepends new alternatives to a
/// pre-existing definition. Both directives require the target to pre-exist —
/// matching Python Lark's `_define(override=True)` / `_extend` (`load_grammar.py`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Directive {
    Plain,
    Override,
    Extend,
}

#[derive(Debug, Clone)]
pub(super) struct RawRule {
    pub(super) name: String,
    pub(super) modifiers: String,
    pub(super) params: Vec<String>,
    pub(super) priority: i32,
    pub(super) expansions: Vec<AliasedExpansion>,
    pub(super) directive: Directive,
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
        kind: RepeatKind,
    },
    Group(Vec<AliasedExpansion>),
    Maybe(Vec<AliasedExpansion>),
}

/// Which surface operator produced a [`Expr::Repeat`]. The two cases that share a
/// `(min, max)` — `X?` and `X~0..1` (both `min: 0, max: Some(1)`) — diverge under
/// `maybe_placeholders`: `?` is Python's `maybe()` (the empty arm inherits the
/// inner's placeholders, so `([A])?` parses `""` to `[None]`), whereas `~0..1` is
/// `_generate_repeats` whose `k == 0` count is a *pristine* empty expansion with no
/// placeholder, so `[A]~0..1` parses `""` to `[]`. The parser tags each node so
/// `compile_slot` can route `?`/`*`/`+` through the maybe-bearing distribution and
/// every `~n..m` (including `~0..1`) through the placeholder-free `inline_repeat`
/// fan-out (#258).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RepeatKind {
    /// A `?` / `*` / `+` operator (Python's `maybe` / `EBNF_to_BNF` recurse).
    Op,
    /// A `~n` / `~n..m` repetition (Python's `_generate_repeats`).
    Tilde,
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
    pub(super) directive: Directive,
}

#[derive(Debug, Clone)]
pub(super) struct ImportSpec {
    pub(super) path: Vec<String>, // e.g. ["common"] or [".", "mylib"]
    pub(super) relative: bool,
    pub(super) names: Option<Vec<String>>,
    pub(super) alias: Option<String>,
}
