//! Interned grammar IR.
//!
//! The surface [`Grammar`](super::Grammar) produced by the loader identifies
//! every symbol by its `String` name and encodes several *semantic* properties
//! in the spelling of that name (`$root_` = augmented start, a leading `_` =
//! transparent, `__ANON_` = anonymous literal). That is convenient for the
//! loader but wrong for the engine: it costs a string clone + hash on every
//! symbol comparison, forces the parse table to be keyed by `String`, and makes
//! correctness depend on name conventions (a real bug once: `__anon_` colliding
//! with `__RSQB`).
//!
//! This module lowers the surface grammar to a [`CompiledGrammar`] in which:
//!   * every symbol is a `Copy` [`SymbolId`] (a dense `u32` index),
//!   * terminals occupy the contiguous id range `[0, n_terminals)` and
//!     non-terminals the range `[n_terminals, len)`, so both the ACTION and GOTO
//!     tables can be dense `Vec`s indexed directly by id, and
//!   * every semantic property (`kind`, `filter_out`, `transparent`, `is_start`)
//!     is a precomputed field — the engine never inspects a name again.
//!
//! Names survive only in the [`SymbolTable`] as a side-table for diagnostics and
//! for the tree node labels (`Tree::data`).

use std::collections::HashMap;

use super::rule::RuleOptions;
use super::symbol::Symbol;
use super::terminal::TerminalDef;
use super::Grammar;

/// A dense, `Copy` identifier for a grammar symbol (terminal or non-terminal).
///
/// Ids are assigned so that all terminals come first: `id.0 < n_terminals` iff
/// the symbol is a terminal. The synthetic end-of-input terminal is always
/// [`SymbolId::END`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SymbolId(pub u32);

impl SymbolId {
    /// The synthetic end-of-input terminal `$END`. Always interned first.
    pub const END: SymbolId = SymbolId(0);

    /// Placeholder for a token not produced by a lexer (test/manual `Token`s).
    /// Never indexes a parse table — such tokens take the "unexpected" path.
    pub const UNSET: SymbolId = SymbolId(u32::MAX);

    #[inline]
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolKind {
    Terminal,
    NonTerminal,
}

/// Everything the engine needs to know about a symbol, decided once at lowering.
#[derive(Debug, Clone)]
pub struct SymbolInfo {
    pub name: String,
    pub kind: SymbolKind,
    /// Terminal only: token is dropped from the tree unless the rule keeps all
    /// tokens (anonymous literals and `_`-prefixed terminals).
    pub filter_out: bool,
    /// Non-terminal only: the symbol's name marks it for inlining (a leading
    /// `_`, covering both `_name` transparent rules and `__anon_*` EBNF helpers).
    /// Whether a *rule* actually inlines also depends on its alias and start
    /// status — see [`CompiledRule::transparent`].
    pub inline: bool,
    /// Non-terminal only: a synthetic augmented start symbol (`$root_X`).
    pub is_start: bool,
}

/// Interner mapping names ↔ [`SymbolId`]s, with per-symbol metadata.
#[derive(Debug, Clone)]
pub struct SymbolTable {
    infos: Vec<SymbolInfo>,
    by_name: HashMap<String, SymbolId>,
    n_terminals: usize,
    /// Set once every terminal is interned; from then on `n_terminals` is fixed
    /// and only non-terminals may be added. Makes the terminal/non-terminal id
    /// boundary explicit rather than implied by "the first non-terminal".
    sealed: bool,
}

impl SymbolTable {
    fn new() -> Self {
        SymbolTable { infos: Vec::new(), by_name: HashMap::new(), n_terminals: 0, sealed: false }
    }

    /// Seal the terminal id range: all terminals are interned, so the boundary
    /// between the terminal range `[0, n_terminals)` and the non-terminal range
    /// is now fixed — even for a (degenerate) grammar with no non-terminals.
    /// Must be called after the last terminal and before the first non-terminal.
    fn seal_terminals(&mut self) {
        debug_assert!(!self.sealed, "terminal range sealed twice");
        self.n_terminals = self.infos.len();
        self.sealed = true;
    }

