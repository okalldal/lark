//! Lexer implementations: BasicLexer and ContextualLexer.
//!
//! BasicLexer: Combines all terminals into a single alternation regex
//!             and scans the input left-to-right, longest-match.
//!
//! ContextualLexer: At each parser state, only attempts to match the
//!                  terminals that are valid according to the LALR lookahead
//!                  table. This is Lark's key innovation for LALR parsing —
//!                  it resolves terminal conflicts that would require manual
//!                  lexer states in Yacc/Flex.

use std::collections::HashMap;
use regex::Regex;
use crate::grammar::terminal::TerminalDef;
use crate::error::ParseError;
use crate::tree::Token;

// ─── Configuration ────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct LexerConf {
    pub terminals: Vec<TerminalDef>,
    /// Terminal names to discard after matching (from %ignore).
    pub ignore: Vec<String>,
    pub use_bytes: bool,
    pub g_regex_flags: u32,
}

impl LexerConf {
    pub fn new(terminals: Vec<TerminalDef>, ignore: Vec<String>) -> Self {
        LexerConf { terminals, ignore, use_bytes: false, g_regex_flags: 0 }
    }
}

// ─── Lexer trait ─────────────────────────────────────────────────────────────

pub trait Lexer {
    /// Lex the full input text, returning all tokens (ignoring filtered types).
    fn lex<'input>(&self, text: &'input str) -> Result<Vec<Token>, ParseError>;
}

// ─── BasicLexer ──────────────────────────────────────────────────────────────

/// Builds a combined regex from all terminals and scans the input.
/// Terminals are tried in priority order; longest match wins within the same
/// priority band.
pub struct BasicLexer {
    /// Groups of compiled regexes (Python limits groups/regex to ~100).
    mres: Vec<(Regex, Vec<String>)>,
    ignore: Vec<String>,
}

impl BasicLexer {
    pub fn new(conf: &LexerConf) -> Result<Self, crate::error::GrammarError> {
        let mres = build_mres(&conf.terminals, conf.g_regex_flags)?;
        Ok(BasicLexer { mres, ignore: conf.ignore.clone() })
    }
}

impl Lexer for BasicLexer {
    fn lex<'input>(&self, text: &'input str) -> Result<Vec<Token>, ParseError> {
        let mut tokens = Vec::new();
        let mut pos = 0;
        let mut line = 1usize;
        let mut col = 1usize;

        'outer: while pos < text.len() {
            // Try each regex group
            let mut best: Option<(usize, usize, String)> = None; // (end, priority, name)
            for (re, names) in &self.mres {
                if let Some(m) = re.find_at(text, pos) {
                    if m.start() != pos { continue; }
                    let end = m.end();
                    // Find which named group matched
                    if let Some(caps) = re.captures_at(text, pos) {
                        for (i, name) in names.iter().enumerate() {
                            if caps.name(name).is_some() {
                                let priority = i; // lower index = higher priority
                                if let Some(ref b) = best {
                                    if end > b.0 || (end == b.0 && priority < b.1) {
                                        best = Some((end, priority, name.clone()));
                                    }
                                } else {
                                    best = Some((end, priority, name.clone()));
                                }
                                break;
                            }
                        }
                    }
                }
            }

            if let Some((end, _, name)) = best {
                let value = &text[pos..end];
                let start_pos = pos;
                let start_line = line;
                let start_col = col;

                // Update line/col counters
                for ch in value.chars() {
                    if ch == '\n' { line += 1; col = 1; } else { col += 1; }
                }

                pos = end;

                if !self.ignore.contains(&name) {
                    tokens.push(Token {
                        type_: name,
                        value: value.to_string(),
                        line: start_line,
                        column: start_col,
                        end_line: line,
                        end_column: col,
                        start_pos,
                        end_pos: end,
                    });
                }
            } else {
                let ch = text[pos..].chars().next().unwrap();
                return Err(ParseError::UnexpectedCharacter {
                    ch,
                    line,
                    col,
                    pos,
                    expected: "any token".to_string(),
                });
            }
        }

        tokens.push(Token::new("$END", "").with_position(line, col, pos, pos));
        Ok(tokens)
    }
}

