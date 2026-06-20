//! Phase 3 — the grammar compiler's shared state and staging.
//!
//! [`GrammarCompiler`] owns every cross-phase cache; the phase logic lives in
//! sibling modules as further `impl GrammarCompiler` blocks: [`super::imports`]
//! (`%import` resolution), [`super::terminals`] (terminal-algebra → regex),
//! [`super::ebnf`] (rule bodies / EBNF expansion), and [`super::templates`]
//! (template instantiation). This module holds the staging order
//! ([`process_items`](GrammarCompiler::process_items)) and the final assembly
//! into a [`Grammar`] ([`compile`](GrammarCompiler::compile)).

use super::ast::*;
use super::ebnf::{CompiledAlt, HelperKey};
use super::imports::spec_final_names;
use crate::error::GrammarError;
use crate::grammar::rule::{Rule, RuleOptions};
use crate::grammar::symbol::Symbol;
use crate::grammar::terminal::{Pattern, TerminalDef};
use crate::grammar::Grammar;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

/// The closed set of anonymous-helper flavours the compiler generates, each
/// rendered as a `__anon_{tag}_{n}` rule name (terminals use `__ANON_{n}` via
/// [`GrammarCompiler::fresh_terminal`]). Typed so a new helper cannot pick a
/// colliding tag by typo, and so the rendering lives in exactly one place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AnonKind {
    /// `(...)` group helper (also the optional-group form).
    Group,
    /// `[...]` under `maybe_placeholders`.
    Maybe,
    /// `x?` optional wrapper.
    Opt,
    /// `x*` nullable wrapper over the shared `+`-recurse helper.
    Star,
    /// The shared `+`-recurse helper (`P: inner | P inner`).
    Plus,
    /// `x~n` exact repetition.
    Rep,
    /// `x~n..m` bounded-range repetition.
    RepRange,
}

impl AnonKind {
    fn tag(self) -> &'static str {
        match self {
            AnonKind::Group => "group",
            AnonKind::Maybe => "maybe",
            AnonKind::Opt => "opt",
            AnonKind::Star => "star",
            AnonKind::Plus => "plus",
            AnonKind::Rep => "rep",
            AnonKind::RepRange => "rep_range",
        }
    }
}