    /// Intern a terminal. Idempotent by name. Must be called for every terminal
    /// *before* any non-terminal so the terminal id range stays contiguous.
    fn intern_terminal(&mut self, name: &str, filter_out: bool) -> SymbolId {
        if let Some(&id) = self.by_name.get(name) {
            debug_assert_eq!(self.infos[id.index()].kind, SymbolKind::Terminal,
                "symbol {name:?} interned as both terminal and non-terminal");
            return id;
        }
        debug_assert!(!self.sealed, "interning new terminal {name:?} after the range was sealed");
        let id = SymbolId(self.infos.len() as u32);
        self.infos.push(SymbolInfo {
            name: name.to_string(),
            kind: SymbolKind::Terminal,
            filter_out,
            inline: false,
            is_start: false,
        });
        self.by_name.insert(name.to_string(), id);
        id
    }

    /// Intern a non-terminal. Idempotent by name. Requires the terminal range to
    /// have been sealed first (see [`seal_terminals`](Self::seal_terminals)).
    fn intern_nonterminal(&mut self, name: &str, inline: bool, is_start: bool) -> SymbolId {
        if let Some(&id) = self.by_name.get(name) {
            debug_assert_eq!(self.infos[id.index()].kind, SymbolKind::NonTerminal,
                "symbol {name:?} interned as both terminal and non-terminal");
            // A name first seen via a rule body and later as a start symbol can
            // upgrade its flags.
            let info = &mut self.infos[id.index()];
            info.inline |= inline;
            info.is_start |= is_start;
            return id;
        }
        debug_assert!(self.sealed, "interning non-terminal {name:?} before the terminal range was sealed");
        let id = SymbolId(self.infos.len() as u32);
        self.infos.push(SymbolInfo {
            name: name.to_string(),
            kind: SymbolKind::NonTerminal,
            filter_out: false,
            inline,
            is_start,
        });
        self.by_name.insert(name.to_string(), id);
        id
    }

    #[inline]
    pub fn info(&self, id: SymbolId) -> &SymbolInfo {
        &self.infos[id.index()]
    }

    #[inline]
    pub fn name(&self, id: SymbolId) -> &str {
        &self.infos[id.index()].name
    }

    #[inline]
    pub fn kind(&self, id: SymbolId) -> SymbolKind {
        self.infos[id.index()].kind
    }

    #[inline]
    pub fn is_terminal(&self, id: SymbolId) -> bool {
        self.infos[id.index()].kind == SymbolKind::Terminal
    }

    /// Look up a symbol id by name (diagnostics / construction only — not a hot
    /// path).
    pub fn id(&self, name: &str) -> Option<SymbolId> {
        self.by_name.get(name).copied()
    }

    /// Number of terminal-kind symbols; equals the exclusive upper bound of the
    /// terminal id range.
    #[inline]
    pub fn n_terminals(&self) -> usize {
        self.n_terminals
    }

    #[inline]
    pub fn n_nonterminals(&self) -> usize {
        self.infos.len() - self.n_terminals
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.infos.len()
    }

    pub fn is_empty(&self) -> bool {
        self.infos.is_empty()
    }

