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
//! completed start symbol's [`SymbolNode`] is the root of a DAG of all
//! derivations.
//!
//! The forest is *binarized*: a rule `A → x1 x2 … xk` is built one symbol at a
//! time through **intermediate** nodes (keyed by the dotted rule `(rule, ptr)`),
//! each packed node carrying a `left` child (the prefix, an intermediate node or
//! nothing) and a `right` child (the symbol just consumed). This is what lets the
//! forest share sub-derivations as a DAG instead of an exponential tree. The
//! Joop-Leo right-recursion optimization is *not* ported (it is dead code in the
//! Python reference — `create_leo_transitives` is commented out and the
//! `transitives` table stays empty), which simplifies the port considerably.
//!
//! Nullable handling follows the reference's *held completions* (`H` in Scott's
//! paper): when an ε-derivation completes at a column it is remembered, so a
//! later prediction of that same nullable symbol can advance immediately without
//! a separate ε-closure pass — and the chart still terminates.
//!
//! ## Forest → tree
//!
//! [`Transformer`] walks the SPPF bottom-up, reusing the shared
//! [`TreeBuilder`](super::tree_builder::TreeBuilder) for every rule's tree
//! shaping (filtering, transparent splice, `expand1`, aliases) — so the forest
//! walk and the LALR reducer cannot grow two subtly different shapers. With
//! `ambiguity='resolve'` it picks the single highest-priority derivation per
//! symbol node (Lark's `ForestSumVisitor` order: non-empty first, then priority,
//! then rule order); with `ambiguity='explicit'` it emits every derivation under
//! an `_ambig` node.

use std::collections::{HashMap, HashSet};

use crate::error::ParseError;
use crate::grammar::intern::{CompiledGrammar, SymbolId};
use crate::lexer::DynamicMatcher;
use crate::tree::{Child, ParseTree, Token, Tree};

use super::tree_builder::{NodeValue, TreeBuilder};

// ─── Chart items ──────────────────────────────────────────────────────────────

/// An Earley item: a dotted rule, the column where the rule began, and the SPPF
/// node for the symbol/intermediate it has built so far (`None` before the first
/// symbol is consumed — Scott's `w`).
#[derive(Clone, Copy, PartialEq, Eq)]
struct Item {
    rule: usize,
    dot: usize,
    origin: usize,
    node: ForestRef,
}

/// One Earley chart column: items that are complete or expect a non-terminal (the
/// set `R`). Terminal-expecting items live in the separate scan buffer. Ordered +
/// de-duplicated; insertion order is load-bearing for resolve tie-breaks.
#[derive(Default)]
struct Column {
    items: Vec<Item>,
    seen: HashSet<(usize, usize, usize)>,
}

impl Column {
    fn new() -> Self {
        Column::default()
    }

    /// Add `item` unless an equal one (same rule, dot, origin) is already present;
    /// returns whether it was newly inserted.
    fn add(&mut self, item: Item) -> bool {
        if self.seen.insert((item.rule, item.dot, item.origin)) {
            self.items.push(item);
            true
        } else {
            false
        }
    }
}

// ─── Shared Packed Parse Forest ───────────────────────────────────────────────

/// Identity of an SPPF symbol node within a column: either a completed
/// non-terminal (`Sym`) or an intermediate dotted rule (`Inter(rule, ptr)`),
/// plus its start column (the end is the column it lives in).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum NodeKey {
    Sym(SymbolId),
    Inter(usize, usize),
}

impl NodeKey {
    fn is_intermediate(self) -> bool {
        matches!(self, NodeKey::Inter(_, _))
    }
}

/// A reference to a forest child inside a packed node: nothing (Scott's `None`,
/// for the first symbol of a rule or an ε production), a symbol/intermediate
/// node, or a scanned-token leaf.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum ForestRef {
    None,
    Node(usize),
    Tok(usize),
}

/// A single derivation of a symbol node: which rule, and its (binarized) left and
/// right children.
#[derive(Clone, Copy)]
struct Packed {
    rule: usize,
    left: ForestRef,
    right: ForestRef,
}

/// A symbol or intermediate node. Its `families` are the alternative derivations
/// (packed nodes); more than one means the node is ambiguous.
struct SymbolNode {
    is_intermediate: bool,
    families: Vec<Packed>,
    family_set: HashSet<(ForestRef, ForestRef)>,
}

/// Arena of forest nodes + the scanned-token leaves they reference by index.
struct Forest {
    nodes: Vec<SymbolNode>,
    tokens: Vec<Token>,
}

impl Forest {
    fn new() -> Self {
        Forest {
            nodes: Vec::new(),
            tokens: Vec::new(),
        }
    }