/// Converts the parsed AST into flat BNF rules and terminal definitions.
pub(super) struct GrammarCompiler {
    pub(super) start: Vec<String>,
    pub(super) rules: Vec<Rule>,
    pub(super) terminals: Vec<TerminalDef>,
    /// Raw terminal definitions, collected before any are compiled so a terminal
    /// body may reference another terminal defined later (`C: "C" | D`).
    pub(super) raw_terms: Vec<RawTerm>,
    pub(super) ignore_patterns: Vec<Pattern>,
    /// Counter for generating unique anonymous rule names.
    pub(super) anon_counter: usize,
    /// Counter for generating unique terminal names for literals.
    pub(super) term_counter: usize,
    /// Cache: literal string/regex → auto-generated terminal name.
    pub(super) literal_cache: HashMap<String, String>,
    /// Template definitions: name → (params, expansions, modifiers, priority).
    /// The modifiers (`!` keep-all, `?` expand1) and priority are kept so each
    /// instantiation inherits the template's rule options, exactly as Python Lark
    /// deep-copies the template's `RuleOptions` onto every instance.
    pub(super) templates: HashMap<String, (Vec<String>, Vec<AliasedExpansion>, String, i32)>,
    /// Memo of template instantiations: canonical `name<args>` key → instance rule
    /// name. Lets a self-recursive template (`_sep{x,d}: x | _sep{x,d} d x`) resolve
    /// its own reference to the rule already being built instead of recursing
    /// forever (mirrors Python Lark, which memoizes instantiations).
    pub(super) template_instances: HashMap<String, String>,
    /// Whether absent `[...]` groups emit `None` placeholders (Lark parity).
    pub(super) maybe_placeholders: bool,
    /// The grammar-wide `keep_all_tokens` option: when set, every rule keeps its
    /// tokens, exactly as if each carried the `!` modifier.
    pub(super) global_keep_all: bool,
    /// `keep_all_tokens` of the rule currently being compiled — needed to count
    /// kept symbols for placeholder generation.
    pub(super) current_keep_all: bool,
    /// Inlined "rule size" of each anonymous EBNF helper (maybe / optional /
    /// group), mirroring Python Lark's `FindRuleSize`. An absent `[...]` emits one
    /// `None` per unit of this size, and a *nested* maybe/group inside a `[...]`
    /// must contribute its own size (not 0) so placeholders compose recursively.
    /// `*` / `+` / `~` helpers and transparent `_rules` are deliberately absent
    /// (size 0), exactly as Lark treats `_`-prefixed symbols as removed.
    pub(super) helper_sizes: HashMap<String, usize>,
    /// Cache of the shared one-or-more recurse helper keyed by its inlined inner
    /// alternatives and the keep-all context. Identical `x+`/`x*` occurrences reuse
    /// one rule — Python Lark's `rules_cache`. This sharing is what keeps grammars
    /// like `a+ b | a+` and `a* b | a+` LALR-parseable: with separate recurse rules
    /// the duplicated `… -> "a"` reductions are an unresolvable reduce/reduce. The
    /// key is the inner expression's *compiled alternatives*: Python inlines a
    /// grouped repetition's arms straight into the recurse rule
    /// (`(A | B)+` → `_p: A | B | _p A | _p B`), so two `(A|B)+` occurrences share
    /// iff their cartesian-expanded arms coincide.
    pub(super) recurse_cache: HashMap<(Vec<super::ebnf::CompiledAlt>, bool), String>,
    /// Cache of every other anonymous EBNF helper — groups, optionals, `?`/`*`
    /// wrappers — keyed by its [`HelperKey`] structural identity. Extends the
    /// single-symbol `recurse_cache` sharing to grouped repetition: Python Lark's
    /// `rules_cache`. Without it, each `(",", X)*` occurrence gets a fresh helper,
    /// so structurally-identical nullable rules collide as unresolvable
    /// reduce/reduce (e.g. `python.lark`'s many `(",", param)*` patterns).
    pub(super) helper_cache: HashMap<HelperKey, String>,
    /// Anon helper rules that already derive ε (the `?`/`*` helpers). A `?` applied
    /// to one of these is redundant — `(X?)?` is just `X?` — so it is collapsed
    /// rather than stacked, which is what Python Lark's distribute+dedup achieves
    /// and what keeps `("A"?)?` from building two ambiguous empty rules.
    pub(super) nullable_opts: std::collections::HashSet<String>,
    /// Directory that relative file imports resolve against (the importing
    /// grammar's directory). `None` when the grammar was built from a string with
    /// no source location, in which case only `%import common.*` resolves.
    pub(super) base_path: Option<PathBuf>,
    /// In-memory grammar sources for relative imports (the #47 follow-up: the WASM
    /// no-filesystem case): virtual `/`-separated path (e.g. `"dir/tokens.lark"`)
    /// → grammar text. When `Some`, file imports resolve against this map *only*
    /// — the filesystem is never consulted — with `base_path` acting as a virtual
    /// prefix (default: the map root). Shared down nested imports via `Arc`.
    pub(super) import_sources: Option<Arc<HashMap<String, String>>>,
    /// User-authored rule names (rules, templates, import targets), collected up
    /// front so [`fresh_anon_rule`](Self::fresh_anon_rule) never hands out a name
    /// the grammar already claims — `__anon_group_0` is a *valid* user rule name,
    /// and a generated duplicate would silently merge two unrelated origins.
    /// Generated names never collide with each other (one monotonic counter), and
    /// import-mangled dependencies (`mod__name` / `_mod__name`) cannot take the
    /// `__anon_{tag}_{n}` shape, so user-authored names are the only hazard.
    reserved_rule_names: HashSet<String>,
    /// User-authored terminal names (terminals, declares, import targets), the
    /// same guard for [`fresh_terminal`](Self::fresh_terminal)'s `__ANON_{n}`.
    /// Unlike rules, generated terminal names must *also* dodge live state: a
    /// literal `"__anon_5"` interns under the hint `__ANON_5` (its uppercase
    /// form), which no up-front scan can see.
    reserved_term_names: HashSet<String>,
}

