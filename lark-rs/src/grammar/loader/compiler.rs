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
pub enum AnonKind {
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
    /// Audit mode (ADR-0013, RC7/#272): when set, [`recurse_helper`] keys its
    /// `recurse_cache` on the inner expression's **source-AST** structural key
    /// (Python Lark's `EBNF_to_BNF._add_recurse_rule`, which keys on the inner
    /// `expr` Tree) instead of the compiled arms. This reproduces Python's
    /// *un-shared* helper split — `r0*` and `(r0)*` get distinct helpers — so the
    /// post-lowering reduce/reduce audit can run the real LALR conflict detector
    /// over a Python-faithful shadow grammar and surface the collision the real
    /// (shared) grammar masks, **without** un-sharing the real `recurse_cache` (the
    /// sharing is load-bearing: un-sharing regresses the LALR bank 512→482).
    /// The shadow grammar is build-gating only; it never parses.
    pub(super) python_keyed_recurse: bool,
    /// Audit-only recurse-helper cache keyed on `(inner-AST key, keep_all)`,
    /// matching Python Lark's `EBNF_to_BNF.rules_cache` (keyed on the inner `expr`
    /// Tree). Populated only while [`python_keyed_recurse`](Self::python_keyed_recurse)
    /// is set, so it never affects the real (compiled-arms-keyed) `recurse_cache`.
    pub(super) recurse_cache_ast: HashMap<(String, bool), String>,
    /// The inner-AST key that first created each real `recurse_cache` entry, keyed
    /// by that entry's `(arms, keep_all)`. On a later cache *hit* with a **different**
    /// inner-AST key, the real (compiled-arms) sharing has collapsed two helpers
    /// Python Lark would have minted distinctly — exactly the RC7/#272 over-share.
    /// [`recurse_overshare_seen`](Self::recurse_overshare_seen) flips, telling the
    /// loader an audit shadow is worth building. Tracked only in the real pass.
    pub(super) recurse_cache_origin_key: HashMap<(Vec<super::ebnf::CompiledAlt>, bool), String>,
    /// Set in the real pass when a `recurse_cache` hit fuses two distinct inner-AST
    /// shapes into one helper (see [`recurse_cache_origin_key`](Self::recurse_cache_origin_key)).
    /// When `false`, the Python-keyed shadow is byte-identical to the real grammar's
    /// recurse helpers, so the loader skips building it (no audit needed).
    pub(super) recurse_overshare_seen: bool,
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
    /// Cache of the factored bounded-repeat sub-rules a large `x~mn..mx` (`mx ≥
    /// 50`, Python's `REPEAT_BREAK_THRESHOLD`) breaks into — Python Lark's
    /// `rules_cache` keyed on the `_add_repeat_rule`/`_add_repeat_opt_rule`
    /// arguments `(a, b, target, atom, opt)`. Sharing the sub-rules is what keeps
    /// the factored lowering O(log n) in grammar size: two `x~0..n` over the same
    /// `x` reuse the `x x x …` chunk rules instead of minting fresh ones.
    ///
    /// The key intentionally **omits `keep_all`**, exactly mirroring Python Lark's
    /// `EBNF_to_BNF.rules_cache` (`load_grammar.py`, keyed `(a, b, target, atom[,
    /// "opt"])` with no keep-all). Python's `EBNF_to_BNF` instance — and its cache —
    /// is shared across every rule, so the *first* rule to build a given chunk
    /// freezes its `rule_options` (keep-all and all) into the shared sub-rule, and a
    /// later sibling reuses it verbatim. This makes a `!a: "x"~50` next to a plain
    /// `b: "x"~50` share one chunk whose keep-all is whichever of `a`/`b` compiled
    /// first — an order-dependent quirk, but it is the oracle's quirk, so lark-rs
    /// reproduces it byte-for-byte (ADR-0017: a circumstantial leak that is *cheap*
    /// to match → match it). Pinned by `keep_all_repeat_chunk_sharing_matches_oracle`
    /// in `tests/test_repeat_factoring.rs`.
    pub(super) repeat_cache: HashMap<(usize, usize, String, String, bool), String>,
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
    /// Provenance of every generated anonymous EBNF helper rule, keyed by the name
    /// [`fresh_anon_rule`](Self::fresh_anon_rule) minted for it. This is the
    /// *source-provenance* discriminator the engine needs (#101): a nullable
    /// `Nt::Orig` that is a generated helper (`(B*)~2`'s `__anon_rep_*`) is
    /// accepted by CYK, but a user-written nullable rule (`_a: B?`, or a user rule
    /// the author *named* `__anon_star_0`) is rejected — exactly Python Lark's CYK
    /// behavior. The discriminator is whether the name was generated here, never
    /// the `__anon_` spelling (a user can author that exact name, #144), so it is
    /// recorded at mint time rather than sniffed downstream.
    ///
    /// `pub(super)` so the sibling `imports` module can carry an imported helper's
    /// provenance across `import_rule_closure`'s rename (#101 import path).
    pub(super) anon_kinds: HashMap<String, AnonKind>,
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
            python_keyed_recurse: false,
            recurse_cache_ast: HashMap::new(),
            recurse_cache_origin_key: HashMap::new(),
            recurse_overshare_seen: false,
            helper_cache: HashMap::new(),
            nullable_opts: std::collections::HashSet::new(),
            repeat_cache: HashMap::new(),
            base_path,
            import_sources,
            reserved_rule_names: HashSet::new(),
            anon_kinds: HashMap::new(),
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
                // Record the generated-helper provenance so the engine can tell a
                // generated nullable helper from a user rule by *source*, never by
                // the `__anon_` spelling (#101 / #144).
                self.anon_kinds.insert(name.clone(), kind);
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
                    // Register *plain* templates here; `%override`/`%extend` of a
                    // template are resolved (with their pre-existence gate) during
                    // the staging pass, so they must not pre-seed `self.templates`
                    // ahead of that gate.
                    if !r.params.is_empty() && r.directive == Directive::Plain {
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

        // `%import`s populate the unified definition namespace *before* any
        // statement runs in Python Lark (`load_grammar.py` resolves all imports,
        // then walks the statements). So an `%override`/`%extend` may target an
        // imported symbol regardless of where the directive sits — collect the
        // imported names up front, classified rule vs terminal by the leading-case
        // convention the loader uses everywhere, so the pre-existence gate below
        // sees them.
        //
        // These same sets are the *single-definition-per-origin* ledger (#270):
        // every plain definition (rule, terminal, `%declare`) records its origin
        // here as it is staged, and a second plain definition of an
        // already-defined name is rejected — matching Python's `_define`, which
        // raises `"{Type} '{name}' defined more than once"` when a statement names
        // a key already in `_definitions` (imports included). `%declare`s are
        // *not* pre-seeded: like every other statement they are processed in
        // document order, so two `%declare`s of one name collide just as Python's
        // two `_define(name, …, None)` calls do.
        let mut defined_rule_names: HashSet<String> = HashSet::new();
        let mut defined_term_names: HashSet<String> = HashSet::new();
        for item in &items {
            if let Item::ImportItem(spec) = item {
                for name in spec_final_names(spec) {
                    if Self::name_is_terminal(&name) {
                        defined_term_names.insert(name);
                    } else {
                        defined_rule_names.insert(name);
                    }
                }
            }
        }

        // Staged compilation. Terminals are resolved as a whole *before* rule bodies
        // so that (a) a string literal in a rule can unify with an already-known
        // terminal and (b) a terminal body may reference any other terminal,
        // regardless of definition order. Imports/declares run first so terminal
        // bodies can reference imported terminals.
        //
        // `%override` / `%extend` are resolved here, in document order, against the
        // running definition state (`defined_*_names`), matching Python Lark's
        // `_define(override=True)` / `_extend`: both require the target to
        // pre-exist (else a `GrammarError`); override *replaces* the prior body,
        // extend *prepends* new alternatives to it.
        let mut rule_items: Vec<RawRule> = Vec::new();
        let mut ignore_items = Vec::new();
        for item in items {
            match item {
                Item::ImportItem(spec) => self.resolve_import(spec)?,
                Item::DeclareItem(syms) => self.declare_terminals(syms, &mut defined_term_names)?,
                Item::TermItem(t) => {
                    self.stage_term_directive(t, &mut defined_term_names)?;
                }
                Item::RuleItem(r) if !r.params.is_empty() => {
                    // A parameterized rule is a template, instantiated on demand
                    // rather than compiled as a flat rule. The directive is
                    // resolved against `self.templates` (which the first pass
                    // pre-seeded only for the plain case).
                    self.stage_template_directive(r, &mut defined_rule_names)?;
                }
                Item::RuleItem(r) => {
                    self.stage_rule_directive(r, &mut rule_items, &mut defined_rule_names)?;
                }
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

    /// Resolve a rule definition's `%override` / `%extend` directive (or stage a
    /// plain definition), in document order, against the running set of defined
    /// rule names. Mirrors Python Lark's `_define(override=True)` / `_extend`
    /// (`load_grammar.py`): both directives require the target rule to pre-exist;
    /// override *replaces* its body, extend *prepends* new alternatives.
    fn stage_rule_directive(
        &mut self,
        r: RawRule,
        rule_items: &mut Vec<RawRule>,
        defined: &mut HashSet<String>,
    ) -> Result<(), GrammarError> {
        match r.directive {
            Directive::Plain => {
                // Single-definition-per-origin (#270): a plain rule whose name is
                // already defined (by an import or a prior rule) is a duplicate,
                // exactly as Python's `_define` rejects it. `%override`/`%extend`
                // carry a non-`Plain` directive and are handled below — they
                // *legitimately* redefine, so they must not trip this check.
                if !defined.insert(r.name.clone()) {
                    return Err(Self::duplicate_definition_error(false, &r.name));
                }
                rule_items.push(r);
            }
            Directive::Override => {
                if !defined.contains(&r.name) {
                    return Err(GrammarError::Other {
                        msg: format!("Cannot override a nonexisting rule {}", r.name),
                    });
                }
                // Replace the prior body outright: drop any same-grammar
                // alternatives collected so far and any already-imported rules at
                // this origin, then stage the override body. (Orphaned imported
                // helper rules prune away in `compile()` if nothing references
                // them.)
                rule_items.retain(|prev| prev.name != r.name);
                self.rules.retain(|rule| rule.origin.name != r.name);
                rule_items.push(r);
            }
            Directive::Extend => {
                if !defined.contains(&r.name) {
                    return Err(GrammarError::Other {
                        msg: format!("Can't extend rule {} as it wasn't defined before", r.name),
                    });
                }
                // Prepend the new alternatives to the existing definition. For a
                // same-grammar target, splice them onto the front of the staged
                // `RawRule` so they compile as one rule (Python's
                // `base.children.insert(0, exp)`). For an imported target, stage
                // them as an additional definition at the same origin.
                if let Some(existing) = rule_items.iter_mut().find(|prev| prev.name == r.name) {
                    let mut merged = r.expansions;
                    merged.append(&mut existing.expansions);
                    existing.expansions = merged;
                } else {
                    rule_items.push(RawRule {
                        directive: Directive::Plain,
                        ..r
                    });
                }
            }
        }
        Ok(())
    }

    /// Resolve a terminal definition's `%override` / `%extend` directive (or stage
    /// a plain definition), in document order, against the running set of defined
    /// terminal names. Terminal sibling of [`stage_rule_directive`].
    fn stage_term_directive(
        &mut self,
        t: RawTerm,
        defined: &mut HashSet<String>,
    ) -> Result<(), GrammarError> {
        match t.directive {
            Directive::Plain => {
                // Single-definition-per-origin (#270): a plain terminal whose name
                // is already defined (by an import, a `%declare`, or a prior
                // terminal) is a duplicate — Python's `_define` rejects it. This is
                // the RC2/RC2b fix site: an imported `INT` then re-`%declare`d or
                // locally redefined now collides instead of silently keeping one.
                if !defined.insert(t.name.clone()) {
                    return Err(Self::duplicate_definition_error(true, &t.name));
                }
                self.raw_terms.push(t);
            }
            Directive::Override => {
                if !defined.contains(&t.name) {
                    return Err(GrammarError::Other {
                        msg: format!("Cannot override a nonexisting terminal {}", t.name),
                    });
                }
                // Replace any prior same-grammar body and any already-imported
                // terminal at this name, then stage the override body.
                self.raw_terms.retain(|prev| prev.name != t.name);
                self.terminals.retain(|td| td.name != t.name);
                self.raw_terms.push(t);
            }
            Directive::Extend => {
                if !defined.contains(&t.name) {
                    return Err(GrammarError::Other {
                        msg: format!(
                            "Can't extend terminal {} as it wasn't defined before",
                            t.name
                        ),
                    });
                }
                // Prepend the new alternatives to the existing terminal. A
                // same-grammar terminal is still a `RawTerm` here (terminals
                // resolve as a whole later), so splice onto its front.
                //
                // KNOWN GAP (#286): an *imported* terminal has already been
                // compiled into `self.terminals` (not `raw_terms`), and
                // `resolve_terminals` skips a `RawTerm` whose name is already a
                // resolved terminal — so a staged extend body for an imported
                // terminal would be silently dropped. Rather than drop it, we leave
                // the imported terminal unchanged; the divergence is pinned as an
                // XFAIL (`n1_extend_imported_terminal_*`) and tracked in #286.
                if let Some(existing) = self.raw_terms.iter_mut().find(|prev| prev.name == t.name) {
                    let mut merged = t.expansions;
                    merged.append(&mut existing.expansions);
                    existing.expansions = merged;
                }
            }
        }
        Ok(())
    }

    /// Resolve a parameterized rule (template) definition's `%override` /
    /// `%extend` directive (or register a plain template), against the running set
    /// of defined rule names. A template lives in `self.templates` and is
    /// instantiated on demand, so override *replaces* its tuple and extend
    /// *prepends* alternatives there — never as a flat rule (whose body would try
    /// to compile the template's parameters as ordinary symbols). Mirrors Python
    /// Lark, which keys templates in the same `_definitions` map as plain rules.
    fn stage_template_directive(
        &mut self,
        r: RawRule,
        defined: &mut HashSet<String>,
    ) -> Result<(), GrammarError> {
        match r.directive {
            Directive::Plain => {
                // The first pass already registered the plain template; just
                // record it as defined for any later directive's gate. A template
                // shares the rule namespace (Python keys it in `_definitions`), so
                // a duplicate template — or a template colliding with a plain rule
                // of the same name — is rejected like any other rule (#270).
                if !defined.insert(r.name.clone()) {
                    return Err(Self::duplicate_definition_error(false, &r.name));
                }
            }
            Directive::Override => {
                if !defined.contains(&r.name) {
                    return Err(GrammarError::Other {
                        msg: format!("Cannot override a nonexisting rule {}", r.name),
                    });
                }
                self.templates.insert(
                    r.name.clone(),
                    (r.params, r.expansions, r.modifiers, r.priority),
                );
            }
            Directive::Extend => {
                if !defined.contains(&r.name) {
                    return Err(GrammarError::Other {
                        msg: format!("Can't extend rule {} as it wasn't defined before", r.name),
                    });
                }
                // Prepend the new alternatives to the existing template body
                // (Python's `base.children.insert(0, exp)`). The target is
                // guaranteed registered: a plain template seeded `self.templates`
                // in the first pass, and a prior override re-inserted it.
                if let Some(entry) = self.templates.get_mut(&r.name) {
                    let mut merged = r.expansions;
                    merged.append(&mut entry.1);
                    entry.1 = merged;
                }
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
    /// `_DEDENT`. A `%declare` is a definition like any other (Python's
    /// `_define(name, is_term, None)`): declaring a name already defined — by an
    /// import, a prior `%declare`, or a local terminal — is rejected as a
    /// duplicate (#270).
    fn declare_terminals(
        &mut self,
        syms: Vec<Symbol>,
        defined: &mut HashSet<String>,
    ) -> Result<(), GrammarError> {
        for sym in syms {
            if let Symbol::Terminal(t) = sym {
                if !defined.insert(t.name.clone()) {
                    return Err(Self::duplicate_definition_error(true, &t.name));
                }
                if !self.terminals.iter().any(|td| td.name == t.name) {
                    self.terminals.push(TerminalDef::declared(&t.name));
                }
            }
        }
        Ok(())
    }

    /// Whether `name` is a terminal name under Lark's lexical convention: a
    /// terminal is all-uppercase (with `_`/digits), a rule is lowercase — so a
    /// leading `_` alone does not decide it (`_INDENT` is a terminal, `_expr` a
    /// rule). Matches the tokenizer's `Terminal` vs `Rule` dispatch
    /// (`tokenizer.rs`): the presence of any lowercase letter is the discriminator.
    /// Used to bucket an `%import` target into the right single-definition ledger
    /// so an imported `_INDENT` then re-`%declare`d collides as the oracle does.
    fn name_is_terminal(name: &str) -> bool {
        !name.chars().any(|c| c.is_ascii_lowercase())
    }

    /// Python Lark's `"{Type} '{name}' defined more than once"` (`_define`,
    /// `load_grammar.py`), raised when a plain rule / terminal / `%declare`
    /// definition names an origin already defined. `is_term` picks the `Terminal`
    /// vs `Rule` wording, matching the oracle's exact message (RC1/RC2, #270).
    fn duplicate_definition_error(is_term: bool, name: &str) -> GrammarError {
        let kind = if is_term { "Terminal" } else { "Rule" };
        GrammarError::Other {
            msg: format!("{kind} '{name}' defined more than once"),
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

        // Sort terminals by (priority desc, max_width desc, raw_value_len desc,
        // name asc) — the same total order the lexer plan uses
        // (`lexer/plan.rs::sort_terminals`, Python `lark/lexer.py:583`). This sort
        // feeds SymbolId assignment, so keeping the two in lockstep means the raw
        // pattern-length tiebreak (#268, N2: flags stored separately, not baked into
        // the length) can never diverge between interning order and lexer order.
        self.terminals.sort_by(|a, b| {
            b.priority
                .cmp(&a.priority)
                .then_with(|| {
                    let bw = b.pattern.max_width().unwrap_or(usize::MAX);
                    let aw = a.pattern.max_width().unwrap_or(usize::MAX);
                    bw.cmp(&aw)
                })
                .then_with(|| b.pattern.raw_value_len().cmp(&a.pattern.raw_value_len()))
                .then_with(|| a.name.cmp(&b.name))
        });

        Ok(Grammar {
            rules: self.rules,
            terminals: self.terminals,
            ignore: ignore_names,
            start: self.start,
            anon_kinds: self.anon_kinds,
            lalr_audit: None,
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

    // ── `%override` / `%extend` directive semantics (N1, #269) ──────────────────

    fn load(g: &str) -> Result<crate::grammar::Grammar, crate::error::GrammarError> {
        load_grammar(g, &["start".to_string()], false, false)
    }

    /// `%override start: B` *replaces* the prior `start` body — the grammar is
    /// `start: B`, not the merged `start: A | B` lark-rs used to build (N1a). The
    /// directive previously never reached the compiler.
    #[test]
    fn override_replaces_rule_body() {
        let g = load("start: A\n%override start: B\nA: \"a\"\nB: \"b\"\n").unwrap();
        let bodies = rule_bodies(&g, "start");
        assert_eq!(
            bodies,
            vec![(0, vec!["B".to_string()])],
            "override must replace `start` with B, not merge to `A | B`"
        );
    }

    /// `%extend start: B` *prepends* the new alternative to the existing body, so
    /// `start: B | A` (both kept). Python's `base.children.insert(0, exp)`.
    #[test]
    fn extend_prepends_rule_alternatives() {
        let g = load("start: A\n%extend start: B\nA: \"a\"\nB: \"b\"\n").unwrap();
        let bodies = rule_bodies(&g, "start");
        assert_eq!(
            bodies,
            vec![(0, vec!["B".to_string()]), (1, vec!["A".to_string()]),],
            "extend must prepend B ahead of the original A"
        );
    }

    /// `%override` of a rule that was never defined is rejected at load, with
    /// Python Lark's exact message (N1b).
    #[test]
    fn override_nonexisting_rule_rejected() {
        let err = load("%override foo: A\nstart: A\nA: \"a\"\n").unwrap_err();
        assert!(
            err.to_string()
                .contains("Cannot override a nonexisting rule foo"),
            "got: {err}"
        );
    }

    /// `%extend` of a rule that was never defined is rejected at load, with
    /// Python Lark's exact message (N1c).
    #[test]
    fn extend_nonexisting_rule_rejected() {
        let err = load("%extend foo: A\nstart: A\nA: \"a\"\n").unwrap_err();
        assert!(
            err.to_string()
                .contains("Can't extend rule foo as it wasn't defined before"),
            "got: {err}"
        );
    }

    /// A forward reference does not satisfy pre-existence: `%override start` *before*
    /// `start` is defined is rejected, exactly as Python (definitions are processed
    /// in document order, imports excepted).
    #[test]
    fn override_forward_reference_rejected() {
        let err = load("%override start: B\nstart: A\nA: \"a\"\nB: \"b\"\n").unwrap_err();
        assert!(
            err.to_string()
                .contains("Cannot override a nonexisting rule start"),
            "got: {err}"
        );
    }

    /// Directives are namespace-aware: `%override FOO` (terminal) does not see a
    /// rule `foo`, so it is a nonexisting *terminal* — Python's behavior.
    #[test]
    fn override_kind_mismatch_rejected() {
        let err = load("start: foo\nfoo: \"a\"\n%override FOO: \"b\"\n").unwrap_err();
        assert!(
            err.to_string()
                .contains("Cannot override a nonexisting terminal FOO"),
            "got: {err}"
        );
    }

    /// Terminal directives work too: `%override A: "b"` replaces terminal `A`'s
    /// body, so the old `"a"` is gone.
    #[test]
    fn override_replaces_terminal_body() {
        let g = load("A: \"a\"\n%override A: \"b\"\nstart: A\n").unwrap();
        let a = g
            .terminals
            .iter()
            .find(|t| t.name == "A")
            .expect("terminal A survives");
        assert_eq!(a.pattern.as_regex_str(), "b");
    }

    /// `%override` of an imported terminal replaces the imported body (the import
    /// runs first in Python, then the override wins).
    #[test]
    fn override_imported_terminal() {
        let g = load("%import common.INT\nstart: INT\n%override INT: \"z\"\n").unwrap();
        let int = g
            .terminals
            .iter()
            .find(|t| t.name == "INT")
            .expect("INT survives");
        assert_eq!(int.pattern.as_regex_str(), "z");
    }

    // ── Single-definition-per-origin (#270, bounty RC1/RC2) ─────────────────────

    /// RC1: a rule defined twice is rejected with Python's exact message, instead
    /// of silently merging the two bodies into `a: "x" | "y"`.
    #[test]
    fn duplicate_rule_definition_rejected() {
        let err = load("start: a\na: \"x\"\na: \"y\"\n").unwrap_err();
        assert!(
            err.to_string().contains("Rule 'a' defined more than once"),
            "got: {err}"
        );
    }

    /// RC2: an imported terminal then re-`%declare`d collides — `Terminal 'INT'
    /// defined more than once` — instead of keeping one definition silently.
    #[test]
    fn duplicate_terminal_import_then_declare_rejected() {
        let err = load("%import common.INT\n%declare INT\nstart: INT\n").unwrap_err();
        assert!(
            err.to_string()
                .contains("Terminal 'INT' defined more than once"),
            "got: {err}"
        );
    }

    /// RC2b: an imported terminal then redefined locally collides too, order-
    /// independent (the import populates the namespace before the local term).
    #[test]
    fn duplicate_terminal_import_then_local_rejected() {
        let err = load("%import common.INT\nINT: \"x\"\nstart: INT\n").unwrap_err();
        assert!(
            err.to_string()
                .contains("Terminal 'INT' defined more than once"),
            "got: {err}"
        );
    }

    /// Two `%declare`s of one name collide — Python processes each as a
    /// `_define(name, …, None)`, so the second is a duplicate.
    #[test]
    fn duplicate_declare_rejected() {
        let err = load("%declare FOO\n%declare FOO\nstart: FOO\n").unwrap_err();
        assert!(
            err.to_string()
                .contains("Terminal 'FOO' defined more than once"),
            "got: {err}"
        );
    }

    /// A template shares the rule namespace: a second plain template of one name
    /// is a duplicate, like a plain rule (`Rule 'foo' defined more than once`).
    #[test]
    fn duplicate_template_rejected() {
        let err = load("start: foo{A}\nfoo{x}: x\nfoo{x}: x x\nA: \"a\"\n").unwrap_err();
        assert!(
            err.to_string()
                .contains("Rule 'foo' defined more than once"),
            "got: {err}"
        );
    }

    /// A rule and a same-named template collide (both occupy the rule namespace),
    /// regardless of which comes first.
    #[test]
    fn rule_and_template_same_name_rejected() {
        let err = load("start: foo\nfoo: \"z\"\nfoo{x}: x\n").unwrap_err();
        assert!(
            err.to_string()
                .contains("Rule 'foo' defined more than once"),
            "got: {err}"
        );
    }

    /// Not a duplicate: one definition split across `|` arms is a single origin
    /// (Python accepts `a: "x" | "y"`). The single-definition check must not fire.
    #[test]
    fn single_definition_with_alternatives_accepted() {
        let g = load("start: a\na: \"x\" | \"y\"\n").unwrap();
        let bodies = rule_bodies(&g, "a");
        assert_eq!(bodies.len(), 2, "both arms of the one definition survive");
    }

    /// Not a duplicate: re-importing the *same* symbol is idempotent in Python
    /// (`imports` dedups by dotted-path + alias), so it must still build.
    #[test]
    fn repeated_identical_import_accepted() {
        let g = load("%import common.INT\n%import common.INT\nstart: INT\n").unwrap();
        assert!(g.terminals.iter().any(|t| t.name == "INT"));
    }

    /// A leading-underscore terminal (`_INDENT`) imported then re-`%declare`d
    /// collides too: the import ledger classifies a name by Lark's lexical
    /// convention (no lowercase = terminal), not a bare leading-case check, so
    /// `_INDENT` lands in the terminal namespace where the `%declare` can see it.
    #[test]
    fn duplicate_underscore_terminal_import_then_declare_rejected() {
        let err = load("%import python._INDENT\n%declare _INDENT\nstart: _INDENT\n").unwrap_err();
        assert!(
            err.to_string()
                .contains("Terminal '_INDENT' defined more than once"),
            "got: {err}"
        );
    }

    /// A transparent (`_`-prefixed) *rule* is correctly NOT bucketed as a terminal
    /// — `name_is_terminal` keys on lowercase letters, so `_w` stays a rule and a
    /// single definition of it still builds.
    #[test]
    fn underscore_rule_is_not_misclassified_as_terminal() {
        assert!(load("start: _w\n_w: \"x\"\n").is_ok());
    }

    /// A legitimate `%override` / `%extend` redefines its target and must *not* be
    /// caught by the duplicate-definition check (it carries a non-`Plain`
    /// directive, #269).
    #[test]
    fn override_and_extend_not_treated_as_duplicates() {
        assert!(load("start: A\n%override start: B\nA: \"a\"\nB: \"b\"\n").is_ok());
        assert!(load("start: A\n%extend start: B\nA: \"a\"\nB: \"b\"\n").is_ok());
    }
}
