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

use super::loader::AnonKind;
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
    /// Non-terminal only: the symbol's name marks it for inlining (a leading
    /// `_`, covering both `_name` transparent rules and `__anon_*` EBNF helpers).
    /// Whether a *rule* actually inlines also depends on its alias and start
    /// status — see [`CompiledRule::transparent`].
    pub inline: bool,
    /// Non-terminal only: a synthetic augmented start symbol (`$root_X`).
    pub is_start: bool,
    /// Non-terminal only: `Some(kind)` iff this origin is a *generated* anonymous
    /// EBNF helper the loader minted via `fresh_anon_rule` (a `*`/`?`/`~n`/group/
    /// `[…]` helper); `None` for every user-written rule. This is **source
    /// provenance**, distinct from [`inline`](Self::inline): a transparent user
    /// rule (`_a`) and a user rule the author *named* `__anon_star_0` are both
    /// `inline` yet have `anon_kind == None`, while a generated `__anon_rep_*`
    /// helper is `Some(..)`. CYK keys empty-rule rejection on this (#101, ADR-0024):
    /// a nullable user rule is rejected (matching Python), a nullable generated
    /// helper is accepted — never sniffing the `__anon_` spelling, which a user can
    /// author (#144).
    pub anon_kind: Option<AnonKind>,
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
        SymbolTable {
            infos: Vec::new(),
            by_name: HashMap::new(),
            n_terminals: 0,
            sealed: false,
        }
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
    fn intern_terminal(&mut self, name: &str) -> SymbolId {
        if let Some(&id) = self.by_name.get(name) {
            debug_assert_eq!(
                self.infos[id.index()].kind,
                SymbolKind::Terminal,
                "symbol {name:?} interned as both terminal and non-terminal"
            );
            return id;
        }
        debug_assert!(
            !self.sealed,
            "interning new terminal {name:?} after the range was sealed"
        );
        let id = SymbolId(self.infos.len() as u32);
        self.infos.push(SymbolInfo {
            name: name.to_string(),
            kind: SymbolKind::Terminal,
            inline: false,
            is_start: false,
            anon_kind: None,
        });
        self.by_name.insert(name.to_string(), id);
        id
    }

    /// Intern a non-terminal. Idempotent by name. Requires the terminal range to
    /// have been sealed first (see [`seal_terminals`](Self::seal_terminals)).
    fn intern_nonterminal(
        &mut self,
        name: &str,
        inline: bool,
        is_start: bool,
        anon_kind: Option<AnonKind>,
    ) -> SymbolId {
        if let Some(&id) = self.by_name.get(name) {
            debug_assert_eq!(
                self.infos[id.index()].kind,
                SymbolKind::NonTerminal,
                "symbol {name:?} interned as both terminal and non-terminal"
            );
            // A name first seen via a rule body and later as a start symbol can
            // upgrade its flags. Provenance is a property of the name's source, so
            // it is monotone too: once a generated helper, always a generated
            // helper (the loader never reuses a minted name for a user rule).
            let info = &mut self.infos[id.index()];
            info.inline |= inline;
            info.is_start |= is_start;
            if info.anon_kind.is_none() {
                info.anon_kind = anon_kind;
            }
            return id;
        }
        debug_assert!(
            self.sealed,
            "interning non-terminal {name:?} before the terminal range was sealed"
        );
        let id = SymbolId(self.infos.len() as u32);
        self.infos.push(SymbolInfo {
            name: name.to_string(),
            kind: SymbolKind::NonTerminal,
            inline,
            is_start,
            anon_kind,
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
    /// Per-position keep mask, parallel to `expansion`: `filter_pos[i]` is true
    /// when a token reduced at position `i` is dropped from the tree (unless the
    /// rule keeps all tokens). This is per *occurrence*, not per terminal — the
    /// same terminal can be kept at one position and filtered at another (Python
    /// Lark's model), which is what lets a unified literal (`"a"` lexing as `A`)
    /// be filtered while a sibling `A` reference is kept. Non-terminal positions
    /// are always `false` (they reduce to trees/inlines, never filtered here).
    pub filter_pos: Vec<bool>,
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
    /// Python Lark's `propagate_positions`: when set, a node's `meta` span is
    /// derived from its rule's pre-filter children (so filtered punctuation
    /// contributes — #402). A parse-global, set after lowering from `LarkOptions`
    /// (`lower` itself defaults it `false`); the engines thread it to the
    /// [`TreeOutputBuilder`](crate::parsers::tree_builder).
    pub propagate_positions: bool,
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

/// The tree label for a (possibly template-instance) rule name: everything before
/// the `{` that marks a template instantiation (`expr{0}` → `expr`). Ordinary
/// names contain no `{`, so they pass through unchanged.
fn template_base(name: &str) -> &str {
    name.split_once('{').map_or(name, |(base, _)| base)
}

/// Lower a surface [`Grammar`] to a [`CompiledGrammar`].
///
/// Interning order is load-bearing: `$END`, then every terminal, then every
/// non-terminal — so terminals occupy `[0, n_terminals)` and the parse tables
/// can index by id directly.
pub fn lower(grammar: &Grammar) -> CompiledGrammar {
    let mut symbols = SymbolTable::new();

    // ── Terminals first, $END at id 0. ──────────────────────────────────────
    let end = symbols.intern_terminal("$END");
    debug_assert_eq!(end, SymbolId::END);
    for t in &grammar.terminals {
        symbols.intern_terminal(&t.name);
    }
    // Defensive: a terminal referenced in a rule body but somehow absent from
    // the terminal list still belongs to the terminal id range.
    for rule in &grammar.rules {
        for sym in &rule.expansion {
            if let Symbol::Terminal(t) = sym {
                symbols.intern_terminal(&t.name);
            }
        }
    }

    // Every terminal is now interned; fix the terminal id range before any
    // non-terminal is added.
    symbols.seal_terminals();

    // ── Non-terminals: augmented starts, then origins, then referenced. ──────
    // Generated-helper provenance is carried by `grammar.anon_kinds` (keyed by
    // origin name, populated by the loader's `fresh_anon_rule`), *not* inferred
    // from the `__anon_` spelling — a user grammar may author that exact name
    // (#144), and the discriminator must stay source-based (#101).
    let anon_kind = |name: &str| -> Option<AnonKind> { grammar.anon_kinds.get(name).copied() };
    for start in &grammar.start {
        symbols.intern_nonterminal(&format!("$root_{}", start), false, true, None);
    }
    for rule in &grammar.rules {
        let name = &rule.origin.name;
        symbols.intern_nonterminal(name, name.starts_with('_'), false, anon_kind(name));
    }
    for rule in &grammar.rules {
        for sym in &rule.expansion {
            if let Symbol::NonTerminal(nt) = sym {
                symbols.intern_nonterminal(
                    &nt.name,
                    nt.name.starts_with('_'),
                    false,
                    anon_kind(&nt.name),
                );
            }
        }
    }

    let id = |sym: &Symbol| -> SymbolId { symbols.id(sym.name()).expect("symbol interned above") };

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
        // Per-occurrence keep mask: a terminal symbol's own `filter_out` decides
        // its position; non-terminals never filter here.
        let filter_pos: Vec<bool> = rule
            .expansion
            .iter()
            .map(|s| matches!(s, Symbol::Terminal(t) if t.filter_out))
            .collect();
        let inline = symbols.info(origin).inline;
        let is_start_origin = start_ids.contains(&origin);
        // A start symbol's rule is never inlined: the root must form a node.
        let transparent = inline && rule.alias.is_none() && !is_start_origin;
        // A template instance is named `base{N}`; its tree label is the base name
        // (Lark's `template_source`), so strip the `{…}` marker. Ordinary rule
        // names never contain `{`, so this is a no-op for them.
        let tree_name = rule
            .alias
            .clone()
            .unwrap_or_else(|| template_base(&rule.origin.name).to_string());
        rules.push(CompiledRule {
            origin,
            expansion,
            filter_pos,
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
            filter_pos: vec![false],
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
        // A parse-global, set after lowering from `LarkOptions`; `lower` itself
        // has no options, so it defaults `false`.
        propagate_positions: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grammar::load_grammar;

    fn compile(src: &str) -> CompiledGrammar {
        let g = load_grammar(src, &["start".to_string()], false, false).unwrap();
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
                i,
                info.name,
                n
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

    /// Provenance plumbing (#101, ADR-0024): a *generated* anonymous EBNF helper
    /// carries `Some(AnonKind)`, while a user-written rule — even one transparent
    /// (`_a`) or spelled like a helper (`__anon_star_0`) — carries `None`. This is
    /// the discriminator CYK keys empty-rule rejection on, and it must be source
    /// provenance, not the `__anon_` name spelling (#144).
    #[test]
    fn anon_kind_marks_generated_helpers_not_user_rules() {
        // `(B*)~2` forces a standalone nullable helper; `_a` is a transparent user
        // rule; `__anon_star_0` is a user rule the author happened to name like a
        // helper.
        let cg = compile("start: _a (B*)~2 __anon_star_0\n_a: B\n__anon_star_0: B\nB: \"b\"\n");
        // The transparent user rule and the helper-looking user rule are user-written.
        for user in ["_a", "__anon_star_0", "start"] {
            let id = cg.symbols.id(user).unwrap();
            assert!(
                cg.symbols.info(id).anon_kind.is_none(),
                "user rule {user:?} must have no anon provenance"
            );
        }
        // At least one generated helper exists and is marked with its kind.
        let generated: Vec<_> = (0..cg.symbols.len())
            .map(|i| cg.symbols.info(SymbolId(i as u32)))
            .filter(|info| info.anon_kind.is_some())
            .collect();
        assert!(
            !generated.is_empty(),
            "`(B*)~2` must emit at least one generated nullable helper marked with an AnonKind"
        );
        // Every marked symbol is a generated `__anon_*` helper (the loader's only
        // minting path), confirming the marker is set at generation, not by spelling.
        for info in generated {
            assert!(
                info.name.starts_with("__anon_"),
                "only generated helpers carry anon_kind; saw {:?}",
                info.name
            );
        }
    }

    #[test]
    fn alias_on_inlined_rule_is_rejected() {
        // An alias on an inlined (`_`-prefixed) rule is rejected at load, exactly
        // as Python Lark does ("Rule _inner is marked for expansion … isn't
        // allowed to have aliases"; RC4a, issue #271). Earlier lark-rs accepted it
        // and (over-permissively) emitted the aliased node — the
        // unfalsifiable-permissiveness bug the ADR-0017 corollary forbids. This
        // grammar can no longer reach `lower()`, so the alias-overrides-transparency
        // interning path it used to exercise is now unreachable by construction.
        let r = load_grammar(
            "start: _inner\n_inner: WORD -> kept | NUM\nWORD: /[a-z]+/\nNUM: /[0-9]+/\n",
            &["start".to_string()],
            false,
            false,
        );
        assert!(
            r.is_err(),
            "alias on an inlined _rule must be rejected at load (RC4a)"
        );
    }

    #[test]
    fn inline_rule_without_alias_stays_transparent() {
        // A bare inlined rule (no alias, the only form Python permits) interns
        // transparent — the surviving half of the old
        // `underscore_rule_is_transparent_alias_overrides` pin.
        let cg = compile("start: _inner\n_inner: WORD | NUM\nWORD: /[a-z]+/\nNUM: /[0-9]+/\n");
        let inner = cg.symbols.id("_inner").unwrap();
        assert!(cg.symbols.info(inner).inline);
        assert!(cg
            .rules
            .iter()
            .filter(|r| r.origin == inner)
            .all(|r| r.transparent));
    }
}
