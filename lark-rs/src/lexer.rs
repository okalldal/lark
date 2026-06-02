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
//! This is what makes `if` lex as `IF` while `iffy` stays `NAME`, without any
//! cross-terminal longest-match scan — and it matches Python Lark exactly.

use std::collections::{HashMap, HashSet};
use regex::Regex;
use crate::grammar::terminal::{TerminalDef, Pattern};
use crate::error::{ParseError, GrammarError};
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

// ─── Scanner: one compiled alternation over a set of terminals ────────────────

/// A compiled scanner over a fixed set of terminals.
///
/// Matching is leftmost-first (Python-`re` semantics), so the alternation order
/// breaks ties. The `unless` map carries Lark's keyword retyping (see module
/// docs).
struct Scanner {
    re: Regex,
    /// Real terminal name + its sanitized regex group name, in alternation order.
    groups: Vec<(String, String)>,
    /// regex-terminal-name → (matched-text → keyword-terminal-name).
    unless: HashMap<String, HashMap<String, String>>,
}

impl Scanner {
    /// Build a scanner from candidate terminals (deduplicated by name).
    fn build(terminals: &[&TerminalDef]) -> Result<Scanner, GrammarError> {
        // Deduplicate by name (a terminal can appear via both the state set and
        // `always_accept`); duplicate capture-group names would not compile.
        let mut seen = HashSet::new();
        let terms: Vec<&TerminalDef> = terminals
            .iter()
            .copied()
            .filter(|t| seen.insert(t.name.as_str()))
            .collect();

        // ── unless: embed string terminals fully matched by a same-priority
        //    regex terminal, and record the retype.
        let unless = compute_unless(&terms)?;
        let embedded: HashSet<&str> = unless
            .values()
            .flat_map(|m| m.values())
            .map(|s| s.as_str())
            .collect();

        // Scanner terminals = everything not embedded, sorted Python-style.
        let mut scan: Vec<&TerminalDef> = terms
            .iter()
            .copied()
            .filter(|t| !embedded.contains(t.name.as_str()))
            .collect();
        sort_terminals(&mut scan);

        let mut parts = Vec::with_capacity(scan.len());
        let mut groups = Vec::with_capacity(scan.len());
        for term in scan {
            let safe = safe_group_name(&term.name);
            parts.push(format!("(?P<{}>{})", safe, term.pattern.as_regex_str()));
            groups.push((term.name.clone(), safe));
        }
        let pattern = parts.join("|");
        let re = Regex::new(&pattern).map_err(|e| GrammarError::InvalidRegex {
            pattern: pattern.clone(),
            reason: e.to_string(),
        })?;
        Ok(Scanner { re, groups, unless })
    }

    /// Match a single token starting exactly at `pos`. Returns `(type, value)`,
    /// with keyword retyping already applied. `None` means nothing matched here.
    fn match_at(&self, text: &str, pos: usize) -> Option<(String, String)> {
        let caps = self.re.captures_at(text, pos)?;
        let m0 = caps.get(0)?;
        // captures_at finds the leftmost match at *or after* pos; we only accept
        // a token that begins exactly at pos. Reject empty matches so a nullable
        // terminal can never stall the scan.
        if m0.start() != pos || m0.end() == pos {
            return None;
        }
        let value = m0.as_str();
        for (real, safe) in &self.groups {
            if caps.name(safe).is_some() {
                let ty = self
                    .unless
                    .get(real)
                    .and_then(|m| m.get(value))
                    .cloned()
                    .unwrap_or_else(|| real.clone());
                return Some((ty, value.to_string()));
            }
        }
        None
    }
}

/// For each regex terminal, find the same-priority string terminals it fully
/// matches; those strings are embedded (dropped from the alternation) and retyped
/// after the fact. Mirrors Python Lark's `_create_unless`.
fn compute_unless(
    terms: &[&TerminalDef],
) -> Result<HashMap<String, HashMap<String, String>>, GrammarError> {
    let res: Vec<&&TerminalDef> = terms
        .iter()
        .filter(|t| matches!(t.pattern, Pattern::Re(_)))
        .collect();
    let strs: Vec<&&TerminalDef> = terms
        .iter()
        .filter(|t| matches!(t.pattern, Pattern::Str(_)))
        .collect();
    if res.is_empty() || strs.is_empty() {
        return Ok(HashMap::new());
    }

    let mut unless: HashMap<String, HashMap<String, String>> = HashMap::new();
    for re_t in &res {
        // Anchored full-match form of this regex terminal.
        let full_src = format!("^(?:{})$", re_t.pattern.as_regex_str());
        let full = Regex::new(&full_src).map_err(|e| GrammarError::InvalidRegex {
            pattern: full_src.clone(),
            reason: e.to_string(),
        })?;
        for s_t in &strs {
            if s_t.priority != re_t.priority {
                continue;
            }
            let value = match &s_t.pattern {
                Pattern::Str(p) => &p.value,
                Pattern::Re(_) => continue,
            };
            if full.is_match(value) {
                unless
                    .entry(re_t.name.clone())
                    .or_default()
                    .insert(value.clone(), s_t.name.clone());
            }
        }
    }
    Ok(unless)
}

