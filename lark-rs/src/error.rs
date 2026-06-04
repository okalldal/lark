use thiserror::Error;

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
    #[error("{report}")]
    Collision { report: String },
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
