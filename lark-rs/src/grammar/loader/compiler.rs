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
use super::imports::{spec_final_names, split_import_directive};
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

/// The single auditable home of every generated-name decision — the #101/#144
/// namespace invariants in one place. Owns the two monotonic counters
/// (`__anon_{tag}_{n}` rules, `__ANON_{n}` terminals), the literal→terminal-name
/// memo, the up-front user-name reservations in *both* namespaces, and the
/// generated-helper provenance map (`anon_kinds`).
///
/// **Mint-time provenance (#101/ADR-0024, #144).** [`fresh_anon_rule`](NameMinter::fresh_anon_rule)
/// records each helper's [`AnonKind`] into `anon_kinds` **at mint time** — never
/// re-derived downstream from the `__anon_` spelling (a user can author that exact
/// name). [`anon_terminal_name_free`](NameMinter::anon_terminal_name_free) checks
/// **both** namespaces (terminal *and* rule reservations) plus the live output
/// vectors, which the caller passes in (the minter never holds the output vectors,
/// per #480's seam note).
pub(super) struct NameMinter {
    /// Counter for generating unique anonymous rule names.
    pub(super) anon_counter: usize,
    /// Counter for generating unique terminal names for literals.
    pub(super) term_counter: usize,
    /// Cache: literal string/regex → auto-generated terminal name.
    pub(super) literal_cache: HashMap<String, String>,
    /// User-authored rule names (rules, templates, import targets), collected up
    /// front so [`fresh_anon_rule`](Self::fresh_anon_rule) never hands out a name
    /// the grammar already claims — `__anon_group_0` is a *valid* user rule name,
    /// and a generated duplicate would silently merge two unrelated origins.
    /// Generated names never collide with each other (one monotonic counter), and
    /// import-mangled dependencies (`mod__name` / `_mod__name`) cannot take the
    /// `__anon_{tag}_{n}` shape, so user-authored names are the only hazard.
    ///
    /// This set is deliberately *over-inclusive* for the anon-name guard: it reserves
    /// **every** import final name from [`spec_final_names`], including a non-surviving
    /// last-alias-wins binding (#388). That is harmless for dodging generated names,
    /// but it is **not** the right discriminator for the #428 user-vs-import-origin
    /// collision — see [`GrammarCompiler::claimed_rule_names`].
    pub(super) reserved_rule_names: HashSet<String>,
    /// User-authored terminal names (terminals, declares, import targets), the
    /// same guard for [`fresh_terminal`](Self::fresh_terminal)'s `__ANON_{n}`.
    /// Unlike rules, generated terminal names must *also* dodge live state: a
    /// literal `"__anon_5"` interns under the hint `__ANON_5` (its uppercase
    /// form), which no up-front scan can see.
    pub(super) reserved_term_names: HashSet<String>,
    /// Provenance of every generated anonymous EBNF helper rule, keyed by the name
    /// [`fresh_anon_rule`](Self::fresh_anon_rule) minted for it. This is the
    /// *source-provenance* discriminator the engine needs (#101): a nullable
    /// `Nt::Orig` that is a generated helper (`(B*)~2`'s `__anon_rep_*`) is
    /// accepted by CYK, but a user-written nullable rule (`_a: B?`, or a user rule
    /// the author *named* `__anon_star_0`) is rejected — exactly Python Lark's CYK
    /// behavior. The discriminator is whether the name was generated here, never
    /// the `__anon_` spelling (a user can author that exact name, #144), so it is
    /// recorded at mint time rather than sniffed downstream.
    pub(super) anon_kinds: HashMap<String, AnonKind>,
}

impl NameMinter {
    fn new() -> Self {
        NameMinter {
            anon_counter: 0,
            term_counter: 0,
            literal_cache: HashMap::new(),
            reserved_rule_names: HashSet::new(),
            reserved_term_names: HashSet::new(),
            anon_kinds: HashMap::new(),
        }
    }

