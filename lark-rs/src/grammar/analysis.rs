//! Grammar analysis: FIRST, FOLLOW, NULLABLE sets.
//! These are needed by both the Earley and LALR parsers.

use std::collections::{HashMap, HashSet};
use super::{Grammar, symbol::*, rule::Rule};

/// Pre-computed analysis of a grammar.
#[derive(Debug, Clone)]
pub struct GrammarAnalysis {
    /// NULLABLE[A] = true iff A can derive the empty string.
    pub nullable: HashSet<NonTerminal>,
    /// FIRST[A] = set of terminals that can start a string derived from A.
    pub first: HashMap<NonTerminal, HashSet<Terminal>>,
    /// FOLLOW[A] = set of terminals that can follow A in any sentential form.
    pub follow: HashMap<NonTerminal, HashSet<Terminal>>,
}

/// The synthetic end-of-input terminal.
pub const END_TERMINAL: &str = "$END";

impl GrammarAnalysis {
    pub fn compute(grammar: &Grammar) -> Self {
        let nullable = compute_nullable(grammar);
        let first = compute_first(grammar, &nullable);
        let follow = compute_follow(grammar, &nullable, &first);
        GrammarAnalysis { nullable, first, follow }
    }

    /// FIRST set for a sequence of symbols (handles nullable prefixes).
    pub fn first_of_seq(&self, seq: &[Symbol]) -> (HashSet<Terminal>, bool) {
        let mut result = HashSet::new();
        let mut all_nullable = true;
        for sym in seq {
            match sym {
                Symbol::Terminal(t) => {
                    result.insert(t.clone());
                    all_nullable = false;
                    break;
                }
                Symbol::NonTerminal(nt) => {
                    if let Some(first_nt) = self.first.get(nt) {
                        result.extend(first_nt.iter().cloned());
                    }
                    if !self.nullable.contains(nt) {
                        all_nullable = false;
                        break;
                    }
                }
            }
        }
        (result, all_nullable)
    }
}

fn compute_nullable(grammar: &Grammar) -> HashSet<NonTerminal> {
    let mut nullable: HashSet<NonTerminal> = HashSet::new();
    let mut changed = true;
    while changed {
        changed = false;
        for rule in &grammar.rules {
            if nullable.contains(&rule.origin) {
                continue;
            }
            if rule.expansion.iter().all(|sym| match sym {
                Symbol::NonTerminal(nt) => nullable.contains(nt),
                Symbol::Terminal(_) => false,
            }) {
                nullable.insert(rule.origin.clone());
                changed = true;
            }
        }
    }
    nullable
}

fn compute_first(
    grammar: &Grammar,
    nullable: &HashSet<NonTerminal>,
) -> HashMap<NonTerminal, HashSet<Terminal>> {
    let mut first: HashMap<NonTerminal, HashSet<Terminal>> = HashMap::new();
    // Initialise empty sets for all nonterminals
    for rule in &grammar.rules {
        first.entry(rule.origin.clone()).or_default();
    }

    let mut changed = true;
    while changed {
        changed = false;
        for rule in &grammar.rules {
            for sym in &rule.expansion {
                match sym {
                    Symbol::Terminal(t) => {
                        let set = first.entry(rule.origin.clone()).or_default();
                        if set.insert(t.clone()) { changed = true; }
                        break; // Terminal blocks further propagation
                    }
                    Symbol::NonTerminal(nt) => {
                        let nt_first: Vec<Terminal> = first
                            .get(nt)
                            .map(|s| s.iter().cloned().collect())
                            .unwrap_or_default();
                        let set = first.entry(rule.origin.clone()).or_default();
                        for t in nt_first {
                            if set.insert(t) { changed = true; }
                        }
                        if !nullable.contains(nt) {
                            break;
                        }
                    }
                }
            }
        }
    }
    first
}

fn compute_follow(
    grammar: &Grammar,
    nullable: &HashSet<NonTerminal>,
    first: &HashMap<NonTerminal, HashSet<Terminal>>,
) -> HashMap<NonTerminal, HashSet<Terminal>> {
    let mut follow: HashMap<NonTerminal, HashSet<Terminal>> = HashMap::new();
    for rule in &grammar.rules {
        follow.entry(rule.origin.clone()).or_default();
        for sym in &rule.expansion {
            if let Symbol::NonTerminal(nt) = sym {
                follow.entry(nt.clone()).or_default();
            }
        }
    }

    // Add $END to all start symbols
    let end_t = Terminal::new(END_TERMINAL);
    for start in &grammar.start {
        follow.entry(NonTerminal::new(start)).or_default()
            .insert(end_t.clone());
    }

    let mut changed = true;
    while changed {
        changed = false;
        for rule in &grammar.rules {
            // Walk expansion right-to-left
            let n = rule.expansion.len();
            // Trailing FOLLOW: everything in FOLLOW(origin) propagates to last non-nullable suffix
            let mut trailer: HashSet<Terminal> = follow
                .get(&rule.origin)
                .cloned()
                .unwrap_or_default();

            for i in (0..n).rev() {
                match &rule.expansion[i] {
                    Symbol::NonTerminal(nt) => {
                        let cur = follow.entry(nt.clone()).or_default();
                        let before = cur.len();
                        cur.extend(trailer.iter().cloned());
                        if cur.len() != before { changed = true; }

                        if nullable.contains(nt) {
                            // Also add FIRST(nt) to trailer
                            if let Some(f) = first.get(nt) {
                                trailer.extend(f.iter().cloned());
                            }
                        } else {
                            trailer = first.get(nt).cloned().unwrap_or_default();
                        }
                    }
                    Symbol::Terminal(t) => {
                        trailer = std::iter::once(t.clone()).collect();
                    }
                }
            }
        }
    }
    follow
}

/// A pointer into a rule at a given position (used for LR item construction).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RulePtr {
    pub rule_idx: usize,
    pub ptr: usize,
}

impl RulePtr {
    pub fn new(rule_idx: usize, ptr: usize) -> Self {
        RulePtr { rule_idx, ptr }
    }

    /// The symbol expected at the current position (None if at end of rule).
    pub fn expected<'a>(&self, rules: &'a [Rule]) -> Option<&'a Symbol> {
        rules[self.rule_idx].expansion.get(self.ptr)
    }

    pub fn advance(&self) -> Self {
        RulePtr { rule_idx: self.rule_idx, ptr: self.ptr + 1 }
    }

    pub fn is_complete(&self, rules: &[Rule]) -> bool {
        self.ptr >= rules[self.rule_idx].expansion.len()
    }

    pub fn rule<'a>(&self, rules: &'a [Rule]) -> &'a Rule {
        &rules[self.rule_idx]
    }
}
