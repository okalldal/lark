//! Grammar analysis: NULLABLE and FIRST sets over the interned grammar.
//!
//! These feed the LALR(1) lookahead computation (`first_of_seq`). FOLLOW sets
//! are intentionally absent: the parser uses true LALR(1) lookaheads
//! (spontaneous-generation + propagation), not SLR FOLLOW sets, so FOLLOW would
//! be dead weight.

use std::collections::HashSet;

use super::intern::{CompiledGrammar, SymbolId};

/// Pre-computed analysis of a grammar. All sets are over interned [`SymbolId`]s.
#[derive(Debug, Clone)]
pub struct GrammarAnalysis {
    /// `nullable[id]` = the symbol can derive ε (always false for terminals).
    nullable: Vec<bool>,
    /// `first[nonterminal_index]` = terminals that can start the non-terminal.
    first: Vec<HashSet<SymbolId>>,
    n_terminals: usize,
}

impl GrammarAnalysis {
    pub fn compute(grammar: &CompiledGrammar) -> Self {
        let n_symbols = grammar.symbols.len();
        let n_terminals = grammar.n_terminals();
        let nullable = compute_nullable(grammar, n_symbols);
        let first = compute_first(grammar, &nullable);
        GrammarAnalysis {
            nullable,
            first,
            n_terminals,
        }
    }

    #[inline]
    fn is_terminal(&self, id: SymbolId) -> bool {
        id.index() < self.n_terminals
    }

    #[inline]
    pub fn is_nullable(&self, id: SymbolId) -> bool {
        self.nullable[id.index()]
    }

    #[inline]
    fn first_of(&self, nonterminal: SymbolId) -> &HashSet<SymbolId> {
        &self.first[nonterminal.index() - self.n_terminals]
    }

    /// FIRST set of a symbol sequence. Returns the terminals that can begin it
    /// and whether the whole sequence is nullable.
    pub fn first_of_seq(&self, seq: &[SymbolId]) -> (HashSet<SymbolId>, bool) {
        let mut result = HashSet::new();
        for &sym in seq {
            if self.is_terminal(sym) {
                result.insert(sym);
                return (result, false);
            }
            result.extend(self.first_of(sym).iter().copied());
            if !self.is_nullable(sym) {
                return (result, false);
            }
        }
        (result, true)
    }
}

fn compute_nullable(grammar: &CompiledGrammar, n_symbols: usize) -> Vec<bool> {
    let mut nullable = vec![false; n_symbols];
    let mut changed = true;
    while changed {
        changed = false;
        for rule in &grammar.rules {
            if nullable[rule.origin.index()] {
                continue;
            }
            // Terminals are never nullable, so any terminal in the expansion
            // (whose slot stays false) blocks this.
            if rule.expansion.iter().all(|s| nullable[s.index()]) {
                nullable[rule.origin.index()] = true;
                changed = true;
            }
        }
    }
    nullable
}

fn compute_first(grammar: &CompiledGrammar, nullable: &[bool]) -> Vec<HashSet<SymbolId>> {
    let n_terminals = grammar.n_terminals();
    let mut first: Vec<HashSet<SymbolId>> = vec![HashSet::new(); grammar.symbols.n_nonterminals()];
    let nt_index = |id: SymbolId| id.index() - n_terminals;
    let is_terminal = |id: SymbolId| id.index() < n_terminals;

    let mut changed = true;
    while changed {
        changed = false;
        for rule in &grammar.rules {
            let origin = nt_index(rule.origin);
            for &sym in &rule.expansion {
                if is_terminal(sym) {
                    if first[origin].insert(sym) {
                        changed = true;
                    }
                    break; // a terminal blocks further propagation
                }
                // Non-terminal: fold in its FIRST set (clone to avoid aliasing
                // the mutable borrow of `first[origin]`).
                let src: Vec<SymbolId> = first[nt_index(sym)].iter().copied().collect();
                for t in src {
                    if first[origin].insert(t) {
                        changed = true;
                    }
                }
                if !nullable[sym.index()] {
                    break;
                }
            }
        }
    }
    first
}
