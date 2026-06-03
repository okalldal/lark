//! Lexer implementations: BasicLexer and ContextualLexer.
//!
//! BasicLexer: one combined alternation regex over all terminals, scanning the
//!             input left-to-right.
//!
//! ContextualLexer: at each parser state, only attempts the terminals that are
//!                  valid according to the LALR action table. This is Lark's key
//!                  innovation for LALR parsing — the parser table tells the lexer
//!                  which terminals to try, resolving terminal conflicts that would
//!                  otherwise need hand-written lexer states.
//!
//! Both share a [`Scanner`]. The alternation uses the `regex` crate's
//! leftmost-first semantics, which are identical to Python `re` — so terminal
//! *order* decides ties, exactly as in Python Lark. Order is
//! `(priority desc, max_width desc, pattern-length desc, name asc)`.
//!
//! On top of that, the scanner implements Lark's **"unless" keyword retyping**
//! (`_create_unless` in Python Lark): a string terminal whose value is fully
//! matched by a regex terminal of the same priority (e.g. the keyword `if` inside
//! the identifier pattern `CNAME`) is *removed* from the alternation, and the
//! regex match is retyped back to the keyword when the matched text equals it.
//! This is what makes `if` lex as `IF` while `iffy` stays `NAME`.
//!
//! Every matched terminal is identified by its interned [`SymbolId`]; the parser
//! dispatches on that id directly. The token's name is carried only for display.

use std::collections::{HashMap, HashSet};

use regex::Regex;

use crate::error::{GrammarError, ParseError};
use crate::grammar::intern::SymbolId;
use crate::grammar::terminal::{Pattern, TerminalDef};
use crate::tree::Token;

// ─── Configuration ────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct LexerConf {
    /// Terminal id paired with its definition.
    pub terminals: Vec<(SymbolId, TerminalDef)>,
    /// Terminal ids to discard after matching (from `%ignore`).
    pub ignore: Vec<SymbolId>,
}

impl LexerConf {
    pub fn new(terminals: Vec<(SymbolId, TerminalDef)>, ignore: Vec<SymbolId>) -> Self {
        LexerConf { terminals, ignore }
    }

    /// id → name map for token display.
    fn names(&self) -> HashMap<SymbolId, String> {
        self.terminals.iter().map(|(id, t)| (*id, t.name.clone())).collect()
    }
}

// ─── Lexer trait ─────────────────────────────────────────────────────────────

pub trait Lexer {
    /// Lex the full input text, returning all tokens (ignoring filtered types).
    fn lex(&self, text: &str) -> Result<Vec<Token>, ParseError>;
}

// ─── Scanner: one compiled alternation over a set of terminals ────────────────

/// A compiled scanner over a fixed set of terminals.
///
/// Matching is leftmost-first (Python-`re` semantics), so the alternation order
/// breaks ties. The `unless` map carries Lark's keyword retyping (see module
/// docs). Capture-group names are derived from the symbol id (`g{n}`), so no
/// terminal-name sanitization is needed.
struct Scanner {
    re: Regex,
    /// (terminal id, capture-group name), in alternation order.
    groups: Vec<(SymbolId, String)>,
    /// regex-terminal-id → (matched-text → keyword-terminal-id).
    unless: HashMap<SymbolId, HashMap<String, SymbolId>>,
}

impl Scanner {
    /// Build a scanner from candidate terminals (deduplicated by id).
    fn build(terminals: &[(SymbolId, &TerminalDef)]) -> Result<Scanner, GrammarError> {
        let mut seen = HashSet::new();
        let terms: Vec<(SymbolId, &TerminalDef)> =
            terminals.iter().copied().filter(|(id, _)| seen.insert(*id)).collect();

        // unless: embed string terminals fully matched by a same-priority regex
        // terminal, and record the retype.
        let unless = compute_unless(&terms)?;
        let embedded: HashSet<SymbolId> = unless.values().flat_map(|m| m.values().copied()).collect();

        // Scanner terminals = everything not embedded, sorted Python-style.
        let mut scan: Vec<(SymbolId, &TerminalDef)> =
            terms.iter().copied().filter(|(id, _)| !embedded.contains(id)).collect();
        sort_terminals(&mut scan);

        let mut parts = Vec::with_capacity(scan.len());
        let mut groups = Vec::with_capacity(scan.len());
        for (id, term) in scan {
            let group = format!("g{}", id.0);
            parts.push(format!("(?P<{}>{})", group, term.pattern.as_regex_str()));
            groups.push((id, group));
        }
        let pattern = parts.join("|");
        let re = Regex::new(&pattern)
            .map_err(|e| GrammarError::InvalidRegex { pattern: pattern.clone(), reason: e.to_string() })?;
        Ok(Scanner { re, groups, unless })
    }