impl GrammarCompiler {
    pub(super) fn new(
        start: Vec<String>,
        maybe_placeholders: bool,
        keep_all_tokens: bool,
        base_path: Option<PathBuf>,
        import_sources: Option<Arc<HashMap<String, String>>>,
    ) -> Self {
        GrammarCompiler {
            start,
            rules: Vec::new(),
            terminals: Vec::new(),
            raw_terms: Vec::new(),
            ignore_patterns: Vec::new(),
            anon_counter: 0,
            term_counter: 0,
            literal_cache: HashMap::new(),
            templates: HashMap::new(),
            template_instances: HashMap::new(),
            maybe_placeholders,
            global_keep_all: keep_all_tokens,
            current_keep_all: keep_all_tokens,
            helper_sizes: HashMap::new(),
            recurse_cache: HashMap::new(),
            helper_cache: HashMap::new(),
            nullable_opts: std::collections::HashSet::new(),
            base_path,
            import_sources,
            reserved_rule_names: HashSet::new(),
            reserved_term_names: HashSet::new(),
        }
    }

    /// A fresh `__anon_{tag}_{n}` helper-rule name, skipping any name the user's
    /// grammar already claims (see [`reserved_rule_names`](Self::reserved_rule_names)).
    pub(super) fn fresh_anon_rule(&mut self, kind: AnonKind) -> String {
        loop {
            let name = format!("__anon_{}_{}", kind.tag(), self.anon_counter);
            self.anon_counter += 1;
            if !self.reserved_rule_names.contains(&name) {
                return name;
            }
        }
    }

    /// Options for anonymous EBNF helper rules (groups, optionals, repetition).
    /// `keep_all_tokens` propagates from the enclosing rule so that `!rule` keeps
    /// tokens inside its `[...]`, `(...)`, `*`, `+` sub-expressions too.
    pub(super) fn anon_opts(&self) -> RuleOptions {
        RuleOptions {
            keep_all_tokens: self.current_keep_all,
            ..RuleOptions::default()
        }
    }

    /// Whether `name` is free to assign to an anonymous (generated or
    /// hint-named) terminal. Checks **both** namespaces, not just terminals:
    /// the lowerer interns every symbol into one `by_name` table, so a terminal
    /// that shadows a *rule* name corrupts the id space — `intern_nonterminal`
    /// would hand back the terminal's id (guarded only by a `debug_assert` in
    /// release builds). `__ANON_0` is a valid user *rule* name (a leading `__`
    /// lexes as a rule token), so the rule namespace is reachable. Reservations
    /// cover user-authored names known up front; the live lists are the
    /// defensive backstop for names minted mid-compile (uppercase literal
    /// hints) and anything reservation cannot see.
    fn anon_terminal_name_free(&self, name: &str) -> bool {
        !self.reserved_term_names.contains(name)
            && !self.reserved_rule_names.contains(name)
            && !self.terminals.iter().any(|t| t.name == name)
            && !self.rules.iter().any(|r| r.origin.name == name)
    }

    /// A fresh `__ANON_{n}` terminal name, skipping names the user's grammar
    /// claims in either namespace (see
    /// [`anon_terminal_name_free`](Self::anon_terminal_name_free)).
    pub(super) fn fresh_terminal(&mut self) -> String {
        loop {
            let name = format!("__ANON_{}", self.term_counter);
            self.term_counter += 1;
            if self.anon_terminal_name_free(&name) {
                return name;
            }
        }
    }

