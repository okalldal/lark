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

use crate::error::ParseError;
use crate::grammar::intern::SymbolId;
use crate::lexer::{ContextualLexer, LexerState};
use crate::postlex::{Indenter, IndenterStream};
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

/// Why a [`TokenSource`] could not yield the next token.
///
/// Most sources only fail at the lexer level ([`LexFailure`]), which the parser
/// enriches with its expected-terminal set. A *postlex* source can also fail with
/// an already-formed [`ParseError`] — e.g. the indenter rejecting a bad dedent
/// column — which the parser propagates verbatim.
pub enum SourceError {
    Lex(LexFailure),
    Postlex(ParseError),
}

impl From<LexFailure> for SourceError {
    fn from(f: LexFailure) -> Self {
        SourceError::Lex(f)
    }
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
    fn peek(&mut self, state: usize) -> Result<Token, SourceError>;

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
    fn peek(&mut self, _state: usize) -> Result<Token, SourceError> {
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
    fn peek(&mut self, state: usize) -> Result<Token, SourceError> {
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

// ─── PostlexContextual: contextual lexing with a streaming postlex hook ────────

/// A [`TokenSource`] that lexes lazily with the [`Contextual`] lexer *and* runs a
/// streaming postlex hook ([`IndenterStream`]) over the result, injecting
/// synthetic INDENT/DEDENT tokens between the real ones (issue #67).
///
/// The contextual lexer can't be materialized up front — it narrows the candidate
/// terminals by the live parser state — so the postlex has to sit inside the lazy
/// pull loop rather than after a finished `Vec`. This adapter pulls one real token
/// at a time from the inner lexer (at the current parser state), feeds it to the
/// indenter, and serves tokens from the indenter's output queue. The indenter's
/// newline terminal is forced into every state's scanner (`always_accept`, set up
/// in `build_frontend`) so the lazy lexer still emits the newlines it measures
/// indentation from — mirroring Python Lark's `PostLex.always_accept`.
///
/// Note the inner lexer is advanced *eagerly* as each real token is pulled, while
/// the `peek`/`advance` the parser sees operate on the indenter's output queue.
/// The two are decoupled: a real token is only pulled once the queue drains, so
/// every pull happens at the parser state that follows the previously injected
/// tokens — exactly the state at which that token must be lexed.
pub struct PostlexContextual<'a> {
    inner: Contextual<'a>,
    stream: IndenterStream<'a>,
    /// The token currently offered to the parser (cached across REDUCEs).
    current: Option<Token>,
    /// The real `$END` token, held back until the indenter's trailing DEDENTs have
    /// been flushed (Python emits them *before* end of input).
    end_token: Option<Token>,
    /// Whether [`IndenterStream::finish`] has run (the EOF flush happens once).
    finished: bool,
}

impl<'a> PostlexContextual<'a> {
    pub(crate) fn new(inner: Contextual<'a>, stream: IndenterStream<'a>) -> Self {
        PostlexContextual {
            inner,
            stream,
            current: None,
            end_token: None,
            finished: false,
        }
    }
}

impl<'a> TokenSource for PostlexContextual<'a> {
    fn peek(&mut self, state: usize) -> Result<Token, SourceError> {
        if let Some(tok) = &self.current {
            return Ok(tok.clone());
        }
        loop {
            // Serve any token the indenter has already queued.
            if let Some(tok) = self.stream.pop() {
                self.current = Some(tok.clone());
                return Ok(tok);
            }
            // Queue empty. If the inner lexer is exhausted, flush trailing DEDENTs
            // once (they land before `$END`), then serve the held-back `$END`.
            if let Some(end) = &self.end_token {
                if !self.finished {
                    self.stream.finish();
                    self.finished = true;
                    continue;
                }
                let end = end.clone();
                self.current = Some(end.clone());
                return Ok(end);
            }
            // Pull the next real token from the contextual lexer at the current
            // parser state, advancing the inner stream past it.
            let tok = self.inner.peek(state)?;
            self.inner.advance();
            if tok.type_id == SymbolId::END {
                self.end_token = Some(tok);
            } else {
                self.stream.feed(tok).map_err(SourceError::Postlex)?;
            }
        }
    }

    fn advance(&mut self) {
        self.current = None;
    }
}

/// Drive the LALR parser over the contextual lexer with a streaming [`Indenter`]
/// postlex hook. Resolves the indenter's `%declare`d ids up front, then loops the
/// shared parser driver against a [`PostlexContextual`] source.
pub fn postlex_contextual_source<'a>(
    text: &'a str,
    lexer: &'a ContextualLexer,
    postlex: &'a Indenter,
    symbols: &crate::grammar::intern::SymbolTable,
) -> Result<PostlexContextual<'a>, ParseError> {
    let stream = IndenterStream::new(postlex, symbols)?;
    Ok(PostlexContextual::new(Contextual::new(text, lexer), stream))
}
