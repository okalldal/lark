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
//! Both share a combined scanner behind the [`ScannerBackend`] seam — the
//! `regex`-crate [`scanner::Scanner`] or the default `regex-automata`
//! [`dfa::DfaScanner`]. The alternation uses leftmost-first semantics, identical
//! to Python `re` — so terminal *order* decides ties, exactly as in Python Lark.
//! Order is `(priority desc, max_width desc, pattern-length desc, name asc)`.
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
//!
//! Module layout (one concern per file):
//!
//! ```text
//! plan        ScannerPlan: selection, Python-style ordering, `unless` retyping
//! pattern     flag-wrapper algebra (the loader's baked `(?is:…)` and its inverse)
//! route       THE refusal seam: lower a regex-rejected terminal or scope-error
//! guard       compiled boundary/lookbehind guards + their compilation context
//! scanner     the `regex`-crate combined-alternation backend (+ side-probes)
//! dfa         the `regex-automata` multi-pattern DFA backend (the default)
//! fence       the fence-idiom matcher (tag-echo heredocs / bracket arguments)
//! dynamic     per-terminal matching for Earley's dynamic lexer
//! collision   strict-mode regex-collision + zero-width construction checks
//! ```

mod collision;
mod dfa;
mod dynamic;
mod fence;
mod guard;
mod pattern;
mod plan;
mod route;
mod scanner;
#[cfg(test)]
mod tests;

pub use collision::{check_regex_collisions, check_zero_width_terminals};
pub use dynamic::DynamicMatcher;
pub use plan::{scanner_plan, ScannerPlan, UnlessEntry};

use std::collections::{HashMap, HashSet};

use dfa::DfaScanner;
use scanner::Scanner;

use crate::error::{GrammarError, ParseError};
use crate::grammar::intern::SymbolId;
use crate::grammar::terminal::TerminalDef;
use crate::tree::Token;

/// Account the forward-skip cost of one per-position scan attempt for the
/// deterministic lexer-scaling gate ([`crate::perf::lexer_scan_steps`]).
/// `match_start` is where the engine's leftmost match (searched *at or after*
/// `pos`) actually began, or `None` on a miss. The recorded cost is the number of
/// bytes the search skipped *past* `pos` before reporting that match, plus one for
/// the attempt itself.
///
/// A miss is charged a flat `1`, deliberately: from the return value alone an
/// anchored (`\G`) search and an unanchored one are indistinguishable on a no-match
/// (both yield `None`), even though the unanchored one scanned to end-of-input to
/// get there. Charging the miss its true scan length would therefore falsely flag
/// an *anchored* scanner as quadratic. So the pathology is made observable from the
/// other side: a workload that contains a *sparse* match means the unanchored
/// search reports a far-ahead `start` (the skip we count) at every position before
/// it, while the anchored search keeps missing at `pos` — exactly the
/// `tests/test_lexer_scaling.rs` shape. Compiles to nothing without `perf-counters`.
#[inline]
fn record_scan_skip(pos: usize, match_start: Option<usize>) {
    let skip = match match_start {
        Some(start) => start.saturating_sub(pos) as u64,
        None => 0,
    };
    crate::perf::add_lexer_scan_steps(skip + 1);
}

// ─── Configuration ────────────────────────────────────────────────────────────

/// Which engine backs the per-position match (`match_at`). Selects between
/// the two combined-scanner implementations behind the single [`ScannerBackend`]
/// seam, with no behavioral difference — both reproduce Lark's leftmost-first
/// selection, `unless` retyping, and lookaround side-probes byte-for-byte (the L0
/// differential oracle in `tests/test_scanner_differential.rs` is the contract).
///
///   * [`Regex`](LexerBackend::Regex) — the original `regex`-crate combined
///     alternation with capture groups (see [`scanner::Scanner`]).
///   * [`Dfa`](LexerBackend::Dfa) — a `regex-automata` multi-pattern DFA
///     (`docs/LEXER_DFA_PLAN.md`, phase L1; see [`dfa::DfaScanner`]). This is now the
///     default: the L0 differential oracle proves it lexes byte-identically to the
///     `regex` Scanner over the full bank + JSON + python/lark corpora, and it is
///     faster on the all-plain common path (`benches/lex_backends`, `BENCH.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LexerBackend {
    /// The `regex`-crate combined-alternation scanner (the original engine).
    Regex,
    /// The `regex-automata` multi-pattern DFA scanner (phase L1). The default.
    #[default]
    Dfa,
}