    /// Whether a literal's human-readable name *hint* (`","` → `COMMA`,
    /// `"kw"` → `KW`) may be used as the terminal's name. Same availability
    /// rule as a generated name (a hint like `__ANON_5` — the uppercase form of
    /// `"__anon_5"` — must dodge both namespaces too); on rejection the caller
    /// falls back to [`fresh_terminal`](Self::fresh_terminal).
    pub(super) fn hint_name_free(&self, name: &str) -> bool {
        self.anon_terminal_name_free(name)
    }

    pub(super) fn process_items(&mut self, items: Vec<Item>) -> Result<(), GrammarError> {
        // First pass: register templates, and reserve every user-authored name so
        // generated `__anon_*` / `__ANON_*` names can never shadow one. An import's
        // target may be a rule or a terminal — unknowable before resolution — so it
        // reserves in both namespaces (harmless: the namespaces cannot overlap).
        for item in &items {
            match item {
                Item::RuleItem(r) => {
                    self.reserved_rule_names.insert(r.name.clone());
                    if !r.params.is_empty() {
                        self.templates.insert(
                            r.name.clone(),
                            (
                                r.params.clone(),
                                r.expansions.clone(),
                                r.modifiers.clone(),
                                r.priority,
                            ),
                        );
                    }
                }
                Item::TermItem(t) => {
                    self.reserved_term_names.insert(t.name.clone());
                }
                Item::DeclareItem(syms) => {
                    for sym in syms {
                        if let Symbol::Terminal(t) = sym {
                            self.reserved_term_names.insert(t.name.clone());
                        }
                    }
                }
                Item::ImportItem(spec) => {
                    for name in spec_final_names(spec) {
                        self.reserved_rule_names.insert(name.clone());
                        self.reserved_term_names.insert(name);
                    }
                }
                Item::IgnoreItem(_) => {}
            }
        }

        // Staged compilation. Terminals are resolved as a whole *before* rule bodies
        // so that (a) a string literal in a rule can unify with an already-known
        // terminal and (b) a terminal body may reference any other terminal,
        // regardless of definition order. Imports/declares run first so terminal
        // bodies can reference imported terminals.
        let mut rule_items = Vec::new();
        let mut ignore_items = Vec::new();
        for item in items {
            match item {
                Item::ImportItem(spec) => self.resolve_import(spec)?,
                Item::DeclareItem(syms) => self.declare_terminals(syms),
                Item::TermItem(t) => self.raw_terms.push(t),
                Item::RuleItem(r) if !r.params.is_empty() => { /* template — used on demand */ }
                Item::RuleItem(r) => rule_items.push(r),
                Item::IgnoreItem(expansions) => ignore_items.push(expansions),
            }
        }

        // Resolve all terminals (inlining terminal-to-terminal references).
        self.resolve_terminals()?;

        // Rule bodies, then `%ignore` expansions (which may reference terminals).
        for r in rule_items {
            self.compile_rule(r)?;
        }
        for expansions in ignore_items {
            for expansion in expansions {
                let pat = self.expansion_to_pattern(&expansion)?;
                self.ignore_patterns.push(pat);
            }
        }
        Ok(())
    }

    /// Gap vectors are stored on the rule only when they carry placeholders;
    /// the all-zero common case stays an empty `Vec` so ordinary rules pay
    /// nothing.
    pub(super) fn stored_gaps(gaps: Vec<usize>) -> Vec<usize> {
        if gaps.iter().any(|&g| g > 0) {
            gaps
        } else {
            Vec::new()
        }
    }