    /// A fresh `__anon_{tag}_{n}` helper-rule name, skipping any name the user's
    /// grammar already claims (see [`reserved_rule_names`](Self::reserved_rule_names)).
    fn fresh_anon_rule(&mut self, kind: AnonKind) -> String {
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

    /// Whether `name` is free to assign to an anonymous (generated or
    /// hint-named) terminal. Checks **both** namespaces, not just terminals:
    /// the lowerer interns every symbol into one `by_name` table, so a terminal
    /// that shadows a *rule* name corrupts the id space — `intern_nonterminal`
    /// would hand back the terminal's id (guarded only by a `debug_assert` in
    /// release builds). `__ANON_0` is a valid user *rule* name (a leading `__`
    /// lexes as a rule token), so the rule namespace is reachable. Reservations
    /// cover user-authored names known up front; the live lists (`terminals` /
    /// `rules`, passed in by the caller, since the minter never holds them) are
    /// the defensive backstop for names minted mid-compile (uppercase literal
    /// hints) and anything reservation cannot see.
    fn anon_terminal_name_free(
        &self,
        name: &str,
        terminals: &[TerminalDef],
        rules: &[Rule],
    ) -> bool {
        !self.reserved_term_names.contains(name)
            && !self.reserved_rule_names.contains(name)
            && !terminals.iter().any(|t| t.name == name)
            && !rules.iter().any(|r| r.origin.name == name)
    }

    /// A fresh `__ANON_{n}` terminal name, skipping names the user's grammar
    /// claims in either namespace (see
    /// [`anon_terminal_name_free`](Self::anon_terminal_name_free)).
    fn fresh_terminal(&mut self, terminals: &[TerminalDef], rules: &[Rule]) -> String {
        loop {
            let name = format!("__ANON_{}", self.term_counter);
            self.term_counter += 1;
            if self.anon_terminal_name_free(&name, terminals, rules) {
                return name;
            }
        }
    }

    /// Whether a literal's human-readable name *hint* (`","` → `COMMA`,
    /// `"kw"` → `KW`) may be used as the terminal's name. Same availability
    /// rule as a generated name (a hint like `__ANON_5` — the uppercase form of
    /// `"__anon_5"` — must dodge both namespaces too); on rejection the caller
    /// falls back to [`fresh_terminal`](Self::fresh_terminal).
    fn hint_name_free(&self, name: &str, terminals: &[TerminalDef], rules: &[Rule]) -> bool {
        self.anon_terminal_name_free(name, terminals, rules)
    }
}

/// Converts the parsed AST into flat BNF rules and terminal definitions.
/// One resolved `%ignore` directive (see [`GrammarCompiler::ignore_patterns`]).
pub(super) enum IgnoreEntry {
    /// `%ignore NAME` — a single reference to an already-named terminal. The
    /// terminal is added to the ignore set as-is, **preserving its declared
    /// priority**; no new terminal is synthesized (Python `_ignore`).
    Named(String),
    /// `%ignore <inline pattern>` — synthesizes a fresh `__IGNORE_n` terminal
    /// (priority 0) from the pattern, as both engines do for the inline form.
    Pattern(Pattern),
}

pub(super) struct GrammarCompiler {
    pub(super) start: Vec<String>,
    pub(super) rules: Vec<Rule>,
    pub(super) terminals: Vec<TerminalDef>,
    /// Raw terminal definitions, collected before any are compiled so a terminal
    /// body may reference another terminal defined later (`C: "C" | D`).
    pub(super) raw_terms: Vec<RawTerm>,
    /// `%extend` bodies targeting a terminal that is **already a compiled
    /// `TerminalDef`** (an import or `%declare`) by the time the directive is
    /// staged — there is no `RawTerm` to splice onto, and reconstructing one from a
    /// baked regex is lossy (#286). Each entry is `(name, new alternatives)` in
    /// document order; [`resolve_terminals`](GrammarCompiler::resolve_terminals)
    /// prepends them onto the resolved terminal's regex before baking, matching
    /// Python's `_extend` (`base.children.insert(0, exp)` on the still-AST
    /// definition tree). A same-grammar `%extend` (whose target is still a
    /// `RawTerm`) never lands here — it splices in [`stage_term_directive`].
    pub(super) pending_term_extends: Vec<(String, Vec<AliasedExpansion>)>,
    /// `%extend` of an *imported interior* rule origin (#442), deferred so the
    /// prepend ordering is applied *after* the extend body is compiled. Python's
    /// `_extend` prepends: the new alternatives must sort strictly ahead of every
    /// pre-existing (imported) alternative at that origin. The extend body is staged
    /// as a fresh `RawRule`, whose compiled BNF alternatives number from 0; once we
    /// know that count `k`, every pre-existing alternative is shifted up by `k`,
    /// leaving the extend's `0..k` strictly first. Each entry snapshots the
    /// pre-existing (imported) alternatives at stage time (by value), so the shift
    /// can identify exactly which compiled rules to move — a real invariant rather
    /// than the old fixed `EXTEND_ORDER_OFFSET` constant, which could collide once an
    /// extend produced ≥1_000_000 alternatives. See [`stage_rule_directive`] /
    /// [`apply_pending_interior_extends`](GrammarCompiler::apply_pending_interior_extends).
    pub(super) pending_interior_extends: Vec<(String, Vec<Rule>)>,
    /// `%ignore` directives, in document order. A directive that is a single
    /// reference to a named terminal records that terminal's name
    /// ([`IgnoreEntry::Named`]) so it is marked ignored with its **declared
    /// priority** preserved, exactly as Python's `_ignore` short-circuits
    /// (`load_grammar.py`, "Keep terminal name, no need to create a new
    /// definition"); any other (inline) directive carries a synthesized
    /// [`IgnoreEntry::Pattern`] that mints a fresh `__IGNORE_n` terminal.
    pub(super) ignore_patterns: Vec<IgnoreEntry>,
    /// The single auditable home of every generated-name decision (#101/#144):
    /// the `__anon_*` / `__ANON_*` counters, the literal→name memo, the up-front
    /// user-name reservations in both namespaces, and the generated-helper
    /// provenance map. See [`NameMinter`].
    pub(super) minter: NameMinter,
    /// Template definitions: name → (params, expansions, modifiers, priority).
    /// The modifiers (`!` keep-all, `?` expand1) and priority are kept so each
    /// instantiation inherits the template's rule options, exactly as Python Lark
    /// deep-copies the template's `RuleOptions` onto every instance.
    pub(super) templates: HashMap<String, (Vec<String>, Vec<AliasedExpansion>, String, i64)>,
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
    /// Rule names the importing grammar will **actually define as a distinct origin**:
    /// every user-authored rule/template name, plus every import final name that
    /// *survives* last-alias-wins (#388). This is the precise discriminator for the
    /// #428 user-rule-vs-mangled-interior-import-origin collision: a prefix-mangled
    /// interior origin (`python__name`) that lands on a name in this set genuinely
    /// collides (Python's `Rule '…' defined more than once`), whereas one landing on a
    /// *dropped* alias's name does **not** (Python builds it — the dropped alias is
    /// never defined). Built in the first pass, after the per-module alias map is
    /// complete, so it is populated independently of the user-rule-vs-`%import`
    /// document order.
    ///
    /// `pub(super)` so the sibling `imports` module reads it from `import_rule_closure`.
    pub(super) claimed_rule_names: HashSet<String>,
    /// Rule names that an `%override` / `%extend` directive targets (#442). A
    /// directive legitimately *redefines* its target, so such a name must be
    /// excluded from the #428 user-rule-vs-mangled-interior-import-origin collision
    /// guard (`import_rule_closure`): `%override python__name` beside `%import
    /// python (decorator)` must let the interior `python__name` origin be **copied**
    /// (so the override has something to replace / the extend has something to
    /// prepend to), exactly as Python — which resolves the import into
    /// `_definitions` first, then applies `_define(override=True)` / `_extend` on
    /// the now-present key. Unlike a plain user rule (which is in
    /// [`claimed_rule_names`](Self::claimed_rule_names) and *does* collide, #428), an
    /// override/extend target is deliberately kept out of that set.
    ///
    /// `pub(super)` so the sibling `imports` module reads it from `import_rule_closure`.
    pub(super) override_extend_rule_targets: HashSet<String>,
    /// Per-module merged import-alias map, keyed by the resolved module path
    /// (e.g. `["python"]`), mapping each *independently imported* original name
    /// to its registered (aliased) final name. Mirrors Python Lark's per-dotted-
    /// path `aliases` dict (`load_grammar.py`: imports of the same path are merged
    /// before `_get_mangle(prefix, aliases)` runs, #343). When `import_rule_closure`
    /// copies a rule's dependency closure, any closure symbol that is *also* an
    /// independent import of the same module is left **unmangled** under its final
    /// name instead of prefix-mangled — matching Python's `if s in aliases`.
    /// Built up front (first pass) so a later `%import` directive's targets are
    /// already known when an earlier directive's closure is copied.
    pub(super) import_alias_map: HashMap<Vec<String>, HashMap<String, String>>,
    /// Renamed origins already copied by a previous `import_rule_closure` call
    /// (#372). Two rules independently imported from the same module can have
    /// overlapping interior closures; the shared interior origin must be copied
    /// **once**, or the duplicate origin is a spurious reduce/reduce the build
    /// rejects. `import_rule_closure` skips an interior origin already in this set
    /// — scoped to *import-copied* origins only (never a user-authored rule of the
    /// same name), so a genuine collision between a user rule and a mangled import
    /// origin is still rejected, exactly as Python's "defined more than once".
    pub(super) imported_origins: HashSet<String>,
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
            pending_term_extends: Vec::new(),
            pending_interior_extends: Vec::new(),
            ignore_patterns: Vec::new(),
            minter: NameMinter::new(),
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
            claimed_rule_names: HashSet::new(),
            override_extend_rule_targets: HashSet::new(),
            import_alias_map: HashMap::new(),
            imported_origins: HashSet::new(),
        }
    }

    /// A fresh `__anon_{tag}_{n}` helper-rule name (delegates to the
    /// [`NameMinter`]). The mint-time `anon_kinds` recording lives in the minter —
    /// the #101/#144 invariant is preserved there.
    pub(super) fn fresh_anon_rule(&mut self, kind: AnonKind) -> String {
        self.minter.fresh_anon_rule(kind)
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

    /// Whether `name` is free to assign to an anonymous (generated or hint-named)
    /// terminal (delegates to the [`NameMinter`], handing it the live output
    /// vectors per #480's seam note). The dual-namespace + live-vector check lives
    /// in [`NameMinter::anon_terminal_name_free`].
    fn anon_terminal_name_free(&self, name: &str) -> bool {
        self.minter
            .anon_terminal_name_free(name, &self.terminals, &self.rules)
    }

    /// A fresh `__ANON_{n}` terminal name (delegates to the [`NameMinter`], handing
    /// it the live output vectors). Skips names the user's grammar claims in either
    /// namespace.
    pub(super) fn fresh_terminal(&mut self) -> String {
        self.minter.fresh_terminal(&self.terminals, &self.rules)
    }

    /// Whether a literal's human-readable name *hint* (`","` → `COMMA`,
    /// `"kw"` → `KW`) may be used as the terminal's name (delegates to the
    /// [`NameMinter`], handing it the live output vectors). Same availability rule
    /// as a generated name; on rejection the caller falls back to
    /// [`fresh_terminal`](Self::fresh_terminal).
    pub(super) fn hint_name_free(&self, name: &str) -> bool {
        self.minter
            .hint_name_free(name, &self.terminals, &self.rules)
    }

    /// Whether `(module, original) -> final_name` is the **surviving** alias for
    /// that import source under last-alias-wins (#388).
    ///
    /// Python merges every `%import` of one dotted path into a single `aliases`
    /// dict via `import_aliases.update(aliases)` (`load_grammar.py`), which keeps
    /// only the **last** `original -> final` binding. So `%import common.INT -> X`
    /// followed by `%import common.INT -> Y` defines **only** `Y`; the earlier `X`
    /// is dropped and never registered (verified against Python Lark 1.3.1, where
    /// `start: X` then rejects `Rule 'X' used but not defined`). This is *not* a
    /// "defined more than once" collision (that error is for two **different**
    /// sources landing on one final name — #299, which still rejects).
    ///
    /// The per-module merged `import_alias_map` is already keyed by `original` and
    /// already keeps the last final name (its first pass `insert`s in document
    /// order), so it *is* the surviving-alias map: a directive's `final_name`
    /// survives iff it equals the merged map's entry for `(module, original)`.
    ///
    /// Name-list imports (`%import common (INT, FLOAT)`) register `original ==
    /// final`, so they are always their own survivors. A module absent from the
    /// map (no recorded alias) cannot have a dropped alias, so it survives too.
    pub(super) fn alias_survives(
        &self,
        module: &[String],
        original: &str,
        final_name: &str,
    ) -> bool {
        match self
            .import_alias_map
            .get(module)
            .and_then(|m| m.get(original))
        {
            Some(surviving) => surviving == final_name,
            None => true,
        }
    }

    /// Import-vs-import collision pre-pass + ledger seeding, the second
    /// load-bearing step of [`process_items`](Self::process_items)'s staging order
    /// (it runs after the per-module alias map's first pass, which it reads via
    /// [`alias_survives`](Self::alias_survives)). Returns the
    /// `(defined_rule_names, defined_term_names)` ledger seeded with **only the
    /// surviving** import final names.
    ///
    /// `%import`s populate the unified definition namespace *before* any statement
    /// runs in Python Lark (`load_grammar.py` resolves all imports, then walks the
    /// statements). So an `%override`/`%extend` may target an imported symbol
    /// regardless of where the directive sits — the returned sets collect the
    /// imported names up front, classified rule vs terminal by the leading-case
    /// convention the loader uses everywhere, so the pre-existence gate sees them.
    ///
    /// These same sets are the *single-definition-per-origin* ledger (#270): every
    /// plain definition (rule, terminal, `%declare`) records its origin here as it
    /// is staged, and a second plain definition of an already-defined name is
    /// rejected — matching Python's `_define`, which raises `"{Type} '{name}'
    /// defined more than once"` when a statement names a key already in
    /// `_definitions` (imports included). `%declare`s are *not* pre-seeded: like
    /// every other statement they are processed in document order, so two
    /// `%declare`s of one name collide just as Python's two `_define(name, …, None)`
    /// calls do.
    ///
    /// Import-vs-import collision detection (#299, spun out of #270). Python merges
    /// all aliases per *module* (`load_grammar`: `import_aliases.update`) keyed by
    /// the *original* name, then mangles each definition to its final name; two
    /// distinct originals (in one module) mangling to the same final name collide
    /// inside the imported grammar's `_define` (`Terminal 'X' defined more than
    /// once`). An identical re-import of one `(module, original) -> final` triple is
    /// idempotent (the alias dict dedups it). So we key the source by `(module_path,
    /// original)`: registering the same final name from a *different* source is a
    /// duplicate; re-registering the same source is benign. `final_source` maps a
    /// final name to the source that first claimed it.
    ///
    /// **Last-alias-wins (#388).** When the *same* source is imported under
    /// *different* aliases (`%import common.INT -> X` then `-> Y`), Python's
    /// `import_aliases.update` keeps only the **last** binding: only `Y` is defined,
    /// `X` is dropped. So the collision pre-pass considers only the **surviving**
    /// alias per source (`alias_survives`, backed by the merged `import_alias_map`):
    /// an earlier, shadowed alias is neither registered as a final name nor checked
    /// for collision — it simply never exists. (This is distinct from #299's
    /// *different*-source/same-final-name collision, which still rejects, and from
    /// idempotent same-source/*same*-alias re-import, which stays benign because its
    /// single surviving alias is registered exactly once.)
    fn precheck_import_collisions(
        &self,
        items: &[Item],
    ) -> Result<(HashSet<String>, HashSet<String>), GrammarError> {
        let mut defined_rule_names: HashSet<String> = HashSet::new();
        let mut defined_term_names: HashSet<String> = HashSet::new();
        let mut final_source: HashMap<(Vec<String>, String), String> = HashMap::new();
        for item in items {
            if let Item::ImportItem(spec) = item {
                if let Some((module, pairs)) = split_import_directive(spec) {
                    for (original, final_name) in pairs {
                        // Shadowed (non-last) alias for this source: dropped, never
                        // defined — skip without registering or colliding.
                        if !self.alias_survives(&module, &original, &final_name) {
                            continue;
                        }
                        let source = (module.clone(), original);
                        if final_source.contains_key(&source) {
                            // Same `(module, original)` source already imported under
                            // its surviving alias: an identical re-import is
                            // idempotent (Python's per-module alias dict dedups it),
                            // so skip without colliding.
                            continue;
                        }
                        // A *different* source already claimed this final name →
                        // collision, exactly as two distinct originals mangling to
                        // one final name collide inside Python's imported `_define`
                        // (`Terminal 'X' defined more than once`).
                        if final_source.values().any(|f| f == &final_name) {
                            return Err(Self::duplicate_definition_error(
                                Self::name_is_terminal(&final_name),
                                &final_name,
                            ));
                        }
                        final_source.insert(source, final_name);
                    }
                }
                // Seed the running definition ledger with only the surviving final
                // names: a shadowed last-alias-wins binding (#388) is never defined,
                // so it must not pre-seed `defined_*_names` (else a later statement
                // colliding with the *dropped* name would wrongly reject).
                if let Some((module, pairs)) = split_import_directive(spec) {
                    for (original, final_name) in pairs {
                        if !self.alias_survives(&module, &original, &final_name) {
                            continue;
                        }
                        if Self::name_is_terminal(&final_name) {
                            defined_term_names.insert(final_name);
                        } else {
                            defined_rule_names.insert(final_name);
                        }
                    }
                }
            }
        }
        Ok((defined_rule_names, defined_term_names))
    }

    pub(super) fn process_items(&mut self, items: Vec<Item>) -> Result<(), GrammarError> {
        // First pass: register templates, and reserve every user-authored name so
        // generated `__anon_*` / `__ANON_*` names can never shadow one. An import's
        // target may be a rule or a terminal — unknowable before resolution — so it
        // reserves in both namespaces (harmless: the namespaces cannot overlap).
        for item in &items {
            match item {
                Item::RuleItem(r) => {
                    self.minter.reserved_rule_names.insert(r.name.clone());
                    // A *plain* user-authored rule/template name is unconditionally a
                    // name the grammar defines — the precise discriminator for the
                    // #428 user-vs-import-origin collision (a *surviving* import final
                    // name is added after this loop, once the alias map is complete).
                    //
                    // An `%override`/`%extend` directive (#442) is the exception: it
                    // *redefines* an existing origin rather than introducing a new one,
                    // so its target must NOT enter `claimed_rule_names` — otherwise the
                    // #428 guard would reject `%override python__name` beside `%import
                    // python (decorator)` as a collision with the very interior origin
                    // the override means to replace. Record it as an override/extend
                    // target instead, so the import-closure copy is allowed to proceed.
                    if r.directive == Directive::Plain {
                        self.claimed_rule_names.insert(r.name.clone());
                    } else {
                        self.override_extend_rule_targets.insert(r.name.clone());
                    }
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
                    self.minter.reserved_term_names.insert(t.name.clone());
                }
                Item::DeclareItem(syms) => {
                    for sym in syms {
                        if let Symbol::Terminal(t) = sym {
                            self.minter.reserved_term_names.insert(t.name.clone());
                        }
                    }
                }
                Item::ImportItem(spec) => {
                    for name in spec_final_names(spec) {
                        self.minter.reserved_rule_names.insert(name.clone());
                        self.minter.reserved_term_names.insert(name);
                    }
                    // Pre-build the per-module merged alias map (#343). Python
                    // merges every `%import` of one dotted path into a single
                    // `aliases` dict *before* any closure is copied, so an
                    // imported rule's dependency that is independently imported
                    // from the same module stays unmangled. Collect it up front,
                    // across all directives, so directive order does not matter.
                    if let Some((module_path, pairs)) = split_import_directive(spec) {
                        let entry = self.import_alias_map.entry(module_path).or_default();
                        for (original, final_name) in pairs {
                            entry.insert(original, final_name);
                        }
                    }
                }
                Item::IgnoreItem(_) => {}
            }
        }

        // Now that the per-module alias map is complete, fold each import's
        // *surviving* final name into `claimed_rule_names` (the #428 discriminator).
        // A name dropped by last-alias-wins (#388) is never defined, so it is
        // excluded — `alias_survives` is the exact filter the rest of the loader uses.
        // Done in a second pass over the imports because `alias_survives` reads the
        // merged `import_alias_map`, which the loop above only finishes building on its
        // last iteration.
        for item in &items {
            if let Item::ImportItem(spec) = item {
                if let Some((module, pairs)) = split_import_directive(spec) {
                    for (original, final_name) in pairs {
                        if self.alias_survives(&module, &original, &final_name) {
                            self.claimed_rule_names.insert(final_name);
                        }
                    }
                }
            }
        }

        // Detect import-vs-import collisions and seed the running definition
        // ledger with the surviving import final names. This is the staging order's
        // **second** load-bearing step (after the per-module alias map's first pass
        // above): the ledger is seeded with *surviving* names only (#388/#299).
        let (mut defined_rule_names, mut defined_term_names) =
            self.precheck_import_collisions(&items)?;

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

        // Resolve every `%import` *before* staging any rule/terminal directive,
        // mirroring Python Lark (`load_grammar.py` resolves all imports into
        // `_definitions`, then walks the statements). This makes an `%override` /
        // `%extend` of a mangled interior import origin (#442) order-independent: the
        // interior origin (`python__name` under `%import python (decorator)`) is
        // already copied into `self.rules` when the directive is staged, whichever
        // document order the directive and the `%import` appear in. Imports stage into
        // `self.rules` / `self.terminals` and never into `rule_items` / `raw_terms`,
        // and terminal *resolution* runs later in `resolve_terminals`, so hoisting the
        // imports ahead of the other directives changes nothing else (the relative
        // order *among* imports, which last-alias-wins depends on, is preserved).
        for item in &items {
            if let Item::ImportItem(spec) = item {
                self.resolve_import(spec.clone())?;
            }
        }
        // Now that every interior import origin is present, any `%override`/`%extend`
        // whose target is such an origin (#442) sees a pre-existing rule: seed the
        // override/extend pre-existence ledger with the interior origins now in
        // `self.rules` that a directive targets. Import *final* names are already in
        // `defined_rule_names` (seeded by `precheck_import_collisions`); this adds the
        // *interior* origins, which never reach that ledger.
        for rule in &self.rules {
            if self
                .override_extend_rule_targets
                .contains(&rule.origin.name)
            {
                defined_rule_names.insert(rule.origin.name.clone());
            }
        }

        for item in items {
            match item {
                // Imports already resolved above (hoisted, #442).
                Item::ImportItem(_) => {}
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

        // H2 (#331): template parameter-list well-formedness, mirroring Python
        // Lark's `GrammarDefinition.validate()` (`load_grammar.py`). For each
        // template, every parameter name is checked, in order, against (a) the
        // full set of *defined* names — a param that shadows a rule/terminal/
        // template/import is rejected, exactly as Python's `p in self._definitions`
        // — and (b) the parameters seen earlier in the same list (`p in
        // params[:i]`), rejecting a duplicate. The conflict check runs *before* the
        // duplicate check at each index, matching Python's error precedence.
        self.validate_template_params(&defined_rule_names, &defined_term_names)?;

        // Resolve all terminals (inlining terminal-to-terminal references).
        self.resolve_terminals()?;

        // Rule bodies, then `%ignore` expansions (which may reference terminals).
        for r in rule_items {
            self.compile_rule(r)?;
        }

        // Apply the deferred `%extend`-of-imported-interior-origin prepend (#442/#505):
        // now that every extend body has compiled (numbering its alternatives `0..k`),
        // shift each origin's pre-existing imported alternatives up by `k` so the
        // extend's alternatives sort strictly first, exactly as Python's `_extend`
        // prepends. Computed from the actual alternative count — no fixed offset bound.
        self.apply_pending_interior_extends()?;
        for expansions in ignore_items {
            // Mirror Python's `_ignore` (`load_grammar.py`): a directive that is a
            // single expansion containing a single value which is a reference to a
            // named terminal marks *that* terminal ignored (keeping its declared
            // priority) — "no need to create a new definition". Anything else
            // (multiple alternatives, a sequence, or an inline literal/regex)
            // synthesizes a fresh `__IGNORE_n` terminal, as before.
            if let [single_expansion] = expansions.as_slice() {
                if let [Expr::Value(Value::Terminal(name))] = single_expansion.as_slice() {
                    self.ignore_patterns.push(IgnoreEntry::Named(name.clone()));
                    continue;
                }
            }
            for expansion in expansions {
                let pat = self.expansion_to_pattern(&expansion)?;
                self.ignore_patterns.push(IgnoreEntry::Pattern(pat));
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
                //
                // A user rule colliding with a *mangled interior* import origin
                // (`python__name` under `%import python (decorator)`, #428) is caught
                // in `import_rule_closure` against `reserved_rule_names`, in either
                // document order — not here, because the interior origin never enters
                // the `defined` ledger (which holds only import *final* names).
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
                // An earlier `%extend` of this same imported interior origin recorded
                // a deferred prepend (`pending_interior_extends`) whose snapshot points
                // at the imported alternatives we just deleted. Python's `_extend` then
                // `_define(override=True)` replaces the whole definition, discarding any
                // alternatives the prior `%extend` inserted — so drop the pending
                // prepend too (the terminal sibling does the same with
                // `pending_term_extends`). Leaving it stale would underflow the
                // computed shift in `apply_pending_interior_extends` (`total` now < the
                // snapshot length) — issue #505 review finding.
                self.pending_interior_extends
                    .retain(|(name, _)| name != &r.name);
                rule_items.push(r);
            }
            Directive::Extend => {
                if !defined.contains(&r.name) {
                    return Err(GrammarError::Other {
                        msg: format!("Can't extend rule {} as it wasn't defined before", r.name),
                    });
                }
                // Prepend the new alternatives to the existing definition (Python's
                // `_extend`: `base.children.insert(0, exp)`). For a same-grammar
                // target, splice them onto the front of the staged `RawRule` so they
                // compile as one rule — and a *second* `%extend` of the same origin
                // finds that staged `RawRule` here and prepends onto it in turn, so
                // the later extend ends up frontmost, exactly as Python's repeated
                // `insert(0, …)`.
                if let Some(existing) = rule_items.iter_mut().find(|prev| prev.name == r.name) {
                    let mut merged = r.expansions;
                    merged.append(&mut existing.expansions);
                    existing.expansions = merged;
                } else {
                    // No staged same-grammar `RawRule`: the target is an *imported
                    // interior origin* (#442), already compiled into `self.rules` with
                    // its alternatives' preserved `rule.order`. Stage the extend body
                    // as a deferred definition at the same origin — but it must
                    // *prepend*: Python's `_extend` gives the new alternative the
                    // lowest `order` and shifts the originals down. `compile_rule`
                    // numbers a fresh definition's alternatives from 0, and `order` is
                    // used only as a *relative* tie-break in resolve disambiguation
                    // (Earley `(is_empty, -priority, order)`; LALR same-reduction
                    // collapse "first arm wins"), never as an index. The prepend is
                    // therefore "every extend alternative must sort strictly ahead of
                    // every pre-existing imported alternative at this origin".
                    //
                    // Rather than bump the existing alternatives by a fixed constant
                    // (the old `EXTEND_ORDER_OFFSET = 1_000_000`, which could overlap
                    // once an extend produced ≥1_000_000 alternatives — issue #505), we
                    // *defer* the shift: snapshot the pre-existing imported
                    // alternatives now and record the origin. After the extend body is
                    // compiled (numbering its alternatives `0..k`), `apply_pending_
                    // interior_extends` shifts each pre-existing alternative up by the
                    // computed `k`, leaving the extend's `0..k` strictly first — a real
                    // invariant for any `k`. (Without the prepend the extend was
                    // *appended* — its `0..k` tied with the originals' low orders but
                    // lost insertion order — a resolve divergence the named-terminal
                    // differential never surfaced because a distinct terminal
                    // disambiguates at the lexer instead.)
                    let preexisting: Vec<Rule> = self
                        .rules
                        .iter()
                        .filter(|x| x.origin.name == r.name)
                        .cloned()
                        .collect();
                    self.pending_interior_extends
                        .push((r.name.clone(), preexisting));
                    rule_items.push(RawRule {
                        directive: Directive::Plain,
                        ..r
                    });
                }
            }
        }
        Ok(())
    }

    /// Apply every deferred `%extend`-of-imported-interior-origin prepend recorded in
    /// [`pending_interior_extends`](Self::pending_interior_extends). Called once after
    /// all rule bodies have compiled.
    ///
    /// At record time we snapshotted the pre-existing (imported) alternatives at the
    /// origin (orders untouched); the extend body has since compiled to a fresh
    /// definition whose alternatives number `0..k`. To realize Python's `_extend`
    /// *prepend*, every pre-existing alternative is shifted up by exactly `k` — the
    /// **computed** alternative count, not a fixed constant — so the extend's `0..k`
    /// orders sort strictly ahead of all of them, for any `k`. `k` is recovered as
    /// "rules at the origin that are *not* in the snapshot" (the just-compiled extend
    /// alternatives). A pre-existing alternative is matched by value; should an extend
    /// alternative happen to be byte-identical *and* share an order with a pre-existing
    /// one, shifting either is equivalent, so the match remains correct.
    ///
    /// Robustness (#527): the prepend count is `total - preexisting.len()`, which is
    /// non-negative only as long as the snapshot invariant holds — every snapshotted
    /// pre-existing alternative is still present at the origin when this runs. The one
    /// path that could break it (`%extend`-then-`%override`, which deletes the
    /// snapshotted alternatives) is already closed at the `%override` site by dropping
    /// the stale pending entry (#505). We still compute the count with `checked_sub` so
    /// that a *future* regression which violates the invariant surfaces as a clear
    /// internal error rather than an underflow panic (debug) or a silent wrap into a
    /// huge `k` (release).
    fn apply_pending_interior_extends(&mut self) -> Result<(), GrammarError> {
        let pending = std::mem::take(&mut self.pending_interior_extends);
        for (origin, preexisting) in pending {
            // Count the extend alternatives = rules at this origin minus the snapshot.
            let total = self
                .rules
                .iter()
                .filter(|x| x.origin.name == origin)
                .count();
            let k = total
                .checked_sub(preexisting.len())
                .ok_or_else(|| GrammarError::Other {
                    msg: format!(
                        "internal error: pending interior %extend at origin `{origin}` \
                         snapshotted {} pre-existing alternatives but only {total} remain \
                         — the snapshot invariant was violated \
                         (apply_pending_interior_extends, #527)",
                        preexisting.len(),
                    ),
                })?;
            if k == 0 {
                continue;
            }
            // Shift each pre-existing (imported) alternative up by `k`, consuming each
            // snapshot entry once so duplicate-valued alternatives are each shifted.
            let mut remaining = preexisting;
            for rule in self.rules.iter_mut().filter(|x| x.origin.name == origin) {
                if let Some(pos) = remaining.iter().position(|p| p == rule) {
                    remaining.swap_remove(pos);
                    rule.order += k;
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
                // terminal at this name, then stage the override body. A pending
                // imported-terminal `%extend` staged *earlier* (#286) is discarded:
                // Python's `_define(override=True)` replaces the whole `Definition`,
                // dropping any alternatives a prior `_extend` had inserted onto it.
                self.raw_terms.retain(|prev| prev.name != t.name);
                self.terminals.retain(|td| td.name != t.name);
                self.pending_term_extends
                    .retain(|(name, _)| name != &t.name);
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
                // Reject `%extend` of an abstract (`%declare`d, pattern-less)
                // terminal (#299). A declared terminal lives in `self.terminals`
                // with `declared == true` and has no `RawTerm` body to splice onto;
                // Python's `_extend` rejects it (`d.tree is None`) with
                // `Can't extend terminal FOO - it is abstract.`. Without this gate
                // the extend body was silently dropped and the grammar built.
                if self
                    .terminals
                    .iter()
                    .any(|td| td.name == t.name && td.declared)
                    && !self.raw_terms.iter().any(|prev| prev.name == t.name)
                {
                    return Err(GrammarError::Other {
                        msg: format!("Can't extend terminal {} - it is abstract.", t.name),
                    });
                }
                // Prepend the new alternatives to the existing terminal. A
                // same-grammar terminal is still a `RawTerm` here (terminals
                // resolve as a whole later), so splice onto its front.
                //
                // An *imported* terminal has already been compiled into
                // `self.terminals` (not `raw_terms`) by the time the directive is
                // staged, and reconstructing a `RawTerm` from its baked regex is
                // lossy (#286). So stage the new alternatives in
                // `pending_term_extends`; `resolve_terminals` prepends them onto the
                // resolved terminal's regex before baking — matching Python's
                // `_extend`, which does `base.children.insert(0, exp)` on the
                // *still-AST* definition tree (so both the new alt and the original
                // body survive). The same-grammar splice below stays the fast path.
                if let Some(existing) = self.raw_terms.iter_mut().find(|prev| prev.name == t.name) {
                    let mut merged = t.expansions;
                    merged.append(&mut existing.expansions);
                    existing.expansions = merged;
                } else {
                    self.pending_term_extends.push((t.name, t.expansions));
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

    /// H2 (#331): validate every template's parameter list, mirroring Python
    /// Lark's `GrammarDefinition.validate()` (`load_grammar.py`):
    /// for each parameter `p` at index `i`, reject it if it shadows a defined
    /// name (rule/terminal/template/import — Python's `p in self._definitions`,
    /// the "conflicts with rule" error) *before* checking whether it duplicates an
    /// earlier parameter (`p in params[:i]`, the "Duplicate Template Parameter"
    /// error). Template names are visited in a deterministic (sorted) order so the
    /// reported error is stable; the oracle only pins single-template grammars.
    fn validate_template_params(
        &self,
        defined_rules: &HashSet<String>,
        defined_terms: &HashSet<String>,
    ) -> Result<(), GrammarError> {
        let mut names: Vec<&String> = self.templates.keys().collect();
        names.sort();
        for name in names {
            let params = &self.templates[name].0;
            for (i, p) in params.iter().enumerate() {
                if defined_rules.contains(p) || defined_terms.contains(p) {
                    return Err(GrammarError::Other {
                        msg: format!(
                            "Template Parameter conflicts with rule {p} (in template {name})"
                        ),
                    });
                }
                if params[..i].contains(p) {
                    return Err(GrammarError::Other {
                        msg: format!("Duplicate Template Parameter {p} (in template {name})"),
                    });
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
    ///
    /// Both stages compare symbols by the **filter-out-agnostic** key
    /// [`sym_key`](Self::sym_key) — `(is_terminal, name)` — exactly mirroring
    /// Python's `Symbol.__eq__`/`__hash__`, which ignore the per-occurrence
    /// `filter_out` flag (and so does `Rule.__eq__`, keyed on `origin` +
    /// `expansion` of those symbols). This is what collapses an alternative that
    /// is a string literal equal to a named terminal — `start: A | "a"` with
    /// `A: "a"`, where the literal unifies onto `A` for lexing but carries
    /// `filter_out=true` while the `A` reference carries `filter_out=false` — to a
    /// single arm (#347, H4-9). Without it the two arms survive as two byte-
    /// identical `CompiledRule`s differing only in `filter_pos`, a spurious LALR
    /// reduce/reduce and an Earley `explicit` phantom empty derivation Python never
    /// produces. Stage 1 keeps the **first** occurrence, so its `filter_out` (hence
    /// the kept/dropped fate of the token) wins exactly as Python's `dedup_list`
    /// keeps the first tree — `A | "a"` keeps `A` (token kept), `"a" | A` keeps
    /// `"a"` (token dropped). An alias-differing pair (`X -> p | X -> q`) still
    /// survives stage 1 (alias is part of the key) to collide in stage 2.
    pub(super) fn dedup_and_check_alts(
        origin: &str,
        alts: Vec<(CompiledAlt, Option<String>)>,
    ) -> Result<Vec<(CompiledAlt, Option<String>)>, GrammarError> {
        // Stage-1 dedup key: filter-out-agnostic symbols + gaps + alias, so a
        // literal-vs-named pair collapses but an alias-differing pair survives to
        // collide in stage 2 (as Python's "Rules defined twice").
        //
        // An *empty* expansion keys on emptiness + alias **alone** — its gaps (the
        // distributed-absent `None`/`_EMPTY` placeholder counts) are dropped from
        // the key. Python tolerates and dedups duplicate empty rules regardless of
        // their `empty_indices` (the line-780 "Rules defined twice" check fires only
        // for non-empty `dups[0].expansion`), keeping the first. Two distributed
        // optionals whose absent arms differ only in placeholder count — e.g.
        // `[A] | ["a"]`, where `["a"]`'s filtered literal contributes a 0-size
        // absent arm while `[A]`'s contributes a 1-size one (`FindRuleSize` /
        // `_will_not_get_removed`) — would otherwise survive as two empty `start ->`
        // productions, a spurious reduce/reduce Python never reports (#347, adjacent
        // to H4-9: same `filter_out`-leak root, surfaced by the differential audit).
        // The surviving arm in `out` keeps its real gaps (first occurrence), so the
        // `maybe_placeholders` `None` count is preserved exactly as Python keeps the
        // first absent arm's `empty_indices`.
        type AltKey = (Vec<(bool, String)>, Vec<usize>, Option<String>);
        let alt_key = |alt: &(CompiledAlt, Option<String>)| -> AltKey {
            let ((syms, gaps), alias) = alt;
            let gap_key = if syms.is_empty() {
                Vec::new() // empty arm: dedup on emptiness alone, ignore placeholder count
            } else {
                gaps.clone()
            };
            (
                syms.iter().map(Self::sym_key).collect(),
                gap_key,
                alias.clone(),
            )
        };
        let mut seen: std::collections::HashSet<AltKey> = std::collections::HashSet::new();
        let mut out: Vec<(CompiledAlt, Option<String>)> = Vec::with_capacity(alts.len());
        let mut seen_syms: std::collections::HashSet<Vec<(bool, String)>> =
            std::collections::HashSet::new();
        for alt in alts {
            if !seen.insert(alt_key(&alt)) {
                continue; // exact duplicate — Python's AST-level dedup_list
            }
            let syms = &alt.0 .0;
            // Stage-2 collision key mirrors Python's `Rule.__eq__` (origin +
            // expansion only): filter-out-agnostic symbols, no gaps/alias.
            let syms_key: Vec<(bool, String)> = syms.iter().map(Self::sym_key).collect();
            if !syms.is_empty() && !seen_syms.insert(syms_key) {
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

    /// A symbol's identity for cross-alternative dedup/collision, mirroring Python
    /// Lark's `Symbol.__eq__`/`__hash__`: `(is_terminal, name)`. The per-occurrence
    /// `Terminal::filter_out` flag is *deliberately* excluded so a literal unified
    /// onto a named terminal (`"a"` → `A`, `filter_out` differing) compares equal to
    /// a direct reference to that terminal (#347). lark-rs's derived `Eq`/`Hash` on
    /// `Symbol` *do* include `filter_out`, so this is the one place that must
    /// canonicalize it away. Also used by the recurse-helper arm dedup
    /// (`ebnf::recurse_helper_keyed`), where Python likewise builds the one-or-more
    /// rule from the filter-out-agnostic *set* of inner expansions.
    pub(super) fn sym_key(sym: &Symbol) -> (bool, String) {
        match sym {
            Symbol::Terminal(t) => (true, t.name.clone()),
            Symbol::NonTerminal(nt) => (false, nt.name.clone()),
        }
    }

    /// Register each `%declare`d name as a pattern-less terminal. A declared
    /// terminal is never lexed — it is interned (so rules can reference it and the
    /// parse table reserves a column) and injected into the token stream by a
    /// postlex hook, e.g. an [`Indenter`](crate::postlex::Indenter)'s `_INDENT` /
    /// `_DEDENT`. A `%declare` is a definition like any other (Python's
    /// `_define(name, is_term, None)`): declaring a name already defined — by an
    /// import, a prior `%declare`, or a local terminal — is rejected as a
    /// duplicate (#270).
    ///
    /// A `%declare` target must be a terminal (UPPERCASE) name. A rule-cased
    /// (lowercase) target — which the grammar parser surfaces as a
    /// [`Symbol::NonTerminal`] — is rejected (#353, H4-11): Python Lark only ever
    /// builds a `TerminalDef` from a declared symbol, so `%declare foo` blows up
    /// internally (an `AttributeError`) rather than succeeding. We pin the
    /// reject/accept verdict, not Python's accidental message, with a clean
    /// `GrammarError`.
    fn declare_terminals(
        &mut self,
        syms: Vec<Symbol>,
        defined: &mut HashSet<String>,
    ) -> Result<(), GrammarError> {
        for sym in syms {
            match sym {
                Symbol::Terminal(t) => {
                    if !defined.insert(t.name.clone()) {
                        return Err(Self::duplicate_definition_error(true, &t.name));
                    }
                    if !self.terminals.iter().any(|td| td.name == t.name) {
                        self.terminals.push(TerminalDef::declared(&t.name));
                    }
                }
                Symbol::NonTerminal(nt) => {
                    return Err(GrammarError::Other {
                        msg: format!(
                            "Cannot %declare a rule-cased name '{}': %declare targets must be UPPERCASE terminal names",
                            nt.name
                        ),
                    });
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
    ///
    /// `pub(super)` so the sibling `imports` module raises the identical message
    /// for a user-rule-vs-mangled-import-origin collision (#428).
    pub(super) fn duplicate_definition_error(is_term: bool, name: &str) -> GrammarError {
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

        // Final assembly is four named phases, run in order: synthesize the
        // `%ignore` terminals, reject use-before-definition, prune unreferenced
        // terminals, then sort the survivors into the lexer/intern order. The
        // phase boundaries are load-bearing — the undefined-reference check must
        // run on the *full* terminal set (before pruning), and the sort feeds
        // SymbolId assignment last.
        let ignore_names = self.synthesize_ignore_terminals()?;
        self.check_undefined_references()?;
        self.prune_unused_terminals(&ignore_names);
        self.sort_terminals();

        Ok(Grammar {
            rules: self.rules,
            terminals: self.terminals,
            ignore: ignore_names,
            start: self.start,
            anon_kinds: self.minter.anon_kinds,
            lalr_audit: None,
        })
    }

    /// Synthesize the `%ignore` terminals (one terminal per ignore pattern) and
    /// return their names in document order. `__IGNORE_{n}` is the third
    /// generated-name family, and the import-alias route reaches it like the
    /// others (`%import common.WS -> __IGNORE_0`), so it skips user-claimed names
    /// via the same availability check. `%ignore` tokens never reach the tree (the
    /// parse loop skips them), so they need no per-occurrence filter — they appear
    /// in no rule body.
    fn synthesize_ignore_terminals(&mut self) -> Result<Vec<String>, GrammarError> {
        let ignore_patterns = std::mem::take(&mut self.ignore_patterns);
        let mut ignore_names: Vec<String> = Vec::with_capacity(ignore_patterns.len());
        for entry in ignore_patterns {
            match entry {
                // `%ignore NAME`: add the existing terminal to the ignore set with
                // its declared priority intact — no clone (Python's `_ignore`
                // short-circuit). Reject a name that resolves to no defined
                // terminal, mirroring Python's "Terminals %s were marked to ignore
                // but were not defined!" (a bare `%ignore WS` does not auto-import).
                IgnoreEntry::Named(name) => {
                    // A `%ignore NAME` whose terminal is absent, **or** present only
                    // as a pattern-less `%declare`d terminal, is rejected — matching
                    // Python's `LexError: Ignore terminals are not defined: {…}`. A
                    // declared terminal carries no pattern and is absent from the
                    // lexer's terminal list, so Python's ignore-set difference is
                    // non-empty even though the name *is* defined as a symbol; our
                    // existing presence check passed for it (bounty H7-1, #414). Per
                    // ADR-0017, being more permissive than the oracle is unfalsifiable,
                    // so we reject it at build.
                    match self.terminals.iter().find(|t| t.name == name) {
                        None => return Err(GrammarError::UndefinedTerminal { name }),
                        Some(t) if t.declared => {
                            return Err(GrammarError::UndefinedTerminal { name })
                        }
                        Some(_) => {}
                    }
                    ignore_names.push(name);
                }
                // Inline pattern: synthesize a fresh `__IGNORE_n` terminal at the
                // default priority, exactly as both engines do for the inline form.
                // The base index is the count of ignore entries seen so far
                // (Python's `'__IGNORE_%d' % len(self._ignore_names)`), so a named
                // entry preceding an inline one bumps the inline name's number; the
                // availability skip is lark-rs's extra collision guard (#326 import
                // alias) layered on top of that base.
                IgnoreEntry::Pattern(pat) => {
                    let mut ignore_counter = ignore_names.len();
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
            }
        }
        Ok(ignore_names)
    }

    /// Reject use-before-definition: a rule body that references a symbol which
    /// is neither a defined rule nor a defined terminal is a grammar error, as in
    /// Python Lark (`GrammarError("Rule 'X' used but not defined")`). Run *before*
    /// pruning so the full terminal set is visible. Template parameters never reach
    /// here — templates are instantiated on demand and only their (fully
    /// substituted) instances live in `self.rules` — and anonymous literal
    /// terminals are interned as they are compiled, so they are always defined.
    fn check_undefined_references(&self) -> Result<(), GrammarError> {
        let defined_rules: std::collections::HashSet<&str> =
            self.rules.iter().map(|r| r.origin.name.as_str()).collect();
        let defined_terms: std::collections::HashSet<&str> =
            self.terminals.iter().map(|t| t.name.as_str()).collect();
        // A start symbol (default `start` or a custom one) that resolves to no
        // defined rule is rejected here, exactly as Python Lark does
        // (`GrammarError: Using an undefined rule: NonTerminal('start')`). Without
        // this gate, `lower()` reached an undefined start at
        // `symbols.id(s).expect("start symbol interned")` and **panicked** instead
        // of returning a clean error — a robustness/DoS hole on user- or
        // attacker-supplied grammars (bug-bounty H1, #330). A start is always a
        // non-terminal, so only the rule set matters.
        for start in &self.start {
            if !defined_rules.contains(start.as_str()) {
                return Err(GrammarError::UndefinedRule {
                    name: start.clone(),
                });
            }
        }
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
        Ok(())
    }

    /// Prune terminals that no rule (or `%ignore`) references. A terminal used
    /// only inside another terminal (`C: "C" | D` — `D` is inlined into `C`)
    /// has no token of its own, exactly as Python Lark drops it. Terminals
    /// referenced by a rule body, and the synthetic `%ignore` terminals, stay.
    fn prune_unused_terminals(&mut self, ignore_names: &[String]) {
        let mut used: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for rule in &self.rules {
            for sym in &rule.expansion {
                if let Symbol::Terminal(t) = sym {
                    used.insert(t.name.as_str());
                }
            }
        }
        for name in ignore_names {
            used.insert(name.as_str());
        }
        self.terminals.retain(|t| used.contains(t.name.as_str()));
    }

    /// Sort terminals by (priority desc, max_width desc, raw_value_len desc,
    /// name asc) — the same total order the lexer plan uses
    /// (`lexer/plan.rs::sort_terminals`, Python `lark/lexer.py:583`). This sort
    /// feeds SymbolId assignment, so keeping the two in lockstep means the raw
    /// pattern-length tiebreak (#268, N2: flags stored separately, not baked into
    /// the length) can never diverge between interning order and lexer order.
    fn sort_terminals(&mut self) {
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
    }
}

#[cfg(test)]
mod tests {
    use crate::grammar::{load_grammar, lower, SymbolKind};

    /// #144 was a release-only id-space corruption: a user-authored `__ANON_0`
    /// (a leading `__` lexes as a rule name) collided with the counter-generated
    /// terminal name for `/x/`, and the lowerer's single `by_name` table made the
    /// rule resolve to the terminal's id. Since #361 the collision vector is closed
    /// one layer earlier — Python rejects a `__`-leading name token at grammar-parse
    /// (`RULE`/`TERMINAL` = `_?[a-z]…`/`_?[A-Z]…`), so lark-rs does too, and such a
    /// grammar never reaches the lowerer. This pins that parity; the counter
    /// skip-forward in `reserved_rule_names` survives as defense-in-depth.
    #[test]
    fn user_double_underscore_terminal_name_rejected() {
        assert!(
            load_grammar(
                "start: /x/ __ANON_0\n__ANON_0: \"y\"\n",
                &["start".to_string()],
                false,
                false,
            )
            .is_err(),
            "a user-authored `__`-leading name is rejected at the tokenizer (Python parity, #361)"
        );
    }

    /// The hint variant of the #144 route, likewise closed by #361: the uppercase
    /// hint of a literal `"__anon_5"` is `__ANON_5`, which a user *rule* `__ANON_5`
    /// would have collided with — but `__ANON_5` is a `__`-leading name token and is
    /// now rejected at grammar-parse, exactly as Python rejects it. (The
    /// hint-vs-counter uniqueness invariant itself stays covered by
    /// `generated_terminal_skips_hint_minted_name`, whose hint is minted internally,
    /// not lexed as a user name.)
    #[test]
    fn user_double_underscore_terminal_name_with_hint_rejected() {
        assert!(
            load_grammar(
                "start: \"__anon_5\" __ANON_5\n__ANON_5: \"y\"\n",
                &["start".to_string()],
                false,
                false,
            )
            .is_err(),
            "a user-authored `__ANON_5` rule name is rejected at the tokenizer (Python parity, #361)"
        );
    }

    /// `__anon_plus_0` once collided with the `thing+` helper name (#144). It is a
    /// `__`-leading name, so Python rejects it at grammar-parse and lark-rs now does
    /// too (#361) — the helper-collision can no longer be authored.
    #[test]
    fn user_double_underscore_helper_rule_name_rejected() {
        assert!(
            load_grammar(
                "start: thing+ __anon_plus_0\n__anon_plus_0: \"b\"\nthing: \"a\"\n",
                &["start".to_string()],
                false,
                false,
            )
            .is_err(),
            "a user-authored `__anon_plus_0` rule name is rejected at the tokenizer (Python parity, #361)"
        );
    }

    /// A user cannot *define* a terminal named `__ANON_0` (a leading `__` lexes as a
    /// name token, now rejected; #361), and Python rejects an **import alias** to one
    /// just the same — `%import common.INT -> __ANON_0` fails at grammar-parse on the
    /// alias-target name. This pins that parity; pre-#144 the alias was the one route
    /// that could register a generated-lookalike name and shadow the inline literal.
    #[test]
    fn import_alias_to_double_underscore_name_rejected() {
        assert!(
            load_grammar(
                "start: /x/\n%import common.INT -> __ANON_0\n",
                &["start".to_string()],
                false,
                false,
            )
            .is_err(),
            "an import alias to a `__`-leading name is rejected at the tokenizer (Python parity, #361)"
        );
    }

    /// `__IGNORE_{n}` is the third generated-name family, reachable pre-#361 by the
    /// same import-alias route as `__ANON_{n}` (`%import common.WS -> __IGNORE_0`).
    /// Python rejects the `__`-leading alias target at grammar-parse; lark-rs now
    /// does too (#361), so the duplicate-`__IGNORE_0` collision can no longer be
    /// authored.
    #[test]
    fn import_alias_to_double_underscore_ignore_name_rejected() {
        assert!(
            load_grammar(
                "start: \"a\"\n%ignore \" \"\n%import common.WS -> __IGNORE_0\n",
                &["start".to_string()],
                false,
                false,
            )
            .is_err(),
            "an import alias to a `__`-leading ignore name is rejected at the tokenizer (Python parity, #361)"
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

    /// `%extend` of an *imported* terminal adds the new alternative to its body — the
    /// imported terminal is already a compiled `TerminalDef`, so the extend is staged
    /// in `pending_term_extends` and prepended onto the resolved regex in
    /// `resolve_terminals` (#286, Python's `_extend`). The terminal becomes an
    /// alternation of the new arm and the original body (so both `"z"` and the
    /// original digit body match), and it is no longer a `string_type`.
    #[test]
    fn extend_imported_terminal_keeps_both_alternatives() {
        let g = load("%import common.INT\nstart: INT\n%extend INT: \"z\"\n").unwrap();
        let int = g
            .terminals
            .iter()
            .find(|t| t.name == "INT")
            .expect("INT survives");
        let re = int.pattern.as_regex_str();
        // The combined regex is `original_body | z` (each arm wrapped in `(?:…)`),
        // so it carries the imported INT's digit class, the new `z` arm, and a
        // top-level alternation. (We assert the stable pieces, not the exact
        // `(?:…)` nesting the inline-regex builder happens to produce.)
        assert!(
            re.contains("[0-9]") && re.contains('z') && re.contains('|'),
            "extended INT must keep both the original [0-9] body and the new \"z\" \
             arm in an alternation; got: {re}"
        );
        assert!(
            !int.string_type,
            "an extended terminal is an alternation (PatternRE), not a string literal"
        );
    }

    /// A later `%override` of an imported terminal *discards* an earlier `%extend`'s
    /// staged alternatives — Python's `_define(override=True)` replaces the whole
    /// `Definition`, dropping anything a prior `_extend` inserted (#286 edge). So
    /// `INT` ends up exactly the override body `"z"`, with no `[0-9]+` arm.
    #[test]
    fn override_after_extend_discards_pending_extend() {
        let g = load("%import common.INT\nstart: INT\n%extend INT: \"y\"\n%override INT: \"z\"\n")
            .unwrap();
        let int = g
            .terminals
            .iter()
            .find(|t| t.name == "INT")
            .expect("INT survives");
        assert_eq!(
            int.pattern.as_regex_str(),
            "z",
            "override after extend keeps only the override body, dropping the extend arm"
        );
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

    /// H2a (#331): a template with a duplicate parameter name is rejected with
    /// Python Lark's verbatim message (`GrammarDefinition.validate()`).
    #[test]
    fn duplicate_template_param_rejected() {
        let err = load("foo{x,x}: x\nstart: foo{\"a\",\"b\"}\n").unwrap_err();
        assert!(
            err.to_string()
                .contains("Duplicate Template Parameter x (in template foo)"),
            "got: {err}"
        );
    }

    /// H2b (#331): a template parameter whose name shadows a defined rule is
    /// rejected — the "conflicts with rule" check, run *before* the duplicate check.
    #[test]
    fn template_param_shadows_rule_rejected() {
        let err = load("x: \"z\"\nfoo{x}: x\nstart: foo{\"a\"}\n").unwrap_err();
        assert!(
            err.to_string()
                .contains("Template Parameter conflicts with rule x (in template foo)"),
            "got: {err}"
        );
    }

    /// No over-rejection: a well-formed template whose parameter names are distinct
    /// and don't shadow any definition still builds (guards the H2 validation pass
    /// against false positives).
    #[test]
    fn well_formed_template_params_accepted() {
        load("foo{a,b}: a b\nstart: foo{\"x\",\"y\"}\n").unwrap();
        load("_sep{item, sep}: item (sep item)*\nstart: _sep{NAME, \",\"}\nNAME: /[a-z]+/\n")
            .unwrap();
    }

    /// H3 (#331): a top-level alias (`->`) in a terminal definition is rejected
    /// with Python's verbatim message. A *group-nested* alias is also rejected (our
    /// walk catches it); Python rejects it too, though via a later "used but not
    /// defined" path — both reject, which is the contract.
    #[test]
    fn alias_in_terminal_rejected() {
        let top = load("A: \"a\" -> foo\nstart: A\n").unwrap_err();
        assert!(
            top.to_string()
                .contains("Aliasing not allowed in terminals (You used -> in the wrong place)"),
            "got: {top}"
        );
        // Both engines reject the nested case; we report it precisely as an
        // aliasing error rather than letting it leak downstream.
        assert!(load("A: (\"a\" -> foo)\nstart: A\n").is_err());
    }

    /// No over-rejection: an alias on a *rule* (not a terminal) is still legal.
    #[test]
    fn alias_on_rule_still_accepted() {
        load("start: \"a\" -> foo\n").unwrap();
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

    /// #527 hardening: `apply_pending_interior_extends` computes the prepend count as
    /// `total - preexisting.len()`. The snapshot invariant (every snapshotted
    /// pre-existing alternative is still present at the origin) is preserved by the
    /// #505 `%override`-drops-pending fix on every reachable grammar path, so this
    /// subtraction never underflows in practice. This test exercises the *defensive*
    /// branch directly by violating the invariant — a snapshot longer than the actual
    /// rules at the origin — which is exactly the shape a future same-origin mutation
    /// could regress into. With the raw subtraction this underflow-**panics** in debug
    /// (and wraps to a huge `k` in release); with `checked_sub` it returns a clear
    /// internal `GrammarError` instead. Asserting `is_err()` (not catching a panic)
    /// is the post-fix contract.
    #[test]
    fn pending_interior_extend_snapshot_underflow_is_internal_error_not_panic() {
        use super::GrammarCompiler;
        use crate::grammar::rule::{Rule, RuleOptions};
        use crate::grammar::symbol::NonTerminal;

        let mut compiler =
            GrammarCompiler::new(vec!["start".to_string()], false, false, None, None);

        let origin = NonTerminal::new("mod__inner");
        // Exactly one real rule lives at the origin …
        compiler.rules.push(Rule::new(
            origin.clone(),
            Vec::new(),
            None,
            RuleOptions::default(),
            0,
        ));
        // … but the pending snapshot claims two pre-existing alternatives, so
        // `total (1) - preexisting.len() (2)` would underflow. This is the
        // invariant-violation shape the hardening guards against.
        let snapshot = vec![
            Rule::new(origin.clone(), Vec::new(), None, RuleOptions::default(), 0),
            Rule::new(origin.clone(), Vec::new(), None, RuleOptions::default(), 1),
        ];
        compiler
            .pending_interior_extends
            .push(("mod__inner".to_string(), snapshot));

        let err = compiler
            .apply_pending_interior_extends()
            .expect_err("snapshot longer than live rules must yield an internal error, not panic");
        assert!(
            err.to_string().contains("snapshot invariant was violated"),
            "expected the #527 internal-error message, got: {err}"
        );
    }
}