#[derive(Debug, Clone)]
pub struct LexerConf {
    /// Terminal id paired with its definition.
    pub terminals: Vec<(SymbolId, TerminalDef)>,
    /// Terminal ids to discard after matching (from `%ignore`).
    pub ignore: Vec<SymbolId>,
    /// Global regex flags (Lark's `g_regex_flags`) applied to every terminal in
    /// the combined scanner regex. Zero leaves each terminal's own flags as-is.
    pub global_flags: u32,
    /// Which combined-scanner engine to build (see [`LexerBackend`]). Defaults to
    /// the `regex-automata` [`dfa::DfaScanner`]; the original `regex` Scanner is
    /// opt-in.
    pub backend: LexerBackend,
}

impl LexerConf {
    pub fn new(terminals: Vec<(SymbolId, TerminalDef)>, ignore: Vec<SymbolId>) -> Self {
        LexerConf {
            terminals,
            ignore,
            global_flags: 0,
            backend: LexerBackend::default(),
        }
    }

    /// Set the global regex flags (builder-style) for `g_regex_flags` support.
    pub fn with_global_flags(mut self, flags: u32) -> Self {
        self.global_flags = flags;
        self
    }

    /// Select the combined-scanner backend (builder-style). The default is the
    /// `regex-automata` [`dfa::DfaScanner`]; choosing [`LexerBackend::Regex`] swaps
    /// back to the original `regex`-crate [`scanner::Scanner`] without changing any
    /// lexing semantics. Both refuse the same patterns with the same categorized
    /// scope errors (`docs/LOOKAROUND_SCOPE.md`); a lowered lookaround terminal
    /// rides the shared DFA branches there and a per-terminal side-probe here (the
    /// TEST-ONLY `fancy-oracle` feature swaps the probe for the historical fancy
    /// reference).
    pub fn with_backend(mut self, backend: LexerBackend) -> Self {
        self.backend = backend;
        self
    }

    /// id → name map for token display.
    fn names(&self) -> HashMap<SymbolId, String> {
        self.terminals
            .iter()
            .map(|(id, t)| (*id, t.name.clone()))
            .collect()
    }
}

// ─── Lexer trait ─────────────────────────────────────────────────────────────

pub trait Lexer {
    /// Lex the full input text, returning all tokens (ignoring filtered types).
    fn lex(&self, text: &str) -> Result<Vec<Token>, ParseError>;
}

// ─── ScannerBackend: the match_at seam over the two combined-scanner engines ───

/// The single insertion point both [`BasicLexer`] and the per-state
/// [`ContextualLexer`] funnel every token through: `match_at(text, pos) ->
/// Option<(SymbolId, &str)>`. It wraps whichever combined-scanner engine
/// [`LexerConf::backend`] selected, so the lexers never branch on the engine and a
/// new backend lands behind this one seam (`docs/LEXER_DFA_PLAN.md`).
///
/// Static dispatch (an enum, not a trait object) keeps the hot per-position call a
/// direct branch — this runs once per token on the contextual lexer's pull path.
enum ScannerBackend {
    /// The `regex`-crate combined-alternation scanner (today's engine).
    Regex(Scanner),
    /// The `regex-automata` multi-pattern DFA scanner (phase L1).
    Dfa(DfaScanner),
}

impl ScannerBackend {
    /// Build the backend named by `backend` over the candidate terminals. Both
    /// engines reproduce Lark's selection byte-for-byte (the L0 differential oracle,
    /// `tests/test_scanner_differential.rs`, is the contract).
    fn build(
        terminals: &[(SymbolId, &TerminalDef)],
        global_flags: u32,
        backend: LexerBackend,
    ) -> Result<ScannerBackend, GrammarError> {
        match backend {
            LexerBackend::Regex => Ok(ScannerBackend::Regex(Scanner::build(
                terminals,
                global_flags,
            )?)),
            LexerBackend::Dfa => Ok(ScannerBackend::Dfa(DfaScanner::build(
                terminals,
                global_flags,
            )?)),
        }
    }

