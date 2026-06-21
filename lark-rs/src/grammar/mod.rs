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
    /// rejection on provenance, not name spelling (#101, ADR-0021).
    pub anon_kinds: HashMap<String, AnonKind>,
}
