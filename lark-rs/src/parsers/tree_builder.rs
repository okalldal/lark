//! The shared tree-builder and the `OutputBuilder` seam.
//!
//! Turning a reduced rule plus its child values into the parent's value is the
//! one place where Lark's tree-shaping semantics live: punctuation filtering,
//! transparent (`_rule` / `__anon_*`) splicing, `?rule` (`expand1`), aliases, and
//! `maybe_placeholders`. Both parser backends must apply these *identically* — the
//! LALR reducer today, and the Earley forest-walk that materializes trees from the
//! SPPF tomorrow. Keeping the logic here, rather than inline in either driver,
//! guarantees there is exactly one definition of "what tree does this rule build",
//! and makes it the single chokepoint where the node representation could later
//! change (e.g. an arena or interned labels) without touching the parsers.
//!
//! ## Architecture: `OutputBuilder` seam (issue #227)
//!
//! The `OutputBuilder` trait is the internal seam between the engine's tree-shaping
//! logic and the value it produces. The engine drives all shaping decisions — token
//! filtering, transparent splicing, `expand1` unwrapping, placeholder insertion —
//! through the [`Slot`] discriminant on the parse stack. The `OutputBuilder` only
//! ever sees the *flat, already-shaped* children and builds the final value from
//! them.
//!
//! [`TreeOutputBuilder`] is the default (and currently only) implementation:
//! `Value = Child`, producing the same `Tree`/`Token` parse trees as before.
//! Future implementations (e.g. a semantic-action builder) will use a different
//! `Value` type but share the same engine-driven shaping. The trait is internal
//! (not re-exported from the public API); the public trait shape is deferred to
//! issue #231.

use crate::grammar::intern::CompiledRule;
use crate::tree::{Child, Meta, Token, Tree};

// ─── Slot: the engine's internal stack currency ─────────────────────────────

/// A semantic value on the parse stack. This is the engine's internal currency:
/// it carries the shaping discriminant (`Token`/`Tree` vs `Inline`) that drives
/// transparent splicing, so the engine can flatten `Inline` children before
/// handing the shaped result to the [`OutputBuilder`]. The builder never sees
/// `Inline` — only the already-flattened child list.
///
/// `Clone` so the Earley forest-walk can memoize a shared SPPF node's assembled
/// value (a DAG node is reachable by many parents).
#[derive(Clone)]
pub(crate) enum Slot {
    Token(Token),
    Tree(Tree),
    /// Children of a transparent (`_rule` / `__anon_*`) reduction, to be spliced
    /// into the parent's child list rather than wrapped in a node.
    Inline(Vec<Child>),
}

// Backward-compat alias: all internal code that used `NodeValue` keeps compiling.
pub(crate) type NodeValue = Slot;

// ─── OutputBuilder trait ────────────────────────────────────────────────────

/// The internal seam between engine-driven tree shaping and value construction.
///
/// The engine handles all shaping decisions (token filtering, transparent
/// splicing, `expand1` unwrapping, placeholder insertion) and calls the builder
/// only with the flat, already-shaped children. The builder's job is to wrap
/// those children into its `Value` type.
///
/// This trait is **internal** — it is not part of the public API. The public
/// trait shape (for user-facing semantic actions) is deferred to issue #231.
pub(crate) trait OutputBuilder {
    /// The value type this builder produces for a non-terminal node.
    /// For the default tree builder, this is `Child`.
    type Value;

    /// Build a tree node from a rule's tree name and its flat children.
    ///
    /// Called when a rule reduces to a normal (non-transparent, non-expand1) node.
    fn build_node(&self, tree_name: &str, children: Vec<Child>) -> Self::Value;

    /// Build a token value from a shifted terminal.
    fn build_token(&self, token: Token) -> Self::Value;
}

// ─── TreeOutputBuilder: the default implementation ──────────────────────────

/// The default [`OutputBuilder`] implementation: builds `Tree`/`Token`/`Child`
/// parse trees, identical to the output before the seam was introduced.
///
/// Applies a rule's tree-shaping options to its assembled children. Borrows the
/// compiled rules from the parse table; holds no mutable state, so a fresh one can
/// be made per reduction for free. Token filtering is per *rule position* (each
/// rule carries its own keep mask), not per terminal — see [`CompiledRule::filter_pos`].
pub(crate) struct TreeOutputBuilder<'g> {
    rules: &'g [CompiledRule],
    /// When true (Python Lark's `propagate_positions=True`), a node's `meta` span
    /// is derived from its rule's **pre-filter** children, so filtered punctuation
    /// (`"(" A ")"`) contributes to the container span. Mirrors Python's
    /// `PropagatePositions`, which wraps the node builder *outside* the child
    /// filter and so sees the unfiltered children (bug-bounty H6-5, #402). When
    /// false the span is computed from the already-filtered children, exactly as
    /// before (lark-rs always populates `meta`; the flag only changes *which*
    /// children feed the span).
    propagate_positions: bool,
}

impl<'g> OutputBuilder for TreeOutputBuilder<'g> {
    type Value = Child;