    /// Match a single token starting exactly at `pos`. Returns `(terminal id,
    /// value)`, with keyword retyping already applied. `None` means nothing
    /// matched here.
    fn match_at<'t>(&self, text: &'t str, pos: usize) -> Option<(SymbolId, &'t str)> {
        let caps = self.re.captures_at(text, pos)?;
        let m0 = caps.get(0)?;
        // captures_at finds the leftmost match at *or after* pos; only accept a
        // token beginning exactly at pos, and reject empty matches so a nullable
        // terminal can never stall the scan.
        if m0.start() != pos || m0.end() == pos {
            return None;
        }
        let value = m0.as_str();
        for (id, group) in &self.groups {
            if caps.name(group).is_some() {
                let ty = self.unless.get(id).and_then(|m| m.get(value)).copied().unwrap_or(*id);
                return Some((ty, value));
            }
        }
        None
    }
}

/// For each regex terminal, find the same-priority string terminals it fully
/// matches; those strings are embedded (dropped from the alternation) and
/// retyped after the fact. Mirrors Python Lark's `_create_unless`.
fn compute_unless(
    terms: &[(SymbolId, &TerminalDef)],
) -> Result<HashMap<SymbolId, HashMap<String, SymbolId>>, GrammarError> {
    let res: Vec<&(SymbolId, &TerminalDef)> =
        terms.iter().filter(|(_, t)| matches!(t.pattern, Pattern::Re(_))).collect();
    let strs: Vec<&(SymbolId, &TerminalDef)> =
        terms.iter().filter(|(_, t)| matches!(t.pattern, Pattern::Str(_))).collect();
    if res.is_empty() || strs.is_empty() {
        return Ok(HashMap::new());
    }

    let mut unless: HashMap<SymbolId, HashMap<String, SymbolId>> = HashMap::new();
    for (re_id, re_t) in &res {
        let full_src = format!("^(?:{})$", re_t.pattern.as_regex_str());
        let full = Regex::new(&full_src)
            .map_err(|e| GrammarError::InvalidRegex { pattern: full_src.clone(), reason: e.to_string() })?;
        for (s_id, s_t) in &strs {
            if s_t.priority != re_t.priority {
                continue;
            }
            let value = match &s_t.pattern {
                Pattern::Str(p) => &p.value,
                Pattern::Re(_) => continue,
            };
            if full.is_match(value) {
                unless.entry(*re_id).or_default().insert(value.clone(), *s_id);
            }
        }
    }
    Ok(unless)
}

/// Python Lark's terminal ordering: `(-priority, -max_width, -len(pattern), id)`.
/// Regex terminals have unbounded `max_width` and therefore sort ahead of fixed
/// strings; the leftmost-first alternation then matches them greedily.
fn sort_terminals(terms: &mut [(SymbolId, &TerminalDef)]) {
    terms.sort_by(|(a_id, a), (b_id, b)| {
        let aw = a.pattern.max_width().unwrap_or(usize::MAX);
        let bw = b.pattern.max_width().unwrap_or(usize::MAX);
        b.priority
            .cmp(&a.priority)
            .then_with(|| bw.cmp(&aw))
            .then_with(|| b.pattern.as_regex_str().len().cmp(&a.pattern.as_regex_str().len()))
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a_id.cmp(b_id))
    });
}

// ─── BasicLexer ──────────────────────────────────────────────────────────────

/// Scans the whole input with a single combined regex over all terminals.
pub struct BasicLexer {
    scanner: Scanner,
    names: HashMap<SymbolId, String>,
    ignore: HashSet<SymbolId>,
}

impl BasicLexer {
    pub fn new(conf: &LexerConf) -> Result<Self, GrammarError> {
        let refs: Vec<(SymbolId, &TerminalDef)> = conf.terminals.iter().map(|(id, t)| (*id, t)).collect();
        let scanner = Scanner::build(&refs)?;
        Ok(BasicLexer {
            scanner,
            names: conf.names(),
            ignore: conf.ignore.iter().copied().collect(),
        })
    }
}

