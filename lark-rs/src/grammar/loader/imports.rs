//! Phase 3b — `%import` resolution: the bundled grammar libraries (`common`,
//! `python`, `unicode`, `lark`) and sibling-file imports, with rule-closure
//! copying and module-prefix mangling.

use super::ast::ImportSpec;
use super::compiler::GrammarCompiler;
use super::{load_grammar, load_grammar_with_base};
use crate::error::GrammarError;
use crate::grammar::rule::Rule;
use crate::grammar::symbol::{NonTerminal, Symbol, Terminal};
use crate::grammar::terminal::{Pattern, PatternRe, TerminalDef};
use crate::grammar::Grammar;
use std::collections::HashMap;
use std::path::PathBuf;

/// Synthetic start rule appended to an imported file so the requested terminals
/// survive dead-terminal pruning while the file is compiled. Never copied out.
const IMPORT_PROBE_RULE: &str = "__lark_import_probe";

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
        // closure-copy path as a file import — just with the source embedded in the
        // binary instead of read from disk.
        if is_library {
            if let Some(src) = bundled_grammar_source(&module_path[0]) {
                return self.import_from_source(src, None, &module_path, &names_to_import);
            }
        }

        self.resolve_file_import(&module_path, &names_to_import)
    }

    /// Resolve a file import: load and parse a sibling `.lark` file through
    /// `load_grammar`, then copy the requested terminals/rules (and, for a rule,
    /// its dependency closure) into this grammar — mirroring Python Lark's
    /// `GrammarLoader.do_import` + `_remove_unused`.
    fn resolve_file_import(
        &mut self,
        module_path: &[String],
        names_to_import: &[(String, Option<String>)],
    ) -> Result<(), GrammarError> {
        let dotted = module_path.join(".");
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

    /// Parse a grammar `text` (read from a sibling file or embedded as a bundled
    /// library) and copy the requested terminals/rules — and, for a rule, its
    /// dependency closure — into this grammar. `sub_base` is the directory the
    /// imported grammar's *own* relative imports resolve against (`None` for an
    /// embedded library, which can only re-import other libraries, never files).
    fn import_from_source(
        &mut self,
        text: &str,
        sub_base: Option<PathBuf>,
        module_path: &[String],
        names_to_import: &[(String, Option<String>)],
    ) -> Result<(), GrammarError> {
        let dotted = module_path.join(".");
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
        let imported = load_grammar_with_base(
            &probe,
            &[IMPORT_PROBE_RULE.to_string()],
            self.maybe_placeholders,
            self.global_keep_all,
            sub_base,
        )?;

        // Dependency names are namespaced under the module path so an imported
        // rule's private helpers/terminals never collide with the importing
        // grammar's. Requested names keep their (aliased) name. Matches Python
        // Lark's `_get_mangle('__'.join(dotted_path), aliases, ...)`.
        let prefix = module_path.join("__");
        for (name, alias) in names_to_import {
            let final_name = alias.clone().unwrap_or_else(|| name.clone());
            if imported.terminals.iter().any(|t| &t.name == name) {
                self.import_terminal(&imported, name, &final_name);
            } else if imported.rules.iter().any(|r| &r.origin.name == name) {
                self.import_rule_closure(&imported, name, &final_name, &prefix);
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
    fn import_rule_closure(
        &mut self,
        imported: &Grammar,
        name: &str,
        final_name: &str,
        prefix: &str,
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

        // Name map: requested symbol → final name; everything else → mangled.
        let rename = |n: &str| -> String {
            if n == name {
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
