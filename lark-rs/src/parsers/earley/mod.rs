//! Earley parser with a Shared Packed Parse Forest (SPPF) — Phase 2, Sprints 1–2.
//!
//! The Earley algorithm parses any context-free grammar (including ambiguous and
//! non-deterministic ones), which is the key differentiator of Lark vs. other
//! Rust parsing libraries. Sprint 1 landed the boolean recognizer; Sprint 2 adds
//! the **SPPF** and the **forest→tree** walk, so `parser='earley'` now produces
//! real [`Tree`]s — identical to LALR on unambiguous grammars, and resolved /
//! explicit `_ambig` forests on ambiguous ones.
//!
//! ## Algorithm — Elizabeth Scott's binarized SPPF
//!
//! This is a close port of Python Lark's `earley.py` + `earley_forest.py`, which
//! implement Scott's construction (the SPPF section of
//! <https://www.sciencedirect.com/science/article/pii/S1571066108001497>). The
//! recognizer is the standard predict / scan / complete loop; on top of it every
//! advance records a **packed node** in the forest so that, at the end, the
//! completed start symbol's [`SymbolNode`](forest::SymbolNode) is the root of a DAG
//! of all derivations.
//!
//! The forest is *binarized*: a rule `A → x1 x2 … xk` is built one symbol at a
//! time through **intermediate** nodes (keyed by the dotted rule `(rule, ptr)`),
//! each packed node carrying a `left` child (the prefix, an intermediate node or
//! nothing) and a `right` child (the symbol just consumed). This is what lets the
//! forest share sub-derivations as a DAG instead of an exponential tree. The
//! Joop-Leo right-recursion optimization is *reimplemented* here (it is dead code
//! in the Python reference — `create_leo_transitives` is commented out and the
//! `transitives` table stays empty — lark-parser/lark#397); see [`leo`].
//!
//! Nullable handling follows the reference's *held completions* (`H` in Scott's
//! paper): when an ε-derivation completes at a column it is remembered, so a
//! later prediction of that same nullable symbol can advance immediately without
//! a separate ε-closure pass — and the chart still terminates.
//!
//! ## Forest → tree
//!
//! [`Transformer`](tree_walk::Transformer) walks the SPPF bottom-up, reusing the
//! shared [`TreeOutputBuilder`](super::tree_builder::TreeOutputBuilder) for every
//! rule's tree shaping (filtering, transparent splice, `expand1`, aliases) — so the
//! forest walk and the LALR reducer cannot grow two subtly different shapers. With
//! `ambiguity='resolve'` it picks the single highest-priority derivation per
//! symbol node (Lark's `ForestSumVisitor` order: non-empty first, then priority,
//! then rule order); with `ambiguity='explicit'` it emits every derivation under
//! an `_ambig` node.
//!
//! ## Module map
//!
//! Split from the former single `earley.rs` file (pure file-movement, no logic
//! change, issue #477):
//! - [`chart`] — `Item`, `Column`, `ScanSet`, `Delayed`
//! - [`forest`] — the SPPF: `NodeKey`, `Packed`, `SymbolNode`, `Trans`, `Forest`
//! - [`recognizer`] — `build_chart`, `predict_and_complete`, `scan`
//! - [`leo`] — the Joop-Leo right-recursion fns (laziness load-bearing, #61)
//! - [`dynamic`] — `build_chart_dynamic`, `scan_dynamic`
//! - [`tree_walk`] — the de-recursed forest→tree walk (#33/#151) + the `_ambig`
//!   dedup (#159) and its guard tests

use std::collections::HashMap;

use crate::error::ParseError;
use crate::grammar::intern::{CompiledGrammar, SymbolId};
use crate::lexer::DynamicMatcher;
use crate::tree::{Child, ParseTree, Token, Tree};

use super::tree_builder::Slot;

use chart::Item;
use forest::{Forest, NodeKey};
use tree_walk::Transformer;

mod chart;
mod dynamic;
mod forest;
mod leo;
mod recognizer;
mod tree_walk;

// Backward-compat alias within earley — keeps diff minimal for this refactor.
type NodeValue = Slot;