    /// Node id for `(key, start)` in the current column's `cache`, creating it on
    /// first sight. `end` is the column being built; it is metadata only.
    fn get_or_create(
        &mut self,
        cache: &mut HashMap<(NodeKey, usize), usize>,
        key: NodeKey,
        start: usize,
    ) -> usize {
        if let Some(&id) = cache.get(&(key, start)) {
            return id;
        }
        let id = self.nodes.len();
        self.nodes.push(SymbolNode {
            is_intermediate: key.is_intermediate(),
            families: Vec::new(),
            family_set: HashSet::new(),
        });
        cache.insert((key, start), id);
        id
    }

    /// Record a derivation (packed node) on `node_id`, de-duplicated by its
    /// `(left, right)` children exactly as Python's `PackedNode` equality.
    fn add_family(&mut self, node_id: usize, rule: usize, left: ForestRef, right: ForestRef) {
        let node = &mut self.nodes[node_id];
        if node.family_set.insert((left, right)) {
            node.families.push(Packed { rule, left, right });
        }
    }

    fn add_token(&mut self, token: Token) -> usize {
        let id = self.tokens.len();
        self.tokens.push(token);
        id
    }
}

/// The scan buffer (`Q` in Scott's paper): terminal-expecting items for the
/// current column. Ordered + de-duplicated.
#[derive(Default)]
struct ScanSet {
    items: Vec<Item>,
    seen: HashSet<(usize, usize, usize)>,
}

impl ScanSet {
    fn new() -> Self {
        ScanSet::default()
    }
    fn add(&mut self, item: Item) {
        if self.seen.insert((item.rule, item.dot, item.origin)) {
            self.items.push(item);
        }
    }
}

/// A match queued by the **dynamic** scanner, to be acted on at the input step
/// where it ends (Scott's `delayed_matches`). A token advances the item that
/// predicted it; an ignored span instead carries the item over unchanged.
enum Delayed {
    Tok { item: Item, token: Token },
    Carry { item: Item },
}

// ─── Parser ───────────────────────────────────────────────────────────────────

/// An Earley parser over the interned grammar.
pub struct EarleyParser {
    grammar: CompiledGrammar,
    /// Non-terminal id → indices of the rules producing it (the predictor index).
    rules_by_origin: HashMap<SymbolId, Vec<usize>>,
}

impl EarleyParser {
    pub fn new(grammar: CompiledGrammar) -> Self {
        let mut rules_by_origin: HashMap<SymbolId, Vec<usize>> = HashMap::new();
        for (i, rule) in grammar.rules.iter().enumerate() {
            rules_by_origin.entry(rule.origin).or_default().push(i);
        }
        EarleyParser {
            grammar,
            rules_by_origin,
        }
    }

    #[inline]
    fn is_terminal(&self, sym: SymbolId) -> bool {
        sym.index() < self.grammar.n_terminals()
    }

    /// The symbol expected next by `item`, or `None` if it is complete.
    #[inline]
    fn expect(&self, item: &Item) -> Option<SymbolId> {
        self.grammar.rules[item.rule]
            .expansion
            .get(item.dot)
            .copied()
    }

    #[inline]
    fn is_complete(&self, item: &Item) -> bool {
        item.dot >= self.grammar.rules[item.rule].expansion.len()
    }

    #[inline]
    fn expects_terminal(&self, item: &Item) -> bool {
        self.expect(item).is_some_and(|s| self.is_terminal(s))
    }

    /// The forest key for the symbol an item *represents* at its dot: the origin
    /// non-terminal once complete, otherwise the intermediate dotted rule.
    fn node_key(&self, rule: usize, dot: usize) -> NodeKey {
        if dot >= self.grammar.rules[rule].expansion.len() {
            NodeKey::Sym(self.grammar.rules[rule].origin)
        } else {
            NodeKey::Inter(rule, dot)
        }
    }

    fn start_id(&self, start: Option<&str>) -> Option<SymbolId> {
        match start {
            Some(name) => self.grammar.symbols.id(name),
            None => self.grammar.start.first().copied(),
        }
    }

