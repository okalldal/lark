//! Parses `.lark` EBNF grammar text into a compiled [`Grammar`].
//!
//! The `.lark` format:
//! - Lowercase names are rules; UPPERCASE names are terminals
//! - Rule modifiers: `!rule` (keep all tokens), `?rule` (inline if single child)
//! - EBNF operators: `+`, `*`, `?`, `|`
//! - Repetition: `expr~n` (exactly n), `expr~n..m` (n to m)
//! - Optional groups: `[...]`
//! - Inline rules: `(...)` group as anonymous rule
//! - Range: `"a".."z"`
//! - Aliases: `expansion -> alias_name`
//! - Directives: `%ignore`, `%import`, `%declare`, `%override`, `%extend`
//!
//! Loading is a staged pipeline, one phase per submodule:
//!
//! ```text
//! .lark text
//!   → tokenizer   Tok stream (hand-written lexer)
//!   → parser      recursive descent → ast (RawRule / RawTerm / ImportSpec)
//!   → compiler    GrammarCompiler stages the items, then delegates:
//!       imports     %import resolution (bundled libraries + sibling files)
//!       terminals   terminal-algebra → regex / PatternStr classification
//!       ebnf        rule bodies: EBNF expansion, distribution, helper sharing
//!       templates   parameterized template instantiation
//!   → Grammar     { rules, terminals, ignore, start }  (surface, string-named)
//! ```

mod ast;
mod audit;
mod compiler;
mod ebnf;
mod imports;
mod parser;
mod templates;
mod terminals;
mod tokenizer;

use super::Grammar;
use crate::error::GrammarError;
pub use compiler::AnonKind;
use compiler::GrammarCompiler;
use parser::GrammarParser;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

/// Convert grammar text to a compiled [`Grammar`].
///
/// File imports (`%import .module (...)`) cannot be resolved through this entry
/// point — it carries no base path. Use [`load_grammar_with_base`] when the
/// grammar may import from sibling files.
pub fn load_grammar(
    grammar_text: &str,
    start: &[String],
    maybe_placeholders: bool,
    keep_all_tokens: bool,
) -> Result<Grammar, GrammarError> {
    load_grammar_with_base(
        grammar_text,
        start,
        maybe_placeholders,
        keep_all_tokens,
        None,
    )
}

/// Like [`load_grammar`], but `base_path` is the directory that relative file
/// imports (`%import .module (...)`) resolve against — the directory of the
/// importing grammar's own file, mirroring Python Lark's `GrammarLoader`.
pub fn load_grammar_with_base(
    grammar_text: &str,
    start: &[String],
    maybe_placeholders: bool,
    keep_all_tokens: bool,
    base_path: Option<PathBuf>,
) -> Result<Grammar, GrammarError> {
    load_grammar_with_sources(
        grammar_text,
        start,
        maybe_placeholders,
        keep_all_tokens,
        base_path,
        None,
    )
}

/// Like [`load_grammar_with_base`], but with optional in-memory grammar sources
/// for relative file imports (the #47 follow-up): a map of virtual `/`-separated path
/// (e.g. `"dir/tokens.lark"`) → grammar text. When `import_sources` is `Some`,
/// `%import .module (...)` resolves against the map *only* — the filesystem is
/// never consulted — with `base_path` as an optional virtual prefix. This is how
/// environments without a filesystem (WASM, #47) supply sibling grammars; an
/// imported grammar's own relative imports resolve against its virtual
/// directory, exactly like the filesystem path.
pub fn load_grammar_with_sources(
    grammar_text: &str,
    start: &[String],
    maybe_placeholders: bool,
    keep_all_tokens: bool,
    base_path: Option<PathBuf>,
    import_sources: Option<Arc<HashMap<String, String>>>,
) -> Result<Grammar, GrammarError> {
    load_grammar_inner(
        grammar_text,
        start,
        maybe_placeholders,
        keep_all_tokens,
        base_path,
        import_sources,
        None,
    )
}