    /// Match a single token starting exactly at `pos` — the seam every lexer uses.
    #[inline]
    fn match_at<'t>(&self, text: &'t str, pos: usize) -> Option<(SymbolId, &'t str)> {
        match self {
            ScannerBackend::Regex(s) => s.match_at(text, pos),
            ScannerBackend::Dfa(s) => s.match_at(text, pos),
        }
    }
}

// ─── BasicLexer ──────────────────────────────────────────────────────────────

/// Scans the whole input with a single combined regex over all terminals.
pub struct BasicLexer {
    scanner: ScannerBackend,
    names: HashMap<SymbolId, String>,
    ignore: HashSet<SymbolId>,
}

impl BasicLexer {
    pub fn new(conf: &LexerConf) -> Result<Self, GrammarError> {
        let refs: Vec<(SymbolId, &TerminalDef)> =
            conf.terminals.iter().map(|(id, t)| (*id, t)).collect();
        let scanner = ScannerBackend::build(&refs, conf.global_flags, conf.backend)?;
        Ok(BasicLexer {
            scanner,
            names: conf.names(),
            ignore: conf.ignore.iter().copied().collect(),
        })
    }

    /// The single token the combined scanner matches starting **exactly** at byte
    /// `pos` — the terminal id (after `unless` retyping) and the matched slice — or
    /// `None` if nothing matches there. This is the raw `match_at` seam without the
    /// streaming loop or `%ignore` handling; it lets the L2 lowering harness probe a
    /// terminal's anchored match at a position without lexing the whole input
    /// (`tests/common/lowering.rs`).
    pub fn match_at<'t>(&self, text: &'t str, pos: usize) -> Option<(SymbolId, &'t str)> {
        self.scanner.match_at(text, pos)
    }
}

/// A running source cursor (byte offset + 1-based line/column), advanced one
/// matched span or one skipped character at a time. Shared by the eager
/// [`BasicLexer::lex`] and the recovering [`BasicLexer::lex_recovering`] so the
/// newline-aware position bookkeeping lives in exactly one place.
struct LexCursor {
    pos: usize,
    line: usize,
    col: usize,
}

impl LexCursor {
    fn new() -> Self {
        LexCursor {
            pos: 0,
            line: 1,
            col: 1,
        }
    }

    /// Advance the cursor over `value` (a matched terminal slice or a skipped
    /// run of text), tracking newlines.
    fn feed(&mut self, value: &str) {
        for ch in value.chars() {
            if ch == '\n' {
                self.line += 1;
                self.col = 1;
            } else {
                self.col += 1;
            }
        }
        self.pos += value.len();
    }
}

impl BasicLexer {
    /// Build a [`Token`] for a matched terminal `id`/`value` starting at `start`,
    /// advancing `cur` past it. Returns `None` when the matched terminal is an
    /// `%ignore` type (it still advances the cursor, but produces no token).
    fn make_token(&self, cur: &mut LexCursor, id: SymbolId, value: &str) -> Option<Token> {
        let start_pos = cur.pos;
        let start_line = cur.line;
        let start_col = cur.col;
        cur.feed(value);
        if self.ignore.contains(&id) {
            return None;
        }
        Some(Token {
            type_id: id,
            type_: self.names[&id].clone(),
            value: value.to_string(),
            line: start_line,
            column: start_col,
            end_line: cur.line,
            end_column: cur.col,
            start_pos,
            end_pos: cur.pos,
        })
    }