    /// Recognize `tokens` from `start`: does the grammar derive this token
    /// sequence? Re-uses the full chart build (and discards the forest), so it
    /// accepts exactly what [`parse`](Self::parse) parses.
    ///
    /// A trailing `$END` token (the basic lexer appends one) is ignored.
    pub fn recognize(&self, tokens: &[Token], start: Option<&str>) -> bool {
        let Some(start_id) = self.start_id(start) else {
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
        let start_id = self
            .start_id(start)
            .ok_or_else(|| ParseError::UnexpectedEof {
                line: 0,
                col: 0,
                expected: vec![],
            })?;
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
        let start_id = self
            .start_id(start)
            .ok_or_else(|| ParseError::UnexpectedEof {
                line: 0,
                col: 0,
                expected: vec![],
            })?;
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
        // The forest→tree walk recurses to the depth of the parse forest, which is
        // O(input length) for left-recursive list grammars — enough to blow the
        // default stack on a long input. Run it on a generous dedicated stack
        // (`thread::scope` keeps the borrows of `self.grammar` / `forest` valid).
        let grammar = &self.grammar;
        let value = std::thread::scope(|s| {
            std::thread::Builder::new()
                .stack_size(256 * 1024 * 1024)
                .spawn_scoped(s, || {
                    let mut tr = Transformer::new(grammar, &forest, resolve, term_priority);
                    let mut visiting = HashSet::new();
                    tr.eval_symbol(root, &mut visiting)
                })
                .expect("spawn forest-walk thread")
                .join()
                .unwrap_or(None)
        })
        .ok_or_else(|| ParseError::UnexpectedEof {
            line: 0,
            col: 0,
            expected: vec![],
        })?;
        Ok(match value {
            NodeValue::Tree(t) => ParseTree::Tree(t),
            NodeValue::Token(t) => ParseTree::Token(t),
            // A start rule is never transparent, so its value is never Inline; be
            // defensive rather than panic.
            NodeValue::Inline(mut cs) if cs.len() == 1 => match cs.pop().unwrap() {
                Child::Tree(t) => ParseTree::Tree(t),
                Child::Token(t) => ParseTree::Token(t),
                Child::None => ParseTree::Tree(Tree::new(
                    self.grammar.symbols.name(start_id).to_string(),
                    vec![],
                )),
            },
            NodeValue::Inline(cs) => ParseTree::Tree(Tree::new(
                self.grammar.symbols.name(start_id).to_string(),
                cs,
            )),
        })
    }

    // ─── Chart construction (recognizer + forest) ─────────────────────────────

    /// Build the Earley chart and SPPF over `toks` from `start_id`. On success
    /// returns the forest and the node id of the completed start symbol spanning
    /// the whole input; otherwise a parse error.
    fn build_chart(
        &self,
        toks: &[&Token],
        start_id: SymbolId,
    ) -> Result<(Forest, usize), ParseError> {
        let n = toks.len();
        let mut forest = Forest::new();
        let mut columns: Vec<Column> = vec![Column::new()];
        let mut to_scan = ScanSet::new();

        // Predict the start symbol into column 0 (non-terminal items) / the scan
        // buffer (terminal items).
        if let Some(prods) = self.rules_by_origin.get(&start_id) {
            for &ri in prods {
                let item = Item {
                    rule: ri,
                    dot: 0,
                    origin: 0,
                    node: ForestRef::None,
                };
                if self.expects_terminal(&item) {
                    to_scan.add(item);
                } else {
                    columns[0].add(item);
                }
            }
        }

        // node_cache lives one column at a time; the start column uses a fresh one.
        let mut node_cache: HashMap<(NodeKey, usize), usize> = HashMap::new();

        let mut i = 0;
        loop {
            self.predict_and_complete(i, &mut columns, &mut to_scan, &mut forest, &mut node_cache);
            if i == n {
                break;
            }
            let token = toks[i];
            match self.scan(token, &mut columns, &to_scan, &mut forest) {
                Some((next_scan, next_cache)) => {
                    to_scan = next_scan;
                    node_cache = next_cache;
                }
                None => {
                    return Err(ParseError::UnexpectedToken {
                        token: token.value.clone(),
                        token_type: token.type_.clone(),
                        line: token.line,
                        col: token.column,
                        expected: vec![],
                    });
                }
            }
            i += 1;
        }

        // The root is the completed start symbol over (0, n) — found in the final
        // column's node cache, which collects all of its derivations into one node.
        node_cache
            .get(&(NodeKey::Sym(start_id), 0))
            .copied()
            .map(|root| (forest, root))
            .ok_or_else(|| {
                let (line, col) = toks
                    .last()
                    .map(|t| (t.end_line.max(t.line), t.end_column.max(t.column)))
                    .unwrap_or((1, 1));
                ParseError::UnexpectedEof {
                    line,
                    col,
                    expected: vec![],
                }
            })
    }

    /// Scott's predictor + completer for one column. Processes the column as a
    /// LIFO worklist (matching the reference's `deque.pop()`), so newly derived
    /// items are handled before older ones — the order that fixes resolve
    /// tie-breaks.
    fn predict_and_complete(
        &self,
        i: usize,
        columns: &mut Vec<Column>,
        to_scan: &mut ScanSet,
        forest: &mut Forest,
        node_cache: &mut HashMap<(NodeKey, usize), usize>,
    ) {
        // Held (ε) completions at this column: origin → its empty node.
        let mut held: HashMap<SymbolId, usize> = HashMap::new();
        let mut stack: Vec<Item> = columns[i].items.clone();

        while let Some(item) = stack.pop() {
            if self.is_complete(&item) {
                let origin = self.grammar.rules[item.rule].origin;

                // Ensure this completed item has a forest node. A node is absent
                // only for an ε production predicted directly as complete — give it
                // the empty (None, None) family that represents the ε derivation.
                let node_id = match item.node {
                    ForestRef::Node(id) => id,
                    _ => {
                        let id =
                            forest.get_or_create(node_cache, NodeKey::Sym(origin), item.origin);
                        forest.add_family(id, item.rule, ForestRef::None, ForestRef::None);
                        id
                    }
                };

                if item.origin == i {
                    held.insert(origin, node_id);
                }

                // Advance every item in the origin column that was waiting on this
                // non-terminal. Snapshot first (we mutate columns[i] below).
                let originators: Vec<Item> = columns[item.origin]
                    .items
                    .iter()
                    .filter(|o| self.expect(o) == Some(origin))
                    .copied()
                    .collect();

                for o in originators {
                    let key = self.node_key(o.rule, o.dot + 1);
                    let new_node = forest.get_or_create(node_cache, key, o.origin);
                    forest.add_family(new_node, o.rule, o.node, ForestRef::Node(node_id));
                    let advanced = Item {
                        rule: o.rule,
                        dot: o.dot + 1,
                        origin: o.origin,
                        node: ForestRef::Node(new_node),
                    };
                    if self.expects_terminal(&advanced) {
                        to_scan.add(advanced);
                    } else if columns[i].add(advanced) {
                        stack.push(advanced);
                    }
                }
            } else if let Some(sym) = self.expect(&item) {
                if self.is_terminal(sym) {
                    continue; // terminal-expecting items belong to the scan buffer
                }

                let mut new_items: Vec<Item> = Vec::new();
                if let Some(prods) = self.rules_by_origin.get(&sym) {
                    for &ri in prods {
                        new_items.push(Item {
                            rule: ri,
                            dot: 0,
                            origin: i,
                            node: ForestRef::None,
                        });
                    }
                }
                // If `sym` already completed empty at this column, advance past it
                // now (held completion) so ε-derivations propagate.
                if let Some(&hnode) = held.get(&sym) {
                    let key = self.node_key(item.rule, item.dot + 1);
                    let new_node = forest.get_or_create(node_cache, key, item.origin);
                    forest.add_family(new_node, item.rule, item.node, ForestRef::Node(hnode));
                    new_items.push(Item {
                        rule: item.rule,
                        dot: item.dot + 1,
                        origin: item.origin,
                        node: ForestRef::Node(new_node),
                    });
                }

                for new in new_items {
                    if self.expects_terminal(&new) {
                        to_scan.add(new);
                    } else if columns[i].add(new) {
                        stack.push(new);
                    }
                }
            }
        }
    }

    /// Scott's scanner: advance every terminal-expecting item that matches
    /// `token`, recording a token-leaf packed node. Returns the next column's scan
    /// buffer and node cache, or `None` if nothing matched (a parse failure).
    fn scan(
        &self,
        token: &Token,
        columns: &mut Vec<Column>,
        to_scan: &ScanSet,
        forest: &mut Forest,
    ) -> Option<(ScanSet, HashMap<(NodeKey, usize), usize>)> {
        let mut next_scan = ScanSet::new();
        let mut next_col = Column::new();
        let mut next_cache: HashMap<(NodeKey, usize), usize> = HashMap::new();

        // One token leaf per position, shared by every item that scans it (so
        // packed-node de-duplication works).
        let tok_ref = ForestRef::Tok(forest.add_token(token.clone()));

        for item in &to_scan.items {
            if self.expect(item) == Some(token.type_id) {
                let key = self.node_key(item.rule, item.dot + 1);
                let new_node = forest.get_or_create(&mut next_cache, key, item.origin);
                forest.add_family(new_node, item.rule, item.node, tok_ref);
                let advanced = Item {
                    rule: item.rule,
                    dot: item.dot + 1,
                    origin: item.origin,
                    node: ForestRef::Node(new_node),
                };
                if self.expects_terminal(&advanced) {
                    next_scan.add(advanced);
                } else {
                    next_col.add(advanced);
                }
            }
        }

        if next_scan.items.is_empty() && next_col.items.is_empty() {
            return None;
        }
        columns.push(next_col);
        Some((next_scan, next_cache))
    }

    // ─── Dynamic lexer (Sprint 5) ─────────────────────────────────────────────

    /// Build the Earley chart and SPPF over `text` using the dynamic lexer.
    ///
    /// Columns are indexed by **character step** `0..=n`; `boundaries[i]` is the
    /// byte offset where step `i` starts (regex matching is byte-based). The
    /// predict/complete phase is identical to the basic-lexer path
    /// ([`predict_and_complete`](Self::predict_and_complete)); only the scanner
    /// differs — see [`scan_dynamic`](Self::scan_dynamic).
    fn build_chart_dynamic(
        &self,
        text: &str,
        start_id: SymbolId,
        matcher: &DynamicMatcher,
        complete_lex: bool,
    ) -> Result<(Forest, usize), ParseError> {
        // Byte offset of every character start, plus the end-of-input offset.
        let boundaries: Vec<usize> = text
            .char_indices()
            .map(|(b, _)| b)
            .chain(std::iter::once(text.len()))
            .collect();
        let n = boundaries.len() - 1;
        let byte_to_step: HashMap<usize, usize> = boundaries
            .iter()
            .enumerate()
            .map(|(i, &b)| (b, i))
            .collect();

        // Per-step (line, column), 1-based and newline-aware — for token positions.
        let mut lines = Vec::with_capacity(n + 1);
        let mut cols = Vec::with_capacity(n + 1);
        {
            let (mut line, mut col) = (1usize, 1usize);
            let mut chars = text.chars();
            for _ in 0..=n {
                lines.push(line);
                cols.push(col);
                match chars.next() {
                    Some('\n') => {
                        line += 1;
                        col = 1;
                    }
                    Some(_) => col += 1,
                    None => {}
                }
            }
        }

        let mut forest = Forest::new();
        let mut columns: Vec<Column> = vec![Column::new()];
        let mut to_scan = ScanSet::new();
        let mut delayed: HashMap<usize, Vec<Delayed>> = HashMap::new();

        // Predict the start symbol into column 0 / the scan buffer.
        if let Some(prods) = self.rules_by_origin.get(&start_id) {
            for &ri in prods {
                let item = Item {
                    rule: ri,
                    dot: 0,
                    origin: 0,
                    node: ForestRef::None,
                };
                if self.expects_terminal(&item) {
                    to_scan.add(item);
                } else {
                    columns[0].add(item);
                }
            }
        }

        let mut node_cache: HashMap<(NodeKey, usize), usize> = HashMap::new();

        for i in 0..=n {
            self.predict_and_complete(i, &mut columns, &mut to_scan, &mut forest, &mut node_cache);
            if i == n {
                break;
            }
            match self.scan_dynamic(
                i,
                start_id,
                text,
                &boundaries,
                &byte_to_step,
                &lines,
                &cols,
                matcher,
                complete_lex,
                &mut columns,
                &to_scan,
                &mut forest,
                &mut delayed,
            ) {
                Some((next_scan, next_cache)) => {
                    to_scan = next_scan;
                    node_cache = next_cache;
                }
                None => {
                    let pos = boundaries[i];
                    let ch = text[pos..].chars().next().unwrap_or('\u{0}');
                    return Err(ParseError::UnexpectedCharacter {
                        ch,
                        line: lines[i],
                        col: cols[i],
                        pos,
                        expected: "valid token for the dynamic lexer".to_string(),
                    });
                }
            }
        }

        node_cache
            .get(&(NodeKey::Sym(start_id), 0))
            .copied()
            .map(|root| (forest, root))
            .ok_or(ParseError::UnexpectedEof {
                line: *lines.last().unwrap_or(&1),
                col: *cols.last().unwrap_or(&1),
                expected: vec![],
            })
    }

    /// The dynamic scanner for character step `i`: match each scan-set item's
    /// predicted terminal at `boundaries[i]` (plus the `%ignore` terminals),
    /// queuing every hit into `delayed` keyed by the step where it ends; then
    /// drain whatever completes at step `i+1` into the freshly pushed column.
    #[allow(clippy::too_many_arguments)]
    fn scan_dynamic(
        &self,
        i: usize,
        start_id: SymbolId,
        text: &str,
        boundaries: &[usize],
        byte_to_step: &HashMap<usize, usize>,
        lines: &[usize],
        cols: &[usize],
        matcher: &DynamicMatcher,
        complete_lex: bool,
        columns: &mut Vec<Column>,
        to_scan: &ScanSet,
        forest: &mut Forest,
        delayed: &mut HashMap<usize, Vec<Delayed>>,
    ) -> Option<(ScanSet, HashMap<(NodeKey, usize), usize>)> {
        let pos = boundaries[i];
        let mk_token = |term: SymbolId, value: &str, end_step: usize| Token {
            type_id: term,
            type_: matcher.name(term).to_string(),
            value: value.to_string(),
            line: lines[i],
            column: cols[i],
            end_line: lines[end_step],
            end_column: cols[end_step],
            start_pos: pos,
            end_pos: boundaries[end_step],
        };

        // 1) Match each scan-set item's predicted terminal here. A hit is *delayed*
        //    to the step where it ends, so a longer token is acted on only when the
        //    parse reaches its end (this is what makes overlapping terminals work).
        for item in &to_scan.items {
            let Some(term) = self.expect(item) else {
                continue;
            };
            if let Some(value) = matcher.match_at(term, text, pos) {
                let end_step = byte_to_step[&(pos + value.len())];
                let token = mk_token(term, value, end_step);
                delayed
                    .entry(end_step)
                    .or_default()
                    .push(Delayed::Tok { item: *item, token });

                // dynamic_complete: also queue every shorter prefix tokenization.
                if complete_lex {
                    for &cut in &boundaries[i + 1..end_step] {
                        if let Some(short) = matcher.match_in(term, &text[pos..cut]) {
                            let es = byte_to_step[&(pos + short.len())];
                            let token = mk_token(term, short, es);
                            delayed
                                .entry(es)
                                .or_default()
                                .push(Delayed::Tok { item: *item, token });
                        }
                    }
                }
            }
        }

        // 2) Ignored spans (e.g. whitespace): carry every scan-set item — and any
        //    completed start item — past the span unchanged, so ignored text can
        //    sit between tokens (and after the last one) without consuming a symbol.
        for &ig in matcher.ignore() {
            if let Some(value) = matcher.match_at(ig, text, pos) {
                let end_step = byte_to_step[&(pos + value.len())];
                let bucket = delayed.entry(end_step).or_default();
                for item in &to_scan.items {
                    bucket.push(Delayed::Carry { item: *item });
                }
                for item in &columns[i].items {
                    if self.is_complete(item)
                        && item.origin == 0
                        && self.grammar.rules[item.rule].origin == start_id
                    {
                        bucket.push(Delayed::Carry { item: *item });
                    }
                }
            }
        }

        // 3) Drain the matches that complete at step i+1 into the next column.
        let mut next_scan = ScanSet::new();
        let mut next_col = Column::new();
        let mut next_cache: HashMap<(NodeKey, usize), usize> = HashMap::new();

        for d in delayed.remove(&(i + 1)).unwrap_or_default() {
            match d {
                Delayed::Tok { item, token } => {
                    let key = self.node_key(item.rule, item.dot + 1);
                    let new_node = forest.get_or_create(&mut next_cache, key, item.origin);
                    let tok_ref = ForestRef::Tok(forest.add_token(token));
                    forest.add_family(new_node, item.rule, item.node, tok_ref);
                    let advanced = Item {
                        rule: item.rule,
                        dot: item.dot + 1,
                        origin: item.origin,
                        node: ForestRef::Node(new_node),
                    };
                    if self.expects_terminal(&advanced) {
                        next_scan.add(advanced);
                    } else {
                        next_col.add(advanced);
                    }
                }
                Delayed::Carry { item } => {
                    // The item keeps its dot and prefix node; it just reappears in
                    // the column after the ignored span. A completed item is also
                    // registered in this column's node cache so the root (and any
                    // waiting completer) finds it across trailing/inner ignores.
                    if self.is_complete(&item) {
                        if let ForestRef::Node(id) = item.node {
                            let key = NodeKey::Sym(self.grammar.rules[item.rule].origin);
                            next_cache.entry((key, item.origin)).or_insert(id);
                        }
                        next_col.add(item);
                    } else if self.expects_terminal(&item) {
                        next_scan.add(item);
                    } else {
                        next_col.add(item);
                    }
                }
            }
        }

        columns.push(next_col);

        // Dead end: nothing advanced into the next column and no match is still
        // pending further ahead — the input cannot be tokenized from here.
        if next_scan.items.is_empty() && columns[i + 1].items.is_empty() && delayed.is_empty() {
            return None;
        }
        Some((next_scan, next_cache))
    }
}

// ─── Forest → tree conversion ─────────────────────────────────────────────────

/// Walks the SPPF bottom-up, building parse trees through the shared
/// [`TreeBuilder`]. Symbol-node results are memoized (a forest node is reached by
/// many parents); intermediate nodes are expanded inline into their parent rule's
/// child list. Priorities are computed lazily, à la Lark's `ForestSumVisitor`.
struct Transformer<'a> {
    grammar: &'a CompiledGrammar,
    forest: &'a Forest,
    builder: TreeBuilder<'a>,
    resolve: bool,
    /// Per-terminal-id priority, summed into the forest priority only when the
    /// dynamic lexer is used (the basic lexer consumes terminal priorities in its
    /// terminal ordering). Empty otherwise.
    term_priority: HashMap<SymbolId, i32>,
    /// Memoized symbol-node values (final assembled trees).
    memo: HashMap<usize, NodeValue>,
    /// Memoized node priorities + the in-progress set for cycle-safe summing.
    prio: HashMap<usize, i32>,
    prio_visiting: HashSet<usize>,
}