    /// Dense index for a non-terminal into a GOTO row (`id - n_terminals`).
    #[inline]
    pub fn nonterminal_index(&self, id: SymbolId) -> usize {
        id.index() - self.n_terminals
    }
}

/// A grammar rule with every symbol interned and every tree-shaping decision
/// precomputed, so the parser's reduce path does zero string work.
#[derive(Debug, Clone)]
pub struct CompiledRule {
    pub origin: SymbolId,
    pub expansion: Vec<SymbolId>,
    pub alias: Option<String>,
    pub options: RuleOptions,
    pub order: usize,
    /// Label for the tree node this rule builds (alias, else origin name).
    pub tree_name: String,
    /// The rule's children splice into the parent instead of forming a node
    /// (`_name` / `__anon_*` rules, unless aliased or a start symbol).
    pub transparent: bool,
    /// Synthetic augmented-start rule `$root_X → X`; its reduction is ACCEPT.
    pub is_start: bool,
}

impl CompiledRule {
    pub fn is_empty(&self) -> bool {
        self.expansion.is_empty()
    }
}

impl std::fmt::Display for CompiledRule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ->", self.tree_name)?;
        for &s in &self.expansion {
            write!(f, " {}", s.0)?;
        }
        Ok(())
    }
}

/// The fully-interned, engine-facing grammar.
#[derive(Debug, Clone)]
pub struct CompiledGrammar {
    pub symbols: SymbolTable,
    /// Compiled rules, followed by the synthetic augmented-start rules.
    pub rules: Vec<CompiledRule>,
    /// Terminal definitions (patterns/priorities), in lexer order.
    pub terminals: Vec<TerminalDef>,
    /// `%ignore` terminal ids.
    pub ignore: Vec<SymbolId>,
    /// User start non-terminal ids (not the augmented `$root_` symbols).
    pub start: Vec<SymbolId>,
}

impl CompiledGrammar {
    #[inline]
    pub fn n_terminals(&self) -> usize {
        self.symbols.n_terminals()
    }

    /// The augmented-start symbol (`$root_X`) for a user start symbol, if any.
    pub fn augmented_start(&self, start: SymbolId) -> Option<SymbolId> {
        let name = format!("$root_{}", self.symbols.name(start));
        self.symbols.id(&name)
    }
}

