pub mod symbol;
pub mod rule;
pub mod terminal;
pub mod loader;
pub mod analysis;
pub mod intern;

pub use loader::load_grammar;
pub use intern::{lower, CompiledGrammar, CompiledRule, SymbolId, SymbolInfo, SymbolKind, SymbolTable};

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
}
