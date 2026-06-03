pub mod symbol;
pub mod rule;
pub mod terminal;
pub mod loader;
pub mod analysis;
pub mod intern;

pub use loader::load_grammar;
pub use intern::{lower, CompiledGrammar, CompiledRule, SymbolId, SymbolInfo, SymbolKind, SymbolTable};

use std::collections::HashMap;
use rule::Rule;
use terminal::TerminalDef;

/// Compiled grammar ready for parser construction.
#[derive(Debug, Clone)]
pub struct Grammar {
    pub rules: Vec<Rule>,
    pub terminals: Vec<TerminalDef>,
    /// Terminal names that should be discarded (from %ignore)
    pub ignore: Vec<String>,
    pub start: Vec<String>,
}

impl Grammar {
    pub fn rules_for<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Rule> + 'a {
        self.rules.iter().filter(move |r| r.origin.name == name)
    }

    pub fn terminal(&self, name: &str) -> Option<&TerminalDef> {
        self.terminals.iter().find(|t| t.name == name)
    }

    pub fn terminal_map(&self) -> HashMap<&str, &TerminalDef> {
        self.terminals.iter().map(|t| (t.name.as_str(), t)).collect()
    }
}
