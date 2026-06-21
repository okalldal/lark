//! Interactive LALR parser (issue #168).
//!
//! A driveable parser: feed tokens one at a time, inspect which terminals the
//! parser would accept next, fork an independent cursor, and resume automated
//! parsing. It ports the **oracle-backed subset** of Python Lark's
//! `InteractiveParser` (`lark/parsers/lalr_interactive_parser.py`) — `feed_token`,
//! `accepts`, `feed_eof`, `exhaust_lexer`, `resume`, `copy` (here `fork`),
//! `pretty`, `result` — plus one ergonomic wrapper, [`feed`](InteractiveParser::feed)
//! `(name, value)` over `feed_token`. Python's `choices()` / `__eq__` /
//! `ImmutableInteractiveParser` are not part of v1 (ADR-0026: only what the oracle
//! grounds, plus the named convenience). The shared operations are differentially
//! tested against Python (`tests/test_interactive.rs`).
//!
//! It is a *view* onto the shared state machine: every mutation goes through
//! [`ParserStack::feed_token`](super::lalr::ParserStack::feed_token), the same
//! reduce/shift loop the batch and recovering drivers use (ADR-0015) — there is no
//! second parser here.
//!
//! **Lazy lexing.** Like Python, the lexer is driven *as the caller drives the
//! parser*, not up front: `parse_interactive` over broken editor text succeeds, and
//! an un-lexable character surfaces only when `exhaust_lexer`/`resume` reaches it.
//! Manual `feed`/`feed_token` inject caller-supplied tokens and ignore the lexer.
//! v1 lexes with the **basic** lexer; the contextual lexer is a follow-up.

use crate::error::ParseError;
use crate::grammar::intern::SymbolId;
use crate::lexer::BasicLexer;
use crate::tree::{ParseTree, Token};

use super::lalr::{Feed, LalrParser, ParserStack};

/// A driveable LALR parse in progress. Obtained from
/// [`Lark::parse_interactive`](crate::Lark::parse_interactive).
///
/// Borrows the parser (and lexer) it was created from, so it lives no longer than
/// the [`Lark`](crate::Lark). The raw state/value stacks are deliberately *not*
/// public — callers drive the machine through [`feed`](Self::feed) /
/// [`accepts`](Self::accepts), never by poking the stack.
pub struct InteractiveParser<'a> {
    parser: &'a LalrParser,
    /// The basic lexer driven lazily by `exhaust_lexer`/`resume`. `None` leaves
    /// those ops a no-op (manual-feed-only); v1 always wires `Some`.
    lexer: Option<&'a BasicLexer>,
    stack: ParserStack,
    /// Owned input, lexed lazily from a hand-tracked cursor (avoids a
    /// self-referential borrow of a `LexerState`). `line`/`col` are 1-based to match
    /// [`LexerState`](crate::lexer::LexerState).
    text: String,
    pos: usize,
    line: usize,
    col: usize,
    /// The finished tree once a `$END` feed reached ACCEPT (Python's `.result`).
    result: Option<ParseTree>,
}

impl<'a> InteractiveParser<'a> {
    pub(crate) fn new(
        parser: &'a LalrParser,
        lexer: Option<&'a BasicLexer>,
        stack: ParserStack,
        text: String,
    ) -> Self {
        InteractiveParser {
            parser,
            lexer,
            stack,
            text,
            pos: 0,
            line: 1,
            col: 1,
            result: None,
        }
    }

    /// Feed one token, advancing through any REDUCEs to the next SHIFT or ACCEPT.
    /// Returns `Ok(Some(tree))` when this token drove ACCEPT (a `$END`), `Ok(None)`
    /// when it was shifted, and `Err` (the same `UnexpectedToken` a batch parse would
    /// raise) when the parser has no action for it. Mirrors Python's `feed_token`.
    ///
    /// A caller-built `Token::new("NUMBER", "1")` carries no interned id; like Python
    /// (which dispatches on `token.type`, a name), this resolves the terminal *name*
    /// to its id when the token's `type_id` is unset, so a hand-made token Just Works.
    /// Lexer-produced tokens and `$END` already carry a real id and are used as-is.
    pub fn feed_token(&mut self, mut token: Token) -> Result<Option<ParseTree>, ParseError> {
        if token.type_id == SymbolId::UNSET {
            match self.parser.table.symbols.id(&token.type_) {
                Some(id) => token.type_id = id,
                None => {
                    return Err(ParseError::UnexpectedToken {
                        token: token.value.clone(),
                        token_type: token.type_.clone(),
                        line: token.line,
                        col: token.column,
                        expected: self.accepts(),
                    })
                }
            }
        }
        match self.stack.feed_token(&self.parser.table, &token) {
            Feed::Shifted => Ok(None),
            Feed::Accepted(tree) => {
                self.result = Some(tree.clone());
                Ok(Some(tree))
            }
            Feed::Error(e) => Err(e),
            Feed::NoAction => Err(self.parser.unexpected(self.stack.position(), &token)),
        }
    }