    fn build_node(&self, tree_name: &str, children: Vec<Child>) -> Child {
        Child::Tree(Tree::new(tree_name.to_string(), children))
    }

    fn build_token(&self, token: Token) -> Child {
        Child::Token(token)
    }
}

impl<'g> TreeOutputBuilder<'g> {
    pub fn new(rules: &'g [CompiledRule]) -> Self {
        TreeOutputBuilder {
            rules,
            propagate_positions: false,
        }
    }

    /// As [`new`](Self::new), with Python Lark's `propagate_positions` flag.
    pub fn with_propagate_positions(rules: &'g [CompiledRule], propagate_positions: bool) -> Self {
        TreeOutputBuilder {
            rules,
            propagate_positions,
        }
    }

    /// Build the value the parent sees when `rule_idx` reduces over `child_values`
    /// (in left-to-right order, one per expansion symbol). This is backend-agnostic:
    /// the LALR reducer drains them off its value stack; an Earley walk collects
    /// them from a forest node.
    pub fn assemble(&self, rule_idx: usize, child_values: Vec<Slot>) -> Slot {
        // Flatten child values into the parent's child list: drop filtered
        // punctuation tokens (unless the rule keeps all tokens), and splice the
        // children of an inlined (transparent) sub-rule in place. Inlined children
        // were already filtered when their own rule reduced. The child at index `i`
        // corresponds to expansion symbol `i`, so its keep/drop is `filter_pos[i]`.
        //
        // Under `propagate_positions`, the node's span must come from the *pre*-
        // filter children (so a filtered `"("`/`")"` still bounds the span — #402),
        // so accumulate the container Meta over every value as it streams by, kept
        // or dropped, before the kept list is shaped into a node.
        let mut children: Vec<Child> = Vec::new();
        let mut container = ContainerSpan::new();
        for (i, value) in child_values.into_iter().enumerate() {
            for _ in 0..self.nones_at(rule_idx, i) {
                children.push(Child::None);
            }
            match value {
                Slot::Token(t) => {
                    if self.propagate_positions {
                        container.observe_token(&t);
                    }
                    if self.keep_token(rule_idx, i) {
                        children.push(Child::Token(t));
                    }
                }
                Slot::Tree(t) => {
                    if self.propagate_positions {
                        container.observe_meta(&t.meta);
                    }
                    children.push(Child::Tree(t));
                }
                Slot::Inline(cs) => {
                    if self.propagate_positions {
                        for c in &cs {
                            container.observe_child(c);
                        }
                    }
                    children.extend(cs);
                }
            }
        }
        self.shape_with_container(rule_idx, children, container)
    }

    /// Number of `None` placeholders a distributed absent `[...]` left before
    /// expansion position `gap` of `rule_idx` (position `expansion.len()` is
    /// trailing — appended by [`shape`](Self::shape)). 0 for ordinary rules.
    pub fn nones_at(&self, rule_idx: usize, gap: usize) -> usize {
        self.rules[rule_idx]
            .options
            .nones_before
            .get(gap)
            .copied()
            .unwrap_or(0)
    }

    /// Whether the token at expansion position `pos` of `rule_idx` is kept (not a
    /// filtered punctuation terminal), honoring `keep_all_tokens`. Split out of
    /// [`assemble`] so a streaming forest walk can apply the *same* per-position
    /// filtering rule without first materializing a per-symbol value list.
    pub fn keep_token(&self, rule_idx: usize, pos: usize) -> bool {
        let rule = &self.rules[rule_idx];
        rule.options.keep_all_tokens || !rule.filter_pos.get(pos).copied().unwrap_or(false)
    }

    /// Turn a rule's already-filtered child list into the value its parent sees:
    /// append `maybe_placeholders`, then splice (transparent), unwrap (`expand1`),
    /// or wrap in a [`Tree`]. The tail half of [`assemble`], shared with the Earley
    /// streaming walk so both produce identical shaping.
    ///
    /// The streaming Earley walk filters tokens *before* `shape`, so the dropped
    /// punctuation is gone here — under `propagate_positions` that caller threads
    /// the pre-filter container span through
    /// [`shape_with_container`](Self::shape_with_container) instead. With the flag
    /// off (the common case), the two are identical.
    pub fn shape(&self, rule_idx: usize, children: Vec<Child>) -> Slot {
        self.shape_with_container(rule_idx, children, ContainerSpan::new())
    }

