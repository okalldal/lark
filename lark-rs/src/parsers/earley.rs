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
//! Consequence (measured under #56): hand-written right recursion (`a: X a | X`)
//! costs O(n²) completed items — parity with the Python reference, not a
//! regression. The completer no longer *also* pays an O(column) rescan on top of
//! that: each [`Column`] indexes its waiters by expected symbol (see `Column.waiting`).
//! Linearizing right recursion outright needs the Leo optimization (tracked follow-up).
//!
//! Nullable handling follows the reference's *held completions* (`H` in Scott's
//! paper): when an ε-derivation completes at a column it is remembered, so a
//! later prediction of that same nullable symbol can advance immediately without
//! a separate ε-closure pass — and the chart still terminates.
//!
//! ## Forest → tree
//!
//! [`Transformer`] walks the SPPF bottom-up, reusing the shared
//! [`TreeOutputBuilder`](super::tree_builder::TreeOutputBuilder) for every rule's tree
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

use super::tree_builder::{Slot, TreeOutputBuilder};

// Backward-compat alias within earley — keeps diff minimal for this refactor.
type NodeValue = Slot;

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
    /// Index from the non-terminal an item expects next → positions in `items` of
    /// the items waiting on it. The completer reads only the relevant bucket
    /// instead of rescanning the whole column with a linear `.filter`. That rescan
    /// is the "completer rescans the origin column" cost #54 suspected and #56
    /// confirmed: later columns accumulate O(n) completed items, so an unindexed
    /// scan is O(column) per completion → super-linear on a right-recursive grammar
    /// (`a: X a | X`), even though only O(1) items per column actually wait on any
    /// given symbol. This index is the Joop-Leo-free cure (the reference's Leo
    /// transitives are dead code; this fixes the same asymptotics without them).
    waiting: HashMap<SymbolId, Vec<usize>>,
}

impl Column {
    fn new() -> Self {
        Column::default()
    }

    /// Add `item` unless an equal one (same rule, dot, origin) is already present;
    /// returns whether it was newly inserted. `expected` is the non-terminal `item`
    /// expects next (`None` if it is complete), used to index it for the completer.
    fn add(&mut self, item: Item, expected: Option<SymbolId>) -> bool {
        if self.seen.insert((item.rule, item.dot, item.origin)) {
            let idx = self.items.len();
            self.items.push(item);
            if let Some(sym) = expected {
                self.waiting.entry(sym).or_default().push(idx);
            }
            true
        } else {
            false
        }
    }

    /// Positions in `items` of the items waiting on `sym`, in insertion order
    /// (load-bearing for resolve tie-breaks). Empty if none — an O(1) lookup that
    /// replaces the O(column) rescan.
    fn waiting_on(&self, sym: SymbolId) -> &[usize] {
        self.waiting.get(&sym).map(Vec::as_slice).unwrap_or(&[])
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
    /// Expansion position of `right` within `rule` (the dot before it was
    /// consumed). Lets the streaming forest walk apply per-position token
    /// filtering without rebuilding a per-symbol value list. Unused when
    /// `right` is `None` (an ε derivation).
    right_pos: usize,
}

/// A symbol or intermediate node. Its `families` are the alternative derivations
/// (packed nodes); more than one means the node is ambiguous.
struct SymbolNode {
    is_intermediate: bool,
    families: Vec<Packed>,
    family_set: HashSet<(ForestRef, ForestRef)>,
    /// Joop-Leo deferred reconstructions: `(transitive, bottom_node, end_col)`. A
    /// Leo completion records a path here instead of materializing the O(n) skipped
    /// reduction nodes eagerly; `load_leo_paths` expands them (once, lazily) into
    /// `families` before the forest→tree walk. Empty for non-Leo nodes.
    paths: Vec<(usize, ForestRef, usize)>,
}

/// A Joop-Leo transitive item, memoized per `(start column, recognized symbol)`.
///
/// It records the one deterministic reduction step a completion of `recognized`
/// (spanning `(key_start, i)`) takes: it advances the unique originator `red`
/// (`= [B → β•(recognized)γ, red.origin]`, with `red.node` its built-so-far left
/// child) and, since `γ` is empty, completes `B`. `parent` chains one level up
/// toward `top`, the topmost item the whole chain collapses to. The completer
/// jumps straight to `top`; `load_leo_paths` walks the `parent` chain to rebuild
/// the skipped reduction spine on demand.
#[derive(Clone, Copy)]
struct Trans {
    /// The recognized non-terminal and the column it starts at (the map key).
    recognized: SymbolId,
    key_start: usize,
    /// The unique originator `[B → β•(recognized)γ, red.origin]` being advanced;
    /// `red.node` is its built-so-far left child.
    red: Item,
    /// Next level up the deterministic reduction path, or `None` if this level's
    /// completion (`B`) is itself the topmost.
    parent: Option<usize>,
    /// The topmost item the chain collapses to (its `node` is unused). Identical
    /// for every level of one chain; the completer builds it at `(top.origin, i)`.
    top: Item,
}

/// Arena of forest nodes + the scanned-token leaves they reference by index.
struct Forest {
    nodes: Vec<SymbolNode>,
    tokens: Vec<Token>,
    /// Global identity index: `(key, start, end) → node id`. A node is one symbol
    /// (or intermediate dotted rule) over one span, no matter how many derivations
    /// reach it — every completion of `(key, start, end)` merges its family here.
    /// Keying on `end` (not a per-column cache) is what lets Joop-Leo's lazy spine
    /// reconstruction *reuse* the chart's existing nodes instead of forking a
    /// parallel copy, so a symbol's Leo-derived and normally-derived families land
    /// in the same node (required for `ambiguity='resolve'` to compare them).
    index: HashMap<(NodeKey, usize, usize), usize>,
}

impl Forest {
    fn new() -> Self {
        Forest {
            nodes: Vec::new(),
            tokens: Vec::new(),
            index: HashMap::new(),
        }
    }

    /// Node id for the symbol/intermediate `key` spanning `(start, end)`, creating
    /// it on first sight and returning the same id on every later request.
    fn get_or_create(&mut self, key: NodeKey, start: usize, end: usize) -> usize {
        if let Some(&id) = self.index.get(&(key, start, end)) {
            return id;
        }
        let id = self.nodes.len();
        self.nodes.push(SymbolNode {
            is_intermediate: key.is_intermediate(),
            families: Vec::new(),
            family_set: HashSet::new(),
            paths: Vec::new(),
        });
        self.index.insert((key, start, end), id);
        crate::perf::add_forest_node();
        id
    }

    /// Record a derivation (packed node) on `node_id`, de-duplicated by its
    /// `(left, right)` children exactly as Python's `PackedNode` equality.
    fn add_family(
        &mut self,
        node_id: usize,
        rule: usize,
        left: ForestRef,
        right: ForestRef,
        right_pos: usize,
    ) {
        let node = &mut self.nodes[node_id];
        if node.family_set.insert((left, right)) {
            node.families.push(Packed {
                rule,
                left,
                right,
                right_pos,
            });
        }
    }

    fn add_token(&mut self, token: Token) -> usize {
        let id = self.tokens.len();
        self.tokens.push(token);
        id
    }