// ─── ContextualLexer ─────────────────────────────────────────────────────────

/// A lexer that narrows the set of candidate terminals based on which
/// terminals are valid in the current LALR parser state.
///
/// This is Lark's primary innovation for LALR parsing: instead of requiring
/// the grammar author to declare lexer states, the parser table itself tells
/// the lexer exactly which terminals to try. This eliminates virtually all
/// shift/reduce conflicts caused by terminal overlap.
pub struct ContextualLexer {
    /// Per-state compiled regexes. State 0 is the root (fallback) lexer.
    state_lexers: HashMap<usize, StateLexer>,
    /// Terminals that are always valid regardless of state (e.g., whitespace).
    always_accept: Vec<String>,
    ignore: Vec<String>,
}

struct StateLexer {
    re: Regex,
    names: Vec<String>,
}

impl ContextualLexer {
    /// Build a contextual lexer.
    ///
    /// `state_terminals`: maps LALR state ID → set of valid terminal names.
    /// `all_terminals`: all terminal definitions (for building per-state regexes).
    pub fn new(
        conf: &LexerConf,
        state_terminals: &HashMap<usize, Vec<String>>,
        always_accept: Vec<String>,
    ) -> Result<Self, crate::error::GrammarError> {
        let term_map: HashMap<&str, &TerminalDef> = conf.terminals.iter()
            .map(|t| (t.name.as_str(), t))
            .collect();

        let mut state_lexers = HashMap::new();
        for (state_id, valid_names) in state_terminals {
            let terms: Vec<&TerminalDef> = valid_names.iter()
                .chain(always_accept.iter())
                .filter_map(|n| term_map.get(n.as_str()).copied())
                .collect();
            if terms.is_empty() { continue; }
            let sl = build_state_lexer(&terms, conf.g_regex_flags)?;
            state_lexers.insert(*state_id, sl);
        }

        Ok(ContextualLexer { state_lexers, always_accept, ignore: conf.ignore.clone() })
    }

    pub fn ignore(&self) -> &[String] { &self.ignore }

    /// Lex the next token at `pos` given the current parser state.
    pub fn next_token(
        &self,
        text: &str,
        pos: usize,
        state: usize,
        line: usize,
        col: usize,
    ) -> Result<Option<Token>, ParseError> {
        let sl = self.state_lexers.get(&state)
            .or_else(|| self.state_lexers.get(&0));

        let sl = match sl {
            Some(sl) => sl,
            None => return Ok(None),
        };

        if let Some(caps) = sl.re.captures_at(text, pos) {
            if let Some(m) = caps.get(0) {
                if m.start() != pos { return Ok(None); }
                for name in &sl.names {
                    if let Some(group) = caps.name(name) {
                        let value = group.as_str();
                        let end = pos + value.len();
                        if self.ignore.contains(name) {
                            return Ok(Some(Token {
                                type_: name.clone(),
                                value: value.to_string(),
                                line, column: col,
                                end_line: line, end_column: col + value.len(),
                                start_pos: pos, end_pos: end,
                            }));
                        }
                        return Ok(Some(Token {
                            type_: name.clone(),
                            value: value.to_string(),
                            line, column: col,
                            end_line: line, end_column: col + value.len(),
                            start_pos: pos, end_pos: end,
                        }));
                    }
                }
            }
        }

        if pos >= text.len() {
            return Ok(Some(Token::new("$END", "").with_position(line, col, pos, pos)));
        }

        let ch = text[pos..].chars().next().unwrap();
        Err(ParseError::UnexpectedCharacter {
            ch, line, col, pos,
            expected: format!("one of {:?}", sl.names),
        })
    }
}

// ─── Regex builder helpers ────────────────────────────────────────────────────

/// Python's `re` module limits named groups to 100 per pattern.
/// We split terminals into chunks of ≤ 98 to stay under the limit.
const MAX_GROUPS: usize = 98;

