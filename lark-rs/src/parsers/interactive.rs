//! Interactive LALR parser (issue #168).
//!
//! A driveable parser: feed tokens one at a time, inspect which terminals the
//! parser would accept next, fork an independent cursor, and resume automated
//! parsing. This is a port of Python Lark's `InteractiveParser`
//! (`lark/parsers/lalr_interactive_parser.py`), so its behaviour is
//! oracle-checkable — a sequence of operations driven against both engines must
//! agree on the `accepts()` set after each step and on the resulting tree.
//!
//! It is a *view* onto the shared state machine: every mutation goes through
//! [`ParserStack::feed_token`](super::lalr::ParserStack::feed_token), the same
//! reduce/shift loop the batch and recovering drivers use (ADR-0015) — there is no
//! second parser here.
//!
//! v1 surface (deliberately exactly Python's operations, no extras — ADR-0026):
//! `feed_token` / `feed` / `accepts` / `feed_eof` / `exhaust_lexer` / `resume` /
//! `fork` / `result` / `pretty`. v1 is wired over the **basic lexer**; the
//! contextual lexer and `on_error`-callback integration are follow-ups.

use std::collections::VecDeque;

use crate::error::ParseError;
use crate::tree::{ParseTree, Token};

use super::lalr::{Feed, LalrParser, ParserStack};

/// A driveable LALR parse in progress. Obtained from
/// [`Lark::parse_interactive`](crate::Lark::parse_interactive).
///
/// Borrows the parser it was created from, so it lives no longer than the
/// [`Lark`](crate::Lark). The raw state/value stacks are deliberately *not* public
/// — callers drive the machine through [`feed`](Self::feed)/[`accepts`](Self::accepts),
/// never by poking the stack.
pub struct InteractiveParser<'a> {
    parser: &'a LalrParser,
    stack: ParserStack,
    /// Remaining lexer tokens (basic-lexer v1), drained by `exhaust_lexer`/`resume`.
    /// Manual `feed`/`feed_token` ignore this — they inject caller-supplied tokens.
    queue: VecDeque<Token>,
    /// The finished tree once a `$END` feed reached ACCEPT (Python's `.result`).
    result: Option<ParseTree>,
}

impl<'a> InteractiveParser<'a> {
    pub(crate) fn new(parser: &'a LalrParser, stack: ParserStack, tokens: Vec<Token>) -> Self {
        InteractiveParser {
            parser,
            stack,
            queue: tokens.into(),
            result: None,
        }
    }

    /// Feed one already-built token, advancing through any REDUCEs to the next SHIFT
    /// or ACCEPT. Returns `Ok(Some(tree))` when this token drove ACCEPT (a `$END`),
    /// `Ok(None)` when it was shifted, and `Err` (the same `UnexpectedToken` a batch
    /// parse would raise) when the parser has no action for it. Mirrors Python's
    /// `feed_token`.
    pub fn feed_token(&mut self, token: Token) -> Result<Option<ParseTree>, ParseError> {
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
    /// returns and the oracle speaks. Resolves the name against the grammar; errors
    /// with the current `accepts()` as the expected set if the name is unknown.
    pub fn feed(&mut self, terminal: &str, value: &str) -> Result<Option<ParseTree>, ParseError> {
        let id =
            self.parser
                .table
                .symbols
                .id(terminal)
                .ok_or_else(|| ParseError::UnexpectedToken {
                    token: value.to_string(),
                    token_type: terminal.to_string(),
                    line: 0,
                    col: 0,
                    expected: self.accepts(),
                })?;
        let mut token = Token::new(terminal, value);
        token.type_id = id;
        self.feed_token(token)
    }

    /// The terminal names that would advance the parser from the current state,
    /// sorted and deterministic — the primary oracle comparand. Mirrors Python's
    /// `accepts()` (computed value-free here: only the state stack is simulated).
    pub fn accepts(&self) -> Vec<String> {
        self.stack.accepts(&self.parser.table)
    }

    /// Feed a synthetic `$END`, finishing the parse. Returns the tree if ACCEPT was
    /// reached. Mirrors Python's `feed_eof`.
    pub fn feed_eof(&mut self) -> Result<Option<ParseTree>, ParseError> {
        self.feed_token(Token::end())
    }

    /// Feed the rest of the (basic-lexer) token stream, **without** a `$END`;
    /// returns the tokens consumed. Mirrors Python's `exhaust_lexer`.
    pub fn exhaust_lexer(&mut self) -> Result<Vec<Token>, ParseError> {
        let mut fed = Vec::with_capacity(self.queue.len());
        while let Some(token) = self.queue.pop_front() {
            let echo = token.clone();
            self.feed_token(token)?;
            fed.push(echo);
        }
        Ok(fed)
    }

    /// Resume fully-automated parsing to completion: feed the rest of the lexer, then
    /// a `$END`. Consumes the cursor and returns the finished tree. Mirrors Python's
    /// `resume_parse`.
    pub fn resume(mut self) -> Result<ParseTree, ParseError> {
        self.exhaust_lexer()?;
        match self.feed_eof()? {
            Some(tree) => Ok(tree),
            None => Err(ParseError::unexpected_eof(0, 0, vec![])),
        }
    }

    /// An independent cursor: feeds on the fork do not affect this one, or vice-versa.
    /// Mirrors Python's `copy()`. Cheap — `accepts()` already avoids cloning tree
    /// values, and this clones the value stack only on an explicit branch.
    pub fn fork(&self) -> InteractiveParser<'a> {
        InteractiveParser {
            parser: self.parser,
            stack: self.stack.clone(),
            queue: self.queue.clone(),
            result: self.result.clone(),
        }
    }

    /// The finished tree once a `$END` feed reached ACCEPT (Python's `.result`).
    pub fn result(&self) -> Option<&ParseTree> {
        self.result.as_ref()
    }

    /// A short debug rendering of the current state and accepted terminals (Python's
    /// `pretty()`). For humans only — not oracle-pinned.
    pub fn pretty(&self) -> String {
        format!(
            "InteractiveParser(state {}, accepts {:?})",
            self.stack.position(),
            self.accepts()
        )
    }
}