/// The synthetic name the import/terminal-table probe rule is injected under.
///
/// It is **deliberately a `__`-leading name** — a name token the grammar
/// tokenizer *rejects* at parse (`reject_double_underscore_name`, #361, matching
/// Python Lark's `RULE`/`TERMINAL` = `_?[a-z]…`/`_?[A-Z]…`). Because no user
/// grammar *source* can lex a `__`-leading name, this name can never collide with
/// a rule a user authored — and the probe is therefore injected straight into the
/// **AST** (see [`ProbeSpec`] / [`load_grammar_with_probe`]), bypassing the
/// tokenizer that would otherwise reject its own name. Appending it as *source*
/// text instead (the pre-correction approach) reserved the valid, user-authorable
/// `_lark_import_probe`, which a legal imported grammar could already define — a
/// duplicate-definition reject-where-Python-accepts regression (#361/#446).
pub(super) const IMPORT_PROBE_RULE: &str = "__lark_import_probe";

/// A synthetic probe rule to inject into a grammar's AST after parsing, so the
/// listed names survive dead-rule/dead-terminal pruning while the grammar is
/// compiled — without writing a user-authorable rule into the grammar *source*.
///
/// The rule is built directly as AST (never lexed), so its [`IMPORT_PROBE_RULE`]
/// name — which the tokenizer would reject — is safe, and it can never collide
/// with any user-authored name (user source cannot spell a `__`-leading name).
/// Each listed name is referenced as a terminal or a rule by the *same* case rule
/// the tokenizer uses (an uppercase first letter, optionally behind a single `_`,
/// is a terminal; otherwise a rule), so the injected body classifies identically
/// to the old appended-source probe. The probe rule is never copied out of the
/// compiled grammar — only the explicitly requested names are.
pub(super) struct ProbeSpec<'a> {
    pub(super) names: &'a [String],
}

/// Like [`load_grammar_with_sources`], but injects a [`ProbeSpec`] probe rule into
/// the parsed AST under [`IMPORT_PROBE_RULE`] before compiling. The caller passes
/// that same name as the `start` symbol so the probe rule (and thus every name it
/// references) survives pruning. Used by `%import` resolution and the bundled
/// terminal-table compile (`imports.rs`).
pub(super) fn load_grammar_with_probe(
    grammar_text: &str,
    start: &[String],
    maybe_placeholders: bool,
    keep_all_tokens: bool,
    base_path: Option<PathBuf>,
    import_sources: Option<Arc<HashMap<String, String>>>,
    probe: ProbeSpec<'_>,
) -> Result<Grammar, GrammarError> {
    load_grammar_inner(
        grammar_text,
        start,
        maybe_placeholders,
        keep_all_tokens,
        base_path,
        import_sources,
        Some(probe),
    )
}

/// Build the synthetic probe [`ast::Item`] (a `RawRule` named [`IMPORT_PROBE_RULE`])
/// whose single alternative references every name in `names`. A name is classified
/// terminal-vs-rule exactly as the grammar tokenizer's dispatch does: an uppercase
/// first letter — optionally behind a single leading `_` — is a terminal; anything
/// else (lowercase, or `_` followed by a non-uppercase) is a rule. This reproduces
/// the classification the old appended-source probe got from the lexer, so the only
/// behavioral change is that the probe's *name* is now un-lexable (and thus
/// collision-proof) rather than a valid name a user could also define.
fn make_probe_item(names: &[String]) -> ast::Item {
    fn is_terminal_name(name: &str) -> bool {
        let mut bytes = name.bytes();
        match bytes.next() {
            Some(b) if b.is_ascii_uppercase() => true,
            Some(b'_') => matches!(bytes.next(), Some(b) if b.is_ascii_uppercase()),
            _ => false,
        }
    }
    let expansion = names
        .iter()
        .map(|n| {
            let value = if is_terminal_name(n) {
                ast::Value::Terminal(n.clone())
            } else {
                ast::Value::Rule(n.clone())
            };
            ast::Expr::Value(value)
        })
        .collect();
    ast::Item::RuleItem(ast::RawRule {
        name: IMPORT_PROBE_RULE.to_string(),
        modifiers: String::new(),
        params: Vec::new(),
        priority: 0,
        expansions: vec![ast::AliasedExpansion {
            expansion,
            alias: None,
        }],
        directive: ast::Directive::Plain,
    })
}