    /// Build and feed a token by terminal *name* — the form [`accepts`](Self::accepts)
    /// returns. A thin ergonomic wrapper over [`feed_token`](Self::feed_token)
    /// (`feed("NUMBER", "1")` ≡ `feed_token(Token::new("NUMBER", "1"))`); the name is
    /// resolved by `feed_token`.
    pub fn feed(&mut self, terminal: &str, value: &str) -> Result<Option<ParseTree>, ParseError> {
        self.feed_token(Token::new(terminal, value))
    }

    /// The terminal names that would advance the parser from the current state,
    /// sorted and deterministic — the primary oracle comparand. Mirrors Python's
    /// `accepts()` (computed value-free here: only the state stack is simulated).
    pub fn accepts(&self) -> Vec<String> {
        self.stack.accepts(&self.parser.table)
    }

    /// Feed a synthetic `$END` at the current input position, finishing the parse.
    /// Returns the tree if ACCEPT was reached. Mirrors Python's `feed_eof` (which
    /// likewise borrows the position from where lexing left off).
    pub fn feed_eof(&mut self) -> Result<Option<ParseTree>, ParseError> {
        self.feed_token(self.eof_token())
    }

    /// Feed the rest of the (basic-lexer) token stream, **without** a `$END`;
    /// returns the tokens consumed. An un-lexable character raises here (Python's
    /// lazy `UnexpectedCharacters`), not at construction. Mirrors `exhaust_lexer`.
    pub fn exhaust_lexer(&mut self) -> Result<Vec<Token>, ParseError> {
        let mut fed = Vec::new();
        loop {
            let token = self.next_lexed()?;
            if token.type_id == SymbolId::END {
                break; // never feed `$END` here; the cursor sits at end for feed_eof
            }
            let echo = token.clone();
            self.feed_token(token)?;
            fed.push(echo);
        }
        Ok(fed)
    }

    /// Resume fully-automated parsing to completion: feed the rest of the lexer, then
    /// a `$END`. Returns the finished tree. Mirrors Python's `resume_parse`.
    ///
    /// Rust API shape (not a Python-parity claim): this **consumes** the cursor
    /// (`self`), since after resuming to `$END` there is nothing more to drive — you
    /// wanted the tree, not the handle. The step-wise ops (`feed_token`/`feed_eof`)
    /// keep `&mut self`. Fork first (`p.fork().resume()`) if you need the cursor back.
    pub fn resume(mut self) -> Result<ParseTree, ParseError> {
        self.exhaust_lexer()?;
        match self.feed_eof()? {
            Some(tree) => Ok(tree),
            None => Err(ParseError::unexpected_eof(self.line, self.col, vec![])),
        }
    }

    /// An independent cursor: feeds on the fork do not affect this one, or vice-versa.
    /// Mirrors Python's `copy()`. Cheap — `accepts()` already avoids cloning tree
    /// values; this clones the value stack only on an explicit branch.
    pub fn fork(&self) -> InteractiveParser<'a> {
        InteractiveParser {
            parser: self.parser,
            lexer: self.lexer,
            stack: self.stack.clone(),
            text: self.text.clone(),
            pos: self.pos,
            line: self.line,
            col: self.col,
            result: self.result.clone(),
        }
    }

    /// The finished tree once a `$END` feed reached ACCEPT (Python's `.result`).
    pub fn result(&self) -> Option<&ParseTree> {
        self.result.as_ref()
    }

    /// A short debug rendering of the current state and accepted terminals (Python's
    /// `pretty()` renders `choices()`; we render `accepts()` — debug only, not
    /// oracle-pinned).
    pub fn pretty(&self) -> String {
        format!(
            "InteractiveParser(state {}, accepts {:?})",
            self.stack.position(),
            self.accepts()
        )
    }

    // ─── Lazy basic-lexer cursor ─────────────────────────────────────────────

    /// The synthetic `$END` token at the current cursor (its position is where lexing
    /// left off — after `exhaust_lexer`, the end of input; before any drive, the
    /// start). This is what fixes premature-EOF diagnostics carrying a real location.
    fn eof_token(&self) -> Token {
        Token::end().with_position(self.line, self.col, self.pos, self.pos)
    }

    /// Lex the next non-ignored token, advancing the cursor, or the positioned `$END`
    /// at end of input. `Err(UnexpectedCharacter)` at an un-lexable character (Python
    /// raises here rather than recovering — that is the recovery path's job, not the
    /// interactive parser's). A no-op `$END` when there is no lexer wired.
    fn next_lexed(&mut self) -> Result<Token, ParseError> {
        let Some(lexer) = self.lexer else {
            return Ok(self.eof_token());
        };
        loop {
            if self.pos >= self.text.len() {
                return Ok(self.eof_token());
            }
            match lexer.next_token_at(&self.text, self.pos, self.line, self.col) {
                Ok(token) => {
                    self.pos = token.end_pos;
                    self.line = token.end_line;
                    self.col = token.end_column;
                    if lexer.is_ignored(token.type_id) {
                        continue;
                    }
                    return Ok(token);
                }
                Err(()) => {
                    let ch = self.text[self.pos..].chars().next().unwrap();
                    return Err(ParseError::UnexpectedCharacter {
                        ch,
                        line: self.line,
                        col: self.col,
                        pos: self.pos,
                        expected: "any token".to_string(),
                    });
                }
            }
        }
    }
}