impl<'a> Transformer<'a> {
    fn new(
        grammar: &'a CompiledGrammar,
        forest: &'a Forest,
        resolve: bool,
        term_priority: bool,
    ) -> Self {
        // Map each terminal id to its declared priority (only consulted under the
        // dynamic lexer; built empty otherwise so the lookup is a no-op).
        let term_priority = if term_priority {
            grammar
                .terminals
                .iter()
                .filter_map(|t| grammar.symbols.id(&t.name).map(|id| (id, t.priority)))
                .filter(|(_, p)| *p != 0)
                .collect()
        } else {
            HashMap::new()
        };
        Transformer {
            grammar,
            forest,
            builder: TreeBuilder::new(&grammar.rules),
            resolve,
            term_priority,
            memo: HashMap::new(),
            prio: HashMap::new(),
            prio_visiting: HashSet::new(),
        }
    }

    /// ForestSumVisitor: a node's priority is the max over its derivations.
    fn node_priority(&mut self, id: usize) -> i32 {
        if let Some(&p) = self.prio.get(&id) {
            return p;
        }
        if !self.prio_visiting.insert(id) {
            return 0; // cycle: contribute nothing
        }
        let forest = self.forest;
        let node = &forest.nodes[id];
        let parent_inter = node.is_intermediate;
        let mut best = if node.families.is_empty() {
            0
        } else {
            i32::MIN
        };
        for k in 0..node.families.len() {
            let p = forest.nodes[id].families[k];
            let v = self.packed_priority(&p, parent_inter);
            if v > best {
                best = v;
            }
        }
        self.prio_visiting.remove(&id);
        self.prio.insert(id, best);
        best
    }