// ─── Parser ───────────────────────────────────────────────────────────────────

/// An Earley parser over the interned grammar.
pub struct EarleyParser {
    pub(crate) grammar: CompiledGrammar,
    /// Non-terminal id → indices of the rules producing it (the predictor index).
    pub(crate) rules_by_origin: HashMap<SymbolId, Vec<usize>>,
    /// `nullable[id.index()]` = the symbol can derive ε. Indexed by `SymbolId`.
    /// Used by [`Self::eps_node`] to rebuild a skipped ε-tail.
    pub(crate) nullable: Vec<bool>,
    /// `eps_only[id.index()]` = the symbol can derive **only** ε (nullable *and*
    /// cannot derive any non-empty string). Used by the Joop-Leo completer
    /// (`is_quasi_complete`) to admit a nullable tail after the recognized symbol
    /// (#64) ONLY when the tail is ε-only: an *optional* tail (nullable but able
    /// to match real tokens, e.g. `opt: Y |`) must NOT be linearized, or the
    /// non-empty derivation becomes unreachable and valid input is rejected.
    pub(crate) eps_only: Vec<bool>,
}

impl EarleyParser {
    pub fn new(grammar: CompiledGrammar) -> Self {
        let mut rules_by_origin: HashMap<SymbolId, Vec<usize>> = HashMap::new();
        for (i, rule) in grammar.rules.iter().enumerate() {
            rules_by_origin.entry(rule.origin).or_default().push(i);
        }
        let nullable = crate::grammar::analysis::nullable_set(&grammar);
        let eps_only = crate::grammar::analysis::eps_only_set(&grammar);
        EarleyParser {
            grammar,
            rules_by_origin,
            nullable,
            eps_only,
        }
    }

    #[inline]
    pub(crate) fn is_terminal(&self, sym: SymbolId) -> bool {
        sym.index() < self.grammar.n_terminals()
    }

    /// The symbol expected next by `item`, or `None` if it is complete.
    #[inline]
    pub(crate) fn expect(&self, item: &Item) -> Option<SymbolId> {
        self.grammar.rules[item.rule]
            .expansion
            .get(item.dot)
            .copied()
    }

    #[inline]
    pub(crate) fn is_complete(&self, item: &Item) -> bool {
        item.dot >= self.grammar.rules[item.rule].expansion.len()
    }

    #[inline]
    pub(crate) fn expects_terminal(&self, item: &Item) -> bool {
        self.expect(item).is_some_and(|s| self.is_terminal(s))
    }

    /// The forest key for the symbol an item *represents* at its dot: the origin
    /// non-terminal once complete, otherwise the intermediate dotted rule.
    pub(crate) fn node_key(&self, rule: usize, dot: usize) -> NodeKey {
        if dot >= self.grammar.rules[rule].expansion.len() {
            NodeKey::Sym(self.grammar.rules[rule].origin)
        } else {
            NodeKey::Inter(rule, dot)
        }
    }

    /// Resolve the start symbol, mirroring Python Lark's `_verify_start` via the
    /// shared [`resolve_start`](super::resolve_start) — a default (`None`) start
    /// is the single configured one or a rejection on >1 starts (issue #256),
    /// and an explicit start must be one of the configured starts. Identical to
    /// LALR's resolution, so the diagnostics match.
    fn start_id(&self, start: Option<&str>) -> Result<SymbolId, ParseError> {
        super::resolve_start(&self.grammar.start, &self.grammar.symbols, start)
    }

    /// Recognize `tokens` from `start`: does the grammar derive this token
    /// sequence? Re-uses the full chart build (and discards the forest), so it
    /// accepts exactly what [`parse`](Self::parse) parses.
    ///
    /// A trailing `$END` token (the basic lexer appends one) is ignored.
    pub fn recognize(&self, tokens: &[Token], start: Option<&str>) -> bool {
        let Ok(start_id) = self.start_id(start) else {
            return false;
        };
        let toks: Vec<&Token> = tokens
            .iter()
            .filter(|t| t.type_id != SymbolId::END)
            .collect();
        self.build_chart(&toks, start_id).is_ok()
    }

