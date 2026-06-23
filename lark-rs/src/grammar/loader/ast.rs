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

impl Expr {
    /// A structural string key for the *source* AST subtree, used by the
    /// post-lowering reduce/reduce audit (ADR-0013, RC7/#272) to reproduce Python
    /// Lark's `EBNF_to_BNF._add_recurse_rule` sharing decision. Python keys its
    /// `rules_cache` on the inner `expr` **Tree** — so `r0*` (inner `value(r0)`) and
    /// `(r0)*` (inner `expansions(expansion(value(r0)))`) get *distinct* star
    /// helpers, whereas lark-rs's real `recurse_cache` keys on the *compiled* arms
    /// (which collapse the single-symbol group wrapper) and so shares one helper.
    /// This key preserves the full group-nesting structure of the source, matching
    /// Python's verdict that `r0* | ((r0))*` splits but `((r0))* | ((r0))*` shares
    /// (verified against Python Lark 1.3.1). It is only ever a *cache key* for the
    /// audit shadow grammar — never a rule name or a parsed value.
    pub(super) fn python_recurse_key(&self) -> String {
        let mut s = String::new();
        self.write_recurse_key(&mut s);
        s
    }

    fn write_recurse_key(&self, out: &mut String) {
        match self {
            Expr::Value(v) => {
                out.push_str("V(");
                v.write_recurse_key(out);
                out.push(')');
            }
            Expr::Repeat {
                inner,
                min,
                max,
                kind,
            } => {
                out.push_str(&format!("R{min}_{max:?}_{kind:?}("));
                inner.write_recurse_key(out);
                out.push(')');
            }
            Expr::Group(alts) => {
                out.push_str("G(");
                Self::write_alts_key(alts, out);
                out.push(')');
            }
            Expr::Maybe(alts) => {
                out.push_str("M(");
                Self::write_alts_key(alts, out);
                out.push(')');
            }
        }
    }

    fn write_alts_key(alts: &[AliasedExpansion], out: &mut String) {
        for (i, alt) in alts.iter().enumerate() {
            if i > 0 {
                out.push('|');
            }
            if let Some(a) = &alt.alias {
                out.push_str("->");
                out.push_str(a);
                out.push(':');
            }
            for (j, e) in alt.expansion.iter().enumerate() {
                if j > 0 {
                    out.push(' ');
                }
                e.write_recurse_key(out);
            }
        }
    }
}

impl Value {
    fn write_recurse_key(&self, out: &mut String) {
        match self {
            Value::Terminal(n) => {
                out.push_str("T:");
                out.push_str(n);
            }
            Value::Rule(n) => {
                out.push_str("r:");
                out.push_str(n);
            }
            Value::Literal(LiteralVal::Str(s, ci)) => {
                out.push_str(&format!("Ls:{ci}:{s:?}"));
            }
            Value::Literal(LiteralVal::Re(p, f)) => {
                out.push_str(&format!("Lr:{f}:{p:?}"));
            }
            Value::Range(a, b) => {
                out.push_str(&format!("Rng:{a:?}..{b:?}"));
            }
            Value::TemplateUsage { name, args } => {
                out.push_str("Tpl:");
                out.push_str(name);
                out.push('<');
                for (i, a) in args.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    a.write_recurse_key(out);
                }
                out.push('>');
            }
        }
    }
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
