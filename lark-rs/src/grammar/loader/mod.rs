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
    let mut parser = GrammarParser::new(grammar_text);
    let items = parser.parse_start()?;

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
    // `*`/`+`/`~` operator. We cheap-scan for that first and only *clone* the AST
    // when it could possibly over-share, so a repetition-free grammar (and the
    // common Earley/CYK load that never reaches the LALR audit) pays nothing.
    let could_overshare = items_contain_repeat(&items);
    let items_for_audit = could_overshare.then(|| items.clone());
    compiler.process_items(items)?;
    let recurse_overshare_seen = compiler.recurse_overshare_seen;
    let mut grammar = compiler.compile()?;
    // `recurse_overshare_seen` can only be set when `could_overshare` was true, so
    // `items_for_audit` is `Some` whenever an audit is actually needed.
    if recurse_overshare_seen {
        let items_for_audit = items_for_audit.expect("over-share implies a repeat operator");
        let mut shadow_compiler = GrammarCompiler::new(
            start.to_vec(),
            maybe_placeholders,
            keep_all_tokens,
            base_path,
            import_sources,
        );
        shadow_compiler.python_keyed_recurse = true;
        shadow_compiler.process_items(items_for_audit)?;
        let shadow = shadow_compiler.compile()?;
        grammar.lalr_audit = Some(Box::new(shadow));
    }
    Ok(grammar)
}

/// Cheap syntactic pre-check for the RC7/#272 recurse-over-share audit: whether any
/// rule body in `items` uses a `*`/`+`/`~` operator (an [`Expr::Repeat`]). A grammar
/// with no repetition can never mint a recurse helper, so it can never over-share —
/// letting [`load_grammar_with_sources`] skip cloning the AST entirely for the
/// common repetition-free / Earley-only load. Walks only the raw rule bodies; it
/// does not lower anything.
fn items_contain_repeat(items: &[ast::Item]) -> bool {
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
        _ => false,
    })
}