    /// A derivation's priority: the rule's own priority (only counted at a real
    /// symbol node, not at intermediates) plus its children's priorities. Token
    /// leaves count 0 — the basic lexer already "used up" terminal priorities.
    fn packed_priority(&mut self, packed: &Packed, parent_inter: bool) -> i32 {
        let rule_prio = self.grammar.rules[packed.rule].options.priority;
        let base = if !parent_inter && rule_prio != 0 {
            rule_prio
        } else {
            0
        };
        let child = |this: &mut Self, r: ForestRef| match r {
            ForestRef::Node(id) => this.node_priority(id),
            // A scanned token contributes its terminal's priority — but only under
            // the dynamic lexer (`term_priority` is empty for the basic lexer).
            ForestRef::Tok(t) => this
                .term_priority
                .get(&this.forest.tokens[t].type_id)
                .copied()
                .unwrap_or(0),
            ForestRef::None => 0,
        };
        base + child(self, packed.left) + child(self, packed.right)
    }

    /// Family indices of `node_id` in Lark's `sort_key` order: non-empty
    /// derivations first, then higher priority, then lower rule order. A stable
    /// sort keeps insertion order among ties (which is how Lark breaks otherwise
    /// equal derivations).
    fn sorted_families(&mut self, node_id: usize) -> Vec<usize> {
        let forest = self.forest;
        let node = &forest.nodes[node_id];
        let parent_inter = node.is_intermediate;
        let prios: Vec<i32> = (0..node.families.len())
            .map(|k| {
                let p = node.families[k];
                self.packed_priority(&p, parent_inter)
            })
            .collect();
        let mut idx: Vec<usize> = (0..node.families.len()).collect();
        let fams = &forest.nodes[node_id].families;
        let grammar = self.grammar;
        idx.sort_by(|&a, &b| {
            let empty = |p: &Packed| {
                matches!(p.left, ForestRef::None) && matches!(p.right, ForestRef::None)
            };
            empty(&fams[a])
                .cmp(&empty(&fams[b]))
                .then(prios[b].cmp(&prios[a]))
                .then(
                    grammar.rules[fams[a].rule]
                        .order
                        .cmp(&grammar.rules[fams[b].rule].order),
                )
        });
        idx
    }

