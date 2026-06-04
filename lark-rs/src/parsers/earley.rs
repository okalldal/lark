//! Earley recognizer — Phase 2, Sprint 1.
//!
//! The Earley algorithm parses any context-free grammar (including ambiguous and
//! non-deterministic ones), which is the key differentiator of Lark vs. other
//! Rust parsing libraries. This sprint lands the **recognizer**: a boolean
//! accept/reject over the interned grammar, with no parse forest yet.
//!
//! ## What this sprint does (and deliberately does not)
//!
//! [`EarleyParser::recognize`] answers *"does the grammar derive this token
//! sequence?"* — nothing more. There is no SPPF, no tree, and it is **not** yet
//! wired into [`build_frontend`](super::build_frontend): a tree-producing
//! `parser='earley'` still returns "not yet implemented". That is on purpose. The
//! Earley oracle/compliance tests compare *trees*, and they self-activate the
//! moment the Earley frontend builds (see `common::earley_unimplemented`). Wiring
//! the frontend before a forest exists would flip that gate against an engine that
//! cannot yet produce trees. So Sprint 1 verifies the recognizer through its own
//! accept/reject oracle (`tests/test_earley_recognizer.rs`) and leaves the gate
//! closed; Sprint 2 (SPPF + forest→tree) is what flips it.
//!
//! ## Algorithm
//!
//! Standard Earley (predict / scan / complete) over a chart of one [`Column`] per
//! input position, with items keyed by the interned [`SymbolId`] — never a name.
//! Nullable symbols are handled the Aycock–Horspool way: when a prediction lands
//! on a nullable non-terminal, the predicting item is eagerly advanced past it, so
//! ε-derivations complete without a separate ε-closure pass and the chart always
//! terminates. `NULLABLE` is already precomputed by
//! [`GrammarAnalysis`](crate::grammar::analysis::GrammarAnalysis).
//!
//! ## Design notes for Sprint 2 (SPPF + forest→tree)
//! - Build Elizabeth Scott's SPPF (Shared Packed Parse Forest): Symbol /
//!   Intermediate / Packed nodes, **arena- / index-allocated** (`Vec<Node>` +
//!   `NodeId` indices, not `Rc<RefCell>`) — the DAG Rust ownership can't express
//!   naively.
//! - Reuse the shared [`TreeBuilder`](super::tree_builder::TreeBuilder) for the
//!   forest→tree walk, so the SPPF cannot grow a second, subtly different shaper.
//! - The dynamic lexer (Sprint 5) integrates regex matching into this loop.

use std::collections::{HashMap, HashSet};

use crate::grammar::analysis::GrammarAnalysis;
use crate::grammar::intern::{CompiledGrammar, SymbolId};
use crate::tree::Token;

/// An Earley item: a dotted rule plus the chart position where the rule began.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct Item {
    /// Index into [`CompiledGrammar::rules`].
    rule: usize,
    /// Position of the dot within the rule's expansion.
    dot: usize,
    /// Chart column where this derivation started.
    origin: usize,
}

/// One Earley chart column: an ordered, de-duplicated set of items.
///
/// Insertion order is preserved so the per-column worklist can process items by
/// index while predict/complete append new ones; the `HashSet` makes the
/// append idempotent, which is what bounds the chart (and tames left recursion
/// and unit cycles).
struct Column {
    items: Vec<Item>,
    seen: HashSet<Item>,
}

impl Column {
    fn new() -> Self {
        Column {
            items: Vec::new(),
            seen: HashSet::new(),
        }
    }

    /// Add `item` unless the column already holds it.
    fn add(&mut self, item: Item) {
        if self.seen.insert(item) {
            self.items.push(item);
        }
    }

    fn contains(&self, item: &Item) -> bool {
        self.seen.contains(item)
    }
}

/// An Earley recognizer over the interned grammar.
pub struct EarleyParser {
    grammar: CompiledGrammar,
    analysis: GrammarAnalysis,
    /// Non-terminal id → indices of the rules producing it (the predictor index).
    rules_by_origin: HashMap<SymbolId, Vec<usize>>,
}

impl EarleyParser {
    pub fn new(grammar: CompiledGrammar) -> Self {
        let analysis = GrammarAnalysis::compute(&grammar);
        let mut rules_by_origin: HashMap<SymbolId, Vec<usize>> = HashMap::new();
        for (i, rule) in grammar.rules.iter().enumerate() {
            rules_by_origin.entry(rule.origin).or_default().push(i);
        }
        EarleyParser {
            grammar,
            analysis,
            rules_by_origin,
        }
    }

    #[inline]
    fn is_terminal(&self, sym: SymbolId) -> bool {
        sym.index() < self.grammar.n_terminals()
    }

    /// Index of the augmented start rule (`$root_X → X`) for the requested start
    /// name, or the grammar's first start symbol when `start` is `None`.
    fn start_rule(&self, start: Option<&str>) -> Option<usize> {
        let start_id = match start {
            Some(name) => self.grammar.symbols.id(name)?,
            None => *self.grammar.start.first()?,
        };
        let aug = self.grammar.augmented_start(start_id)?;
        self.grammar
            .rules
            .iter()
            .position(|r| r.is_start && r.origin == aug)
    }