#[allow(clippy::too_many_arguments)]
fn load_grammar_inner(
    grammar_text: &str,
    start: &[String],
    maybe_placeholders: bool,
    keep_all_tokens: bool,
    base_path: Option<PathBuf>,
    import_sources: Option<Arc<HashMap<String, String>>>,
    probe: Option<ProbeSpec<'_>>,
) -> Result<Grammar, GrammarError> {
    let mut parser = GrammarParser::new(grammar_text);
    let mut items = parser.parse_start()?;
    // Inject the probe rule into the AST *after* parsing, so its un-lexable
    // `__`-leading name (see `IMPORT_PROBE_RULE`) bypasses the name-token lexer that
    // would reject it — and can never collide with a user-authored rule of the same
    // name (user source cannot lex a `__`-leading name).
    if let Some(probe) = probe {
        items.push(make_probe_item(probe.names));
    }

    let mut compiler = GrammarCompiler::new(
        start.to_vec(),
        maybe_placeholders,
        keep_all_tokens,
        base_path.clone(),
        import_sources.clone(),
    );
    // Compile the real grammar (load-bearing EBNF helper sharing — ADR-0013). If a
    // recurse helper was over-shared relative to Python Lark's per-inner-AST
    // minting, build a Python-faithful **audit shadow** (recurse helpers keyed on
    // the inner source-AST) and attach it; the LALR build runs the conflict
    // detector over the shadow to surface the reduce/reduce collision the sharing
    // masks, without un-sharing the real cache (RC7/#272).
    //
    // The shadow re-lowers a clone of the parsed `items`, so it can only ever apply
    // to a grammar that mints at least one recurse helper — i.e. one that uses a
    // `*`/`+`/`~` operator — **or** that `%import`s a grammar which might carry its
    // own over-share (RC7/#272 import propagation): an imported over-share flips
    // `recurse_overshare_seen` from inside `resolve_import`, even when the parent body
    // has no repeat of its own (`start: bad`, `bad` imported). We cheap-scan for
    // either condition first and only *clone* the AST when an audit could possibly be
    // needed, so a repetition-free, import-free grammar (and the common Earley/CYK
    // load that never reaches the LALR audit) pays nothing.
    let could_overshare = items_need_audit_clone(&items);
    let items_for_audit = could_overshare.then(|| items.clone());
    compiler.process_items(items)?;
    let recurse_overshare_seen = compiler.audit.overshare_seen();
    let mut grammar = compiler.compile()?;
    // `overshare_seen()` can only be set when `could_overshare` was true (a
    // repeat operator in this body, or an `%import` that could carry one), so
    // `items_for_audit` is `Some` whenever an audit is actually needed.
    if recurse_overshare_seen {
        let items_for_audit =
            items_for_audit.expect("over-share implies a repeat operator or an import");
        let mut shadow_compiler = GrammarCompiler::new(
            start.to_vec(),
            maybe_placeholders,
            keep_all_tokens,
            base_path,
            import_sources,
        );
        shadow_compiler.audit.set_python_keyed();
        shadow_compiler.process_items(items_for_audit)?;
        let shadow = shadow_compiler.compile()?;
        grammar.lalr_audit = Some(Box::new(shadow));
    }
    Ok(grammar)
}

/// Cheap syntactic pre-check for the RC7/#272 recurse-over-share audit: whether the
/// AST could possibly produce a recurse over-share, so the loader must clone `items`
/// to be able to re-lower a Python-keyed audit shadow. True when either:
///
///  - a rule body uses a `*`/`+`/`~` operator (an [`Expr::Repeat`]) — this body can
///    mint a recurse helper and over-share directly; or
///  - the grammar `%import`s another grammar — an imported (or import-straddling)
///    over-share flips `recurse_overshare_seen` from inside `resolve_import`, even
///    when this body has no repeat of its own (`start: bad`, `bad` imported).
///
/// A repetition-free *and* import-free grammar can never over-share, so the common
/// load (and every Earley/CYK load that never reaches the LALR audit) skips the AST
/// clone entirely. Walks only the raw rule bodies / item kinds; it lowers nothing.
fn items_need_audit_clone(items: &[ast::Item]) -> bool {
    fn expr_has_repeat(e: &ast::Expr) -> bool {
        match e {
            ast::Expr::Repeat { .. } => true,
            ast::Expr::Value(_) => false,
            ast::Expr::Group(alts) | ast::Expr::Maybe(alts) => {
                alts.iter().any(|a| a.expansion.iter().any(expr_has_repeat))
            }
        }
    }
    items.iter().any(|item| match item {
        ast::Item::RuleItem(r) => r
            .expansions
            .iter()
            .any(|a| a.expansion.iter().any(expr_has_repeat)),
        // An import could carry (or, straddling, contribute the inner rule of) an
        // over-share the cheap pre-scan cannot see in this body — clone to be safe.
        ast::Item::ImportItem(_) => true,
        _ => false,
    })
}