    /// Evaluate a real (non-intermediate) symbol node to a single value: the best
    /// derivation under `resolve`, or an `_ambig` over all of them under explicit.
    /// Returns `None` if every derivation is discarded (e.g. an ambiguity cycle).
    fn eval_symbol(&mut self, node_id: usize, visiting: &mut HashSet<usize>) -> Option<NodeValue> {
        if let Some(v) = self.memo.get(&node_id) {
            return Some(v.clone());
        }
        if !visiting.insert(node_id) {
            return None; // cycle in the forest — discard this family
        }
        let order = self.sorted_families(node_id);

        let result = if self.resolve {
            let mut chosen = None;
            for fi in order {
                let packed = self.forest.nodes[node_id].families[fi];
                if let Some(list) = self.expand_packed(&packed, visiting).into_iter().next() {
                    chosen = Some(self.builder.assemble(packed.rule, list));
                    break;
                }
            }
            chosen
        } else {
            let mut derivs: Vec<NodeValue> = Vec::new();
            let mut keys: HashSet<String> = HashSet::new();
            for fi in order {
                let packed = self.forest.nodes[node_id].families[fi];
                for list in self.expand_packed(&packed, visiting) {
                    let v = self.builder.assemble(packed.rule, list);
                    if keys.insert(node_value_key(&v)) {
                        derivs.push(v);
                    }
                }
            }
            match derivs.len() {
                0 => None,
                1 => Some(derivs.pop().unwrap()),
                _ => {
                    let children: Vec<Child> =
                        derivs.into_iter().map(node_value_to_child).collect();
                    Some(NodeValue::Tree(Tree::new("_ambig", children)))
                }
            }
        };

        visiting.remove(&node_id);
        if let Some(v) = &result {
            self.memo.insert(node_id, v.clone());
        }
        result
    }

