pub mod analysis;
pub mod intern;
pub mod loader;
pub mod rule;
pub mod symbol;
pub mod terminal;

pub use intern::{
    lower, CompiledGrammar, CompiledRule, SymbolId, SymbolInfo, SymbolKind, SymbolTable,
};
pub use loader::{load_grammar, load_grammar_with_base, load_grammar_with_sources, AnonKind};

use std::collections::HashMap;

use rule::Rule;
use terminal::TerminalDef;

/// The surface grammar produced by the loader: symbols identified by name.
///
/// This is lowered to a [`CompiledGrammar`] (see [`intern`]) before the engine
/// uses it; the engine never reads symbol names off this representation.
#[derive(Debug, Clone)]
pub struct Grammar {
    pub rules: Vec<Rule>,
    pub terminals: Vec<TerminalDef>,
    /// Terminal names that should be discarded (from %ignore)
    pub ignore: Vec<String>,
    pub start: Vec<String>,
    /// Source provenance of every generated anonymous EBNF helper rule, keyed by
    /// rule-origin name. A name appears here iff the loader minted it via
    /// `fresh_anon_rule` (a `*`/`?`/`~n`/group/`[…]` helper) — *not* because it is
    /// spelled `__anon_*`, which a user grammar may also author (#144). Lowering
    /// copies this onto [`SymbolInfo::anon_kind`] so the engine can key empty-rule
    /// rejection on provenance, not name spelling (#101, ADR-0024).
    pub anon_kinds: HashMap<String, AnonKind>,
    /// Optional Python-faithful **audit shadow** of this grammar, used only by the
    /// LALR build to detect a reduce/reduce collision the load-bearing EBNF helper
    /// *sharing* (ADR-0013) masks (RC7/#272). It is this same grammar re-lowered
    /// with recurse helpers keyed on the inner *source-AST* (Python Lark's
    /// `EBNF_to_BNF._add_recurse_rule`), so `r0*` and `(r0)*` get distinct helpers
    /// exactly as Python mints them. `build_lalr` runs the real conflict detector
    /// over the shadow's lowering and surfaces any `Conflict` it finds; the shadow
    /// never parses input. `None` means no recurse helper was over-shared (nothing
    /// to audit) or this grammar *is* the shadow (the audit does not recurse — a
    /// shadow's own `lalr_audit` is always `None`). Set once by the loader. NB the
    /// derived [`Clone`] deep-copies this `Box` like any other field; `Grammar` is
    /// not cloned on any build path, so the shadow is never duplicated in practice.
    ///
    /// `pub(crate)`: this is internal build machinery (set by the loader, read only by
    /// the LALR build, the import resolver, and standalone generation — all inside the
    /// crate), so it is deliberately **not** part of the public API. Keeping it crate-
    /// private means adding the field is not a public-API break (RC7/#272 follow-up).
    pub(crate) lalr_audit: Option<Box<Grammar>>,
}