    /// Lex with character-level error recovery (issue #93). Mirrors [`lex`] but,
    /// at an un-lexable position (no terminal matches), records an
    /// [`UnexpectedCharacter`] error, consults `on_error`, and — if it returns
    /// `true` — skips **exactly one character** and resumes, rather than aborting.
    /// This is the lexer-side analogue of Python Lark's `on_error` loop, whose
    /// `UnexpectedCharacters` branch feeds one char forward
    /// (`s.line_ctr.feed(text[p:p+1])`) and resumes: the handler therefore fires
    /// once per skipped character (two consecutive bad chars = two invocations),
    /// and every skip is appended to `errors`.
    ///
    /// Returns the surviving token stream (terminated by `$END`); the caller then
    /// drives the token-level recovery loop over it, so token-level and
    /// character-level deletions accumulate into the same `errors` list. If
    /// `on_error` returns `false` on a skip, lexing stops there and the tokens
    /// collected so far are returned (with `$END` appended at that position) —
    /// the lexer equivalent of the token loop's "stop with the partial".
    ///
    /// [`lex`]: BasicLexer::lex
    /// [`UnexpectedCharacter`]: crate::error::ParseError::UnexpectedCharacter
    pub fn lex_recovering(
        &self,
        text: &str,
        on_error: &mut dyn FnMut(&ParseError) -> bool,
        errors: &mut Vec<ParseError>,
    ) -> Vec<Token> {
        let mut tokens = Vec::new();
        let mut cur = LexCursor::new();

        while cur.pos < text.len() {
            match self.scanner.match_at(text, cur.pos) {
                Some((id, value)) => {
                    if let Some(tok) = self.make_token(&mut cur, id, value) {
                        tokens.push(tok);
                    }
                }
                None => {
                    let ch = text[cur.pos..].chars().next().unwrap();
                    let err = ParseError::UnexpectedCharacter {
                        ch,
                        line: cur.line,
                        col: cur.col,
                        pos: cur.pos,
                        expected: "any token".to_string(),
                    };
                    let cont = on_error(&err);
                    errors.push(err);
                    if !cont {
                        break;
                    }
                    // Skip exactly one character and resume, as Python advances
                    // its line counter by `text[p:p+1]`.
                    cur.feed(&text[cur.pos..cur.pos + ch.len_utf8()]);
                }
            }
        }

        tokens.push(Token::end().with_position(cur.line, cur.col, cur.pos, cur.pos));
        tokens
    }
}

impl Lexer for BasicLexer {
    fn lex(&self, text: &str) -> Result<Vec<Token>, ParseError> {
        let mut tokens = Vec::new();
        let mut cur = LexCursor::new();

        while cur.pos < text.len() {
            match self.scanner.match_at(text, cur.pos) {
                Some((id, value)) => {
                    if let Some(tok) = self.make_token(&mut cur, id, value) {
                        tokens.push(tok);
                    }
                }
                None => {
                    let ch = text[cur.pos..].chars().next().unwrap();
                    return Err(ParseError::UnexpectedCharacter {
                        ch,
                        line: cur.line,
                        col: cur.col,
                        pos: cur.pos,
                        expected: "any token".to_string(),
                    });
                }
            }
        }

        tokens.push(Token::end().with_position(cur.line, cur.col, cur.pos, cur.pos));
        Ok(tokens)
    }
}

// ─── ContextualLexer ─────────────────────────────────────────────────────────

/// A lexer that narrows the candidate terminals to those valid in the current
/// LALR parser state. States with the same terminal set share one scanner
/// (Python Lark's `lexer_by_tokens` dedup — measured 4–5× fewer scanners on the
/// wild bank), and each scanner is built lazily on first use (Python's
/// `BasicLexer.scanner` property), so states an input never visits cost nothing.
/// Keyword/identifier disambiguation (the `unless` retyping) is still computed
/// per terminal-set, exactly as Python Lark builds one `TraditionalLexer` per
/// distinct set.
pub struct ContextualLexer {
    /// LALR state id → index into `scanners`. States whose terminal sets are
    /// equal map to the same index. State 0 is the root (fallback) entry.
    state_to_scanner: HashMap<usize, usize>,
    /// One entry per distinct terminal set, built lazily on first use.
    scanners: Vec<LazyScanner>,
    /// Owned terminal definitions the lazy builds draw from.
    terminals: HashMap<SymbolId, TerminalDef>,
    global_flags: u32,
    backend: LexerBackend,
    names: HashMap<SymbolId, String>,
    ignore: HashSet<SymbolId>,
}

/// A per-terminal-set scanner slot, built on first use. Single-threaded by
/// design ([`Lark`](crate::Lark) is not `Sync` — the `regex` backend already
/// holds a `RefCell` scratch buffer), so a plain `OnceCell` suffices.
struct LazyScanner {
    /// Sorted, deduped terminal ids — the dedup key. Scanner construction is
    /// order-independent ([`scanner_plan`] sorts by `(-priority, -len, name)`,
    /// a total order), so the set fully determines the scanner.
    term_ids: Vec<SymbolId>,
    cell: std::cell::OnceCell<ScannerBackend>,
}

