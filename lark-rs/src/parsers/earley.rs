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

    /// A reduction path through `item` is taken by Leo only when consuming the
    /// symbol at the dot *completes* the rule — i.e. the recognized symbol is the
    /// rule's final symbol (strict right recursion). Scott/Leo also permit a
    /// nullable tail after the recognized symbol, but the topmost item is then
    /// non-complete and the SPPF spine reconstruction must thread the nullable
    /// completions through it — a subtle case upstream Lark never finished. We
    /// deliberately decline it: such a tail is bounded extra work the regular
    /// completer handles correctly, so the linearization target (`a: X a | X`,
    /// including a nullable base like `a: X a | ε`) is covered while the
    /// nullable-tail interaction that breaks the forest is avoided. The `start_id`
    /// guard refuses to special-case a directly self-recursive start.
    fn is_quasi_complete(&self, item: &Item, start_id: SymbolId) -> bool {
        let expansion = &self.grammar.rules[item.rule].expansion;
        let origin = self.grammar.rules[item.rule].origin;
        // The recognized symbol (at `item.dot`) must be the last in the rule.
        if item.dot + 1 != expansion.len() {
            return false;
        }
        // Refuse a directly self-recursive start (matches the reference guard).
        origin != start_id || expansion[item.dot] != start_id
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
            // The topmost item the chain reaches is the advance of the highest
            // unique originator seen so far (its `node` is unused downstream).
            top = Some(Item {
                rule: o.rule,
                dot: o.dot + 1,
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
                    let cur_node = forest.get_or_create(
                        self.node_key(tr.red.rule, tr.red.dot + 1),
                        tr.red.origin,
                        end,
                    );
                    let right =
                        forest.get_or_create(NodeKey::Sym(tr.recognized), tr.key_start, end);
                    forest.add_family(
                        cur_node,
                        tr.red.rule,
                        tr.red.node,
                        ForestRef::Node(right),
                        tr.red.dot,
                    );
                }
            }
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
        root.map(|root| (forest, root))
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
                    // The item keeps its dot and prefix node; it just reappears in
                    // the column after the ignored span. A completed item is also
                    // registered in this column's node cache so the root (and any
                    // waiting completer) finds it across trailing/inner ignores.
                    if self.is_complete(&item) {
                        if let ForestRef::Node(id) = item.node {
                            let key = NodeKey::Sym(self.grammar.rules[item.rule].origin);
                            // Carry the completed node's identity into this column so
                            // the root (and any waiter) finds it across the ignore.
                            forest.index.entry((key, item.origin, end)).or_insert(id);
                        }
                        next_col.add(item, None);
                    } else if self.expects_terminal(&item) {
                        next_scan.add(item);
                    } else {
                        next_col.add(item, self.expect(&item));
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
    /// Memoized per-symbol-node derivation lists (the deduped alternative values
    /// before they are collapsed into a single value / `_ambig`). Shared by
    /// [`Transformer::eval_symbol`] and the transparent-child ambiguity lifting in
    /// [`Transformer::expand_packed`].
    deriv_memo: HashMap<usize, Vec<NodeValue>>,
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
            deriv_memo: HashMap::new(),
            prio: HashMap::new(),
            prio_visiting: HashSet::new(),
            seen: HashSet::new(),
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

    /// The deduped list of derivation values for a symbol node. Under `resolve`
    /// this is the single best derivation (highest priority, first in `sort_key`
    /// order); under explicit ambiguity it is every distinct derivation. Memoized,
    /// since a shared SPPF node is reachable from many parents.
    fn symbol_derivations(
        &mut self,
        node_id: usize,
        visiting: &mut HashSet<usize>,
    ) -> Vec<NodeValue> {
        if let Some(d) = self.deriv_memo.get(&node_id) {
            return d.clone();
        }
        if !visiting.insert(node_id) {
            return Vec::new(); // cycle in the forest — discard this family
        }
        let order = self.sorted_families(node_id);

        let mut derivs: Vec<NodeValue> = Vec::new();
        if self.resolve {
            for fi in order {
                let packed = self.forest.nodes[node_id].families[fi];
                if let Some(list) = self.expand_packed(&packed, visiting).into_iter().next() {
                    derivs.push(self.builder.assemble(packed.rule, list));
                    break;
                }
            }
        } else {
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
        }

        visiting.remove(&node_id);
        // The *real* #56 Arm-2 cost: explicit mode materializes one owned value per
        // symbol node, and a transparent left-recursive helper's value is the whole
        // accumulated child list — so the SPPF chain of n helper nodes builds Inlines
        // of size 1, 2, …, n = O(n²) elements total (and `deriv_memo` then clones
        // them). Counting the materialized derivation sizes here (behind
        // `perf-counters`) exhibits that quadratic deterministically — the signal the
        // streaming fix (the explicit analog of #55) must flatten. It is *not* the
        // `expand_packed` clone loop the issue guessed (that is linear; see there).
        // Gated at the call site (not just inside the no-op) so the `sum` — itself
        // O(materialized children), i.e. the quadratic we are measuring — is never
        // computed in a normal build.
        #[cfg(feature = "perf-counters")]
        crate::perf::add_explicit_node_children(derivs.iter().map(node_value_size).sum::<u64>());
        self.deriv_memo.insert(node_id, derivs.clone());
        derivs
    }

    /// Evaluate a real (non-intermediate) symbol node to a single value: the best
    /// derivation under `resolve`, or an `_ambig` over all of them under explicit.
    /// Returns `None` if every derivation is discarded (e.g. an ambiguity cycle).
    fn eval_symbol(&mut self, node_id: usize, visiting: &mut HashSet<usize>) -> Option<NodeValue> {
        if let Some(v) = self.memo.get(&node_id) {
            return Some(v.clone());
        }
        // Resolve mode keeps a single derivation, so its value can be assembled by
        // streaming children straight into one buffer — a left-recursive
        // transparent helper (`x*`/`x+`/`_rule`) then costs O(total children)
        // instead of the O(children²) the materialize-then-splice path pays
        // re-copying each growing prefix (issue #54).
        if self.resolve {
            let mut children: Vec<Child> = Vec::new();
            let rule = self.append_rule_children(node_id, &mut children, visiting)?;
            let v = self.builder.shape(rule, children);
            // Memoize only on the second visit: a single-use node is returned by
            // move (no clone). `insert` returns false when the node was already
            // present, i.e. this is its second full build — cache it so any further
            // reuse is a cheap clone, bounding rebuilds to at most two per node.
            if !self.seen.insert(node_id) {
                self.memo.insert(node_id, v.clone());
            }
            return Some(v);
        }
        let mut derivs = self.symbol_derivations(node_id, visiting);
        let result = match derivs.len() {
            0 => None,
            1 => Some(derivs.pop().unwrap()),
            _ => {
                let children: Vec<Child> = derivs.into_iter().map(node_value_to_child).collect();
                Some(NodeValue::Tree(Tree::new("_ambig", children)))
            }
        };

        if let Some(v) = &result {
            self.memo.insert(node_id, v.clone());
        }
        result
    }

    // ─── Resolve-mode streaming assembly (issue #54) ──────────────────────────
    //
    // The explicit-ambiguity path above materializes every alternative as an owned
    // child-list and splices transparent children by copying their `Inline` value
    // into the parent — O(n²) on a left-recursive transparent helper, whose SPPF is
    // a chain of n prefix nodes each one element longer than the last. Resolve mode
    // keeps exactly one derivation, so it can instead *stream* children into a
    // single shared buffer: a transparent child appends its own children in place
    // (no per-level copy), making the walk linear. The three methods below mirror
    // `TreeBuilder::assemble`'s filtering + shaping (via `keep_token` / `shape`) so
    // resolve trees stay byte-for-byte identical to the explicit path and to LALR.

    /// Resolve mode. Pick `node`'s best non-discarded family and append its
    /// rule-position children — post-filter, with transparent children spliced in
    /// place — to `out`. Returns the chosen rule (so the caller can `shape` it), or
    /// `None` if every family is discarded (a forest cycle). Works for both symbol
    /// nodes (a complete rule) and intermediate nodes (a rule prefix); both just
    /// contribute children in left-to-right order.
    fn append_rule_children(
        &mut self,
        node: usize,
        out: &mut Vec<Child>,
        visiting: &mut HashSet<usize>,
    ) -> Option<usize> {
        if !visiting.insert(node) {
            return None; // cycle in the forest — discard this derivation
        }
        let mut chosen = None;
        for fi in self.sorted_families(node) {
            let packed = self.forest.nodes[node].families[fi];
            let mark = out.len();
            if self.append_packed(&packed, out, visiting) {
                chosen = Some(packed.rule);
                break;
            }
            out.truncate(mark); // discarded part-way: roll back, try the next family
        }
        visiting.remove(&node);
        chosen
    }

    /// Append one derivation's children — its left prefix (an intermediate of the
    /// same rule) then its right symbol at `packed.right_pos` — to `out`. Returns
    /// false if any sub-node is discarded, so the caller can try another family.
    fn append_packed(
        &mut self,
        packed: &Packed,
        out: &mut Vec<Child>,
        visiting: &mut HashSet<usize>,
    ) -> bool {
        match packed.left {
            ForestRef::None => {}
            ForestRef::Node(lid) => {
                if self.append_rule_children(lid, out, visiting).is_none() {
                    return false;
                }
            }
            // `left` is always an intermediate node or nothing in the binarized
            // forest; handle a token defensively for symmetry with `expand_packed`.
            ForestRef::Tok(t) => out.push(Child::Token(self.forest.tokens[t].clone())),
        }
        match packed.right {
            ForestRef::None => {} // ε production: no right child
            ForestRef::Tok(t) => {
                if self.builder.keep_token(packed.rule, packed.right_pos) {
                    out.push(Child::Token(self.forest.tokens[t].clone()));
                }
            }
            ForestRef::Node(rid) => {
                if self.is_transparent_node(rid) {
                    // Splice the transparent child's children straight into `out`.
                    if self.splice_node(rid, out, visiting).is_none() {
                        return false;
                    }
                } else {
                    // A real symbol node contributes one shaped value; mirror
                    // `assemble`'s per-value handling (a `Token` is subject to the
                    // position's filter, a `Tree` is always kept).
                    match self.eval_symbol(rid, visiting) {
                        None => return false,
                        Some(NodeValue::Token(tk)) => {
                            if self.builder.keep_token(packed.rule, packed.right_pos) {
                                out.push(Child::Token(tk));
                            }
                        }
                        Some(NodeValue::Tree(tr)) => out.push(Child::Tree(tr)),
                        Some(NodeValue::Inline(cs)) => out.extend(cs),
                    }
                }
            }
        }
        true
    }

    /// Append a transparent symbol node's spliced children (plus its rule's
    /// `maybe_placeholders`) to `out`. Recurses through `append_rule_children`, so a
    /// chain of transparent helpers flattens into `out` in one linear pass.
    fn splice_node(
        &mut self,
        node: usize,
        out: &mut Vec<Child>,
        visiting: &mut HashSet<usize>,
    ) -> Option<usize> {
        let rule = self.append_rule_children(node, out, visiting)?;
        for _ in 0..self.grammar.rules[rule].options.placeholder_count {
            out.push(Child::None);
        }
        Some(rule)
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

        // The alternative values the right symbol contributes. Normally one — but a
        // *transparent* (`_rule` / `__anon_*`) child that is itself ambiguous under
        // explicit ambiguity contributes one alternative per derivation, which we
        // distribute over the parent's child-lists below. This is Lark's
        // `AmbiguousExpander`: rather than nest an `_ambig` under the spliced
        // position, the ambiguity is shifted up so the parent itself becomes the
        // `_ambig` (`parent(_ambig(a, b))` → `_ambig(parent(a), parent(b))`).
        let right_alts: Vec<NodeValue> = match packed.right {
            ForestRef::None => Vec::new(),
            ForestRef::Node(id) => {
                if !self.resolve && self.is_transparent_node(id) {
                    let alts = self.symbol_derivations(id, visiting);
                    if alts.is_empty() {
                        return Vec::new(); // right discarded → whole derivation gone
                    }
                    alts
                } else {
                    match self.eval_symbol(id, visiting) {
                        Some(v) => vec![v],
                        None => return Vec::new(), // right discarded → whole derivation gone
                    }
                }
            }
            ForestRef::Tok(t) => vec![NodeValue::Token(self.forest.tokens[t].clone())],
        };

        if right_alts.is_empty() {
            // ε right: the child-lists are exactly the left prefixes.
            return lefts;
        }

        // The named #56 Arm-2 suspect: clone each growing prefix to form the
        // cartesian product of left prefixes × right values. Counting the
        // `NodeValue`s copied here (behind `perf-counters`) is what *disproves* that
        // guess — it stays **linear** even on a transparent left-recursive helper
        // (`x*` / `x+` / `_rule`), because every rule's binarized RHS prefix is
        // bounded (≤ its arity), so this clone is O(1) per node. The real explicit
        // super-linearity is the per-node derivation-value rebuild counted in
        // `symbol_derivations` — the still-missing explicit analog of #55's
        // resolve-mode streaming. Kept verbatim (no fast path) so the disproof
        // measures the actual loop; the true fix is tracked as a follow-up.
        let mut out: Vec<Vec<NodeValue>> = Vec::with_capacity(lefts.len() * right_alts.len());
        for list in &lefts {
            for rv in &right_alts {
                crate::perf::add_explicit_prefix_copies(list.len() as u64);
                let mut l = list.clone();
                l.push(rv.clone());
                out.push(l);
            }
        }
        out
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
