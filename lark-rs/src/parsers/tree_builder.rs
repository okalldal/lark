//! The shared tree-builder.
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

use crate::grammar::intern::CompiledRule;
use crate::tree::{Child, Token, Tree};

/// A semantic value produced by reducing a rule (or shifting a terminal). It is
/// the currency on the LALR value stack and, in time, the result of walking an
/// SPPF node.
pub enum NodeValue {
    Token(Token),
    Tree(Tree),
    /// Children of a transparent (`_rule` / `__anon_*`) reduction, to be spliced
    /// into the parent's child list rather than wrapped in a node.
    Inline(Vec<Child>),
}

/// Applies a rule's tree-shaping options to its assembled children. Borrows the
/// compiled rules from the parse table; holds no mutable state, so a fresh one can
/// be made per reduction for free. Token filtering is per *rule position* (each
/// rule carries its own keep mask), not per terminal — see [`CompiledRule::filter_pos`].
pub struct TreeBuilder<'g> {
    rules: &'g [CompiledRule],
}

impl<'g> TreeBuilder<'g> {
    pub fn new(rules: &'g [CompiledRule]) -> Self {
        TreeBuilder { rules }
    }

    /// Build the value the parent sees when `rule_idx` reduces over `child_values`
    /// (in left-to-right order, one per expansion symbol). This is backend-agnostic:
    /// the LALR reducer drains them off its value stack; an Earley walk collects
    /// them from a forest node.
    pub fn assemble(&self, rule_idx: usize, child_values: Vec<NodeValue>) -> NodeValue {
        let rule = &self.rules[rule_idx];

        // Flatten child values into the parent's child list: drop filtered
        // punctuation tokens (unless the rule keeps all tokens), and splice the
        // children of an inlined (transparent) sub-rule in place. Inlined children
        // were already filtered when their own rule reduced. The child at index `i`
        // corresponds to expansion symbol `i`, so its keep/drop is `filter_pos[i]`.
        let mut children: Vec<Child> = Vec::new();
        for (i, value) in child_values.into_iter().enumerate() {
            match value {
                NodeValue::Token(t) => {
                    let filtered = rule.filter_pos.get(i).copied().unwrap_or(false);
                    if rule.options.keep_all_tokens || !filtered {
                        children.push(Child::Token(t));
                    }
                }
                NodeValue::Tree(t) => children.push(Child::Tree(t)),
                NodeValue::Inline(cs) => children.extend(cs),
            }
        }

        // maybe_placeholders: an empty `[...]` production emits one `None` per
        // kept symbol of its widest alternative.
        for _ in 0..rule.options.placeholder_count {
            children.push(Child::None);
        }

        if rule.transparent {
            // `_rule` / `__anon_*`: splice children into the parent.
            NodeValue::Inline(children)
        } else if rule.options.expand1
            && rule.alias.is_none()
            && children.len() == 1
            && !matches!(children[0], Child::None)
        {
            // `?rule` with a single child: return that child directly (Token or Tree).
            match children.pop().unwrap() {
                Child::Tree(t) => NodeValue::Tree(t),
                Child::Token(t) => NodeValue::Token(t),
                Child::None => unreachable!("guarded above"),
            }
        } else {
            NodeValue::Tree(Tree::new(rule.tree_name.clone(), children))
        }
    }
}