impl LazyScanner {
    fn get_or_build(
        &self,
        terminals: &HashMap<SymbolId, TerminalDef>,
        global_flags: u32,
        backend: LexerBackend,
    ) -> &ScannerBackend {
        self.cell.get_or_init(|| {
            let terms: Vec<(SymbolId, &TerminalDef)> = self
                .term_ids
                .iter()
                .map(|id| (*id, &terminals[id]))
                .collect();
            // Cannot fail: every terminal here was already routed/lowered by the
            // full-set validation build in `ContextualLexer::new`, and a subset
            // alternation introduces no new failure mode (`compute_unless` pairs
            // and DFA patterns are each a subset of the validated full set).
            ScannerBackend::build(&terms, global_flags, backend).expect(
                "per-state scanner build failed after the full-terminal validation \
                 build succeeded — this is a lark-rs bug",
            )
        })
    }
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
        let terminals: HashMap<SymbolId, TerminalDef> = conf.terminals.iter().cloned().collect();

        // Validate every terminal once, eagerly, by building (and discarding) the
        // full-terminal scanner — the per-state scanners are built lazily on first
        // use, and a grammar whose terminals the lexer refuses (the categorized
        // lookaround scope errors, `docs/LOOKAROUND_SCOPE.md`) must still fail at
        // construction time, not mid-parse. Python Lark's `ContextualLexer` does
        // the same: its eager `root_lexer` init validates every terminal. Pinned by
        // `tests/test_lookaround_scope.rs::scoreboard_rejects_every_case_with_its_category`
        // (every scope case through `Lark::new` on LALR × contextual).
        //
        // This refuses exactly what the per-state builds would have refused: the
        // loader prunes terminals no rule or `%ignore` references (its
        // `_remove_unused` port), so `conf.terminals` is precisely the union of
        // the state sets plus `always_accept` — there is no "unused but broken"
        // terminal this build newly rejects. The one genuinely new failure
        // surface is a combined-build resource limit (one automaton over the
        // union where the old code built only per-state subsets), which matches
        // the basic lexer's existing behavior on the same set.
        {
            let all: Vec<(SymbolId, &TerminalDef)> =
                conf.terminals.iter().map(|(id, t)| (*id, t)).collect();
            ScannerBackend::build(&all, conf.global_flags, conf.backend)?;
        }

        let mut key_to_idx: HashMap<Vec<SymbolId>, usize> = HashMap::new();
        let mut scanners: Vec<LazyScanner> = Vec::new();
        let mut state_to_scanner = HashMap::new();
        for (state_id, valid_ids) in state_terminals {
            let mut ids: Vec<SymbolId> = valid_ids
                .iter()
                .chain(always_accept.iter())
                .filter(|id| terminals.contains_key(id))
                .copied()
                .collect();
            ids.sort_unstable();
            ids.dedup();
            if ids.is_empty() {
                continue;
            }
            let idx = *key_to_idx.entry(ids.clone()).or_insert_with(|| {
                scanners.push(LazyScanner {
                    term_ids: ids,
                    cell: std::cell::OnceCell::new(),
                });
                scanners.len() - 1
            });
            state_to_scanner.insert(*state_id, idx);
        }

        Ok(ContextualLexer {
            state_to_scanner,
            scanners,
            terminals,
            global_flags: conf.global_flags,
            backend: conf.backend,
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
        let scanner = match self
            .state_to_scanner
            .get(&state)
            .or_else(|| self.state_to_scanner.get(&0))
        {
            Some(idx) => {
                self.scanners[*idx].get_or_build(&self.terminals, self.global_flags, self.backend)
            }
            None => return Ok(None),
        };

        if let Some((id, value)) = scanner.match_at(text, pos) {
            let end = pos + value.len();
            // End position is char-based and newline-aware: a token spanning a
            // newline advances the line and resets the column.
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
            ch,
            line,
            col,
            pos,
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
        LexerState {
            text,
            pos: 0,
            line: 1,
            col: 1,
        }
    }

    pub fn is_done(&self) -> bool {
        self.pos >= self.text.len()
    }

    /// Advance `n` bytes, walking the consumed text so line/col stay
    /// newline-aware (columns count characters, not bytes).
    pub fn advance_by(&mut self, n: usize) {
        for ch in self.text[self.pos..self.pos + n].chars() {
            if ch == '\n' {
                self.line += 1;
                self.col = 1;
            } else {
                self.col += 1;
            }
        }
        self.pos += n;
    }
}
