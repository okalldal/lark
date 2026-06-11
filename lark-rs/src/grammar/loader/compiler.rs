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
use crate::error::GrammarError;
use crate::grammar::rule::{Rule, RuleOptions};
use crate::grammar::symbol::Symbol;
use crate::grammar::terminal::{Pattern, TerminalDef};
use crate::grammar::Grammar;
use std::collections::HashMap;
use std::path::PathBuf;

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
    /// Cache of the shared `+`-recurse helper (`P: inner | P inner`) keyed by its
    /// inner symbol and the keep-all context. Identical `x+`/`x*` occurrences reuse
    /// one rule — Python Lark's `rules_cache`. This sharing is what keeps grammars
    /// like `a+ b | a+` and `a* b | a+` LALR-parseable: with separate recurse rules
    /// the duplicated `… -> "a"` reductions are an unresolvable reduce/reduce.
    pub(super) recurse_cache: HashMap<(Symbol, bool), String>,
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
}

impl GrammarCompiler {
    pub(super) fn new(
        start: Vec<String>,
        maybe_placeholders: bool,
        keep_all_tokens: bool,
        base_path: Option<PathBuf>,
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
        }
    }

    pub(super) fn fresh_anon_rule(&mut self, tag: &str) -> String {
        let name = format!("__anon_{}_{}", tag, self.anon_counter);
        self.anon_counter += 1;
        name
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

    pub(super) fn fresh_terminal(&mut self) -> String {
        let name = format!("__ANON_{}", self.term_counter);
        self.term_counter += 1;
        name
    }

    pub(super) fn process_items(&mut self, items: Vec<Item>) -> Result<(), GrammarError> {
        // First pass: register templates
        for item in &items {
            if let Item::RuleItem(r) = item {
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

        // Add ignore terminals (one terminal per ignore pattern)
        let n_ignore = self.ignore_patterns.len();
        let ignore_names: Vec<String> = (0..n_ignore).map(|i| format!("__IGNORE_{}", i)).collect();
        for (i, pat) in self.ignore_patterns.into_iter().enumerate() {
            let name = format!("__IGNORE_{}", i);
            // `%ignore` tokens never reach the tree (the parse loop skips them), so
            // they need no per-occurrence filter — they appear in no rule body.
            self.terminals.push(TerminalDef::new(&name, pat, 0));
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
