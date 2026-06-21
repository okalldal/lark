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
        base_path,
        import_sources,
    );
    compiler.process_items(items)?;
    compiler.compile()
}