    /// Parse `tokens` from `start` into a [`ParseTree`]. `resolve` selects
    /// disambiguation: `true` for `ambiguity='resolve'` (one tree), `false` for
    /// `ambiguity='explicit'` (`_ambig` forests).
    pub fn parse(
        &self,
        tokens: &[Token],
        start: Option<&str>,
        resolve: bool,
    ) -> Result<ParseTree, ParseError> {
        let start_id = self.start_id(start)?;
        let toks: Vec<&Token> = tokens
            .iter()
            .filter(|t| t.type_id != SymbolId::END)
            .collect();
        let (forest, root) = self.build_chart(&toks, start_id)?;
        // Basic lexer: terminal priorities are consumed by the lexer's terminal
        // ordering, so they do NOT feed the forest's priority sum.
        self.forest_to_tree(forest, root, start_id, resolve, false)
    }

    /// Parse `text` from `start` using the **dynamic lexer** (Phase 2, Sprint 5).
    ///
    /// Scanning is integrated into the Earley loop: at each input position the
    /// only terminals tried are the ones the parser predicts there (the scan set),
    /// rather than a token stream produced up front. This is what lets Earley parse
    /// grammars the basic lexer cannot tokenize unambiguously (overlapping
    /// terminals, terminals that depend on parser context). `complete_lex` is
    /// Lark's `dynamic_complete`: also explore *shorter* tokenizations of each
    /// match, so every valid segmentation is considered.
    pub fn parse_dynamic(
        &self,
        text: &str,
        start: Option<&str>,
        resolve: bool,
        complete_lex: bool,
        matcher: &DynamicMatcher,
    ) -> Result<ParseTree, ParseError> {
        let start_id = self.start_id(start)?;
        let (forest, root) = self.build_chart_dynamic(text, start_id, matcher, complete_lex)?;
        // Dynamic lexer: there is no terminal-ordering tie-break to consume the
        // priorities, so they DO feed the forest priority sum (Lark's
        // ForestSumVisitor — "ignore terminal priorities if the basic lexer is used").
        self.forest_to_tree(forest, root, start_id, resolve, true)
    }

    /// Walk the SPPF `forest` from `root` into a [`ParseTree`]. Shared by the
    /// basic-lexer ([`parse`](Self::parse)) and dynamic-lexer
    /// ([`parse_dynamic`](Self::parse_dynamic)) entry points.
    fn forest_to_tree(
        &self,
        forest: Forest,
        root: usize,
        start_id: SymbolId,
        resolve: bool,
        term_priority: bool,
    ) -> Result<ParseTree, ParseError> {
        // The walk is driven by an explicit frame stack (issue #33), so its
        // native-stack use is O(1) no matter how deep the forest is — it runs
        // right here on the caller's stack. (It used to recurse to forest depth,
        // O(input length) for list-like rules, and needed a dedicated thread with
        // a 256 MB stack; `std::thread` does not exist on WASM (#47), so the
        // de-recursion is also what makes this engine portable there.)
        let mut tr = Transformer::new(&self.grammar, &forest, resolve, term_priority);
        let value = tr
            .transform(root)
            .ok_or_else(|| ParseError::unexpected_eof(0, 0, vec![]))?;
        Ok(match value {
            NodeValue::Tree(t) => ParseTree::Tree(t),
            NodeValue::Token(t) => ParseTree::Token(t),
            // A start rule is never transparent. Its value can still be `Inline`
            // when a top-level `?start` collapses a lone-`None` placeholder (RC9 in
            // tree_builder: lone-`None` expand1 → `Inline([None])`). Python Lark
            // returns a bare `None` there (`?start: [A]` on `""`), so emit
            // `ParseTree::None` to match the oracle (#289).
            NodeValue::Inline(mut cs) if cs.len() == 1 => match cs.pop().unwrap() {
                Child::Tree(t) => ParseTree::Tree(t),
                Child::Token(t) => ParseTree::Token(t),
                Child::None => ParseTree::None,
            },
            NodeValue::Inline(cs) => ParseTree::Tree(Tree::new(
                self.grammar.symbols.name(start_id).to_string(),
                cs,
            )),
        })
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
