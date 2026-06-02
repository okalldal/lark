use super::symbol::{Symbol, NonTerminal};

/// A single grammar rule: `origin → expansion`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Rule {
    pub origin: NonTerminal,
    pub expansion: Vec<Symbol>,
    pub alias: Option<String>,
    pub options: RuleOptions,
    /// Index among rules that share the same `origin` (used for stable ordering).
    pub order: usize,
}

impl Rule {
    pub fn new(
        origin: NonTerminal,
        expansion: Vec<Symbol>,
        alias: Option<String>,
        options: RuleOptions,
        order: usize,
    ) -> Self {
        Rule { origin, expansion, alias, options, order }
    }

    /// Convenience: rule with default options and no alias.
    pub fn simple(origin: NonTerminal, expansion: Vec<Symbol>) -> Self {
        Rule::new(origin, expansion, None, RuleOptions::default(), 0)
    }

    /// The name to use for the tree node produced by this rule.
    pub fn tree_name(&self) -> &str {
        self.alias.as_deref().unwrap_or(&self.origin.name)
    }

    pub fn is_empty(&self) -> bool {
        self.expansion.is_empty()
    }
}

impl std::fmt::Display for Rule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let rhs: Vec<&str> = self.expansion.iter().map(|s| s.name()).collect();
        write!(f, "{} -> {}", self.origin, rhs.join(" "))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RuleOptions {
    /// If true and the rule produces exactly one child, replace the node with that child.
    pub expand1: bool,
    /// Preserve all terminals in the tree (don't filter punctuation).
    pub keep_all_tokens: bool,
    /// Priority for disambiguation — higher wins.
    pub priority: i32,
    /// When using `maybe_placeholders`, indices in expansion that came from `?` operators
    /// and may be absent.
    pub empty_indices: Vec<bool>,
    /// Number of `None` placeholder children this (empty `[...]`) production emits
    /// on reduce, under `maybe_placeholders`. 0 for ordinary rules.
    pub placeholder_count: usize,
}

impl Default for RuleOptions {
    fn default() -> Self {
        RuleOptions {
            expand1: false,
            keep_all_tokens: false,
            priority: 0,
            empty_indices: Vec::new(),
            placeholder_count: 0,
        }
    }
}

impl RuleOptions {
    pub fn with_priority(mut self, p: i32) -> Self {
        self.priority = p;
        self
    }

    pub fn with_expand1(mut self) -> Self {
        self.expand1 = true;
        self
    }

    pub fn with_keep_all_tokens(mut self) -> Self {
        self.keep_all_tokens = true;
        self
    }
}
