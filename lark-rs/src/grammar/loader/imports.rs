//! Phase 3b — `%import` resolution: the bundled grammar libraries (`common`,
//! `python`, `unicode`, `lark`) and sibling-file imports, with rule-closure
//! copying and module-prefix mangling.

use super::ast::{ImportSpec, Item};
use super::compiler::GrammarCompiler;
use super::parser::GrammarParser;
use super::{load_grammar, load_grammar_with_base, load_grammar_with_sources};
use crate::error::GrammarError;
use crate::grammar::rule::Rule;
use crate::grammar::symbol::{NonTerminal, Symbol, Terminal};
use crate::grammar::terminal::{Pattern, PatternRe, TerminalDef};
use crate::grammar::Grammar;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

/// Synthetic start rule appended to an imported file so the requested terminals
/// survive dead-terminal pruning while the file is compiled. Never copied out.
const IMPORT_PROBE_RULE: &str = "__lark_import_probe";

/// Canonical key for a virtual path in the in-memory `import_sources` map:
/// components joined with `/`, regardless of the host's path separator, so map
/// keys are written the same way on every platform.
fn virtual_key(path: &std::path::Path) -> String {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// The names an `%import` directive will register in the importing grammar
/// (aliases applied). Used by the compiler to reserve user-claimed names before
/// any anonymous name is generated, and by the bundled-library probe to keep a
/// library's own import targets importable in turn.
pub(super) fn spec_final_names(spec: &ImportSpec) -> Vec<String> {
    if let Some(names) = &spec.names {
        names.clone()
    } else if spec.path.len() > 1 {
        let original = spec.path.last().cloned().unwrap_or_default();
        vec![spec.alias.clone().unwrap_or(original)]
    } else {
        Vec::new()
    }
}

/// Split an `%import` directive into its module path (which file/library to load
/// from) and the `(original_name, final_name)` pairs it registers — the same
/// split `resolve_import` performs, factored out so the first compiler pass can
/// pre-build the per-module merged alias map (see
/// [`GrammarCompiler::import_alias_map`]) without re-running import resolution.
///
/// Returns `None` for a bare single-element path (`%import common` with no name),
/// which imports nothing.
pub(super) fn split_import_directive(
    spec: &ImportSpec,
) -> Option<(Vec<String>, Vec<(String, String)>)> {
    if let Some(names) = &spec.names {
        // Name-list form: a multi-import cannot carry per-name aliases, so each
        // final name equals its original (Python: `dict(zip(names, names))`).
        Some((
            spec.path.clone(),
            names.iter().map(|n| (n.clone(), n.clone())).collect(),
        ))
    } else if spec.path.len() > 1 {
        let original = spec.path.last().cloned().unwrap_or_default();
        let module = spec.path[..spec.path.len() - 1].to_vec();
        let final_name = spec.alias.clone().unwrap_or_else(|| original.clone());
        Some((module, vec![(original, final_name)]))
    } else {
        None
    }
}

impl GrammarCompiler {
    pub(super) fn resolve_import(&mut self, spec: ImportSpec) -> Result<(), GrammarError> {
        // Split the directive into the module path (which file/library to load
        // from) and the list of `(name, alias)` symbols to import. Three forms:
        //   %import common.WORD              → module=["common"],  import WORD
        //   %import common.WS -> _WS         → module=["common"],  import WS as _WS
        //   %import common (WORD, INT, ...)  → module=["common"],  import each
        //   %import .tokens (NUMBER, NAME)   → module=["tokens"] (relative file)
        let (module_path, names_to_import): (Vec<String>, Vec<(String, Option<String>)>) =
            if let Some(names) = spec.names {
                // Name-list form: a multi-import cannot carry per-name aliases.
                (
                    spec.path.clone(),
                    names.into_iter().map(|n| (n, None)).collect(),
                )
            } else if spec.path.len() > 1 {
                // Single import: the last path element is the symbol; the leading
                // elements are the module. An alias may rename it.
                let original = spec.path.last().cloned().unwrap_or_default();
                let module = spec.path[..spec.path.len() - 1].to_vec();
                (module, vec![(original, spec.alias)])
            } else {
                return Ok(()); // nothing to import
            };

        // Last-alias-wins (#388). When one source `(module, original)` is imported
        // under several aliases, Python's per-module `import_aliases.update` keeps
        // only the **last** binding; the earlier aliases are dropped and never
        // copied. Filter them out here, at the single point every import path
        // (common terminal table, bundled closure, file closure) funnels through,
        // so a shadowed alias is never registered under any of them. The surviving
        // alias is taken from the merged `import_alias_map` (`alias_survives`); a
        // name-list import registers `original == final`, always its own survivor.
        let names_to_import: Vec<(String, Option<String>)> = names_to_import
            .into_iter()
            .filter(|(name, alias)| {
                let final_name = alias.clone().unwrap_or_else(|| name.clone());
                self.alias_survives(&module_path, name, &final_name)
            })
            .collect();

        // Bundled grammar libraries (shipped with lark-rs, mirroring the grammars
        // Python Lark ships under `lark/grammars/`) are resolved from embedded
        // sources, not the filesystem. Everything else is a file import resolved
        // relative to the importing grammar's directory. A *relative*
        // `%import .common ...` (leading dot) is a file, not the library.
        let is_library = !spec.relative && module_path.len() == 1;

        // `common` keeps its dedicated terminal-table path (terminals only, no
        // rules) — it is the hot, heavily-pinned library and copies inline regexes
        // directly.
        if is_library && module_path[0] == "common" {
            for (name, alias) in &names_to_import {
                if let Some(regex) = common_terminals().get(name) {
                    let registered_name = alias.as_deref().unwrap_or(name.as_str());
                    let pat = Pattern::Re(PatternRe::new(regex, 0)?);
                    if !self.terminals.iter().any(|t| t.name == registered_name) {
                        self.terminals
                            .push(TerminalDef::new(registered_name, pat, 0));
                    }
                }
                // Rules from common (e.g., %import common.list) are silently skipped for now.
            }
            return Ok(());
        }

        // Other bundled libraries (`python`, `unicode`, `lark`, …) carry rules as
        // well as terminals, so they route through the same source-parse +
        // closure-copy path as a file import — just with the source embedded in
        // the binary instead of read from disk, and compiled **once per process**
        // (per options) instead of once per `%import` directive.
        if is_library {
            if let Some(src) = bundled_grammar_source(&module_path[0]) {
                let imported = compile_bundled_grammar(
                    &module_path[0],
                    src,
                    self.maybe_placeholders,
                    self.global_keep_all,
                )?;
                return self.copy_imported(&imported, &module_path, &names_to_import);
            }
        }

        self.resolve_file_import(&module_path, &names_to_import)
    }

    /// Resolve a file import: load and parse a sibling `.lark` file through
    /// `load_grammar`, then copy the requested terminals/rules (and, for a rule,
    /// its dependency closure) into this grammar — mirroring Python Lark's
    /// `GrammarLoader.do_import` + `_remove_unused`.
    ///
    /// When in-memory `import_sources` are set (the #47 follow-up: the WASM no-filesystem
    /// case), the same `<base>/a/b/c.lark` path is resolved as a virtual key
    /// into the map instead of the filesystem — so directory layout, nesting,
    /// and error behavior are identical between the two modes.
    fn resolve_file_import(
        &mut self,
        module_path: &[String],
        names_to_import: &[(String, Option<String>)],
    ) -> Result<(), GrammarError> {
        let dotted = module_path.join(".");

        // In-memory mode: the map root is the implicit base, so a grammar built
        // from a bare string can still import (that is the point — WASM has no
        // source location for *any* grammar). The filesystem is never touched.
        if let Some(sources) = &self.import_sources {
            let mut file = self.base_path.clone().unwrap_or_default();
            for comp in module_path {
                file.push(comp);
            }
            file.set_extension("lark");
            let text = sources.get(&virtual_key(&file)).cloned().ok_or_else(|| {
                GrammarError::ImportNotFound {
                    path: dotted.clone(),
                }
            })?;
            let sub_base = file.parent().map(PathBuf::from);
            return self.import_from_source(&text, sub_base, module_path, names_to_import);
        }

        // Resolve `a.b.c` → `<base>/a/b/c.lark`. Without a base path (grammar built
        // from a bare string) a file import is unresolvable, exactly as Python Lark
        // cannot find a relative import with no source location.
        let base = self
            .base_path
            .as_ref()
            .ok_or_else(|| GrammarError::ImportNotFound {
                path: dotted.clone(),
            })?;
        let mut file = base.clone();
        for comp in module_path {
            file.push(comp);
        }
        file.set_extension("lark");
        let text = std::fs::read_to_string(&file).map_err(|_| GrammarError::ImportNotFound {
            path: dotted.clone(),
        })?;

        // The imported grammar's own relative imports resolve against *its*
        // directory, so nested file imports compose.
        let sub_base = file.parent().map(PathBuf::from);
        self.import_from_source(&text, sub_base, module_path, names_to_import)
    }

    /// Parse a grammar `text` read from a sibling file and copy the requested
    /// terminals/rules — and, for a rule, its dependency closure — into this
    /// grammar. `sub_base` is the directory the imported grammar's *own* relative
    /// imports resolve against.
    fn import_from_source(
        &mut self,
        text: &str,
        sub_base: Option<PathBuf>,
        module_path: &[String],
        names_to_import: &[(String, Option<String>)],
    ) -> Result<(), GrammarError> {
        // A pure-terminal source (e.g. `tokens.lark`, `unicode.lark`) has no rule
        // referencing its terminals, so dead-terminal pruning would drop them.
        // Append a probe rule that references every requested name so they survive
        // compilation — the same trick `common_terminals()` uses. The probe is
        // never copied out.
        let probe_body = names_to_import
            .iter()
            .map(|(n, _)| n.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        let probe = format!("{text}\n{IMPORT_PROBE_RULE}: {probe_body}\n");
        let imported = load_grammar_with_sources(
            &probe,
            &[IMPORT_PROBE_RULE.to_string()],
            self.maybe_placeholders,
            self.global_keep_all,
            sub_base,
            self.import_sources.clone(),
        )?;
        self.copy_imported(&imported, module_path, names_to_import)
    }

    /// RC7/#272 import propagation. Copy the requested closure out of an imported
    /// grammar, choosing the **Python-keyed audit shadow** as the source whenever it
    /// is the faithful one to copy — so a reduce/reduce over-share that lives inside
    /// (or is reached through) an imported file is detected exactly as Python detects
    /// it, instead of being masked one `%import` away.
    ///
    /// Two things have to happen for the audit to survive an import boundary:
    ///
    ///  1. **The real (sharing) pass** must learn that an imported grammar over-shares
    ///     internally, so the *parent* loader builds an audit shadow at all. An
    ///     imported grammar carries that signal as `lalr_audit.is_some()` — it built
    ///     its own shadow because it detected an over-share. Propagate it by flipping
    ///     [`recurse_overshare_seen`](GrammarCompiler::recurse_overshare_seen); the
    ///     real parse table still copies the imported grammar's *shared* rules (the
    ///     load-bearing ADR-0013 sharing is untouched).
    ///
    ///  2. **The audit (shadow) pass** must copy the imported rules in their
    ///     *Python-keyed* form. When the imported grammar carries an `lalr_audit`, its
    ///     shadow holds the split (un-shared) recurse helpers Python would mint; copy
    ///     the closure from there. (An imported grammar with no internal over-share
    ///     has `lalr_audit == None`; its real rules are already Python-faithful for
    ///     that file, so the shadow copies them as-is — and a *straddling* over-share,
    ///     where the colliding helpers are minted in the parent from an imported inner
    ///     rule, is re-lowered Python-keyed by the parent shadow itself.)
    fn copy_imported(
        &mut self,
        imported: &Grammar,
        module_path: &[String],
        names_to_import: &[(String, Option<String>)],
    ) -> Result<(), GrammarError> {
        if imported.lalr_audit.is_some() {
            if self.python_keyed_recurse {
                // Shadow pass: copy the Python-keyed split helpers, not the shared
                // real rules — so the masked collision reaches the parent's audit.
                let shadow = imported.lalr_audit.as_deref().unwrap();
                return self.copy_requested(shadow, module_path, names_to_import);
            }
            // Real pass: keep the shared rules, but remember an audit is now needed.
            self.recurse_overshare_seen = true;
        }
        self.copy_requested(imported, module_path, names_to_import)
    }

    /// Copy the requested terminals/rules — and, for a rule, its dependency
    /// closure — out of a compiled imported grammar into this one.
    ///
    /// Dependency names are namespaced under the module path so an imported
    /// rule's private helpers/terminals never collide with the importing
    /// grammar's. Requested names keep their (aliased) name. Matches Python
    /// Lark's `_get_mangle('__'.join(dotted_path), aliases, ...)`.
    fn copy_requested(
        &mut self,
        imported: &Grammar,
        module_path: &[String],
        names_to_import: &[(String, Option<String>)],
    ) -> Result<(), GrammarError> {
        let dotted = module_path.join(".");
        let prefix = module_path.join("__");
        // The per-module merged alias map (#343): every name independently
        // imported from this module, across *all* `%import` directives. A closure
        // symbol present here is left unmangled under its registered final name,
        // mirroring Python's `_get_mangle(prefix, aliases)` (`if s in aliases`).
        // Cloned out so the closure-copy can borrow `self` mutably.
        let module_aliases = self.import_alias_map.get(module_path).cloned();
        for (name, alias) in names_to_import {
            let final_name = alias.clone().unwrap_or_else(|| name.clone());
            if imported.terminals.iter().any(|t| &t.name == name) {
                self.import_terminal(imported, name, &final_name);
            } else if imported.rules.iter().any(|r| &r.origin.name == name) {
                self.import_rule_closure(
                    imported,
                    name,
                    &final_name,
                    &prefix,
                    module_aliases.as_ref(),
                );
            } else {
                return Err(GrammarError::ImportNotFound {
                    path: format!("{dotted}.{name}"),
                });
            }
        }
        Ok(())
    }

    /// Copy a single compiled terminal from an imported grammar under `final_name`.
    fn import_terminal(&mut self, imported: &Grammar, name: &str, final_name: &str) {
        if self.terminals.iter().any(|t| t.name == final_name) {
            return; // already defined locally — don't shadow it
        }
        if let Some(td) = imported.terminals.iter().find(|t| t.name == name) {
            let mut copy = td.clone();
            copy.name = final_name.to_string();
            self.terminals.push(copy);
        }
    }

    /// Copy an imported rule plus every rule/terminal it transitively references.
    /// The requested rule keeps `final_name`; all dependencies are mangled under
    /// `prefix` (underscore-preserving, so transparent `_rules` stay transparent)
    /// to avoid colliding with the importing grammar's own symbols.
    ///
    /// `module_aliases` is the per-module merged import-alias map (#343): a
    /// closure symbol that is *also* independently imported from this module is
    /// left **unmangled** under its registered final name instead of being
    /// prefix-mangled — mirroring Python's `_get_mangle(prefix, aliases)`, whose
    /// `if s in aliases` arm short-circuits the prefix mangle.
    fn import_rule_closure(
        &mut self,
        imported: &Grammar,
        name: &str,
        final_name: &str,
        prefix: &str,
        module_aliases: Option<&HashMap<String, String>>,
    ) {
        // Reachable rule origins (BFS from `name`) and the terminals they touch.
        let mut rule_names: std::collections::HashSet<String> =
            std::collections::HashSet::from([name.to_string()]);
        let mut term_names: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut worklist = vec![name.to_string()];
        while let Some(rn) = worklist.pop() {
            for rule in imported.rules.iter().filter(|r| r.origin.name == rn) {
                for sym in &rule.expansion {
                    match sym {
                        Symbol::Terminal(t) => {
                            term_names.insert(t.name.clone());
                        }
                        Symbol::NonTerminal(nt) => {
                            if rule_names.insert(nt.name.clone()) {
                                worklist.push(nt.name.clone());
                            }
                        }
                    }
                }
            }
        }

        // Don't re-import a rule already defined locally (Python raises; we keep the
        // existing definition rather than duplicate the origin).
        if self.rules.iter().any(|r| r.origin.name == final_name) {
            return;
        }

        // Name map, mirroring Python's `_get_mangle(prefix, aliases)`:
        //   1. `if s in aliases:` → use the registered (final) name, unmangled.
        //      This covers the requested rule itself (`name → final_name`) *and*
        //      any closure symbol independently imported from this module (#343):
        //      a multi-import registers `name → name`, so the reference stays
        //      `NAME`, not `python__NAME`.
        //   2. else mangle under the module prefix (underscore-preserving).
        let rename = |n: &str| -> String {
            if let Some(final_) = module_aliases.and_then(|m| m.get(n)) {
                final_.clone()
            } else if n == name {
                final_name.to_string()
            } else if let Some(rest) = n.strip_prefix('_') {
                format!("_{prefix}__{rest}")
            } else {
                format!("{prefix}__{n}")
            }
        };

        for rule in imported
            .rules
            .iter()
            .filter(|r| rule_names.contains(&r.origin.name))
        {
            let origin = NonTerminal::new(rename(&rule.origin.name));
            // Carry source provenance across the rename: a generated anonymous EBNF
            // helper from the imported grammar (e.g. the `__anon_rep_*` a `(B*)~2`
            // emits) must stay classified as loader-generated after import, or the
            // CYK empty-rule guard (#101, ADR-0024) would reclassify it as a
            // user-written rule and wrongly reject an oracle-accepted import.
            if let Some(kind) = imported.anon_kinds.get(&rule.origin.name).copied() {
                self.anon_kinds.insert(origin.name.clone(), kind);
            }
            let expansion = rule
                .expansion
                .iter()
                .map(|sym| match sym {
                    Symbol::Terminal(t) => Symbol::Terminal(Terminal {
                        name: rename(&t.name),
                        filter_out: t.filter_out,
                    }),
                    Symbol::NonTerminal(nt) => {
                        Symbol::NonTerminal(NonTerminal::new(rename(&nt.name)))
                    }
                })
                .collect();
            // An alias (`-> name`) names the tree node this rule produces; Python
            // Lark mangles it under the module prefix just like a rule origin, so an
            // imported `-> literal` surfaces as `<module>__literal`. Mangle it here
            // too, otherwise the imported grammar's aliased nodes would collide with
            // (or leak into) the importing grammar's namespace.
            let alias = rule.alias.as_deref().map(rename);
            self.rules.push(Rule::new(
                origin,
                expansion,
                alias,
                rule.options.clone(),
                rule.order,
            ));
        }
        for td in imported
            .terminals
            .iter()
            .filter(|t| term_names.contains(&t.name))
        {
            let new_name = rename(&td.name);
            if !self.terminals.iter().any(|t| t.name == new_name) {
                let mut copy = td.clone();
                copy.name = new_name;
                self.terminals.push(copy);
            }
        }
    }
}

/// Embedded source of a bundled grammar library (the equivalents of the grammars
/// Python Lark ships under `lark/grammars/`), keyed by its `%import` module name.
///
/// `common` is handled separately (its dedicated terminal-table fast path); the
/// libraries here carry rules as well as terminals and are imported through the
/// same source-parse + closure-copy path as a sibling-file import. The files are
/// verbatim copies of Python Lark's grammars — a handful of their terminals use
/// lookaround (the `regex` crate has no lookahead/lookbehind), which the lexer
/// **lowers into its DFA** (`docs/LEXER_DFA_PLAN.md`; every bundled lookaround
/// terminal is in scope, `docs/LOOKAROUND_SCOPE.md`), so the grammar text needs no
/// hand-edits. Pinned by `tests/test_stdlib.rs`.
fn bundled_grammar_source(module: &str) -> Option<&'static str> {
    match module {
        "python" => Some(include_str!("../../grammars/python.lark")),
        "unicode" => Some(include_str!("../../grammars/unicode.lark")),
        "lark" => Some(include_str!("../../grammars/lark.lark")),
        _ => None,
    }
}

/// Process-wide cache of compiled bundled libraries. Keyed by the module name
/// plus the two loader options that change the compiled output
/// (`maybe_placeholders` / `keep_all_tokens` alter EBNF expansion); the sources
/// are embedded `&'static str`s, so a cache entry can never go stale.
#[allow(clippy::type_complexity)]
fn bundled_cache() -> &'static Mutex<HashMap<(String, bool, bool), Arc<Grammar>>> {
    static CACHE: OnceLock<Mutex<HashMap<(String, bool, bool), Arc<Grammar>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Compile a bundled library once per process (per options) and cache it.
///
/// A per-`%import` compile could probe just the requested names; to serve every
/// request from one compile, the probe instead references **every importable
/// name** — non-template rules, terminals, `%declare`d terminals, and the
/// library's own `%import` targets (lark.lark registers `NUMBER` via
/// `%import common.SIGNED_INT -> NUMBER`) — so nothing is pruned. The probe is
/// reference-only (no literals, no EBNF operators), so it generates no helper
/// rules and no anonymous terminals: every compiled rule and terminal is
/// byte-identical to what the old per-request probe produced; the only
/// difference is that *more* terminals survive pruning, and `copy_requested`
/// copies only the requested closure anyway.
///
/// The lock is never held across a compile, so a library re-importing another
/// module cannot deadlock. Consequently two threads *can* race the first
/// compile of one key — the duplicate work is benign — but the cache entry is
/// canonical: the loser discards its result and adopts the winner's, so
/// repeated calls with the same key always return the same `Arc`.
fn compile_bundled_grammar(
    module: &str,
    src: &str,
    maybe_placeholders: bool,
    keep_all_tokens: bool,
) -> Result<Arc<Grammar>, GrammarError> {
    let key = (module.to_string(), maybe_placeholders, keep_all_tokens);
    if let Some(g) = bundled_cache().lock().unwrap().get(&key) {
        return Ok(Arc::clone(g));
    }

    let items = GrammarParser::new(src).parse_start()?;
    let mut names: Vec<String> = Vec::new();
    for item in &items {
        match item {
            // A template cannot be referenced bare (it instantiates on demand),
            // so it is not probe-able — and not importable by name either way.
            Item::RuleItem(r) if r.params.is_empty() => names.push(r.name.clone()),
            Item::RuleItem(_) => {}
            Item::TermItem(t) => names.push(t.name.clone()),
            Item::DeclareItem(syms) => {
                for sym in syms {
                    if let Symbol::Terminal(t) = sym {
                        names.push(t.name.clone());
                    }
                }
            }
            Item::ImportItem(spec) => names.extend(spec_final_names(spec)),
            Item::IgnoreItem(_) => {}
        }
    }
    let probe = format!("{src}\n{IMPORT_PROBE_RULE}: {}\n", names.join(" "));
    let grammar = Arc::new(load_grammar_with_base(
        &probe,
        &[IMPORT_PROBE_RULE.to_string()],
        maybe_placeholders,
        keep_all_tokens,
        None,
    )?);
    // Re-check under the lock: if another thread won the compile race, its
    // entry is canonical — never overwrite it (an overwrite would break the
    // same-key ⇒ same-`Arc` guarantee the cache promises).
    let mut cache = bundled_cache().lock().unwrap();
    if let Some(existing) = cache.get(&key) {
        return Ok(Arc::clone(existing));
    }
    cache.insert(key, Arc::clone(&grammar));
    Ok(grammar)
}

/// Lark's `common.lark`, bundled and compiled once into a `name → inline-regex`
/// map for `%import common.X` resolution.
///
/// Rather than maintain a hand-transcribed regex table (which silently drifts from
/// Python Lark), we parse our own bundled copy of `common.lark` through the *same*
/// terminal-algebra path lark-rs uses for user grammars: each terminal's regex is
/// the loader's own compiled output, so a common terminal cannot lex differently
/// from the way the same definition would in a user grammar. The pinned fidelity
/// net is `tests/test_common.rs` (oracles in `fixtures/oracles/common/`).
///
/// The bundled copy carries one documented adaptation (the lookbehind in Lark's
/// escaped-string helpers, which the `regex` crate cannot compile) — see the
/// header of `src/grammars/common.lark`.
pub(super) fn common_terminals() -> &'static HashMap<String, String> {
    use std::sync::OnceLock;
    static MAP: OnceLock<HashMap<String, String>> = OnceLock::new();
    MAP.get_or_init(|| {
        const COMMON_LARK: &str = include_str!("../../grammars/common.lark");
        // Collect every terminal name so a probe rule keeps them all alive through
        // dead-terminal pruning (a terminal only referenced by another terminal is
        // otherwise inlined away and would not be importable).
        let names: Vec<&str> = COMMON_LARK
            .lines()
            .filter_map(|line| {
                let line = line.trim_start();
                let name = line.split_once(':')?.0.trim();
                let is_term_name = !name.is_empty()
                    && name.starts_with(|c: char| c == '_' || c.is_ascii_uppercase())
                    && name
                        .chars()
                        .all(|c| c == '_' || c.is_ascii_uppercase() || c.is_ascii_digit());
                is_term_name.then_some(name)
            })
            .collect();
        let probe = format!("{COMMON_LARK}\n__common_probe: {}\n", names.join(" "));
        let grammar = load_grammar(&probe, &["__common_probe".to_string()], false, false)
            .expect("bundled common.lark must compile");
        grammar
            .terminals
            .into_iter()
            .map(|t| (t.name, t.pattern.to_inline_regex()))
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grammar::load_grammar;

    /// Repeated imports of one bundled library share a single compiled grammar;
    /// distinct loader options compile (and cache) separately, since
    /// `maybe_placeholders` / `keep_all_tokens` change the compiled rules.
    #[test]
    fn bundled_library_compiles_once_per_options() {
        let src = bundled_grammar_source("lark").unwrap();
        let a = compile_bundled_grammar("lark", src, false, false).unwrap();
        let b = compile_bundled_grammar("lark", src, false, false).unwrap();
        assert!(Arc::ptr_eq(&a, &b), "same options must hit the cache");
        let c = compile_bundled_grammar("lark", src, true, false).unwrap();
        assert!(
            !Arc::ptr_eq(&a, &c),
            "different options must compile separately"
        );
    }

    /// A name a bundled library registers via its *own* `%import … -> alias`
    /// stays importable: lark.lark's `NUMBER` is `%import common.SIGNED_INT ->
    /// NUMBER`, which the all-names cache probe must reference or pruning drops
    /// it (the old per-request probe referenced it explicitly).
    #[test]
    fn import_of_a_library_import_alias_resolves() {
        let g = load_grammar(
            "start: NUMBER\n%import lark.NUMBER\n",
            &["start".to_string()],
            false,
            false,
        )
        .unwrap();
        assert!(g.terminals.iter().any(|t| t.name == "NUMBER"));
    }

    /// A `%declare`d (pattern-less) terminal stays importable through the cache
    /// probe: python.lark declares `_INDENT` / `_DEDENT`.
    #[test]
    fn import_of_declared_terminal_resolves() {
        let g = load_grammar(
            "start: _INDENT\n%import python._INDENT\n",
            &["start".to_string()],
            false,
            false,
        )
        .unwrap();
        assert!(g
            .terminals
            .iter()
            .any(|t| t.name == "_INDENT" && t.declared));
    }
}