    /// Expand an intermediate node into the alternative child-lists it contributes
    /// to its parent rule. Under `resolve` only the best (first non-discarded)
    /// derivation is kept.
    fn expand_intermediate(
        &mut self,
        node_id: usize,
        visiting: &mut HashSet<usize>,
    ) -> Vec<Vec<NodeValue>> {
        if !visiting.insert(node_id) {
            return Vec::new(); // cycle — discard
        }
        let order = self.sorted_families(node_id);
        let mut out: Vec<Vec<NodeValue>> = Vec::new();
        for fi in order {
            let packed = self.forest.nodes[node_id].families[fi];
            let lists = self.expand_packed(&packed, visiting);
            if self.resolve {
                if !lists.is_empty() {
                    out = lists;
                    break;
                }
            } else {
                out.extend(lists);
            }
        }
        visiting.remove(&node_id);
        out
    }

    /// Expand one derivation (packed node) into its rule's child-lists. `left` is
    /// always an intermediate (the accumulated prefix) or nothing; `right` is the
    /// symbol just consumed (a symbol node or token leaf) or nothing (ε).
    fn expand_packed(
        &mut self,
        packed: &Packed,
        visiting: &mut HashSet<usize>,
    ) -> Vec<Vec<NodeValue>> {
        let lefts: Vec<Vec<NodeValue>> = match packed.left {
            ForestRef::None => vec![Vec::new()],
            ForestRef::Node(id) => self.expand_intermediate(id, visiting),
            ForestRef::Tok(t) => vec![vec![NodeValue::Token(self.forest.tokens[t].clone())]],
        };
        if lefts.is_empty() {
            return Vec::new();
        }

        let right_val: Option<NodeValue> = match packed.right {
            ForestRef::None => None,
            ForestRef::Node(id) => match self.eval_symbol(id, visiting) {
                Some(v) => Some(v),
                None => return Vec::new(), // right discarded → whole derivation gone
            },
            ForestRef::Tok(t) => Some(NodeValue::Token(self.forest.tokens[t].clone())),
        };

        lefts
            .into_iter()
            .map(|mut list| {
                if let Some(rv) = &right_val {
                    list.push(rv.clone());
                }
                list
            })
            .collect()
    }
}