/// Lower a surface [`Grammar`] to a [`CompiledGrammar`].
///
/// Interning order is load-bearing: `$END`, then every terminal, then every
/// non-terminal — so terminals occupy `[0, n_terminals)` and the parse tables
/// can index by id directly.
pub fn lower(grammar: &Grammar) -> CompiledGrammar {
    let mut symbols = SymbolTable::new();

    // ── Terminals first, $END at id 0. ──────────────────────────────────────
    let end = symbols.intern_terminal("$END", false);
    debug_assert_eq!(end, SymbolId::END);
    for t in &grammar.terminals {
        symbols.intern_terminal(&t.name, t.filter_out);
    }
    // Defensive: a terminal referenced in a rule body but somehow absent from
    // the terminal list still belongs to the terminal id range.
    for rule in &grammar.rules {
        for sym in &rule.expansion {
            if let Symbol::Terminal(t) = sym {
                symbols.intern_terminal(&t.name, t.filter_out);
            }
        }
    }

    // Every terminal is now interned; fix the terminal id range before any
    // non-terminal is added.
    symbols.seal_terminals();

    // ── Non-terminals: augmented starts, then origins, then referenced. ──────
    for start in &grammar.start {
        symbols.intern_nonterminal(&format!("$root_{}", start), false, true);
    }
    for rule in &grammar.rules {
        let name = &rule.origin.name;
        symbols.intern_nonterminal(name, name.starts_with('_'), false);
    }
    for rule in &grammar.rules {
        for sym in &rule.expansion {
            if let Symbol::NonTerminal(nt) = sym {
                symbols.intern_nonterminal(&nt.name, nt.name.starts_with('_'), false);
            }
        }
    }

    let id = |sym: &Symbol| -> SymbolId {
        symbols.id(sym.name()).expect("symbol interned above")
    };

    // ── Compiled rules. ─────────────────────────────────────────────────────
    let start_ids: Vec<SymbolId> = grammar
        .start
        .iter()
        .map(|s| symbols.id(s).expect("start symbol interned"))
        .collect();

    let mut rules: Vec<CompiledRule> = Vec::with_capacity(grammar.rules.len() + start_ids.len());
    for rule in &grammar.rules {
        let origin = symbols.id(&rule.origin.name).expect("origin interned");
        let expansion: Vec<SymbolId> = rule.expansion.iter().map(&id).collect();
        let inline = symbols.info(origin).inline;
        let is_start_origin = start_ids.contains(&origin);
        // A start symbol's rule is never inlined: the root must form a node.
        let transparent = inline && rule.alias.is_none() && !is_start_origin;
        let tree_name = rule.alias.clone().unwrap_or_else(|| rule.origin.name.clone());
        rules.push(CompiledRule {
            origin,
            expansion,
            alias: rule.alias.clone(),
            options: rule.options.clone(),
            order: rule.order,
            tree_name,
            transparent,
            is_start: false,
        });
    }

    // Synthetic augmented-start rules `$root_X → X`, appended last.
    for (start_name, &start_id) in grammar.start.iter().zip(&start_ids) {
        let root = symbols
            .id(&format!("$root_{}", start_name))
            .expect("augmented start interned");
        rules.push(CompiledRule {
            origin: root,
            expansion: vec![start_id],
            alias: None,
            options: RuleOptions::default(),
            order: 0,
            tree_name: symbols.name(root).to_string(),
            transparent: false,
            is_start: true,
        });
    }

    let ignore: Vec<SymbolId> = grammar
        .ignore
        .iter()
        .filter_map(|n| symbols.id(n))
        .collect();

    CompiledGrammar {
        symbols,
        rules,
        terminals: grammar.terminals.clone(),
        ignore,
        start: start_ids,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grammar::load_grammar;

    fn compile(src: &str) -> CompiledGrammar {
        let g = load_grammar(src, &["start".to_string()], false).unwrap();
        lower(&g)
    }

    #[test]
    fn end_is_terminal_zero() {
        let cg = compile("start: WORD\nWORD: /[a-z]+/\n");
        assert_eq!(SymbolId::END, SymbolId(0));
        assert_eq!(cg.symbols.name(SymbolId::END), "$END");
        assert!(cg.symbols.is_terminal(SymbolId::END));
    }

    #[test]
    fn terminals_occupy_low_id_range() {
        let cg = compile("start: WORD\nWORD: /[a-z]+/\n");
        let n = cg.n_terminals();
        for (i, info) in (0..cg.symbols.len()).map(|i| (i, cg.symbols.info(SymbolId(i as u32)))) {
            let is_term = i < n;
            assert_eq!(
                is_term,
                matches!(info.kind, SymbolKind::Terminal),
                "symbol {} ({}) on wrong side of n_terminals={}",
                i, info.name, n
            );
        }
    }

    #[test]
    fn augmented_start_is_flagged_not_named() {
        let cg = compile("start: WORD\nWORD: /[a-z]+/\n");
        let start = cg.start[0];
        let root = cg.augmented_start(start).unwrap();
        assert!(cg.symbols.info(root).is_start);
        let root_rule = cg.rules.iter().find(|r| r.is_start).unwrap();
        assert_eq!(root_rule.origin, root);
        assert_eq!(root_rule.expansion, vec![start]);
    }

    #[test]
    fn underscore_rule_is_transparent_alias_overrides() {
        // `_inner` is transparent; the aliased alternative is not.
        let cg = compile("start: _inner\n_inner: WORD -> kept | WORD\nWORD: /[a-z]+/\n");
        let inner = cg.symbols.id("_inner").unwrap();
        assert!(cg.symbols.info(inner).inline);
        let transparent: Vec<bool> = cg
            .rules
            .iter()
            .filter(|r| r.origin == inner)
            .map(|r| r.transparent)
            .collect();
        // One alternative is aliased (not transparent), one is bare (transparent).
        assert!(transparent.contains(&true));
        assert!(transparent.contains(&false));
    }
}
