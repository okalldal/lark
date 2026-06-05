//! Post-lexer hooks: stream transforms applied between the lexer and the parser.
//!
//! A postlex hook receives the full token stream the lexer produced and yields a
//! transformed stream before it reaches the parser. The canonical use is Python's
//! significant-whitespace syntax: an [`Indenter`] watches newline tokens and
//! injects synthetic `INDENT` / `DEDENT` tokens so an otherwise context-free
//! grammar can express block structure.
//!
//! The injected tokens are `%declare`d terminals (they have no lexer pattern of
//! their own — see [`crate::grammar::terminal::TerminalDef::declared`]). The
//! postlex resolves their interned ids from the [`SymbolTable`] so the parser
//! dispatches on them exactly as it would a lexed token.
//!
//! This is a direct port of Python Lark's `lark.indenter.Indenter`; the algorithm
//! (paren tracking, tab expansion, end-of-stream dedent flush) matches it
//! token-for-token so the oracle trees are identical.

use crate::error::{GrammarError, ParseError};
use crate::grammar::intern::{SymbolId, SymbolTable};
use crate::tree::Token;

/// The subset of a [`Token`]'s position a synthetic token borrows. Copied off the
/// triggering token so the hot loop never clones a whole `Token` just to remember
/// "where the last one was" for the end-of-stream DEDENT flush.
#[derive(Clone, Copy)]
struct Pos {
    line: usize,
    column: usize,
    end_line: usize,
    end_column: usize,
    start_pos: usize,
    end_pos: usize,
}

impl Pos {
    fn of(t: &Token) -> Self {
        Pos {
            line: t.line,
            column: t.column,
            end_line: t.end_line,
            end_column: t.end_column,
            start_pos: t.start_pos,
            end_pos: t.end_pos,
        }
    }
}

/// Injects `INDENT` / `DEDENT` tokens based on indentation, mirroring Python
/// Lark's `Indenter`.
///
/// The terminal *names* are configurable so one `Indenter` covers any grammar:
///   * `nl_type` — the newline terminal whose trailing whitespace measures indent;
///   * `indent_type` / `dedent_type` — the `%declare`d terminals to inject;
///   * `open_paren_types` / `close_paren_types` — bracket terminals inside which
///     indentation is ignored (no INDENT/DEDENT is emitted while nested);
///   * `tab_len` — how many columns a tab counts as.
#[derive(Debug, Clone)]
pub struct Indenter {
    pub nl_type: String,
    pub open_paren_types: Vec<String>,
    pub close_paren_types: Vec<String>,
    pub indent_type: String,
    pub dedent_type: String,
    pub tab_len: usize,
}

impl Default for Indenter {
    /// Python Lark's `PythonIndenter` defaults.
    fn default() -> Self {
        Indenter {
            nl_type: "_NEWLINE".to_string(),
            open_paren_types: vec!["LPAR".into(), "LSQB".into(), "LBRACE".into()],
            close_paren_types: vec!["RPAR".into(), "RSQB".into(), "RBRACE".into()],
            indent_type: "_INDENT".to_string(),
            dedent_type: "_DEDENT".to_string(),
            tab_len: 8,
        }
    }
}

impl Indenter {
    /// Check, at build time, that every terminal name this Indenter *requires*
    /// resolves to a grammar symbol — closing the silent-misparse footgun where a
    /// typo'd `nl_type` (or an undeclared INDENT/DEDENT) turns the hook into a
    /// no-op and the grammar quietly mis-parses instead of erroring.
    ///
    /// `open_paren_types` / `close_paren_types` are intentionally *not* required to
    /// exist: Python Lark treats an unknown bracket name as simply never matching,
    /// and the built-in defaults (`LPAR`/`LSQB`/`LBRACE`) name brackets a given
    /// grammar may legitimately not define. Validating them would reject those
    /// valid setups, so we match Python's leniency there.
    pub fn validate(&self, symbols: &SymbolTable) -> Result<(), GrammarError> {
        if symbols.id(&self.nl_type).is_none() {
            return Err(GrammarError::Other {
                msg: format!(
                    "postlex Indenter nl_type {:?} is not a terminal in the grammar — \
                     the Indenter measures indentation off it",
                    self.nl_type
                ),
            });
        }
        for (label, name) in [
            ("indent_type", &self.indent_type),
            ("dedent_type", &self.dedent_type),
        ] {
            if symbols.id(name).is_none() {
                return Err(GrammarError::Other {
                    msg: format!(
                        "postlex Indenter {label} {name:?} is not declared in the grammar \
                         (add `%declare {name}`)"
                    ),
                });
            }
        }
        Ok(())
    }