    /// Python Lark's two-stage duplicate handling for one origin's compiled
    /// alternatives (`load_grammar.py`). Stage 1, `SimplifyRule_Visitor.expansions`:
    /// alternatives that are identical *trees* — here, identical
    /// `(symbols, gaps, alias)`, since `_EMPTY` markers and alias nodes are part of
    /// Python's tree — are silently deduped, so `a: X | X` and the coinciding
    /// absent arms of `a: [A] C | [B] C` collapse instead of colliding as
    /// reduce/reduce under LALR. Stage 2, the final `Rule` compile: surviving
    /// duplicates of `(origin, expansion)` — `Rule.__eq__` ignores alias and
    /// options — raise "Rules defined twice", which is how a colliding expansion
    /// of optionals (`a: [A] [A] B`, whose two `A B` arms differ only in
    /// placeholder positions) or a same-expansion alias pair (`a: X -> p | X -> q`)
    /// is rejected *at load*, on every parser backend, instead of surfacing as an
    /// LALR-only conflict or being silently resolved by Earley. Duplicate *empty*
    /// expansions are tolerated, as in Python.
    pub(super) fn dedup_and_check_alts(
        origin: &str,
        alts: Vec<(CompiledAlt, Option<String>)>,
    ) -> Result<Vec<(CompiledAlt, Option<String>)>, GrammarError> {
        let mut seen: std::collections::HashSet<(CompiledAlt, Option<String>)> =
            std::collections::HashSet::new();
        let mut out: Vec<(CompiledAlt, Option<String>)> = Vec::with_capacity(alts.len());
        let mut seen_syms: std::collections::HashSet<Vec<Symbol>> =
            std::collections::HashSet::new();
        for alt in alts {
            if !seen.insert(alt.clone()) {
                continue; // exact duplicate — Python's AST-level dedup_list
            }
            let syms = &alt.0 .0;
            if !syms.is_empty() && !seen_syms.insert(syms.clone()) {
                let rhs: Vec<&str> = syms.iter().map(|s| s.name()).collect();
                return Err(GrammarError::Other {
                    msg: format!(
                        "Rules defined twice: {origin} -> {} \
                         (Might happen due to colliding expansion of optionals: [] or ?)",
                        rhs.join(" ")
                    ),
                });
            }
            out.push(alt);
        }
        Ok(out)
    }

    /// Register each `%declare`d name as a pattern-less terminal. A declared
    /// terminal is never lexed — it is interned (so rules can reference it and the
    /// parse table reserves a column) and injected into the token stream by a
    /// postlex hook, e.g. an [`Indenter`](crate::postlex::Indenter)'s `_INDENT` /
    /// `_DEDENT`. Already-defined names are left untouched (an explicit definition
    /// or import wins, matching how imports are kept in `resolve_terminals`).
    fn declare_terminals(&mut self, syms: Vec<Symbol>) {
        for sym in syms {
            if let Symbol::Terminal(t) = sym {
                if !self.terminals.iter().any(|td| td.name == t.name) {
                    self.terminals.push(TerminalDef::declared(&t.name));
                }
            }
        }
    }

