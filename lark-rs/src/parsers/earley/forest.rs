//! The Shared Packed Parse Forest (SPPF): Elizabeth Scott's binarized forest the
//! recognizer builds and the forest→tree walk reads.
//!
//! [`NodeKey`]/[`ForestRef`] (node identity + child references), [`Packed`] (one
//! derivation), [`SymbolNode`] (a node and its alternative families), [`Trans`]
//! (a memoized Joop-Leo transitive), and the [`Forest`] arena. Split out of the
//! former monolithic `earley.rs` (no logic change).

use std::collections::{HashMap, HashSet};

use crate::grammar::intern::SymbolId;
use crate::tree::Token;

use super::chart::Item;

// ─── Shared Packed Parse Forest ───────────────────────────────────────────────

/// Identity of an SPPF symbol node within a column: either a completed
/// non-terminal (`Sym`) or an intermediate dotted rule (`Inter(rule, ptr)`),
/// plus its start column (the end is the column it lives in).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum NodeKey {
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
pub(crate) enum ForestRef {
    None,
    Node(usize),
    Tok(usize),
}

/// A single derivation of a symbol node: which rule, and its (binarized) left and
/// right children.
#[derive(Clone, Copy)]
pub(crate) struct Packed {
    pub(crate) rule: usize,
    pub(crate) left: ForestRef,
    pub(crate) right: ForestRef,
    /// Expansion position of `right` within `rule` (the dot before it was
    /// consumed). Lets the streaming forest walk apply per-position token
    /// filtering without rebuilding a per-symbol value list. Unused when
    /// `right` is `None` (an ε derivation).
    pub(crate) right_pos: usize,
}

/// A symbol or intermediate node. Its `families` are the alternative derivations
/// (packed nodes); more than one means the node is ambiguous.
pub(crate) struct SymbolNode {
    pub(crate) is_intermediate: bool,
    pub(crate) families: Vec<Packed>,
    /// De-duplication set over `(left, right)` — Python Lark's `PackedNode`
    /// equality compares exactly those (`earley_forest.py`: `__eq__` ignores
    /// `rule`). The **ε family** `(None, None)` is special-cased *outside* this set
    /// (`eps_family` below), so it is never inserted here.
    family_set: HashSet<(ForestRef, ForestRef)>,
    /// Index into `families` of this node's single ε derivation (`left`/`right`
    /// both `None`), and the `rule.order` it was recorded with — `None` until one
    /// is added.
    ///
    /// **Why exactly one ε family, by lowest `rule.order` (#432).** Python keeps a
    /// *single* ε packed node per symbol: its `PackedNode.__eq__` compares only
    /// `(left, right)`, so two ε derivations collapse to one (the first inserted),
    /// and its `EBNF_to_BNF` lowering emits just one ε production even for distinct
    /// aliased nullable arms (`p: "a"? -> al1 | "b"? -> al2` → one `p -> ε` aliased
    /// al1). lark-rs keeps **both** aliased ε rules (so the LALR R/R resolver can
    /// pick the first arm by `rule.order`, #401), so its forest would otherwise
    /// carry two ε families differing only by alias/`tree_name`. Resolving them the
    /// way Python's *observable result* does — first-arm-wins — means keeping the
    /// lowest-`rule.order` ε family (al1), discarding the rest. A raw `(left,right)`
    /// dedup keyed the wrong one (al2 drained LIFO, processed/inserted first); a
    /// keep-both key turned the empty input into a spurious `_ambig(al1, al2)` under
    /// `ambiguity='explicit'` (the empty string is a single derivation, just
    /// alias-named). Keeping one ε family by lowest order yields al1 under *both*
    /// resolve and explicit, matching Python's forest (one ε family) and its
    /// first-arm-wins result. Non-empty families are untouched — the `(left,right)`
    /// dedup is load-bearing for the SPPF over-sharing the `_ambig` dedup
    /// compensates for (#159, ADR-0017), and this is scoped to ε derivations alone.
    eps_family: Option<(usize, usize)>,
    /// Joop-Leo deferred reconstructions: `(transitive, bottom_node, end_col)`. A
    /// Leo completion records a path here instead of materializing the O(n) skipped
    /// reduction nodes eagerly; `load_leo_paths` expands them (once, lazily) into
    /// `families` before the forest→tree walk. Empty for non-Leo nodes.
    pub(crate) paths: Vec<(usize, ForestRef, usize)>,
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
pub(crate) struct Trans {
    /// The recognized non-terminal and the column it starts at (the map key).
    pub(crate) recognized: SymbolId,
    pub(crate) key_start: usize,
    /// The unique originator `[B → β•(recognized)γ, red.origin]` being advanced;
    /// `red.node` is its built-so-far left child.
    pub(crate) red: Item,
    /// Next level up the deterministic reduction path, or `None` if this level's
    /// completion (`B`) is itself the topmost.
    pub(crate) parent: Option<usize>,
    /// The topmost item the chain collapses to (its `node` is unused). Identical
    /// for every level of one chain; the completer builds it at `(top.origin, i)`.
    pub(crate) top: Item,
}

/// Arena of forest nodes + the scanned-token leaves they reference by index.
pub(crate) struct Forest {
    pub(crate) nodes: Vec<SymbolNode>,
    pub(crate) tokens: Vec<Token>,
    /// Global identity index: `(key, start, end) → node id`. A node is one symbol
    /// (or intermediate dotted rule) over one span, no matter how many derivations
    /// reach it — every completion of `(key, start, end)` merges its family here.
    /// Keying on `end` (not a per-column cache) is what lets Joop-Leo's lazy spine
    /// reconstruction *reuse* the chart's existing nodes instead of forking a
    /// parallel copy, so a symbol's Leo-derived and normally-derived families land
    /// in the same node (required for `ambiguity='resolve'` to compare them).
    pub(crate) index: HashMap<(NodeKey, usize, usize), usize>,
}

impl Forest {
    pub(crate) fn new() -> Self {
        Forest {
            nodes: Vec::new(),
            tokens: Vec::new(),
            index: HashMap::new(),
        }
    }