/// A stable structural key for de-duplicating equal `_ambig` derivations.
fn node_value_key(v: &NodeValue) -> String {
    fn child_key(c: &Child, out: &mut String) {
        match c {
            Child::Tree(t) => tree_key(t, out),
            Child::Token(t) => {
                out.push_str("T:");
                out.push_str(&t.type_);
                out.push('=');
                out.push_str(&t.value);
            }
            Child::None => out.push_str("None"),
        }
    }
    fn tree_key(t: &Tree, out: &mut String) {
        out.push('(');
        out.push_str(&t.data);
        for c in &t.children {
            out.push(' ');
            child_key(c, out);
        }
        out.push(')');
    }
    let mut out = String::new();
    match v {
        NodeValue::Token(t) => {
            out.push_str("T:");
            out.push_str(&t.type_);
            out.push('=');
            out.push_str(&t.value);
        }
        NodeValue::Tree(t) => tree_key(t, &mut out),
        NodeValue::Inline(cs) => {
            out.push_str("I[");
            for c in cs {
                child_key(c, &mut out);
                out.push(',');
            }
            out.push(']');
        }
    }
    out
}

/// One `_ambig` alternative as a tree child.
fn node_value_to_child(v: NodeValue) -> Child {
    match v {
        NodeValue::Tree(t) => Child::Tree(t),
        NodeValue::Token(t) => Child::Token(t),
        NodeValue::Inline(mut cs) if cs.len() == 1 => cs.pop().unwrap(),
        NodeValue::Inline(cs) => Child::Tree(Tree::new("_ambig", cs)),
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
