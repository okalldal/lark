//! The lexer⇄parser interface.
//!
//! A [`TokenSource`] yields the next actionable token for the parser's current
//! lexer context, hiding *how* that token is produced. Two implementations cover
//! the LALR frontends:
//!
//!   * [`PreLexed`] replays a token vector produced up front by the basic lexer.
//!   * [`Contextual`] lexes lazily, one token at a time, asking the contextual
//!     lexer for only the terminals valid in the current parser state.
//!
//! Collapsing both behind one trait lets the LALR driver ([`LalrParser::run`])
//! be a single state-machine loop instead of two near-identical ones, and gives
//! a future Earley driver a ready-made input interface.
//!
//! [`LalrParser::run`]: super::lalr::LalrParser

use crate::lexer::{ContextualLexer, LexerState};
use crate::tree::Token;

/// The token source could not tokenize the input at the current position.
///
/// It carries only what the *lexer* knows (the offending character and where it
/// is); the parser turns this into a full error, enriched with the terminals it
/// expected in the current state — knowledge only the parser has.
pub struct LexFailure {
    pub ch: char,
    pub line: usize,
    pub col: usize,
}

/// Yields tokens to the parser for a given lexer context.
///
/// `state` is the lexer-context key: for the LALR parser it is the current
/// parser state, which the contextual lexer uses to narrow the candidate
/// terminals. [`PreLexed`] ignores it.
pub trait TokenSource {
    /// The current token to act on. It is *not* consumed — a REDUCE re-reads the
    /// same token, while a SHIFT consumes it via [`advance`](Self::advance).
    /// Ignored terminals (whitespace, comments) are skipped transparently. At end
    /// of input, yields the synthetic `$END` token.
    fn peek(&mut self, state: usize) -> Result<Token, LexFailure>;

    /// Consume the current token (called on SHIFT).
    fn advance(&mut self);
}

// ─── PreLexed: replay a fully lexed token vector ──────────────────────────────

/// A source backed by a token vector the basic lexer produced up front.
pub struct PreLexed {
    tokens: std::vec::IntoIter<Token>,
    current: Option<Token>,
}

impl PreLexed {
    pub fn new(tokens: Vec<Token>) -> Self {
        PreLexed {
            tokens: tokens.into_iter(),
            current: None,
        }
    }
}

impl TokenSource for PreLexed {
    fn peek(&mut self, _state: usize) -> Result<Token, LexFailure> {
        if self.current.is_none() {
            // Past the end, keep yielding `$END` (the driver stops at ACCEPT).
            self.current = Some(self.tokens.next().unwrap_or_else(Token::end));
        }
        Ok(self.current.clone().unwrap())
    }

    fn advance(&mut self) {
        self.current = None;
    }
}

// ─── Contextual: lex lazily, narrowing terminals by parser state ──────────────

/// A source that lexes lazily with the contextual lexer, attempting only the
/// terminals valid in the current parser state. The lexed token is cached and
/// reused across REDUCEs (which do not consume input) until a SHIFT advances it.
pub struct Contextual<'a> {
    lexer: &'a ContextualLexer,
    state: LexerState<'a>,
    current: Option<Token>,
}

impl<'a> Contextual<'a> {
    pub fn new(text: &'a str, lexer: &'a ContextualLexer) -> Self {
        Contextual {
            lexer,
            state: LexerState::new(text),
            current: None,
        }
    }

    /// Lex the next non-ignored token for `parser_state`, or the `$END` token at
    /// end of input.
    fn lex_next(&mut self, parser_state: usize) -> Result<Token, LexFailure> {
        loop {
            if self.state.is_done() {
                return Ok(Token::end().with_position(
                    self.state.line,
                    self.state.col,
                    self.state.pos,
                    self.state.pos,
                ));
            }
            let matched = self.lexer.next_token(
                self.state.text,
                self.state.pos,
                parser_state,
                self.state.line,
                self.state.col,
            );
            match matched {
                // Ignored terminal (whitespace, comment): consume and keep going.
                Ok(Some(tok)) if self.lexer.is_ignored(tok.type_id) => {
                    self.state.advance_by(tok.value.len());
                }
                Ok(Some(tok)) => return Ok(tok),
                // No scanner for this state, or no terminal matched here — a lex
                // failure the parser will enrich with its expected-terminal set.
                Ok(None) | Err(_) => {
                    let ch = self.state.text[self.state.pos..].chars().next().unwrap();
                    return Err(LexFailure {
                        ch,
                        line: self.state.line,
                        col: self.state.col,
                    });
                }
            }
        }
    }
}

impl<'a> TokenSource for Contextual<'a> {
    fn peek(&mut self, state: usize) -> Result<Token, LexFailure> {
        if self.current.is_none() {
            self.current = Some(self.lex_next(state)?);
        }
        Ok(self.current.clone().unwrap())
    }

    fn advance(&mut self) {
        if let Some(tok) = self.current.take() {
            self.state.advance_by(tok.value.len());
        }
    }
}