    /// Node id for the symbol/intermediate `key` spanning `(start, end)`, creating
    /// it on first sight and returning the same id on every later request.
    pub(crate) fn get_or_create(&mut self, key: NodeKey, start: usize, end: usize) -> usize {
        if let Some(&id) = self.index.get(&(key, start, end)) {
            return id;
        }
        let id = self.nodes.len();
        self.nodes.push(SymbolNode {
            is_intermediate: key.is_intermediate(),
            families: Vec::new(),
            family_set: HashSet::new(),
            eps_family: None,
            paths: Vec::new(),
        });
        self.index.insert((key, start, end), id);
        crate::perf::add_forest_node();
        id
    }

    /// Record a derivation (packed node) on `node_id`.
    ///
    /// A non-empty derivation is de-duplicated by its `(left, right)` children,
    /// exactly as Python Lark's `PackedNode` equality (`earley_forest.py`).
    ///
    /// The **ε derivation** `(None, None)` is kept at most **once** per node, by
    /// lowest `rule.order` (`order`): the first ε family added installs it; a later
    /// ε family with a *lower* order replaces it in place; a higher-or-equal order
    /// is dropped. This reproduces Python's single ε packed node and its
    /// first-arm-wins result over lark-rs's twin aliased ε rules — see
    /// [`SymbolNode::eps_family`] for the full rationale (#432).
    pub(crate) fn add_family(
        &mut self,
        node_id: usize,
        rule: usize,
        order: usize,
        left: ForestRef,
        right: ForestRef,
        right_pos: usize,
    ) {
        let node = &mut self.nodes[node_id];
        if matches!((left, right), (ForestRef::None, ForestRef::None)) {
            // ε derivation: keep a single family, the lowest-order arm.
            let packed = Packed {
                rule,
                left,
                right,
                right_pos,
            };
            match node.eps_family {
                None => {
                    node.eps_family = Some((node.families.len(), order));
                    node.families.push(packed);
                }
                Some((idx, best_order)) if order < best_order => {
                    node.families[idx] = packed;
                    node.eps_family = Some((idx, order));
                }
                Some(_) => {} // an ε family of equal/higher order already wins
            }
            return;
        }
        if node.family_set.insert((left, right)) {
            node.families.push(Packed {
                rule,
                left,
                right,
                right_pos,
            });
        }
    }

    pub(crate) fn add_token(&mut self, token: Token) -> usize {
        let id = self.tokens.len();
        self.tokens.push(token);
        id
    }

    /// Record a Joop-Leo deferred reconstruction on `node_id` (which spans
    /// `(_, end)`): completing the chain bottom (`bottom`) under transitive
    /// `trans` rebuilds, lazily, the reduction spine whose top is this node.
    pub(crate) fn add_path(&mut self, node_id: usize, trans: usize, bottom: ForestRef, end: usize) {
        self.nodes[node_id].paths.push((trans, bottom, end));
    }
}