    pub(super) fn compile(mut self) -> Result<Grammar, GrammarError> {
        // Add $END terminal
        if !self.terminals.iter().any(|t| t.name == "$END") {
            // $END is synthetic and handled by the parser, not the lexer.
        }

        // Add ignore terminals (one terminal per ignore pattern). `__IGNORE_{n}`
        // is the third generated-name family, and the import-alias route reaches
        // it like the others (`%import common.WS -> __IGNORE_0`), so it skips
        // user-claimed names via the same availability check. `%ignore` tokens
        // never reach the tree (the parse loop skips them), so they need no
        // per-occurrence filter — they appear in no rule body.
        let ignore_patterns = std::mem::take(&mut self.ignore_patterns);
        let mut ignore_names: Vec<String> = Vec::with_capacity(ignore_patterns.len());
        let mut ignore_counter = 0usize;
        for pat in ignore_patterns {
            let name = loop {
                let candidate = format!("__IGNORE_{}", ignore_counter);
                ignore_counter += 1;
                if self.anon_terminal_name_free(&candidate) {
                    break candidate;
                }
            };
            self.terminals.push(TerminalDef::new(&name, pat, 0));
            ignore_names.push(name);
        }

        // Reject use-before-definition: a rule body that references a symbol which
        // is neither a defined rule nor a defined terminal is a grammar error, as in
        // Python Lark (`GrammarError("Rule 'X' used but not defined")`). We check
        // *before* pruning so the full terminal set is visible. Template parameters
        // never reach here — templates are instantiated on demand and only their
        // (fully substituted) instances live in `self.rules` — and anonymous literal
        // terminals are interned as they are compiled, so they are always defined.
        let defined_rules: std::collections::HashSet<&str> =
            self.rules.iter().map(|r| r.origin.name.as_str()).collect();
        let defined_terms: std::collections::HashSet<&str> =
            self.terminals.iter().map(|t| t.name.as_str()).collect();
        for rule in &self.rules {
            for sym in &rule.expansion {
                match sym {
                    Symbol::NonTerminal(nt) if !defined_rules.contains(nt.name.as_str()) => {
                        return Err(GrammarError::UndefinedRule {
                            name: nt.name.clone(),
                        });
                    }
                    Symbol::Terminal(t) if !defined_terms.contains(t.name.as_str()) => {
                        return Err(GrammarError::UndefinedTerminal {
                            name: t.name.clone(),
                        });
                    }
                    _ => {}
                }
            }
        }

        // Prune terminals that no rule (or `%ignore`) references. A terminal used
        // only inside another terminal (`C: "C" | D` — `D` is inlined into `C`)
        // has no token of its own, exactly as Python Lark drops it. Terminals
        // referenced by a rule body, and the synthetic `%ignore` terminals, stay.
        let mut used: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for rule in &self.rules {
            for sym in &rule.expansion {
                if let Symbol::Terminal(t) = sym {
                    used.insert(t.name.as_str());
                }
            }
        }
        for name in &ignore_names {
            used.insert(name.as_str());
        }
        self.terminals.retain(|t| used.contains(t.name.as_str()));

        // Sort terminals by (priority desc, max_width desc, name asc)
        self.terminals.sort_by(|a, b| {
            b.priority
                .cmp(&a.priority)
                .then_with(|| {
                    let bw = b.pattern.max_width().unwrap_or(usize::MAX);
                    let aw = a.pattern.max_width().unwrap_or(usize::MAX);
                    bw.cmp(&aw)
                })
                .then_with(|| a.name.cmp(&b.name))
        });

        Ok(Grammar {
            rules: self.rules,
            terminals: self.terminals,
            ignore: ignore_names,
            start: self.start,
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::grammar::{load_grammar, lower, SymbolKind};

    /// The review blocker: `__ANON_0` lexes as a *rule* name (a leading `__` is
    /// a rule token), and the counter-generated terminal for `/x/` used to take
    /// that same name. The lowerer interns both namespaces into one `by_name`
    /// table, so the shadow corrupted the id space — the rule resolved to the
    /// terminal's id in release builds (`intern.rs` guards it only with a
    /// `debug_assert`). The generated name must dodge rule names too.
    #[test]
    fn generated_terminal_skips_user_rule_name() {
        let g = load_grammar(
            "start: /x/ __ANON_0\n__ANON_0: \"y\"\n",
            &["start".to_string()],
            false,
            false,
        )
        .unwrap();
        // The literal skipped the rule-claimed name…
        assert!(!g.terminals.iter().any(|t| t.name == "__ANON_0"));
        assert!(g
            .terminals
            .iter()
            .any(|t| t.name == "__ANON_1" && t.pattern.as_regex_str() == "x"));
        // …so lowering interns `__ANON_0` as the rule it is.
        let compiled = lower(&g);
        let id = compiled.symbols.id("__ANON_0").unwrap();
        assert_eq!(compiled.symbols.kind(id), SymbolKind::NonTerminal);
    }

    /// The hint variant of the same route: the uppercase hint of a literal
    /// `"__anon_5"` is `__ANON_5`, which a user *rule* may already claim; the
    /// hint must be rejected (falling back to `__ANON_0`), not shadow the rule.
    #[test]
    fn hint_minted_terminal_skips_user_rule_name() {
        let g = load_grammar(
            "start: \"__anon_5\" __ANON_5\n__ANON_5: \"y\"\n",
            &["start".to_string()],
            false,
            false,
        )
        .unwrap();
        let lit = g
            .terminals
            .iter()
            .find(|t| t.pattern.as_regex_str() == "__anon_5")
            .unwrap();
        assert_ne!(lit.name, "__ANON_5", "hint must not shadow the user's rule");
        let compiled = lower(&g);
        let id = compiled.symbols.id("__ANON_5").unwrap();
        assert_eq!(compiled.symbols.kind(id), SymbolKind::NonTerminal);
    }

    /// `__anon_plus_0` is a valid *user* rule name; the `thing+` helper must not
    /// reuse it (pre-fix, both origins were named `__anon_plus_0`, silently
    /// merging two unrelated rules).
    #[test]
    fn generated_helper_rule_skips_user_taken_name() {
        let g = load_grammar(
            "start: thing+ __anon_plus_0\n__anon_plus_0: \"b\"\nthing: \"a\"\n",
            &["start".to_string()],
            false,
            false,
        )
        .unwrap();
        let user_named: Vec<_> = g
            .rules
            .iter()
            .filter(|r| r.origin.name == "__anon_plus_0")
            .collect();
        assert_eq!(
            user_named.len(),
            1,
            "only the user's rule may carry the user's name"
        );
        // The `+` helper exists under a fresh (skipped-forward) name.
        assert!(
            g.rules
                .iter()
                .any(|r| r.origin.name.starts_with("__anon_plus_")
                    && r.origin.name != "__anon_plus_0")
        );
    }

    /// A user cannot *define* a terminal named `__ANON_0` (a leading `__` lexes
    /// as a rule name), but an import alias can register one: `%import
    /// common.INT -> __ANON_0`. The inline `/x/` literal must not be interned
    /// under that taken name (pre-fix, two TerminalDefs shared it).
    #[test]
    fn generated_terminal_skips_import_alias_taken_name() {
        let g = load_grammar(
            "start: /x/\n%import common.INT -> __ANON_0\n",
            &["start".to_string()],
            false,
            false,
        )
        .unwrap();
        // The literal skipped to the next free generated name…
        assert!(g
            .terminals
            .iter()
            .any(|t| t.name == "__ANON_1" && t.pattern.as_regex_str() == "x"));
        // …so the unreferenced imported terminal prunes away cleanly instead of
        // surviving as a duplicate of the literal's definition.
        assert_eq!(
            g.terminals.iter().filter(|t| t.name == "__ANON_0").count(),
            0
        );
    }

    /// `__IGNORE_{n}` is the third generated-name family, reachable by the same
    /// import-alias route as `__ANON_{n}`: pre-fix, `%import common.WS ->
    /// __IGNORE_0` plus any `%ignore` left two TerminalDefs named `__IGNORE_0`,
    /// both surviving pruning (the ignore-name set keeps the name alive).
    #[test]
    fn generated_ignore_terminal_skips_import_alias_taken_name() {
        let g = load_grammar(
            "start: \"a\"\n%ignore \" \"\n%import common.WS -> __IGNORE_0\n",
            &["start".to_string()],
            false,
            false,
        )
        .unwrap();
        assert_eq!(g.ignore, vec!["__IGNORE_1".to_string()]);
        // The unreferenced imported terminal prunes away; no duplicate survives.
        assert_eq!(
            g.terminals
                .iter()
                .filter(|t| t.name == "__IGNORE_0")
                .count(),
            0
        );
    }

    /// A literal whose uppercase *hint* mints an `__ANON_n` lookalike must not
    /// collide with a later counter-generated name: `"__anon_5"` interns under
    /// the hint `__ANON_5`, so the counter has to skip 5 when it gets there.
    #[test]
    fn generated_terminal_skips_hint_minted_name() {
        let mut grammar = String::from("start: \"__anon_5\"");
        // Burn counters 0..5 with distinct regex literals, then add one more.
        for i in 0..6 {
            grammar.push_str(&format!(" /x{i}/"));
        }
        grammar.push('\n');
        let g = load_grammar(&grammar, &["start".to_string()], false, false).unwrap();
        let anon5: Vec<_> = g
            .terminals
            .iter()
            .filter(|t| t.name == "__ANON_5")
            .collect();
        assert_eq!(anon5.len(), 1, "terminal names must stay unique");
        assert_eq!(anon5[0].pattern.as_regex_str(), "__anon_5");
    }

    /// The body of every rule named `name`, as `(order, [symbol names])`, sorted by
    /// order — a compact shape for the EBNF-expansion structural assertions below.
    fn rule_bodies(g: &crate::grammar::Grammar, name: &str) -> Vec<(usize, Vec<String>)> {
        use crate::grammar::symbol::Symbol;
        let mut out: Vec<(usize, Vec<String>)> = g
            .rules
            .iter()
            .filter(|r| r.origin.name == name)
            .map(|r| {
                let syms = r
                    .expansion
                    .iter()
                    .map(|s| match s {
                        Symbol::Terminal(t) => t.name.clone(),
                        Symbol::NonTerminal(n) => n.name.clone(),
                    })
                    .collect();
                (r.order, syms)
            })
            .collect();
        out.sort_by_key(|(o, _)| *o);
        out
    }

    /// #91/#32 structural fix: a grouped repetition inlines the group's arms
    /// **directly** into the recurse rule — Python Lark's `EBNF_to_BNF`
    /// (`(A | WORD)+` → `_p: A | WORD | _p A | _p WORD`) — instead of nesting an
    /// `(A | WORD)` group helper under a single-symbol `_p: g | _p g`. This is the
    /// shape that removes the dynamic-lexer `dynamic_complete` segmentation reversal
    /// the old `sorted_families` split-point heuristic compensated for. (Grammar is
    /// the `parse:49` dynamic compliance case.)
    #[test]
    fn grouped_plus_inlines_arms_into_recurse_rule() {
        let g = load_grammar(
            "A.2: \"a\"\nWORD: (\"a\"..\"z\")+\nstart: (A | WORD)+\n",
            &["start".to_string()],
            true,
            false,
        )
        .unwrap();
        // No nested `(A | WORD)` group helper is materialized — the only generated
        // helper is the inlined recurse rule.
        assert!(
            !g.rules
                .iter()
                .any(|r| r.origin.name.starts_with("__anon_group")),
            "the group must be inlined into the recurse rule, not given a helper"
        );
        let plus_name = g
            .rules
            .iter()
            .map(|r| r.origin.name.clone())
            .find(|n| n.starts_with("__anon_plus"))
            .expect("a recurse helper exists");
        // `_p: A | WORD | _p A | _p WORD` — base arms first (orders 0,1), then the
        // recurse arms (orders 2,3), matching Python's `EBNF_to_BNF` order.
        assert_eq!(
            rule_bodies(&g, &plus_name),
            vec![
                (0, vec!["A".into()]),
                (1, vec!["WORD".into()]),
                (2, vec![plus_name.clone(), "A".into()]),
                (3, vec![plus_name.clone(), "WORD".into()]),
            ]
        );
    }

    /// `(A | B)*` distributes its empty case into the *parent* (Python's
    /// `SimplifyRule`: `start: _p | ε`) and reuses the same inlined recurse rule as
    /// `+` — there is no longer a `__star: __plus | ε` nullable wrapper.
    #[test]
    fn grouped_star_distributes_empty_into_parent_no_wrapper() {
        let g = load_grammar(
            "start: (A | B)*\nA: \"a\"\nB: \"b\"\n",
            &["start".to_string()],
            true,
            false,
        )
        .unwrap();
        assert!(
            !g.rules
                .iter()
                .any(|r| r.origin.name.starts_with("__anon_star")),
            "`*` must distribute into the parent, not keep a star wrapper"
        );
        let plus_name = g
            .rules
            .iter()
            .map(|r| r.origin.name.clone())
            .find(|n| n.starts_with("__anon_plus"))
            .expect("a recurse helper exists");
        // The parent carries both the present (`_p`) and the empty alternative.
        let starts = rule_bodies(&g, "start");
        assert!(starts.contains(&(0, vec![plus_name.clone()])));
        assert!(
            starts.iter().any(|(_, syms)| syms.is_empty()),
            "the empty case is distributed into the parent"
        );
    }
}