pub fn build_mres(
    terminals: &[TerminalDef],
    global_flags: u32,
) -> Result<Vec<(Regex, Vec<String>)>, crate::error::GrammarError> {
    let chunks: Vec<&[TerminalDef]> = terminals.chunks(MAX_GROUPS).collect();
    let mut result = Vec::new();
    for chunk in chunks {
        let (re, names) = build_combined_regex(chunk, global_flags)?;
        result.push((re, names));
    }
    Ok(result)
}

fn build_combined_regex(
    terminals: &[TerminalDef],
    _global_flags: u32,
) -> Result<(Regex, Vec<String>), crate::error::GrammarError> {
    // Sort: higher priority first, then longer pattern string first, then name ascending.
    // This mirrors Python Lark's terminal ordering so longer/more-specific patterns win.
    let mut sorted: Vec<&TerminalDef> = terminals.iter().collect();
    sorted.sort_by(|a, b| {
        let pa = a.pattern.as_regex_str().len();
        let pb = b.pattern.as_regex_str().len();
        b.priority.cmp(&a.priority)
            .then(pb.cmp(&pa))
            .then(a.name.cmp(&b.name))
    });

    let mut parts = Vec::new();
    let mut names = Vec::new();
    for term in sorted {
        let safe_name = term.name.replace('$', "DOLLAR").replace('-', "_");
        parts.push(format!("(?P<{}>{})", safe_name, term.pattern.as_regex_str()));
        names.push(term.name.clone());
    }
    let pattern = parts.join("|");
    let re = Regex::new(&pattern).map_err(|e| crate::error::GrammarError::InvalidRegex {
        pattern: pattern.clone(),
        reason: e.to_string(),
    })?;
    Ok((re, names))
}

fn build_state_lexer(
    terminals: &[&TerminalDef],
    _global_flags: u32,
) -> Result<StateLexer, crate::error::GrammarError> {
    // Same sort order as build_combined_regex.
    let mut sorted: Vec<&&TerminalDef> = terminals.iter().collect();
    sorted.sort_by(|a, b| {
        let pa = a.pattern.as_regex_str().len();
        let pb = b.pattern.as_regex_str().len();
        b.priority.cmp(&a.priority)
            .then(pb.cmp(&pa))
            .then(a.name.cmp(&b.name))
    });

    let mut parts = Vec::new();
    let mut names = Vec::new();
    for term in sorted {
        let safe_name = term.name.replace('$', "DOLLAR").replace('-', "_");
        parts.push(format!("(?P<{}>{})", safe_name, term.pattern.as_regex_str()));
        names.push(term.name.clone());
    }
    let pattern = parts.join("|");
    let re = Regex::new(&pattern).map_err(|e| crate::error::GrammarError::InvalidRegex {
        pattern,
        reason: e.to_string(),
    })?;
    Ok(StateLexer { re, names })
}

// ─── LexerState: tracks position during incremental lexing ───────────────────

/// Mutable state threaded through contextual lexing.
pub struct LexerState<'a> {
    pub text: &'a str,
    pub pos: usize,
    pub line: usize,
    pub col: usize,
}

impl<'a> LexerState<'a> {
    pub fn new(text: &'a str) -> Self {
        LexerState { text, pos: 0, line: 1, col: 1 }
    }

    pub fn is_done(&self) -> bool {
        self.pos >= self.text.len()
    }

    pub fn advance_by(&mut self, n: usize) {
        for ch in self.text[self.pos..self.pos + n].chars() {
            if ch == '\n' { self.line += 1; self.col = 1; } else { self.col += 1; }
        }
        self.pos += n;
    }

    /// Advance by `n` bytes, using `value` to track line/col correctly.
    pub fn advance_by_lines(&mut self, n: usize, value: &str) {
        for ch in value.chars() {
            if ch == '\n' { self.line += 1; self.col = 1; } else { self.col += 1; }
        }
        self.pos += n;
    }
}