/// Python Lark's terminal ordering: `(-priority, -max_width, -len(value), name)`.
/// Regex terminals have unbounded `max_width` and therefore sort ahead of fixed
/// strings; the leftmost-first alternation then matches them greedily.
fn sort_terminals(terms: &mut [&TerminalDef]) {
    terms.sort_by(|a, b| {
        let aw = a.pattern.max_width().unwrap_or(usize::MAX);
        let bw = b.pattern.max_width().unwrap_or(usize::MAX);
        b.priority
            .cmp(&a.priority)
            .then_with(|| bw.cmp(&aw))
            .then_with(|| b.pattern.as_regex_str().len().cmp(&a.pattern.as_regex_str().len()))
            .then_with(|| a.name.cmp(&b.name))
    });
}

/// A regex named-capture group only accepts `[A-Za-z0-9_]`. Terminal names may
/// contain `$` (synthetics) or `-` (aliases); rewrite them to a safe form.
fn safe_group_name(name: &str) -> String {
    name.replace('$', "DOLLAR").replace('-', "_")
}

// ─── BasicLexer ──────────────────────────────────────────────────────────────

/// Scans the whole input with a single combined regex over all terminals.
pub struct BasicLexer {
    scanner: Scanner,
    ignore: Vec<String>,
}

impl BasicLexer {
    pub fn new(conf: &LexerConf) -> Result<Self, GrammarError> {
        let refs: Vec<&TerminalDef> = conf.terminals.iter().collect();
        let scanner = Scanner::build(&refs)?;
        Ok(BasicLexer { scanner, ignore: conf.ignore.clone() })
    }
}

impl Lexer for BasicLexer {
    fn lex<'input>(&self, text: &'input str) -> Result<Vec<Token>, ParseError> {
        let mut tokens = Vec::new();
        let mut pos = 0;
        let mut line = 1usize;
        let mut col = 1usize;

        while pos < text.len() {
            match self.scanner.match_at(text, pos) {
                Some((name, value)) => {
                    let start_pos = pos;
                    let start_line = line;
                    let start_col = col;

                    for ch in value.chars() {
                        if ch == '\n' { line += 1; col = 1; } else { col += 1; }
                    }
                    pos += value.len();

                    if !self.ignore.contains(&name) {
                        tokens.push(Token {
                            type_: name,
                            value,
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

        tokens.push(Token::new("$END", "").with_position(line, col, pos, pos));
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
    ignore: Vec<String>,
}

impl ContextualLexer {
    /// Build a contextual lexer.
    ///
    /// `state_terminals`: maps LALR state ID → valid terminal names.
    /// `always_accept`: terminals valid in every state (e.g. `%ignore` whitespace).
    pub fn new(
        conf: &LexerConf,
        state_terminals: &HashMap<usize, Vec<String>>,
        always_accept: Vec<String>,
    ) -> Result<Self, GrammarError> {
        let term_map: HashMap<&str, &TerminalDef> = conf.terminals.iter()
            .map(|t| (t.name.as_str(), t))
            .collect();

        let mut state_scanners = HashMap::new();
        for (state_id, valid_names) in state_terminals {
            let terms: Vec<&TerminalDef> = valid_names.iter()
                .chain(always_accept.iter())
                .filter_map(|n| term_map.get(n.as_str()).copied())
                .collect();
            if terms.is_empty() { continue; }
            state_scanners.insert(*state_id, Scanner::build(&terms)?);
        }

        Ok(ContextualLexer { state_scanners, ignore: conf.ignore.clone() })
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
        let scanner = match self.state_scanners.get(&state).or_else(|| self.state_scanners.get(&0)) {
            Some(s) => s,
            None => return Ok(None),
        };

        if let Some((name, value)) = scanner.match_at(text, pos) {
            let end = pos + value.len();
            // End position is char-based and newline-aware: a token spanning a
            // newline (e.g. a multi-line comment/string) advances the line and
            // resets the column, mirroring LexerState::advance_by.
            let (mut end_line, mut end_column) = (line, col);
            for ch in value.chars() {
                if ch == '\n' {
                    end_line += 1;
                    end_column = 1;
                } else {
                    end_column += 1;
                }
            }
            return Ok(Some(Token {
                type_: name,
                value,
                line,
                column: col,
                end_line,
                end_column,
                start_pos: pos,
                end_pos: end,
            }));
        }

        if pos >= text.len() {
            return Ok(Some(Token::new("$END", "").with_position(line, col, pos, pos)));
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
