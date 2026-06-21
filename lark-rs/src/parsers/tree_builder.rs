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
use crate::tree::{Child, Token, Tree};

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
pub enum Slot {
    Token(Token),
    Tree(Tree),
    /// Children of a transparent (`_rule` / `__anon_*`) reduction, to be spliced
    /// into the parent's child list rather than wrapped in a node.
    Inline(Vec<Child>),
}

// Backward-compat alias: all internal code that used `NodeValue` keeps compiling.
pub type NodeValue = Slot;

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
pub trait OutputBuilder {
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
pub struct TreeOutputBuilder<'g> {
    rules: &'g [CompiledRule],
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
        TreeOutputBuilder { rules }
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
        let mut children: Vec<Child> = Vec::new();
        for (i, value) in child_values.into_iter().enumerate() {
            for _ in 0..self.nones_at(rule_idx, i) {
                children.push(Child::None);
            }
            match value {
                Slot::Token(t) => {
                    if self.keep_token(rule_idx, i) {
                        children.push(Child::Token(t));
                    }
                }
                Slot::Tree(t) => children.push(Child::Tree(t)),
                Slot::Inline(cs) => children.extend(cs),
            }
        }
        self.shape(rule_idx, children)
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
    pub fn shape(&self, rule_idx: usize, mut children: Vec<Child>) -> Slot {
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
            // `_rule` / `__anon_*`: splice children into the parent.
            Slot::Inline(children)
        } else if rule.options.expand1
            && rule.alias.is_none()
            && children.len() == 1
            && !matches!(children[0], Child::None)
        {
            // `?rule` with a single child: return that child directly (Token or Tree).
            match children.pop().unwrap() {
                Child::Tree(t) => Slot::Tree(t),
                Child::Token(t) => Slot::Token(t),
                Child::None => unreachable!("guarded above"),
            }
        } else {
            Slot::Tree(Tree::new(rule.tree_name.clone(), children))
        }
    }
}

// Backward-compat: keep `TreeBuilder` as a type alias so any code outside the
// core three files (earley.rs, cyk.rs, lalr.rs) that references it by name
// keeps compiling without a rename. This alias is crate-internal only.
pub type TreeBuilder<'g> = TreeOutputBuilder<'g>;
