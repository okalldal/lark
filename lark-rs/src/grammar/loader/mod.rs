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
use compiler::GrammarCompiler;
use parser::GrammarParser;
use std::path::PathBuf;

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
    let mut parser = GrammarParser::new(grammar_text);
    let items = parser.parse_start()?;

    let mut compiler = GrammarCompiler::new(
        start.to_vec(),
        maybe_placeholders,
        keep_all_tokens,
        base_path,
    );
    compiler.process_items(items)?;
    compiler.compile()
}