    /// Transform `tokens` (a fully-lexed stream ending in the synthetic `$END`
    /// token) by injecting INDENT/DEDENT around newline tokens.
    ///
    /// `symbols` resolves the `%declare`d `indent_type` / `dedent_type` names to
    /// the interned ids the parser dispatches on; a missing one is a configuration
    /// error (the grammar never declared it).
    pub fn process(
        &self,
        tokens: Vec<Token>,
        symbols: &SymbolTable,
    ) -> Result<Vec<Token>, ParseError> {
        let indent_id = self.declared_id(symbols, &self.indent_type)?;
        let dedent_id = self.declared_id(symbols, &self.dedent_type)?;

        let mut out: Vec<Token> = Vec::with_capacity(tokens.len());
        let mut paren_level: usize = 0;
        let mut indent_stack: Vec<usize> = vec![0];
        // The synthetic `$END` is held back so the end-of-stream dedent flush lands
        // *before* it, exactly where Python Lark's generator emits the trailing
        // DEDENTs (after the input loop, before the parser sees end of input).
        let mut end_token: Option<Token> = None;
        // Position of the last real token seen, borrowed onto flushed DEDENTs
        // (Python's `Token.new_borrow_pos(DEDENT, '', token)`). Only the position
        // is kept, so the loop never clones a whole `Token` here.
        let mut last_pos: Option<Pos> = None;

        for tok in tokens {
            if tok.type_id == SymbolId::END {
                end_token = Some(tok);
                break;
            }

            let cur = Pos::of(&tok);
            last_pos = Some(cur);
            // Decide the paren-depth delta from the token's type *before* it may be
            // moved into `out`; apply it *after* yielding, matching Python's order
            // (handle_NL / yield first, then adjust paren_level). A newline is never
            // a bracket, so its delta is always 0.
            let delta: i32 = if self.open_paren_types.iter().any(|t| t == &tok.type_) {
                1
            } else if self.close_paren_types.iter().any(|t| t == &tok.type_) {
                -1
            } else {
                0
            };

            if tok.type_ == self.nl_type {
                self.handle_nl(
                    &tok,
                    paren_level,
                    &mut indent_stack,
                    &mut out,
                    indent_id,
                    dedent_id,
                )?;
            } else {
                out.push(tok); // moved, not cloned
            }

            match delta {
                1 => paren_level += 1,
                -1 => {
                    paren_level = paren_level
                        .checked_sub(1)
                        .ok_or_else(|| ParseError::Postlex {
                            msg: format!(
                                "unbalanced closing bracket at line {}, column {}",
                                cur.line, cur.column
                            ),
                        })?
                }
                _ => {}
            }
        }

        // End of stream: close every still-open indentation level.
        while indent_stack.len() > 1 {
            indent_stack.pop();
            out.push(self.make_token(dedent_id, &self.dedent_type, "", last_pos));
        }

        out.push(end_token.unwrap_or_else(Token::end));
        Ok(out)
    }

    /// Handle a newline token: emit it (unless inside parens), then push INDENT or
    /// pop DEDENT(s) to match the new indentation depth.
    fn handle_nl(
        &self,
        token: &Token,
        paren_level: usize,
        indent_stack: &mut Vec<usize>,
        out: &mut Vec<Token>,
        indent_id: SymbolId,
        dedent_id: SymbolId,
    ) -> Result<(), ParseError> {
        // Inside brackets indentation is insignificant: swallow the newline whole,
        // exactly as Python Lark's `handle_NL` returns without yielding.
        if paren_level > 0 {
            return Ok(());
        }

        out.push(token.clone());

        // Indentation is the whitespace after the *last* newline in the token.
        let indent_str = token
            .value
            .rsplit_once('\n')
            .map(|(_, after)| after)
            .unwrap_or(&token.value);
        let indent =
            indent_str.matches(' ').count() + indent_str.matches('\t').count() * self.tab_len;

        let pos = Some(Pos::of(token));
        let top = *indent_stack.last().expect("indent stack never empty");
        if indent > top {
            indent_stack.push(indent);
            out.push(self.make_token(indent_id, &self.indent_type, indent_str, pos));
        } else {
            while indent < *indent_stack.last().expect("indent stack never empty") {
                indent_stack.pop();
                out.push(self.make_token(dedent_id, &self.dedent_type, indent_str, pos));
            }
            if indent != *indent_stack.last().expect("indent stack never empty") {
                return Err(ParseError::Postlex {
                    msg: format!(
                        "Unexpected dedent to column {}. Expected dedent to {}",
                        indent,
                        indent_stack.last().unwrap()
                    ),
                });
            }
        }
        Ok(())
    }

    /// Build a synthetic token, borrowing its position from `pos` (the newline it
    /// was triggered by, or the last token for the EOF flush), like Python Lark's
    /// `Token.new_borrow_pos`. `None` (an empty stream) leaves zeroed positions.
    fn make_token(&self, id: SymbolId, name: &str, value: &str, pos: Option<Pos>) -> Token {
        let mut t = Token::new(name, value);
        t.type_id = id;
        if let Some(p) = pos {
            t.line = p.line;
            t.column = p.column;
            t.end_line = p.end_line;
            t.end_column = p.end_column;
            t.start_pos = p.start_pos;
            t.end_pos = p.end_pos;
        }
        t
    }

    /// Resolve a `%declare`d terminal's interned id. This duplicates the existence
    /// check in [`validate`](Self::validate); when `process` is reached through
    /// `Lark`/`build_frontend` that check has already passed, so this is a
    /// belt-and-suspenders guard for callers that invoke `process` directly.
    fn declared_id(&self, symbols: &SymbolTable, name: &str) -> Result<SymbolId, ParseError> {
        symbols.id(name).ok_or_else(|| ParseError::Postlex {
            msg: format!(
                "Indenter terminal {name:?} is not declared in the grammar (add `%declare {name}`)"
            ),
        })
    }
}