impl Lexer for BasicLexer {
    fn lex(&self, text: &str) -> Result<Vec<Token>, ParseError> {
        let mut tokens = Vec::new();
        let mut pos = 0;
        let mut line = 1usize;
        let mut col = 1usize;

        while pos < text.len() {
            match self.scanner.match_at(text, pos) {
                Some((id, value)) => {
                    let start_pos = pos;
                    let start_line = line;
                    let start_col = col;

                    for ch in value.chars() {
                        if ch == '\n' { line += 1; col = 1; } else { col += 1; }
                    }
                    pos += value.len();

                    if !self.ignore.contains(&id) {
                        tokens.push(Token {
                            type_id: id,
                            type_: self.names[&id].clone(),
                            value: value.to_string(),
                            line: start_line,
                            column: start_col,
                            end_line: line,
                            end_column: col,
                            start_pos,
                            end_pos: pos,
                        });
                    }
                }
                None => {
                    let ch = text[pos..].chars().next().unwrap();
                    return Err(ParseError::UnexpectedCharacter {
                        ch, line, col, pos,
                        expected: "any token".to_string(),
                    });
                }
            }
        }

        tokens.push(Token::end().with_position(line, col, pos, pos));
        Ok(tokens)
    }
}

// ─── ContextualLexer ─────────────────────────────────────────────────────────

/// A lexer that narrows the candidate terminals to those valid in the current
/// LALR parser state. Each state gets its own [`Scanner`], so keyword/identifier
/// disambiguation (the `unless` retyping) is computed per state — exactly as
/// Python Lark builds one `TraditionalLexer` per parser state.
pub struct ContextualLexer {
    /// Per-state scanner. State 0 is the root (fallback) scanner.
    state_scanners: HashMap<usize, Scanner>,
    names: HashMap<SymbolId, String>,
    ignore: HashSet<SymbolId>,
}

impl ContextualLexer {
    /// Build a contextual lexer.
    ///
    /// `state_terminals`: LALR state id → valid terminal ids.
    /// `always_accept`: terminals valid in every state (e.g. `%ignore`).
    pub fn new(
        conf: &LexerConf,
        state_terminals: &HashMap<usize, Vec<SymbolId>>,
        always_accept: Vec<SymbolId>,
    ) -> Result<Self, GrammarError> {
        let term_map: HashMap<SymbolId, &TerminalDef> =
            conf.terminals.iter().map(|(id, t)| (*id, t)).collect();

        let mut state_scanners = HashMap::new();
        for (state_id, valid_ids) in state_terminals {
            let terms: Vec<(SymbolId, &TerminalDef)> = valid_ids
                .iter()
                .chain(always_accept.iter())
                .filter_map(|id| term_map.get(id).map(|t| (*id, *t)))
                .collect();
            if terms.is_empty() {
                continue;
            }
            state_scanners.insert(*state_id, Scanner::build(&terms)?);
        }

        Ok(ContextualLexer {
            state_scanners,
            names: conf.names(),
            ignore: conf.ignore.iter().copied().collect(),
        })
    }

    #[inline]
    pub fn is_ignored(&self, id: SymbolId) -> bool {
        self.ignore.contains(&id)
    }

    /// Lex the next token at `pos` given the current parser state.
    pub fn next_token(
        &self,
        text: &str,
        pos: usize,
        state: usize,
        line: usize,
        col: usize,
    ) -> Result<Option<Token>, ParseError> {
        let scanner = match self.state_scanners.get(&state).or_else(|| self.state_scanners.get(&0)) {
            Some(s) => s,
            None => return Ok(None),
        };

        if let Some((id, value)) = scanner.match_at(text, pos) {
            let end = pos + value.len();
            // End position is char-based and newline-aware: a token spanning a
            // newline advances the line and resets the column.
            let (mut end_line, mut end_column) = (line, col);
            for ch in value.chars() {
                if ch == '\n' { end_line += 1; end_column = 1; } else { end_column += 1; }
            }
            return Ok(Some(Token {
                type_id: id,
                type_: self.names[&id].clone(),
                value: value.to_string(),
                line,
                column: col,
                end_line,
                end_column,
                start_pos: pos,
                end_pos: end,
            }));
        }

        if pos >= text.len() {
            return Ok(Some(Token::end().with_position(line, col, pos, pos)));
        }

        let ch = text[pos..].chars().next().unwrap();
        Err(ParseError::UnexpectedCharacter {
            ch, line, col, pos,
            expected: "valid token for this state".to_string(),
        })
    }
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