    /// Recognize `tokens` from `start`: does the grammar derive this token
    /// sequence? Boolean accept/reject only — no parse forest yet (Sprint 2).
    ///
    /// A trailing `$END` token (the basic lexer appends one) is ignored: Earley
    /// decides acceptance from the final chart column, not by scanning `$END`.
    pub fn recognize(&self, tokens: &[Token], start: Option<&str>) -> bool {
        let Some(start_rule) = self.start_rule(start) else {
            return false;
        };

        // Real (scannable) tokens, dropping the synthetic end marker.
        let toks: Vec<&Token> = tokens
            .iter()
            .filter(|t| t.type_id != SymbolId::END)
            .collect();
        let n = toks.len();

        let mut chart: Vec<Column> = (0..=n).map(|_| Column::new()).collect();
        chart[0].add(Item {
            rule: start_rule,
            dot: 0,
            origin: 0,
        });

        for i in 0..=n {
            // Worklist over column i. Predict/complete append to this same column;
            // scan appends to column i+1. Re-read the length every iteration so
            // items added mid-pass are processed too.
            let mut k = 0;
            while k < chart[i].items.len() {
                let item = chart[i].items[k];
                k += 1;
                let next = self.grammar.rules[item.rule]
                    .expansion
                    .get(item.dot)
                    .copied();
                match next {
                    // Completed item: advance every item in its origin column that
                    // was waiting on this rule's non-terminal.
                    None => {
                        let lhs = self.grammar.rules[item.rule].origin;
                        let origin = item.origin;
                        // Collect first (releases the borrow of `chart[origin]`),
                        // then add — `origin` may equal `i`, which we are mutating.
                        let advanced: Vec<Item> = chart[origin]
                            .items
                            .iter()
                            .filter(|w| {
                                self.grammar.rules[w.rule].expansion.get(w.dot) == Some(&lhs)
                            })
                            .map(|w| Item {
                                rule: w.rule,
                                dot: w.dot + 1,
                                origin: w.origin,
                            })
                            .collect();
                        for a in advanced {
                            chart[i].add(a);
                        }
                    }
                    // Dot before a terminal: scan the input token at position i.
                    Some(sym) if self.is_terminal(sym) => {
                        if i < n && toks[i].type_id == sym {
                            chart[i + 1].add(Item {
                                rule: item.rule,
                                dot: item.dot + 1,
                                origin: item.origin,
                            });
                        }
                    }
                    // Dot before a non-terminal: predict its productions, and —
                    // Aycock–Horspool — if it is nullable, eagerly advance past it
                    // so ε-derivations complete without a separate ε pass.
                    Some(sym) => {
                        if let Some(prods) = self.rules_by_origin.get(&sym) {
                            for &ri in prods {
                                chart[i].add(Item {
                                    rule: ri,
                                    dot: 0,
                                    origin: i,
                                });
                            }
                        }
                        if self.analysis.is_nullable(sym) {
                            chart[i].add(Item {
                                rule: item.rule,
                                dot: item.dot + 1,
                                origin: item.origin,
                            });
                        }
                    }
                }
            }
        }

        // Accept iff the augmented start rule completed spanning the whole input.
        let accept = Item {
            rule: start_rule,
            dot: self.grammar.rules[start_rule].expansion.len(),
            origin: 0,
        };
        chart[n].contains(&accept)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grammar::{load_grammar, lower};

    fn compile(src: &str) -> CompiledGrammar {
        lower(&load_grammar(src, &["start".to_string()], false, false).unwrap())
    }

    /// A token carrying the interned id of terminal `name` in `cg`.
    fn tok(cg: &CompiledGrammar, name: &str, value: &str) -> Token {
        let mut t = Token::new(name, value);
        t.type_id = cg.symbols.id(name).expect("terminal interned");
        t
    }

    #[test]
    fn recognizes_simple_sequence() {
        let cg = compile("start: A B\nA: \"a\"\nB: \"b\"\n");
        let p = EarleyParser::new(cg.clone());
        assert!(p.recognize(&[tok(&cg, "A", "a"), tok(&cg, "B", "b")], Some("start")));
        // Wrong, short, and over-long inputs all reject.
        assert!(!p.recognize(&[tok(&cg, "A", "a")], Some("start")));
        assert!(!p.recognize(&[tok(&cg, "B", "b"), tok(&cg, "A", "a")], Some("start")));
        assert!(!p.recognize(
            &[tok(&cg, "A", "a"), tok(&cg, "B", "b"), tok(&cg, "B", "b")],
            Some("start")
        ));
        assert!(!p.recognize(&[], Some("start")));
    }

    #[test]
    fn handles_nullable_symbol() {
        // `X?` expands to a nullable anonymous rule between A and B.
        let cg = compile("start: A X? B\nA: \"a\"\nX: \"x\"\nB: \"b\"\n");
        let p = EarleyParser::new(cg.clone());
        // X omitted (the ε derivation) and X present both parse.
        assert!(p.recognize(&[tok(&cg, "A", "a"), tok(&cg, "B", "b")], Some("start")));
        assert!(p.recognize(
            &[tok(&cg, "A", "a"), tok(&cg, "X", "x"), tok(&cg, "B", "b")],
            Some("start")
        ));
        assert!(!p.recognize(&[tok(&cg, "A", "a")], Some("start")));
    }

    #[test]
    fn handles_ambiguous_left_recursion() {
        // Ambiguous and left-recursive: Earley accepts where LALR cannot even build.
        let cg = compile("start: start start | A\nA: \"a\"\n");
        let p = EarleyParser::new(cg.clone());
        for k in 1..=4 {
            let input: Vec<Token> = (0..k).map(|_| tok(&cg, "A", "a")).collect();
            assert!(p.recognize(&input, Some("start")), "k={k} should parse");
        }
        assert!(!p.recognize(&[], Some("start")));
    }
}
