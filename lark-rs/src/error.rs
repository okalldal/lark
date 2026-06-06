use thiserror::Error;

use crate::tree::ParseTree;

#[derive(Debug, Error)]
pub enum LarkError {
    #[error("Grammar error: {0}")]
    Grammar(#[from] GrammarError),
    #[error("Parse error: {0}")]
    Parse(#[from] ParseError),
}

#[derive(Debug, Error, Clone)]
pub enum GrammarError {
    #[error("Undefined terminal: {name}")]
    UndefinedTerminal { name: String },
    #[error("Undefined rule: {name}")]
    UndefinedRule { name: String },
    #[error("Duplicate definition: {name}")]
    DuplicateDefinition { name: String },
    #[error("Invalid regex pattern '{pattern}': {reason}")]
    InvalidRegex { pattern: String, reason: String },
    #[error("Grammar syntax error at line {line}, column {col}: {msg}")]
    SyntaxError {
        line: usize,
        col: usize,
        msg: String,
    },
    #[error("Import not found: {path}")]
    ImportNotFound { path: String },
    #[error("Grammar has unresolvable LALR conflicts:\n{report}")]
    Conflict { report: String },
    #[error("{msg}")]
    Other { msg: String },
}

#[derive(Debug, Error, Clone)]
pub enum ParseError {
    #[error("Unexpected character {ch:?} at line {line}, column {col}\nExpected: {expected}")]
    UnexpectedCharacter {
        ch: char,
        line: usize,
        col: usize,
        pos: usize,
        expected: String,
    },
    #[error(
        "Unexpected token {token:?} at line {line}, column {col}\nExpected one of: {expected:?}"
    )]
    UnexpectedToken {
        token: String,
        token_type: String,
        line: usize,
        col: usize,
        expected: Vec<String>,
    },
    #[error("Unexpected end of input at line {line}, column {col}\nExpected: {expected:?}")]
    UnexpectedEof {
        line: usize,
        col: usize,
        expected: Vec<String>,
    },
    /// A postlex hook (e.g. an [`Indenter`](crate::postlex::Indenter)) rejected the
    /// token stream — most commonly a dedent that does not match any open
    /// indentation level (Python Lark's `DedentError`).
    #[error("Postlex error: {msg}")]
    Postlex { msg: String },
}

/// The result of parsing with error recovery enabled (issue #43).
///
/// Rather than aborting on the first parse error, the recovering driver deletes
/// the offending token(s) and continues, so an editor / LSP gets *both* a tree to
/// work with and the diagnostics to surface. The two halves are:
///
///   * `tree` — a best-effort parse tree. When recovery reached a normal ACCEPT
///     (the surviving tokens form a valid parse), this is the real tree, identical
///     to what Python Lark's `parse(text, on_error=lambda e: True)` returns. When
///     recovery could not reach ACCEPT (e.g. a premature end of input), it is a
///     best-effort scaffold wrapping whatever fragments remain — lark-rs returns a
///     partial tree here instead of raising, where Python re-raises.
///   * `errors` — every error recovered from, in source order. These are the
///     "error nodes": each carries the offending token and its line/column. An
///     empty list means the input parsed cleanly with no recovery.
///
/// lark-rs does not splice error nodes *inline* into the tree: an LR value stack
/// has no symbol/state slot for a synthetic node without a yacc-style `error`
/// production, which Lark's grammar syntax has no way to express. Surfacing the
/// recovered errors alongside the partial tree mirrors Python Lark's own model
/// exactly (its `on_error` recovery likewise drops the bad tokens from the tree
/// and leaves the caller to collect the errors).
#[derive(Debug, Clone)]
pub struct RecoveredTree {
    pub tree: ParseTree,
    pub errors: Vec<ParseError>,
}

impl ParseError {
    /// Format a snippet of the input around the error position.
    pub fn get_context(text: &str, pos: usize, span: usize) -> String {
        let start = pos.saturating_sub(span);
        let end = (pos + span).min(text.len());
        let snippet = &text[start..end];
        let arrow_pos = pos - start;
        format!("{}\n{}^", snippet, " ".repeat(arrow_pos))
    }
}