    /// Record a Joop-Leo deferred reconstruction on `node_id` (which spans
    /// `(_, end)`): completing the chain bottom (`bottom`) under transitive
    /// `trans` rebuilds, lazily, the reduction spine whose top is this node.
    fn add_path(&mut self, node_id: usize, trans: usize, bottom: ForestRef, end: usize) {
        self.nodes[node_id].paths.push((trans, bottom, end));
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
    /// `nullable[id.index()]` = the symbol can derive ε. Indexed by `SymbolId`.
    /// Used by [`Self::eps_node`] to rebuild a skipped ε-tail.
    nullable: Vec<bool>,
    /// `eps_only[id.index()]` = the symbol can derive **only** ε (nullable *and*
    /// cannot derive any non-empty string). Used by the Joop-Leo completer
    /// (`is_quasi_complete`) to admit a nullable tail after the recognized symbol
    /// (#64) ONLY when the tail is ε-only: an *optional* tail (nullable but able
    /// to match real tokens, e.g. `opt: Y |`) must NOT be linearized, or the
    /// non-empty derivation becomes unreachable and valid input is rejected.
    eps_only: Vec<bool>,
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
            .ok_or_else(|| ParseError::unexpected_eof(0, 0, vec![]))?;
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
            .ok_or_else(|| ParseError::unexpected_eof(0, 0, vec![]))?;
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
            // when a top-level `?start` collapses a lone-`None` placeholder (RC9 fix
            // in tree_builder: lone-`None` expand1 → `Inline([None])`). The public
            // `ParseTree` can't hold a bare `None`, so the single-`None` arm below
            // falls back to an empty start node — that root-`?start` corner is a
            // separate tracked divergence (#289). Stay defensive rather than panic.
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

        // Joop-Leo state. `transitives[j]` maps a recognized symbol to the
        // memoized transitive rooted at column `j` (grown in lockstep with
        // `columns`); `trans_arena` is the stable backing store the `parent`
        // links index into.
        let mut transitives: Vec<HashMap<SymbolId, usize>> = vec![HashMap::new()];
        let mut trans_arena: Vec<Trans> = Vec::new();

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
                    columns[0].add(item, self.expect(&item));
                }
            }
        }

        let mut i = 0;
        loop {
            while transitives.len() < columns.len() {
                transitives.push(HashMap::new());
            }
            self.predict_and_complete(
                i,
                &mut columns,
                &mut to_scan,
                &mut forest,
                &mut transitives,
                &mut trans_arena,
                start_id,
            );
            if i == n {
                break;
            }
            let token = toks[i];
            match self.scan(token, &mut columns, &to_scan, &mut forest) {
                Some(next_scan) => {
                    to_scan = next_scan;
                }
                None => {
                    // No per-state expected set exists for Earley (the scan set
                    // is per-item, not a table row), so the report carries none.
                    return Err(ParseError::unexpected_token(token, vec![]));
                }
            }
            i += 1;
        }

        // The root is the completed start symbol spanning (0, n): one node in the
        // forest's global index, holding all of its derivations. Expand the
        // Joop-Leo deferred reconstructions reachable from it before the forest→tree
        // walk reads them.
        let root = forest.index.get(&(NodeKey::Sym(start_id), 0, n)).copied();
        if let Some(root) = root {
            self.load_leo_paths(&mut forest, &trans_arena, root);
        }
        root.map(|root| (forest, root)).ok_or_else(|| {
            let (line, col) = toks
                .last()
                .map(|t| (t.end_line.max(t.line), t.end_column.max(t.column)))
                .unwrap_or((1, 1));
            ParseError::unexpected_eof(line, col, vec![])
        })
    }

    /// Scott's predictor + completer for one column. Processes the column as a
    /// LIFO worklist (matching the reference's `deque.pop()`), so newly derived
    /// items are handled before older ones — the order that fixes resolve
    /// tie-breaks.
    #[allow(clippy::too_many_arguments)]
    fn predict_and_complete(
        &self,
        i: usize,
        columns: &mut Vec<Column>,
        to_scan: &mut ScanSet,
        forest: &mut Forest,
        transitives: &mut [HashMap<SymbolId, usize>],
        trans_arena: &mut Vec<Trans>,
        start_id: SymbolId,
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
                        let id = forest.get_or_create(NodeKey::Sym(origin), item.origin, i);
                        // ε derivation: no right child, so right_pos is unused.
                        forest.add_family(id, item.rule, ForestRef::None, ForestRef::None, 0);
                        id
                    }
                };

                if item.origin == i {
                    held.insert(origin, node_id);
                }

                // ── Joop-Leo right-recursion shortcut ─────────────────────────
                // For a non-empty completion, if the reduction path out of this
                // symbol is *deterministic* (a unique quasi-complete originator at
                // each step), memoize/extend the transitive chain and jump straight
                // to the topmost item instead of cascading through every
                // intermediate completion. The skipped reduction spine is recorded
                // as a deferred path on the topmost node (`load_leo_paths` rebuilds
                // it lazily) so the SPPF stays identical to the non-Leo forest.
                // This collapses O(n²) right-recursion completions to O(n) — and,
                // crucially, the regular waiter cascade below (which the perf
                // counter measures) is skipped entirely on the Leo path.
                if item.origin != i {
                    self.create_leo(
                        origin,
                        item.origin,
                        columns,
                        transitives,
                        trans_arena,
                        start_id,
                    );
                    if let Some(&t) = transitives[item.origin].get(&origin) {
                        let top = trans_arena[t].top;
                        let top_key = self.node_key(top.rule, top.dot);
                        let top_node = forest.get_or_create(top_key, top.origin, i);
                        forest.add_path(top_node, t, ForestRef::Node(node_id), i);
                        let new_item = Item {
                            node: ForestRef::Node(top_node),
                            ..top
                        };
                        if self.expects_terminal(&new_item) {
                            to_scan.add(new_item);
                        } else if columns[i].add(new_item, self.expect(&new_item)) {
                            stack.push(new_item);
                        }
                        continue; // Leo handled it — skip the regular waiter cascade
                    }
                }

                // Advance every item in the origin column that was waiting on this
                // non-terminal. The column's `waiting` index gives exactly those
                // items in O(matches) instead of an O(column) rescan; snapshot into
                // an owned Vec first (we mutate columns[i] below).
                //
                // Counting the items examined here (deterministically, behind the
                // `perf-counters` feature) is the Arm-1 scaling signal #56 demands:
                // it now stays flat per input byte even on right recursion, the
                // gateable proof the rescan quadratic is gone (`tests/test_earley_scaling.rs`).
                let origin_col = &columns[item.origin];
                let waiters = origin_col.waiting_on(origin);
                crate::perf::add_completer_scan_steps(waiters.len() as u64);
                let originators: Vec<Item> =
                    waiters.iter().map(|&idx| origin_col.items[idx]).collect();

                for o in originators {
                    let key = self.node_key(o.rule, o.dot + 1);
                    let new_node = forest.get_or_create(key, o.origin, i);
                    // `origin` was expected at position `o.dot`, so the completed
                    // node is the right child at that expansion position.
                    forest.add_family(new_node, o.rule, o.node, ForestRef::Node(node_id), o.dot);
                    let advanced = Item {
                        rule: o.rule,
                        dot: o.dot + 1,
                        origin: o.origin,
                        node: ForestRef::Node(new_node),
                    };
                    if self.expects_terminal(&advanced) {
                        to_scan.add(advanced);
                    } else if columns[i].add(advanced, self.expect(&advanced)) {
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
                    let new_node = forest.get_or_create(key, item.origin, i);
                    // The held (ε) symbol was expected at position `item.dot`.
                    forest.add_family(
                        new_node,
                        item.rule,
                        item.node,
                        ForestRef::Node(hnode),
                        item.dot,
                    );
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
                    } else if columns[i].add(new, self.expect(&new)) {
                        stack.push(new);
                    }
                }
            }
        }
    }

    /// A reduction path through `item` is taken by Leo only when advancing past
    /// the symbol at the dot leaves the rule *deterministically completable* — i.e.
    /// consuming the recognized symbol either completes the rule (strict right
    /// recursion, `a: X a | X`) or leaves only a **nullable tail** that ε-derives
    /// to completion (`a: X a opt | X` with `opt:` ε-only, the minimal
    /// nullable-tail shape — #64). The topmost item is then non-complete, and the
    /// SPPF spine reconstruction threads the tail's ε-completions through it
    /// (`materialize_leo_paths`), the case upstream Lark never finished
    /// (lark-parser/lark#397).
    ///
    /// **The tail must be ε-ONLY, not merely nullable.** Linearizing collapses the
    /// tail to ε on the Leo shortcut and skips the regular completer's cascade; if
    /// a tail symbol could also match real tokens (an *optional* tail like
    /// `opt: Y |`), that non-empty derivation would become unreachable and valid
    /// input (`a: X a opt | X`, `opt: Y |` on `"xxy"`) would be wrongly rejected.
    /// So `eps_only` (strictly stronger than nullable) is the admission test.
    /// (Python Lark's reference uses `NULLABLE` here, but its Leo completer is dead
    /// code — lark-parser/lark#397 — so it never exercised this distinction; ours
    /// is a live reimplementation and must get it right.)
    ///
    /// The `start_id` guard refuses to special-case a directly self-recursive
    /// start — at the recognized position OR anywhere in the tail (matches the
    /// original reference guard), falling back to the regular completer there.
    fn is_quasi_complete(&self, item: &Item, start_id: SymbolId) -> bool {
        let expansion = &self.grammar.rules[item.rule].expansion;
        let origin = self.grammar.rules[item.rule].origin;
        // Refuse a directly self-recursive start at the recognized position.
        if origin == start_id && expansion[item.dot] == start_id {
            return false;
        }
        // Walk the tail after the recognized symbol (positions `item.dot + 1 ..`).
        // Each must be ε-only (so the rule completes with no further input AND the
        // Leo collapse cannot drop a real derivation), and none may re-enter the
        // recursive start.
        for &sym in &expansion[item.dot + 1..] {
            if !self.eps_only[sym.index()] {
                return false;
            }
            if origin == start_id && sym == start_id {
                return false;
            }
        }
        true
    }

    /// Memoize (or extend) the Joop-Leo transitive chain for completing
    /// `recognized` at column `start`. Walks the deterministic reduction path
    /// upward — each step requires a *unique* quasi-complete originator — recording
    /// one [`Trans`] per level, all sharing the topmost item the chain collapses
    /// to. A no-op if the chain already exists or the path is not deterministic.
    fn create_leo(
        &self,
        recognized: SymbolId,
        start: usize,
        columns: &[Column],
        transitives: &mut [HashMap<SymbolId, usize>],
        trans_arena: &mut Vec<Trans>,
        start_id: SymbolId,
    ) {
        // Benchmark/test affordance (perf-counters only): with Leo disabled, never
        // build transitives, so the completer falls back to the regular cascade —
        // the "without the fix" arm of the #58 before/after proof.
        if crate::perf::leo_disabled() {
            return;
        }
        if transitives[start].contains_key(&recognized) {
            return;
        }
        let mut visited: HashSet<(usize, SymbolId)> = HashSet::new();
        // Levels bottom→top: (recognized, key_start, originator item).
        let mut to_create: Vec<(SymbolId, usize, Item)> = Vec::new();
        let mut rec = recognized;
        let mut col = start;
        // The transitive already present at the top of the walk (if it stopped by
        // meeting an existing chain), and the topmost item the chain collapses to.
        let mut parent: Option<usize> = None;
        let mut top: Option<Item> = None;
        loop {
            if let Some(&t) = transitives[col].get(&rec) {
                parent = Some(t);
                top = Some(trans_arena[t].top);
                break;
            }
            if !visited.insert((col, rec)) {
                break; // cycle guard
            }
            let waiters = columns[col].waiting_on(rec);
            if waiters.len() != 1 {
                break;
            }
            let o = columns[col].items[waiters[0]];
            if !self.is_quasi_complete(&o, start_id) {
                break;
            }
            to_create.push((rec, col, o));
            // The topmost item the chain reaches is the *completed* form of the
            // highest unique originator seen so far — its dot at the end of the
            // rule (its `node` is unused downstream). For strict right recursion
            // `o.dot + 1` already equals the length; with a nullable tail (#64) the
            // tail's ε-completions are consumed here so `top` lands on the
            // completed `Sym(origin)` node, exactly as the regular completer would
            // after held-ε-advancing through the tail. `materialize_leo_paths`
            // rebuilds the skipped intermediate + ε-tail spine lazily.
            top = Some(Item {
                rule: o.rule,
                dot: self.grammar.rules[o.rule].expansion.len(),
                origin: o.origin,
                node: ForestRef::None,
            });
            rec = self.grammar.rules[o.rule].origin;
            col = o.origin;
        }
        let Some(top) = top else { return };
        // Build top→bottom so each new level's `parent` is already created.
        for &(rec, key_start, o) in to_create.iter().rev() {
            let tid = trans_arena.len();
            trans_arena.push(Trans {
                recognized: rec,
                key_start,
                red: o,
                parent,
                top,
            });
            transitives[key_start].insert(rec, tid);
            parent = Some(tid);
        }
    }

    /// Expand every deferred Joop-Leo path into real packed families. For each
    /// recorded `(transitive chain, bottom node, end)` on a topmost node, walk the
    /// chain top→bottom rebuilding one symbol node per skipped reduction level —
    /// the same spine the non-Leo completer would have built, but materialized once
    /// here (O(n)) instead of O(n²) times during the parse.
    fn load_leo_paths(&self, forest: &mut Forest, trans_arena: &[Trans], root: usize) {
        // Reachability DFS from the root: expand only the paths the forest->tree
        // walk will actually read. A Leo completion records a top node at EVERY
        // column, so `start` over every prefix carries a path; expanding them all
        // would rebuild a length-c spine for every column c -- back to O(n^2).
        // Touching only nodes reachable from the real root keeps reconstruction
        // O(n), mirroring Python's `load_paths`-on-`children` laziness.
        let mut stack = vec![root];
        let mut visited: HashSet<usize> = HashSet::new();
        while let Some(id) = stack.pop() {
            if !visited.insert(id) {
                continue;
            }
            self.materialize_leo_paths(forest, trans_arena, id);
            // Descend into the now-complete family children so their own paths, if
            // any, expand too — but only along edges reachable from the root.
            for p in &forest.nodes[id].families {
                for r in [p.left, p.right] {
                    if let ForestRef::Node(c) = r {
                        stack.push(c);
                    }
                }
            }
        }
    }

    /// Expand the deferred Joop-Leo paths recorded on a *single* node into real
    /// packed families (the per-node body of [`load_leo_paths`]). Each recorded
    /// `(transitive chain, bottom node, end)` is walked top→bottom, rebuilding one
    /// symbol node per skipped reduction level. The chain's topmost level lands on
    /// this node itself (the path owner), so afterwards the node carries its
    /// derivations as families. The node's `paths` are taken, so a later call (or
    /// the reachability DFS) never re-expands them.
    ///
    /// Used both by `load_leo_paths`'s DFS and by the dynamic scanner's `%ignore`
    /// carry-over, which must materialize a carried completed item's Leo-deferred
    /// derivation before copying its families to the carried-span node.
    fn materialize_leo_paths(&self, forest: &mut Forest, trans_arena: &[Trans], id: usize) {
        let paths = std::mem::take(&mut forest.nodes[id].paths);
        for (bottom_trans, _bottom_node, end) in paths {
            // Collect the chain bottom→top via `parent`, then walk top→bottom.
            // Every node is addressed by its global `(key, start, end)` identity,
            // so the top level reuses this node, the bottom reuses the chart's
            // already-built completed node, and any intermediate symbol nodes
            // merge with whatever the chart already has at that span — exactly
            // the merge `ambiguity='resolve'` needs.
            let mut chain = Vec::new();
            let mut cur = Some(bottom_trans);
            while let Some(t) = cur {
                chain.push(t);
                cur = trans_arena[t].parent;
            }
            for &t in chain.iter().rev() {
                let tr = trans_arena[t];
                // Build the node for the originator advanced past the recognized
                // symbol: `[red.rule → … recognized • γ]`. With strict right
                // recursion γ is empty, so this is already the completed
                // `Sym(origin)`; with a nullable tail it is the intermediate
                // `Inter(red.rule, red.dot + 1)`.
                let mut prev_node = forest.get_or_create(
                    self.node_key(tr.red.rule, tr.red.dot + 1),
                    tr.red.origin,
                    end,
                );
                let right = forest.get_or_create(NodeKey::Sym(tr.recognized), tr.key_start, end);
                forest.add_family(
                    prev_node,
                    tr.red.rule,
                    tr.red.node,
                    ForestRef::Node(right),
                    tr.red.dot,
                );
                // Thread the nullable tail (#64): each remaining symbol after the
                // recognized one is nullable (guaranteed by `is_quasi_complete`),
                // so it ε-completes at `end`. Advance one binarized level per tail
                // position, each carrying the tail symbol's ε-node as the right
                // child — exactly the spine the regular completer's held-ε
                // advancing would have built, but materialized once here. The empty
                // span `(end, end)` reuses any ε-node the chart already built.
                let expansion = &self.grammar.rules[tr.red.rule].expansion;
                for pos in (tr.red.dot + 1)..expansion.len() {
                    let tail_sym = expansion[pos];
                    let mut building = HashSet::new();
                    let eps = self.eps_node(forest, tail_sym, end, &mut building);
                    let next_node = forest.get_or_create(
                        self.node_key(tr.red.rule, pos + 1),
                        tr.red.origin,
                        end,
                    );
                    forest.add_family(
                        next_node,
                        tr.red.rule,
                        ForestRef::Node(prev_node),
                        ForestRef::Node(eps),
                        pos,
                    );
                    prev_node = next_node;
                }
            }
        }
    }

    /// Build (or reuse) the SPPF node for a *nullable* symbol's ε-derivation at the
    /// empty span `(col, col)`, returning its node id. Mirrors the ε-nodes the
    /// regular completer materializes for a held ε-completion: a node keyed by its
    /// global `(Sym(sym), col, col)` identity, carrying one family per fully
    /// nullable rule of `sym`, each binarized over that rule's own (nullable)
    /// children. Used by the Joop-Leo nullable-tail spine reconstruction (#64) to
    /// thread the skipped ε-tail. Idempotent: a node whose families are already
    /// present is returned untouched (it merges with the chart's own ε-node when
    /// the chart built one), and recursion terminates because every ε-child spans
    /// the same empty column and is dedup-guarded by the global index.
    ///
    /// `building` is a per-reconstruction "already descended into" set guarding a
    /// nullable ε-cycle (e.g. `a: b |`, `b: a`): a symbol already entered returns
    /// its node without re-descending, so the reconstruction terminates. It is
    /// monotonic (never un-set) — safe because every symbol this builds ends with
    /// non-empty `families`, so any later visit short-circuits at the
    /// `families.is_empty()` check above rather than relying on the guard. Such
    /// mutually-ε-recursive nullable symbols are outside the linearization target;
    /// the regular completer still builds their canonical ε-node, which this node
    /// merges into by global identity (`add_family` dedups by `(left, right)`).
    fn eps_node(
        &self,
        forest: &mut Forest,
        sym: SymbolId,
        col: usize,
        building: &mut HashSet<SymbolId>,
    ) -> usize {
        let id = forest.get_or_create(NodeKey::Sym(sym), col, col);
        // Already populated (by the chart or an earlier call): reuse as-is.
        if !forest.nodes[id].families.is_empty() {
            return id;
        }
        // ε-cycle guard: this symbol is already being built further up the stack.
        if !building.insert(sym) {
            return id;
        }
        // Build a family for each fully nullable production of `sym`.
        let Some(prods) = self.rules_by_origin.get(&sym) else {
            return id;
        };
        let prods = prods.clone();
        for ri in prods {
            let expansion = &self.grammar.rules[ri].expansion;
            // Only productions that derive ε wholesale (every symbol nullable)
            // contribute an ε-derivation.
            if !expansion.iter().all(|s| self.nullable[s.index()]) {
                continue;
            }
            if expansion.is_empty() {
                // The empty production: Scott's (None, None) ε family.
                forest.add_family(id, ri, ForestRef::None, ForestRef::None, 0);
            } else {
                // A non-empty all-nullable production: binarize its ε-children
                // left to right, mirroring the regular completer's spine.
                let mut left = ForestRef::None;
                let len = expansion.len();
                for pos in 0..len {
                    let child = self.eps_node(forest, expansion[pos], col, building);
                    let key = self.node_key(ri, pos + 1);
                    let node = forest.get_or_create(key, col, col);
                    forest.add_family(node, ri, left, ForestRef::Node(child), pos);
                    left = ForestRef::Node(node);
                }
            }
        }
        id
    }

    /// Scott's scanner: advance every terminal-expecting item that matches
    /// `token`, recording a token-leaf packed node. Returns the next column's scan
    /// buffer, or `None` if nothing matched (a parse failure).
    fn scan(
        &self,
        token: &Token,
        columns: &mut Vec<Column>,
        to_scan: &ScanSet,
        forest: &mut Forest,
    ) -> Option<ScanSet> {
        let mut next_scan = ScanSet::new();
        let mut next_col = Column::new();
        // The column being built is the next one (its index = current length).
        let end = columns.len();

        // One token leaf per position, shared by every item that scans it (so
        // packed-node de-duplication works).
        let tok_ref = ForestRef::Tok(forest.add_token(token.clone()));

        for item in &to_scan.items {
            if self.expect(item) == Some(token.type_id) {
                let key = self.node_key(item.rule, item.dot + 1);
                let new_node = forest.get_or_create(key, item.origin, end);
                // The scanned terminal is the right child at position `item.dot`.
                forest.add_family(new_node, item.rule, item.node, tok_ref, item.dot);
                let advanced = Item {
                    rule: item.rule,
                    dot: item.dot + 1,
                    origin: item.origin,
                    node: ForestRef::Node(new_node),
                };
                if self.expects_terminal(&advanced) {
                    next_scan.add(advanced);
                } else {
                    next_col.add(advanced, self.expect(&advanced));
                }
            }
        }

        if next_scan.items.is_empty() && next_col.items.is_empty() {
            return None;
        }
        columns.push(next_col);
        Some(next_scan)
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
                    columns[0].add(item, self.expect(&item));
                }
            }
        }

        let mut transitives: Vec<HashMap<SymbolId, usize>> = vec![HashMap::new()];
        let mut trans_arena: Vec<Trans> = Vec::new();

        for i in 0..=n {
            while transitives.len() < columns.len() {
                transitives.push(HashMap::new());
            }
            self.predict_and_complete(
                i,
                &mut columns,
                &mut to_scan,
                &mut forest,
                &mut transitives,
                &mut trans_arena,
                start_id,
            );
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
                &trans_arena,
            ) {
                Some(next_scan) => {
                    to_scan = next_scan;
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

        let root = forest.index.get(&(NodeKey::Sym(start_id), 0, n)).copied();
        if let Some(root) = root {
            self.load_leo_paths(&mut forest, &trans_arena, root);
        }
        root.map(|root| (forest, root)).ok_or_else(|| {
            ParseError::unexpected_eof(
                *lines.last().unwrap_or(&1),
                *cols.last().unwrap_or(&1),
                vec![],
            )
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
        trans_arena: &[Trans],
    ) -> Option<ScanSet> {
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
        let end = i + 1;

        for d in delayed.remove(&(i + 1)).unwrap_or_default() {
            match d {
                Delayed::Tok { item, token } => {
                    let key = self.node_key(item.rule, item.dot + 1);
                    let new_node = forest.get_or_create(key, item.origin, end);
                    let tok_ref = ForestRef::Tok(forest.add_token(token));
                    // The scanned terminal is the right child at position `item.dot`.
                    forest.add_family(new_node, item.rule, item.node, tok_ref, item.dot);
                    let advanced = Item {
                        rule: item.rule,
                        dot: item.dot + 1,
                        origin: item.origin,
                        node: ForestRef::Node(new_node),
                    };
                    if self.expects_terminal(&advanced) {
                        next_scan.add(advanced);
                    } else {
                        next_col.add(advanced, self.expect(&advanced));
                    }
                }
                Delayed::Carry { item } => {
                    // The item keeps its dot, but it now spans one step further (past
                    // the ignored text). Re-anchor its forest node to the carried span
                    // `[origin, end]` through the global `(key, origin, end)` index and
                    // merge in the families of its pre-ignore node — xearley.py's
                    // ignore carry-over (the `token is None` branch), which builds a
                    // fresh node at the new label and copies the old node's children.
                    //
                    // Re-anchoring is what makes the carried derivation survive: a
                    // completion or scan landing at the same `(key, origin, end)` shares
                    // this very node, so their derivations *merge* as alternative
                    // families instead of one shadowing the other. The previous
                    // `index.entry(..).or_insert(..)` dropped the carried derivation on
                    // any such collision (and `ScanSet`/`Column` dedup keeps a single
                    // item, so the lone surviving item must point at the merged node).
                    let carried = if let ForestRef::Node(old) = item.node {
                        // The carried derivation may be a deferred Joop-Leo path
                        // rather than a materialized family (a completed right-
                        // recursive rule reached through the Leo shortcut). Force it
                        // into families first, otherwise the copy below would carry an
                        // empty node and the derivation would be lost across the
                        // ignore (the previous `or_insert` kept the *same* node id, so
                        // the deferred path stayed reachable from the root; copying
                        // into a fresh node does not, so it must be materialized now).
                        self.materialize_leo_paths(forest, trans_arena, old);
                        let key = self.node_key(item.rule, item.dot);
                        let new_node = forest.get_or_create(key, item.origin, end);
                        if old != new_node {
                            let fams = forest.nodes[old].families.clone();
                            for f in fams {
                                forest.add_family(new_node, f.rule, f.left, f.right, f.right_pos);
                            }
                        }
                        Item {
                            node: ForestRef::Node(new_node),
                            ..item
                        }
                    } else {
                        item
                    };
                    if self.is_complete(&carried) {
                        next_col.add(carried, None);
                    } else if self.expects_terminal(&carried) {
                        next_scan.add(carried);
                    } else {
                        next_col.add(carried, self.expect(&carried));
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
        Some(next_scan)
    }
}

// ─── Forest → tree conversion ─────────────────────────────────────────────────

/// Walks the SPPF bottom-up, building parse trees through the shared
/// [`TreeOutputBuilder`]. Symbol-node results are memoized (a forest node is reached by
/// many parents); intermediate nodes are expanded inline into their parent rule's
/// child list. Priorities are computed lazily, à la Lark's `ForestSumVisitor`.
struct Transformer<'a> {
    grammar: &'a CompiledGrammar,
    forest: &'a Forest,
    builder: TreeOutputBuilder<'a>,
    resolve: bool,
    /// Per-terminal-id priority, summed into the forest priority only when the
    /// dynamic lexer is used (the basic lexer consumes terminal priorities in its
    /// terminal ordering). Empty otherwise.
    term_priority: HashMap<SymbolId, i32>,
    /// Memoized symbol-node values (final assembled trees).
    memo: HashMap<usize, NodeValue>,
    /// Memoized per-symbol-node derivation lists (the deduped alternative values
    /// before they are collapsed into a single value / `_ambig`). Shared by
    /// [`Transformer::eval_symbol`] and the transparent-child ambiguity lifting in
    /// [`Transformer::expand_packed`].
    deriv_memo: HashMap<usize, Vec<NodeValue>>,
    /// Explicit mode (#59): memoized "is this node's whole subtree unambiguous?"
    /// (every reachable node has ≤ 1 family, no forest cycle). A `true` node has
    /// exactly one derivation, so the explicit walk would produce the *same* single
    /// value resolve mode does — letting a distributed *transparent* such node be
    /// **spliced** into the parent in one streaming pass (the `Stream*` frames)
    /// instead of re-materializing a growing `Inline` per spine level (the O(n²)
    /// the issue tracked). Genuine ambiguity (any node with > 1 family) stays
    /// `false` and keeps the cartesian `Derivs` distribution that the `_ambig`
    /// oracles pin. Computed once per node by [`Transformer::single_deriv`].
    single_deriv: HashMap<usize, bool>,
    /// Memoized node priorities + the in-progress set for cycle-safe summing.
    prio: HashMap<usize, i32>,
    prio_visiting: HashSet<usize>,
    /// Resolve mode: nodes whose value has been fully built at least once. A value
    /// is memoized (and thereafter cloned) only on its *second* visit — so a node
    /// reached just once (the common case, including every node of a left-recursive
    /// `expr: expr "+" term` chain) is moved into its single parent with no clone,
    /// keeping the walk linear instead of re-cloning each growing subtree. The
    /// Earley SPPF over-shares nodes even for unambiguous grammars, so a static
    /// reference count over-counts; this tracks *actual* reuse (issue #54).
    seen: HashSet<usize>,
}

impl<'a> Transformer<'a> {
    fn new(
        grammar: &'a CompiledGrammar,
        forest: &'a Forest,
        resolve: bool,
        term_priority: bool,
    ) -> Self {
        // `term_priority` is set exactly when the dynamic lexer built the forest.
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
            builder: TreeOutputBuilder::new(&grammar.rules),
            resolve,
            term_priority,
            memo: HashMap::new(),
            deriv_memo: HashMap::new(),
            single_deriv: HashMap::new(),
            prio: HashMap::new(),
            prio_visiting: HashSet::new(),
            seen: HashSet::new(),
        }
    }

    /// ForestSumVisitor: a node's priority is the max over its derivations.
    ///
    /// Iterative two-phase DFS (issue #33 — the priority sum recurses to forest
    /// depth just like the value walk did): `Enter` pushes a node's family
    /// children, `Exit` combines their now-memoized priorities. Semantics are
    /// identical to the natural recursion: results memoize in `prio`, and an edge
    /// back into an in-progress node (`prio_visiting`) contributes 0.
    fn node_priority(&mut self, id: usize) -> i32 {
        if let Some(&p) = self.prio.get(&id) {
            return p;
        }
        if self.prio_visiting.contains(&id) {
            return 0; // cycle: contribute nothing
        }
        enum Step {
            Enter(usize),
            Exit(usize),
        }
        let mut stack = vec![Step::Enter(id)];
        while let Some(step) = stack.pop() {
            match step {
                Step::Enter(n) => {
                    if self.prio.contains_key(&n) || !self.prio_visiting.insert(n) {
                        continue; // memoized, or in-progress (a cycle edge)
                    }
                    stack.push(Step::Exit(n));
                    // Children in reverse push order so they evaluate in family
                    // order, left before right — the recursive call order, which
                    // is load-bearing in cyclic forests (a child's value depends
                    // on which ancestors are in-progress when it is reached).
                    for p in self.forest.nodes[n].families.iter().rev() {
                        for r in [p.right, p.left] {
                            if let ForestRef::Node(c) = r {
                                stack.push(Step::Enter(c));
                            }
                        }
                    }
                }
                Step::Exit(n) => {
                    let node = &self.forest.nodes[n];
                    let parent_inter = node.is_intermediate;
                    let mut best = if node.families.is_empty() {
                        0
                    } else {
                        i32::MIN
                    };
                    for k in 0..self.forest.nodes[n].families.len() {
                        let p = self.forest.nodes[n].families[k];
                        let v = self.packed_priority_value(&p, parent_inter);
                        if v > best {
                            best = v;
                        }
                    }
                    self.prio_visiting.remove(&n);
                    self.prio.insert(n, best);
                }
            }
        }
        self.prio.get(&id).copied().unwrap_or(0)
    }

    /// A derivation's priority: the rule's own priority (only counted at a real
    /// symbol node, not at intermediates) plus its children's priorities. Token
    /// leaves count 0 — the basic lexer already "used up" terminal priorities.
    fn packed_priority(&mut self, packed: &Packed, parent_inter: bool) -> i32 {
        // Make sure both node children are computed (left before right, the
        // recursive evaluation order), then combine by lookup.
        for r in [packed.left, packed.right] {
            if let ForestRef::Node(c) = r {
                self.node_priority(c);
            }
        }
        self.packed_priority_value(packed, parent_inter)
    }

    /// Lookup-only half of [`packed_priority`](Self::packed_priority): combines
    /// child priorities already computed (or in-progress → 0) by the DFS.
    fn packed_priority_value(&self, packed: &Packed, parent_inter: bool) -> i32 {
        let rule_prio = self.grammar.rules[packed.rule].options.priority;
        let base = if !parent_inter && rule_prio != 0 {
            rule_prio
        } else {
            0
        };
        let child = |r: ForestRef| match r {
            ForestRef::Node(id) => {
                if self.prio_visiting.contains(&id) {
                    0 // in-progress: a cycle edge contributes nothing
                } else {
                    self.prio.get(&id).copied().unwrap_or(0)
                }
            }
            // A scanned token contributes its terminal's priority — but only under
            // the dynamic lexer (`term_priority` is empty for the basic lexer).
            ForestRef::Tok(t) => self
                .term_priority
                .get(&self.forest.tokens[t].type_id)
                .copied()
                .unwrap_or(0),
            ForestRef::None => 0,
        };
        base + child(packed.left) + child(packed.right)
    }

    /// Family indices of `node_id` in Lark's `sort_key` order: non-empty
    /// derivations first, then higher priority, then lower rule order. A stable
    /// sort keeps insertion order among ties (which is how Lark breaks otherwise
    /// equal derivations — its `StableSymbolNode` stores packed children in an
    /// `OrderedSet`, so insertion order is the final tie-break there too).
    ///
    /// This is pure `(is_empty, -priority, rule.order)` + insertion order for *both*
    /// lexers. The dynamic-lexer split-point tie-break that #32/#90 added here is
    /// gone: it compensated for lark-rs building a grouped repetition through a
    /// nested `(A|B)` group node whose LIFO completion reversed Python's
    /// earliest-split-first segmentation order. With the EBNF expansion now inlining
    /// the group arms straight into the recurse rule (#91 — matching Python's
    /// `EBNF_to_BNF`), the last symbol of the recursion is a *terminal* built during
    /// the scan, so the segmentations already arrive in Python's order and the
    /// `rule.order` key alone disambiguates `dynamic_complete` ties (e.g. `WORD+`
    /// over `"bc"` resolves to one `WORD "bc"`, the `parse:49/72` cases).
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

    /// Is the symbol node `id` produced by a transparent (`_rule` / `__anon_*`)
    /// rule? All families of a symbol node share its origin non-terminal, and
    /// transparency is a property of the origin, so the first family decides.
    /// Transparent symbols are exactly Lark's `_should_expand` positions — the ones
    /// whose ambiguity must be lifted into the parent (`AmbiguousExpander`).
    fn is_transparent_node(&self, id: usize) -> bool {
        self.forest.nodes[id]
            .families
            .first()
            .map(|p| self.grammar.rules[p.rule].transparent)
            .unwrap_or(false)
    }

    /// Explicit mode (#59): does `id`'s whole subtree have exactly one derivation
    /// — every reachable node ≤ 1 family, and no forest cycle through it? Such a
    /// node's explicit value is identical to the value resolve mode would build (no
    /// ambiguity to fan out), so a distributed *transparent* one can be spliced in a
    /// single streaming pass instead of re-materializing a growing `Inline` per
    /// spine level. Iterative two-phase DFS (`Enter`/`Exit`), memoized in
    /// `single_deriv` and bounded to O(1) native stack per #33: a node reached while
    /// still in-progress is a cycle → not single-derivation (conservatively `false`,
    /// so the cartesian `Derivs` path — which already handles cycles by discarding —
    /// keeps owning it). A node is single-derivation iff it has exactly one family
    /// and every `Node` child of that family is single-derivation.
    fn single_deriv(&mut self, id: usize) -> bool {
        if let Some(&b) = self.single_deriv.get(&id) {
            return b;
        }
        enum Step {
            Enter(usize),
            Exit(usize),
        }
        // In-progress set: a re-entry is a cycle (→ false). Local to this query
        // chain; every node we settle lands in `single_deriv`, so a later query
        // short-circuits at the memo.
        let mut on_stack: HashSet<usize> = HashSet::new();
        let mut stack = vec![Step::Enter(id)];
        while let Some(step) = stack.pop() {
            match step {
                Step::Enter(n) => {
                    if self.single_deriv.contains_key(&n) {
                        continue;
                    }
                    if !on_stack.insert(n) {
                        // Cycle edge: this node participates in a forest cycle.
                        self.single_deriv.insert(n, false);
                        continue;
                    }
                    let node = &self.forest.nodes[n];
                    if node.families.len() != 1 {
                        // 0 families (a discarded/empty node) or > 1 (ambiguous):
                        // not a single clean derivation.
                        on_stack.remove(&n);
                        self.single_deriv.insert(n, node.families.len() == 1);
                        continue;
                    }
                    stack.push(Step::Exit(n));
                    let p = node.families[0];
                    for r in [p.left, p.right] {
                        if let ForestRef::Node(c) = r {
                            stack.push(Step::Enter(c));
                        }
                    }
                }
                Step::Exit(n) => {
                    on_stack.remove(&n);
                    // Already decided as a cycle while on the stack? Keep it.
                    if self.single_deriv.contains_key(&n) {
                        continue;
                    }
                    let p = self.forest.nodes[n].families[0];
                    let child_ok = |r: ForestRef, this: &Self| match r {
                        ForestRef::Node(c) => this.single_deriv.get(&c).copied() == Some(true),
                        _ => true,
                    };
                    let ok = child_ok(p.left, self) && child_ok(p.right, self);
                    self.single_deriv.insert(n, ok);
                }
            }
        }
        self.single_deriv.get(&id).copied().unwrap_or(false)
    }

    // ─── The de-recursed walk (issue #33) ──────────────────────────────────────
    //
    // The walk's natural shape is a set of mutually recursive functions, but its
    // recursion depth is the SPPF chain length — O(input length) for any
    // list-like rule (`x*`, `x+`, `expr: expr "+" term`) — which used to require
    // running the whole walk on a dedicated thread with a 256 MB stack. The
    // recursion is reified instead: each former function is a *work* [`Frame`]
    // variant, each point after a recursive call a *continuation* variant, the
    // locals live in the frame, and the value a call would have returned travels
    // through [`Walk::ret`]. Frames are heap-allocated, so native-stack use is
    // O(1) regardless of forest depth (`std::thread` does not exist on WASM, #47,
    // so this is also what makes the engine portable there).
    //
    // The de-recursion is mechanical and preserves the original semantics
    // exactly, including the parts that are easy to get wrong:
    //  * the `visiting` cycle set (formerly a `&mut HashSet` parameter): inserted
    //    on entry, removed on *every* exit path of the former function;
    //  * resolve-mode rollback: a failed family truncates the shared child buffer
    //    back to the mark taken before its attempt;
    //  * the memoization points (`memo` / `deriv_memo` / resolve's
    //    second-visit-only `seen` rule) fire at the same places.

    /// Walk the forest from `root` to its final value — the de-recursed
    /// `eval_symbol(root)` (see [`Frame`] for the correspondence).
    fn transform(&mut self, root: usize) -> Option<NodeValue> {
        let mut walk = Walk {
            frames: vec![Frame::Eval { node: root }],
            bufs: Vec::new(),
            visiting: HashSet::new(),
            ret: None,
        };
        while let Some(frame) = walk.frames.pop() {
            self.step(frame, &mut walk);
        }
        match walk.ret {
            Some(Ret::Value(v)) => v,
            _ => unreachable!("the root Eval frame returns Ret::Value"),
        }
    }

    /// Execute one frame. A *work* frame ignores `w.ret`; a *continuation* frame
    /// consumes the return value of the child item it was pushed above.
    fn step(&mut self, frame: Frame, w: &mut Walk) {
        match frame {
            // ── eval_symbol: evaluate a real (non-intermediate) symbol node to a
            //    single value — the best derivation under `resolve`, or an
            //    `_ambig` over all of them under explicit. `Ret::Value(None)` if
            //    every derivation is discarded (e.g. an ambiguity cycle).
            Frame::Eval { node } => {
                if let Some(v) = self.memo.get(&node) {
                    w.ret = Some(Ret::Value(Some(v.clone())));
                } else if self.resolve {
                    // Resolve mode keeps a single derivation, so its value is
                    // assembled by streaming children straight into one buffer —
                    // a left-recursive transparent helper (`x*`/`x+`/`_rule`)
                    // then costs O(total children) instead of the O(children²)
                    // the materialize-then-splice path pays re-copying each
                    // growing prefix (issue #54). The streamed frames mirror
                    // `TreeOutputBuilder::assemble`'s filtering + shaping (via
                    // `keep_token` / `shape`) so resolve trees stay
                    // byte-for-byte identical to the explicit path and to LALR.
                    w.bufs.push(Vec::new());
                    w.frames.push(Frame::EvalShape { node });
                    w.frames.push(Frame::AppendRule { node });
                } else {
                    w.frames.push(Frame::EvalAmbig { node });
                    w.frames.push(Frame::Derivs { node });
                }
            }
            // Resolve: shape the children streamed into this node's buffer.
            Frame::EvalShape { node } => {
                let children = w.bufs.pop().expect("Eval pushed a buffer");
                match w.take_ret() {
                    Ret::Rule(None) => w.ret = Some(Ret::Value(None)),
                    Ret::Rule(Some(rule)) => {
                        let v = self.builder.shape(rule, children);
                        // Memoize only on the second visit: a single-use node is
                        // returned by move (no clone). `insert` returns false
                        // when the node was already present, i.e. this is its
                        // second full build — cache it so any further reuse is a
                        // cheap clone, bounding rebuilds to at most two per node.
                        if !self.seen.insert(node) {
                            self.memo.insert(node, v.clone());
                        }
                        w.ret = Some(Ret::Value(Some(v)));
                    }
                    _ => unreachable!("AppendRule returns Ret::Rule"),
                }
            }

            // ── append_rule_children (resolve): pick `node`'s best non-discarded
            //    family and append its rule-position children — post-filter, with
            //    transparent children spliced in place — to the current buffer.
            //    Returns the chosen rule (so the parent can `shape` it), or `None`
            //    if every family is discarded (a forest cycle). Works for both
            //    symbol nodes (a complete rule) and intermediate nodes (a rule
            //    prefix); both just contribute children in left-to-right order.
            Frame::AppendRule { node } => {
                if !w.visiting.insert(node) {
                    w.ret = Some(Ret::Rule(None)); // cycle — discard this derivation
                } else {
                    let fams = self.sorted_families(node);
                    self.rule_try_family(w, node, fams, 0);
                }
            }
            Frame::RuleNext {
                node,
                fams,
                idx,
                mark,
                rule,
            } => {
                let Ret::Packed(ok) = w.take_ret() else {
                    unreachable!("AppendPacked returns Ret::Packed")
                };
                if ok {
                    w.visiting.remove(&node);
                    w.ret = Some(Ret::Rule(Some(rule)));
                } else {
                    // Discarded part-way: roll back, try the next family.
                    w.buf().truncate(mark);
                    self.rule_try_family(w, node, fams, idx + 1);
                }
            }

            // ── append_packed (resolve): append one derivation's children — its
            //    left prefix (an intermediate of the same rule) then its right
            //    symbol at `packed.right_pos`. `Ret::Packed(false)` if any
            //    sub-node is discarded, so the parent can try another family.
            Frame::AppendPacked { packed } => match packed.left {
                ForestRef::None => self.packed_right(w, packed),
                ForestRef::Node(lid) => {
                    w.frames.push(Frame::PackedRight { packed });
                    w.frames.push(Frame::AppendRule { node: lid });
                }
                // `left` is always an intermediate node or nothing in the
                // binarized forest; handle a token defensively for symmetry with
                // the explicit path.
                ForestRef::Tok(t) => {
                    let tok = self.forest.tokens[t].clone();
                    w.buf().push(Child::Token(tok));
                    self.packed_right(w, packed);
                }
            },
            Frame::PackedRight { packed } => match w.take_ret() {
                Ret::Rule(None) => w.ret = Some(Ret::Packed(false)),
                Ret::Rule(Some(_)) => self.packed_right(w, packed),
                _ => unreachable!("AppendRule returns Ret::Rule"),
            },
            Frame::PackedAfterSplice => {
                let Ret::Rule(rule) = w.take_ret() else {
                    unreachable!("Splice returns Ret::Rule")
                };
                w.ret = Some(Ret::Packed(rule.is_some()));
            }
            // A real (non-transparent) right symbol contributes one shaped value;
            // mirror `assemble`'s per-value handling (a `Token` is subject to the
            // position's filter, a `Tree` is always kept).
            Frame::PackedAfterEval { rule, right_pos } => {
                let Ret::Value(v) = w.take_ret() else {
                    unreachable!("Eval returns Ret::Value")
                };
                match v {
                    None => w.ret = Some(Ret::Packed(false)),
                    Some(NodeValue::Token(tk)) => {
                        if self.builder.keep_token(rule, right_pos) {
                            w.buf().push(Child::Token(tk));
                        }
                        w.ret = Some(Ret::Packed(true));
                    }
                    Some(NodeValue::Tree(tr)) => {
                        w.buf().push(Child::Tree(tr));
                        w.ret = Some(Ret::Packed(true));
                    }
                    Some(NodeValue::Inline(cs)) => {
                        w.buf().extend(cs);
                        w.ret = Some(Ret::Packed(true));
                    }
                }
            }

            // ── splice_node (resolve): append a transparent symbol node's spliced
            //    children (plus its rule's `maybe_placeholders`) to the current
            //    buffer, so a chain of transparent helpers flattens in one linear
            //    pass.
            Frame::Splice { node } => {
                w.frames.push(Frame::SpliceTail);
                w.frames.push(Frame::AppendRule { node });
            }
            Frame::SpliceTail => {
                let Ret::Rule(rule) = w.take_ret() else {
                    unreachable!("AppendRule returns Ret::Rule")
                };
                match rule {
                    None => w.ret = Some(Ret::Rule(None)),
                    Some(rule) => {
                        for _ in 0..self.grammar.rules[rule].options.placeholder_count {
                            w.buf().push(Child::None);
                        }
                        // Trailing placeholders of a distributed absent `[...]`
                        // (the streaming mirror of `TreeOutputBuilder::shape`'s
                        // trailing append).
                        let len = self.grammar.rules[rule].expansion.len();
                        self.push_nones_before(rule, len, w.buf());
                        w.ret = Some(Ret::Rule(Some(rule)));
                    }
                }
            }

            // ── eval_symbol, explicit tail: collapse the derivation list to one
            //    value, or an `_ambig` over all of them.
            Frame::EvalAmbig { node } => {
                let Ret::Derivs(mut derivs) = w.take_ret() else {
                    unreachable!("Derivs returns Ret::Derivs")
                };
                let result = match derivs.len() {
                    0 => None,
                    1 => Some(derivs.pop().unwrap()),
                    _ => {
                        let children: Vec<Child> =
                            derivs.into_iter().map(node_value_to_child).collect();
                        Some(NodeValue::Tree(Tree::new("_ambig", children)))
                    }
                };
                if let Some(v) = &result {
                    self.memo.insert(node, v.clone());
                }
                w.ret = Some(Ret::Value(result));
            }

            // ── stream a single-derivation transparent child (explicit, #59):
            //    splice `node` into a fresh buffer with the *resolve* transparent
            //    splice (`Frame::Splice` → `SpliceTail`) — one linear pass down the
            //    spine, no growing per-level `Inline` — then hand the buffer back
            //    wrapped as the lone `Inline` derivation alternative `ExpandCombine`
            //    expects. Reusing `Splice` (not bare `AppendRule`) is what makes the
            //    streamed value byte-identical to the `Derivs` + `assemble` value it
            //    replaces: `SpliceTail` appends the transparent rule's rule-level
            //    `placeholder_count` and trailing `nones_before` `None` slots that
            //    `AppendRule` alone omits. Sound because `single_deriv(node)`
            //    guaranteed exactly one derivation (no ambiguity to fan out).
            Frame::StreamDistribute { node } => {
                w.bufs.push(Vec::new());
                w.frames.push(Frame::StreamDistributeDone);
                w.frames.push(Frame::Splice { node });
            }
            Frame::StreamDistributeDone => {
                let children = w.bufs.pop().expect("StreamDistribute pushed a buffer");
                let derivs = match w.take_ret() {
                    // A discarded family would mean a cycle, which `single_deriv`
                    // already excludes — but stay defensive: no derivation, no
                    // alternative.
                    Ret::Rule(None) => Vec::new(),
                    Ret::Rule(Some(_)) => vec![NodeValue::Inline(children)],
                    _ => unreachable!("Splice returns Ret::Rule"),
                };
                #[cfg(feature = "perf-counters")]
                crate::perf::add_explicit_node_children(
                    derivs.iter().map(node_value_size).sum::<u64>(),
                );
                w.ret = Some(Ret::Derivs(derivs));
            }

            // ── symbol_derivations (explicit): the deduped list of derivation
            //    values for a symbol node — every distinct derivation. Memoized,
            //    since a shared SPPF node is reachable from many parents.
            Frame::Derivs { node } => {
                debug_assert!(
                    !self.resolve,
                    "resolve mode streams; it never materializes derivation lists"
                );
                if let Some(d) = self.deriv_memo.get(&node) {
                    w.ret = Some(Ret::Derivs(d.clone()));
                } else if !w.visiting.insert(node) {
                    // Cycle in the forest — discard this family.
                    w.ret = Some(Ret::Derivs(Vec::new()));
                } else {
                    let fams = self.sorted_families(node);
                    self.derivs_try_family(w, node, fams, 0, Vec::new(), HashSet::new());
                }
            }
            Frame::DerivsNext {
                node,
                fams,
                idx,
                rule,
                mut derivs,
                mut keys,
            } => {
                let Ret::Lists(lists) = w.take_ret() else {
                    unreachable!("ExpandPacked returns Ret::Lists")
                };
                let mut push_deduped = |v: NodeValue| {
                    if keys.insert(node_value_key(&v)) {
                        derivs.push(v);
                    }
                };
                for list in lists {
                    // Python's `_collapse_ambig`: a derivation that assembles
                    // to an `_ambig` (an expand1 rule whose single kept child
                    // is ambiguous) contributes its alternatives flat, not as
                    // a nested `_ambig` (#63). (`Tree` has a manual `Drop`, so
                    // the children are taken, not moved out.)
                    match self.builder.assemble(rule, list) {
                        NodeValue::Tree(mut t) if t.data == "_ambig" => {
                            for c in std::mem::take(&mut t.children) {
                                push_deduped(child_to_node_value(c));
                            }
                        }
                        v => push_deduped(v),
                    }
                }
                self.derivs_try_family(w, node, fams, idx + 1, derivs, keys);
            }

            // ── expand_packed (explicit): expand one derivation into its rule's
            //    child-lists. `left` is always an intermediate (the accumulated
            //    prefix) or nothing; `right` is the symbol just consumed (a
            //    symbol node or token leaf) or nothing (ε).
            Frame::ExpandPacked { packed } => match packed.left {
                ForestRef::None => self.expand_right(w, packed, vec![Vec::new()]),
                ForestRef::Node(lid) => {
                    w.frames.push(Frame::ExpandRight { packed });
                    w.frames.push(Frame::ExpandInter { node: lid });
                }
                ForestRef::Tok(t) => {
                    let tok = self.forest.tokens[t].clone();
                    self.expand_right(w, packed, vec![vec![NodeValue::Token(tok)]]);
                }
            },
            Frame::ExpandRight { packed } => {
                let Ret::Lists(lefts) = w.take_ret() else {
                    unreachable!("ExpandInter returns Ret::Lists")
                };
                if lefts.is_empty() {
                    w.ret = Some(Ret::Lists(Vec::new()));
                } else {
                    self.expand_right(w, packed, lefts);
                }
            }
            Frame::ExpandCombine {
                lefts,
                distribute_right,
            } => {
                let right_alts: Vec<NodeValue> = if distribute_right {
                    let Ret::Derivs(alts) = w.take_ret() else {
                        unreachable!("Derivs returns Ret::Derivs")
                    };
                    alts
                } else {
                    match w.take_ret() {
                        Ret::Value(Some(v)) => vec![v],
                        Ret::Value(None) => Vec::new(),
                        _ => unreachable!("Eval returns Ret::Value"),
                    }
                };
                if right_alts.is_empty() {
                    // Right discarded → the whole derivation is gone.
                    w.ret = Some(Ret::Lists(Vec::new()));
                } else {
                    self.expand_combine(w, &lefts, &right_alts);
                }
            }

            // ── expand_intermediate (explicit): expand an intermediate node into
            //    the alternative child-lists it contributes to its parent rule.
            Frame::ExpandInter { node } => {
                if !w.visiting.insert(node) {
                    w.ret = Some(Ret::Lists(Vec::new())); // cycle — discard
                } else {
                    let fams = self.sorted_families(node);
                    self.inter_try_family(w, node, fams, 0, Vec::new());
                }
            }
            Frame::InterNext {
                node,
                fams,
                idx,
                mut out,
            } => {
                let Ret::Lists(lists) = w.take_ret() else {
                    unreachable!("ExpandPacked returns Ret::Lists")
                };
                out.extend(lists);
                self.inter_try_family(w, node, fams, idx + 1, out);
            }
        }
    }

    /// Resolve: try family `fams[idx]` of `node`, or finish with `Ret::Rule(None)`
    /// once every family has been discarded (the loop of the former
    /// `append_rule_children`).
    fn rule_try_family(&mut self, w: &mut Walk, node: usize, fams: Vec<usize>, idx: usize) {
        match fams.get(idx) {
            None => {
                w.visiting.remove(&node);
                w.ret = Some(Ret::Rule(None));
            }
            Some(&fi) => {
                let packed = self.forest.nodes[node].families[fi];
                let mark = w.buf().len();
                w.frames.push(Frame::RuleNext {
                    node,
                    fams,
                    idx,
                    mark,
                    rule: packed.rule,
                });
                w.frames.push(Frame::AppendPacked { packed });
            }
        }
    }

    /// Resolve: handle `packed.right` once the left prefix has streamed into the
    /// buffer — the tail half of the former `append_packed`.
    fn packed_right(&mut self, w: &mut Walk, packed: Packed) {
        match packed.right {
            // ε production: no right child.
            ForestRef::None => w.ret = Some(Ret::Packed(true)),
            ForestRef::Tok(t) => {
                self.push_nones_before(packed.rule, packed.right_pos, w.buf());
                if self.builder.keep_token(packed.rule, packed.right_pos) {
                    let tok = self.forest.tokens[t].clone();
                    w.buf().push(Child::Token(tok));
                }
                w.ret = Some(Ret::Packed(true));
            }
            ForestRef::Node(rid) => {
                self.push_nones_before(packed.rule, packed.right_pos, w.buf());
                if self.is_transparent_node(rid) {
                    // Splice the transparent child's children straight into the
                    // buffer.
                    w.frames.push(Frame::PackedAfterSplice);
                    w.frames.push(Frame::Splice { node: rid });
                } else {
                    w.frames.push(Frame::PackedAfterEval {
                        rule: packed.rule,
                        right_pos: packed.right_pos,
                    });
                    w.frames.push(Frame::Eval { node: rid });
                }
            }
        }
    }

    /// Explicit: expand family `fams[idx]` of `node`, or finish (memoize + hand
    /// back) the derivation list once every family has been processed (the loop
    /// of the former `symbol_derivations`).
    fn derivs_try_family(
        &mut self,
        w: &mut Walk,
        node: usize,
        fams: Vec<usize>,
        idx: usize,
        derivs: Vec<NodeValue>,
        keys: HashSet<String>,
    ) {
        match fams.get(idx) {
            None => {
                w.visiting.remove(&node);
                // The *real* #56 Arm-2 cost: explicit mode materializes one owned
                // value per symbol node, and a transparent left-recursive helper's
                // value is the whole accumulated child list — so the SPPF chain of
                // n helper nodes builds Inlines of size 1, 2, …, n = O(n²) elements
                // total (and `deriv_memo` then clones them). Counting the
                // materialized derivation sizes here (behind `perf-counters`)
                // exhibits that quadratic deterministically — the signal the
                // streaming fix (the explicit analog of #55) must flatten. It is
                // *not* the cartesian clone loop the issue guessed (that is
                // linear; see `expand_combine`). Gated at the call site (not just
                // inside the no-op) so the `sum` — itself O(materialized
                // children), i.e. the quadratic we are measuring — is never
                // computed in a normal build.
                #[cfg(feature = "perf-counters")]
                crate::perf::add_explicit_node_children(
                    derivs.iter().map(node_value_size).sum::<u64>(),
                );
                self.deriv_memo.insert(node, derivs.clone());
                w.ret = Some(Ret::Derivs(derivs));
            }
            Some(&fi) => {
                let packed = self.forest.nodes[node].families[fi];
                w.frames.push(Frame::DerivsNext {
                    node,
                    fams,
                    idx,
                    rule: packed.rule,
                    derivs,
                    keys,
                });
                w.frames.push(Frame::ExpandPacked { packed });
            }
        }
    }

    /// Explicit: handle `packed.right` once the left prefixes are known — the
    /// tail half of the former `expand_packed`.
    ///
    /// The alternative values the right symbol contributes are normally one — but
    /// an ambiguous child at one of Lark's `AmbiguousExpander` *to_expand*
    /// positions contributes one alternative per derivation, distributed over
    /// the parent's child-lists: rather than nest an `_ambig` under the child's
    /// position, the ambiguity is shifted up so the parent itself becomes the
    /// `_ambig` (`parent(_ambig(a, b))` → `_ambig(parent(a), parent(b))`).
    /// to_expand covers a *transparent* (`_rule` / `__anon_*`) child always (its
    /// alternatives are `Inline` splice values with no node to nest under) and —
    /// since `keep_all_tokens` puts every position in to_expand — ANY child of a
    /// `!` rule (#63). Both consume the node's derivation list (`Derivs`)
    /// directly: it is exactly the list `Eval` would wrap in an `_ambig`, so
    /// distributing it skips the wrap/unwrap and reuses `deriv_memo`.
    fn expand_right(&mut self, w: &mut Walk, packed: Packed, lefts: Vec<Vec<NodeValue>>) {
        match packed.right {
            // ε right: the child-lists are exactly the left prefixes.
            ForestRef::None => w.ret = Some(Ret::Lists(lefts)),
            ForestRef::Node(rid) => {
                let distribute_right = !self.resolve
                    && (self.is_transparent_node(rid)
                        || self.grammar.rules[packed.rule].options.keep_all_tokens);
                w.frames.push(Frame::ExpandCombine {
                    lefts,
                    distribute_right,
                });
                if distribute_right {
                    // #59: a *transparent* distributed child whose whole subtree is
                    // unambiguous has exactly one derivation, so its explicit value
                    // equals the value resolve mode would build — splice it into a
                    // fresh buffer in one streaming pass (yielding the single
                    // `Inline` alternative) instead of materializing a growing
                    // per-spine-level `Inline` through `Derivs` (the O(n²) on a
                    // transparent left-recursive helper that #59 fixes). Ambiguous
                    // children (> 1 derivation anywhere in the subtree) and the
                    // `keep_all_tokens` distribution of a non-transparent child keep
                    // the cartesian `Derivs` path, which the `_ambig` oracles pin.
                    if self.is_transparent_node(rid) && self.single_deriv(rid) {
                        w.frames.push(Frame::StreamDistribute { node: rid });
                    } else {
                        w.frames.push(Frame::Derivs { node: rid });
                    }
                } else {
                    w.frames.push(Frame::Eval { node: rid });
                }
            }
            ForestRef::Tok(t) => {
                let tok = self.forest.tokens[t].clone();
                self.expand_combine(w, &lefts, &[NodeValue::Token(tok)]);
            }
        }
    }

    /// Explicit: the cartesian product of left prefixes × right alternatives.
    ///
    /// The named #56 Arm-2 suspect: clone each growing prefix to form the
    /// cartesian product of left prefixes × right values. Counting the
    /// `NodeValue`s copied here (behind `perf-counters`) is what *disproves* that
    /// guess — it stays **linear** even on a transparent left-recursive helper
    /// (`x*` / `x+` / `_rule`), because every rule's binarized RHS prefix is
    /// bounded (≤ its arity), so this clone is O(1) per node. The real explicit
    /// super-linearity is the per-node derivation-value rebuild counted in
    /// `derivs_try_family` — the still-missing explicit analog of #55's
    /// resolve-mode streaming. Kept verbatim (no fast path) so the disproof
    /// measures the actual loop; the true fix is tracked as a follow-up (#59).
    fn expand_combine(&self, w: &mut Walk, lefts: &[Vec<NodeValue>], right_alts: &[NodeValue]) {
        let mut out: Vec<Vec<NodeValue>> = Vec::with_capacity(lefts.len() * right_alts.len());
        for list in lefts {
            for rv in right_alts {
                crate::perf::add_explicit_prefix_copies(list.len() as u64);
                let mut l = list.clone();
                l.push(rv.clone());
                out.push(l);
            }
        }
        w.ret = Some(Ret::Lists(out));
    }

    /// Explicit: expand family `fams[idx]` of intermediate `node`, or finish (the
    /// loop of the former `expand_intermediate`).
    fn inter_try_family(
        &mut self,
        w: &mut Walk,
        node: usize,
        fams: Vec<usize>,
        idx: usize,
        out: Vec<Vec<NodeValue>>,
    ) {
        match fams.get(idx) {
            None => {
                w.visiting.remove(&node);
                w.ret = Some(Ret::Lists(out));
            }
            Some(&fi) => {
                let packed = self.forest.nodes[node].families[fi];
                w.frames.push(Frame::InterNext {
                    node,
                    fams,
                    idx,
                    out,
                });
                w.frames.push(Frame::ExpandPacked { packed });
            }
        }
    }

    /// Push the `None` placeholders a distributed absent `[...]` left before
    /// expansion position `gap` of `rule` (the streaming mirror of
    /// `TreeOutputBuilder::assemble`'s per-position insert).
    fn push_nones_before(&self, rule: usize, gap: usize, out: &mut Vec<Child>) {
        for _ in 0..self.builder.nones_at(rule, gap) {
            out.push(Child::None);
        }
    }
}

/// One step of the de-recursed forest walk (issue #33). *Work* variants are the
/// entries of the former recursive functions; *continuation* variants resume them
/// after the child item above finishes (its result in [`Walk::ret`]).
///
/// Correspondence to the former recursion — resolve mode (the streaming assembly
/// of #54/#55):
///
/// | function               | work          | continuation(s)                  |
/// |------------------------|---------------|----------------------------------|
/// | `eval_symbol`          | `Eval`        | `EvalShape`                      |
/// | `append_rule_children` | `AppendRule`  | `RuleNext`                       |
/// | `append_packed`        | `AppendPacked`| `PackedRight`, `PackedAfterSplice`, `PackedAfterEval` |
/// | `splice_node`          | `Splice`      | `SpliceTail`                     |
///
/// explicit mode:
///
/// | function               | work           | continuation(s)               |
/// |------------------------|----------------|-------------------------------|
/// | `eval_symbol`          | `Eval`         | `EvalAmbig`                   |
/// | `symbol_derivations`   | `Derivs`       | `DerivsNext`                  |
/// | `expand_packed`        | `ExpandPacked` | `ExpandRight`, `ExpandCombine`|
/// | `expand_intermediate`  | `ExpandInter`  | `InterNext`                   |
/// | stream single-deriv child (#59) | `StreamDistribute` | `StreamDistributeDone` (reuses the resolve `Splice`/`SpliceTail` frames) |
enum Frame {
    Eval {
        node: usize,
    },
    EvalShape {
        node: usize,
    },
    AppendRule {
        node: usize,
    },
    /// Resume after the attempt of family `fams[idx]` (whose rule is `rule`);
    /// `mark` is the buffer length to roll back to if it was discarded.
    RuleNext {
        node: usize,
        fams: Vec<usize>,
        idx: usize,
        mark: usize,
        rule: usize,
    },
    AppendPacked {
        packed: Packed,
    },
    /// Resume after the left prefix of `packed` streamed in.
    PackedRight {
        packed: Packed,
    },
    PackedAfterSplice,
    /// Resume after the right symbol's value; `rule`/`right_pos` locate its
    /// position for per-position token filtering.
    PackedAfterEval {
        rule: usize,
        right_pos: usize,
    },
    Splice {
        node: usize,
    },
    SpliceTail,
    EvalAmbig {
        node: usize,
    },
    /// #59: stream a single-derivation transparent distributed child into a fresh
    /// buffer (via the resolve `Splice`/`SpliceTail` frames), then wrap it as the
    /// lone `Inline` derivation alternative — the explicit reuse of resolve's splice.
    StreamDistribute {
        node: usize,
    },
    StreamDistributeDone,
    Derivs {
        node: usize,
    },
    /// Resume after family `fams[idx]` (rule `rule`) expanded; `derivs`/`keys`
    /// are the accumulated deduped values.
    DerivsNext {
        node: usize,
        fams: Vec<usize>,
        idx: usize,
        rule: usize,
        derivs: Vec<NodeValue>,
        keys: HashSet<String>,
    },
    ExpandPacked {
        packed: Packed,
    },
    /// Resume after the left intermediate's child-lists.
    ExpandRight {
        packed: Packed,
    },
    /// Resume after the right symbol's value(s); `distribute_right` records
    /// which child item was pushed (`Derivs` vs `Eval`), i.e. which `Ret`
    /// variant to consume: a child at one of Python's `AmbiguousExpander`
    /// to_expand positions — transparent, or any position of a
    /// `keep_all_tokens` rule (#63) — distributes its derivation list over
    /// the parent's child-lists instead of nesting an `_ambig`.
    ExpandCombine {
        lefts: Vec<Vec<NodeValue>>,
        distribute_right: bool,
    },
    ExpandInter {
        node: usize,
    },
    /// Resume after family `fams[idx]` expanded; `out` is the accumulated lists.
    InterNext {
        node: usize,
        fams: Vec<usize>,
        idx: usize,
        out: Vec<Vec<NodeValue>>,
    },
}

/// The value a finished walk item hands back — the return value of the
/// corresponding former recursive function.
enum Ret {
    /// `eval_symbol`: the node's single value, or `None` if every derivation was
    /// discarded.
    Value(Option<NodeValue>),
    /// `append_rule_children` / `splice_node`: the chosen rule, or `None` if
    /// every family was discarded.
    Rule(Option<usize>),
    /// `append_packed`: did this derivation contribute its children?
    Packed(bool),
    /// `symbol_derivations`: the node's deduped derivation values.
    Derivs(Vec<NodeValue>),
    /// `expand_packed` / `expand_intermediate`: alternative child-lists.
    Lists(Vec<Vec<NodeValue>>),
}

/// Mutable state of one [`Transformer::transform`] run: the frame stack, the
/// resolve-mode child-buffer stack (each `Eval` pushes a fresh buffer; splices
/// stream into the top one, so `RuleNext`'s rollback marks stay valid), the
/// in-progress cycle set (the former `visiting` parameter), and the return-value
/// slot connecting a finished item to its continuation.
struct Walk {
    frames: Vec<Frame>,
    bufs: Vec<Vec<Child>>,
    visiting: HashSet<usize>,
    ret: Option<Ret>,
}

impl Walk {
    /// Take the child item's return value (each continuation consumes exactly one).
    fn take_ret(&mut self) -> Ret {
        self.ret
            .take()
            .expect("a finished walk item set a return value")
    }

    /// The resolve-mode child buffer currently being streamed into.
    fn buf(&mut self) -> &mut Vec<Child> {
        self.bufs
            .last_mut()
            .expect("a resolve Eval frame pushed a buffer")
    }
}

/// The number of child slots a materialized derivation value occupies — the unit
/// of the [`explicit_node_children`](crate::perf::explicit_node_children) cost
/// signal (#56 Arm 2). A transparent left-recursive helper's value grows by one
/// per SPPF level, so summing this over the chain is the O(n²) the explicit walk
/// pays where resolve mode streams (#55).
#[cfg(feature = "perf-counters")]
fn node_value_size(v: &NodeValue) -> u64 {
    match v {
        NodeValue::Token(_) => 1,
        NodeValue::Tree(t) => t.children.len() as u64,
        NodeValue::Inline(cs) => cs.len() as u64,
    }
}

/// A stable structural key for de-duplicating equal `_ambig` derivations.
/// Iterative (explicit work stack) — a derivation value is as deep as the tree it
/// describes, and the walk that calls this must not recurse to input depth (#33).
fn node_value_key(v: &NodeValue) -> String {
    enum K<'a> {
        Child(&'a Child),
        Tree(&'a Tree),
        Lit(&'static str),
    }
    fn token_key(t: &Token, out: &mut String) {
        out.push_str("T:");
        out.push_str(&t.type_);
        out.push('=');
        out.push_str(&t.value);
    }
    let mut out = String::new();
    let mut stack: Vec<K> = Vec::new();
    match v {
        NodeValue::Token(t) => token_key(t, &mut out),
        NodeValue::Tree(t) => stack.push(K::Tree(t)),
        NodeValue::Inline(cs) => {
            out.push_str("I[");
            stack.push(K::Lit("]"));
            for c in cs.iter().rev() {
                stack.push(K::Lit(","));
                stack.push(K::Child(c));
            }
        }
    }
    while let Some(k) = stack.pop() {
        match k {
            K::Lit(s) => out.push_str(s),
            K::Child(c) => match c {
                Child::Tree(t) => stack.push(K::Tree(t)),
                Child::Token(t) => token_key(t, &mut out),
                Child::None => out.push_str("None"),
            },
            K::Tree(t) => {
                out.push('(');
                out.push_str(&t.data);
                stack.push(K::Lit(")"));
                for c in t.children.iter().rev() {
                    stack.push(K::Child(c));
                    stack.push(K::Lit(" "));
                }
            }
        }
    }
    out
}

/// The inverse of [`node_value_to_child`], for an `_ambig` alternative being
/// lifted back out and re-distributed into a parent derivation (#63).
fn child_to_node_value(c: Child) -> NodeValue {
    match c {
        Child::Tree(t) => NodeValue::Tree(t),
        Child::Token(t) => NodeValue::Token(t),
        // An `_ambig`'s children are full alternative derivations, never a
        // `maybe_placeholders` slot — the same invariant the LALR expand1
        // collapse relies on (the guarded `Child::None` arm in
        // `tree_builder::TreeOutputBuilder::shape`).
        Child::None => unreachable!("an `_ambig` alternative is never a placeholder"),
    }
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

    // ── #159 guard: the explicit-mode `_ambig` dedup (`node_value_key` keyed in
    //    `DerivsNext`) must ONLY ever collapse BYTE-IDENTICAL derivations, never
    //    structurally-distinct ones. lark-rs intentionally diverges from Python
    //    Lark here: Python's `ForestToParseTree` does not dedup, so its `_ambig`
    //    may repeat byte-identical children; we drop those (they carry zero
    //    information — see ADR-0017 "diverge & document" and `docs/STATUS.md`).
    //    Collapsing a *distinct* derivation would be a real bug; these tests are
    //    the tripwire. DO NOT relax them to make the dedup do more.

    /// The keying function is the dedup's decision procedure: equal keys collapse,
    /// distinct keys survive. Pin both directions directly on `node_value_key`.
    #[test]
    fn node_value_key_separates_distinct_collapses_identical() {
        let leaf = |data: &str| Child::Tree(Tree::new(data, vec![]));
        // Two byte-identical trees → identical keys (these are the ONLY thing the
        // dedup is allowed to collapse).
        let a1 = NodeValue::Tree(Tree::new("start", vec![leaf("x"), leaf("x")]));
        let a2 = NodeValue::Tree(Tree::new("start", vec![leaf("x"), leaf("x")]));
        assert_eq!(
            node_value_key(&a1),
            node_value_key(&a2),
            "byte-identical derivations must key equal (so they collapse)"
        );

        // Same node name, DIFFERENT child structure → distinct keys (must survive).
        let b = NodeValue::Tree(Tree::new("start", vec![leaf("y")]));
        assert_ne!(
            node_value_key(&a1),
            node_value_key(&b),
            "structurally-distinct derivations must key apart (never collapse)"
        );

        // Same shape, DIFFERENT kept token value → distinct keys (must survive).
        // `node_value_key` keys tokens by `type_` + `value` (not the interned id).
        let c = NodeValue::Tree(Tree::new("n", vec![Child::Token(Token::new("A", "a"))]));
        let d = NodeValue::Tree(Tree::new("n", vec![Child::Token(Token::new("A", "b"))]));
        assert_ne!(
            node_value_key(&c),
            node_value_key(&d),
            "derivations differing only in a kept token value must key apart"
        );
    }

    /// End-to-end tripwire: a grammar whose ambiguity yields two
    /// STRUCTURALLY-DISTINCT derivations (`start(x x)` vs `start(y(A A))`) must
    /// keep BOTH `_ambig` alternatives — the dedup must not over-merge them.
    #[test]
    fn explicit_keeps_structurally_distinct_ambig_alternatives() {
        let cg = compile("start: x x | y\nx: A\ny: A A\nA: \"a\"\n");
        let p = EarleyParser::new(cg.clone());
        let parsed = p
            .parse(
                &[tok(&cg, "A", "a"), tok(&cg, "A", "a")],
                Some("start"),
                false,
            )
            .expect("parses");
        let tree = parsed.as_tree().expect("root is a tree");
        assert_eq!(
            tree.data, "_ambig",
            "the two readings are genuinely ambiguous"
        );
        // Collect each alternative's shape (its sole child's `data`).
        let mut shapes: Vec<&str> = tree
            .children
            .iter()
            .filter_map(Child::as_tree)
            .flat_map(|alt: &Tree| alt.children.iter().filter_map(Child::as_tree))
            .map(|t| t.data.as_str())
            .collect();
        shapes.sort_unstable();
        shapes.dedup();
        assert_eq!(
            shapes,
            vec!["x", "y"],
            "both distinct derivations (x x and y(A A)) must survive the dedup, got {:?}",
            tree.pretty(0)
        );
    }

    /// End-to-end pin of the #159 *current* (intentional) behavior: when every
    /// derivation is BYTE-IDENTICAL (the distinguishing tokens are filtered out),
    /// the dedup collapses them to a single tree — no `_ambig`. Python Lark keeps
    /// the duplicates; we diverge by design (ADR-0017). This is the behavior the
    /// architect verdict says to KEEP; if it ever changes, that is a real decision,
    /// not an accident.
    #[test]
    fn explicit_collapses_byte_identical_ambig_alternatives() {
        // The issue's repro: `start: "x" start | start "x" | "x"` on "xxx". The
        // `"x"` tokens are filtered, so all derivations assemble byte-identically.
        let cg = compile("start: \"x\" start | start \"x\" | \"x\"\n");
        let p = EarleyParser::new(cg.clone());
        // The `"x"` literal lowers to an anonymous string terminal; resolve its
        // interned name by its pattern value rather than guessing the spelling.
        let term = cg
            .terminals
            .iter()
            .find(|t| matches!(&t.pattern, crate::grammar::terminal::Pattern::Str(s) if s.value == "x"))
            .expect("the \"x\" literal interned as a terminal")
            .name
            .clone();
        let input: Vec<Token> = (0..3).map(|_| tok(&cg, &term, "x")).collect();
        let parsed = p.parse(&input, Some("start"), false).expect("parses");
        let tree = parsed.as_tree().expect("root is a tree");
        assert_ne!(
            tree.data,
            "_ambig",
            "byte-identical derivations collapse to a single tree (no _ambig); got {}",
            tree.pretty(0)
        );
        assert_eq!(tree.data, "start");
        // No `_ambig` anywhere in the collapsed result.
        assert!(
            tree.iter_subtrees().all(|t| t.data != "_ambig"),
            "no nested _ambig should survive the collapse; got {}",
            tree.pretty(0)
        );
    }
}