    /// As [`shape`](Self::shape), but with the rule's pre-filter container span
    /// (the span over *all* children including filtered punctuation). Under
    /// `propagate_positions`, a non-transparent node's `meta` span is widened to
    /// this container so a filtered `"("`/`")"` bounds the parent (#402). An empty
    /// container (or the flag off) leaves the post-filter span untouched.
    pub fn shape_with_container(
        &self,
        rule_idx: usize,
        mut children: Vec<Child>,
        container: ContainerSpan,
    ) -> Slot {
        let rule = &self.rules[rule_idx];

        // maybe_placeholders: an empty `[...]` production emits one `None` per
        // kept symbol of its widest alternative; a distributed absent `[...]` at
        // the end of this alternative appends its trailing placeholders.
        for _ in 0..rule.options.placeholder_count {
            children.push(Child::None);
        }
        for _ in 0..self.nones_at(rule_idx, rule.expansion.len()) {
            children.push(Child::None);
        }

        if rule.transparent {
            // `_rule` / `__anon_*`: splice children into the parent. No node is
            // built, so there is no `meta` to widen — the spliced children carry
            // their own positions up, exactly as Python's `_pp_get_meta` reads them.
            Slot::Inline(children)
        } else if rule.options.expand1 && rule.alias.is_none() && children.len() == 1 {
            // `?rule` with a single child: return that child directly. A lone `None`
            // placeholder (`?w: [A]` on the absent branch) collapses exactly like a
            // real single child — Python yields `start[None]`, not `start[w[None]]`
            // (bounty RC9; the `?` collapse is purely arity-1, never value-typed).
            // An empty `?` rule (`?w: A?` with zero children) is *not* len==1 and so
            // correctly keeps its wrapper (`start[w[]]`).
            //
            // No container widening here: Python's `PropagatePositions` only sets
            // meta when its node builder returns a `Tree` (`isinstance(res, Tree)`);
            // an `ExpandSingleChild` collapse to a bare token/tree skips it, so the
            // collapsed value keeps its own span.
            match children.pop().unwrap() {
                Child::Tree(t) => Slot::Tree(t),
                Child::Token(t) => Slot::Token(t),
                // A bare `None` has no Token/Tree slot; carry it as a single-None
                // inline so the parent splices exactly one `Child::None` in place.
                Child::None => Slot::Inline(vec![Child::None]),
            }
        } else {
            let mut tree = Tree::new(rule.tree_name.clone(), children);
            if self.propagate_positions {
                container.widen_meta(&mut tree.meta);
            }
            Slot::Tree(tree)
        }
    }
}

/// The container span of a rule's **pre-filter** children — the first/last
/// positioned child's start/end, including filtered punctuation tokens that the
/// tree drops. Mirrors Python Lark's `PropagatePositions._pp_get_meta` (which runs
/// outside the child filter), the regression net for bug-bounty H6-5 / #402.
///
/// Built by streaming every child value past [`observe_token`](Self::observe_token)
/// / [`observe_meta`](Self::observe_meta) / [`observe_child`](Self::observe_child)
/// in left-to-right order, then applied with [`widen_meta`](Self::widen_meta).
#[derive(Default, Clone)]
pub(crate) struct ContainerSpan {
    first: Option<Meta>,
    last: Option<Meta>,
}

impl ContainerSpan {
    pub fn new() -> Self {
        ContainerSpan::default()
    }

    /// Record a positioned child's `meta`. The first positioned child fixes the
    /// start fields; every positioned child updates the (so-far) last, so the final
    /// `last` fixes the end fields — matching Python's first/`reversed`-first scan.
    fn observe(&mut self, meta: Meta) {
        if self.first.is_none() {
            self.first = Some(meta.clone());
        }
        self.last = Some(meta);
    }

    /// Observe a (pre-filter) token — it contributes its own start/end.
    pub fn observe_token(&mut self, t: &Token) {
        self.observe(Meta {
            line: Some(t.line),
            column: Some(t.column),
            end_line: Some(t.end_line),
            end_column: Some(t.end_column),
            start_pos: Some(t.start_pos),
            end_pos: Some(t.end_pos),
            empty: false,
        });
    }

    /// Observe a (pre-filter) subtree's `meta`. A positionless empty subtree
    /// contributes nothing (Python skips `c.meta.empty` children in `_pp_get_meta`).
    pub fn observe_meta(&mut self, meta: &Meta) {
        if !meta.empty && meta.line.is_some() {
            self.observe(meta.clone());
        }
    }

    /// Observe a spliced (transparent-rule) child — token or subtree.
    pub fn observe_child(&mut self, c: &Child) {
        match c {
            Child::Token(t) => self.observe_token(t),
            Child::Tree(t) => self.observe_meta(&t.meta),
            Child::None => {}
        }
    }

    /// Widen `meta`'s span to the container: set its start fields from the first
    /// positioned child and its end fields from the last, when the container saw
    /// any positioned child. A node with no positioned pre-filter child (an empty
    /// production) is left untouched.
    fn widen_meta(&self, meta: &mut Meta) {
        if let Some(first) = &self.first {
            meta.line = first.line;
            meta.column = first.column;
            meta.start_pos = first.start_pos;
            meta.empty = false;
        }
        if let Some(last) = &self.last {
            meta.end_line = last.end_line;
            meta.end_column = last.end_column;
            meta.end_pos = last.end_pos;
            meta.empty = false;
        }
    }
}

// Backward-compat: keep `TreeBuilder` as a type alias so any code outside the
// core three files (earley.rs, cyk.rs, lalr.rs) that references it by name
// keeps compiling without a rename. This alias is crate-internal only.
pub(crate) type TreeBuilder<'g> = TreeOutputBuilder<'g>;
