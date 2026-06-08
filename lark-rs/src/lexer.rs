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

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};

use regex::{CaptureLocations, Regex};
use regex_automata::{
    dfa::{dense, Automaton, StartKind},
    hybrid::dfa::DFA as LazyDfa,
    meta::Regex as MetaRegex,
    util::primitives::StateID,
    Anchored, Input, MatchKind,
};

use crate::error::{GrammarError, ParseError};
use crate::grammar::intern::SymbolId;
use crate::grammar::terminal::{Pattern, PatternRe, TerminalDef};
use crate::lookaround::classify::{lower_terminal, Lowered};
use crate::lookaround::lower::LoweredMatcher;
use crate::tree::Token;

// ─── AnyRegex: a per-terminal regex that may need lookaround ──────────────────
//
// The combined scanner is built on the linear-time `regex` crate, which has no
// lookahead/lookbehind. A few bundled-grammar terminals (issue #40) use bounded
// lookaround; those are compiled to `fancy-regex` instead and matched one terminal
// at a time. `AnyRegex` hides the choice behind a uniform anchored-match API so the
// caller never branches on the engine. Because `regex`'s language is a subset of
// `fancy-regex`'s, a pattern is only ever sent to `fancy-regex` when `regex`
// rejects it — every ordinary terminal keeps the fast engine.

enum AnyRegex {
    Plain(Regex),
    Fancy(fancy_regex::Regex),
}

impl AnyRegex {
    /// Compile `src`, preferring the linear `regex` engine and only falling back to
    /// `fancy-regex` for patterns `regex` cannot express (lookaround). An error from
    /// *both* engines surfaces the `regex`-crate message (the familiar one).
    fn compile(src: &str) -> Result<AnyRegex, GrammarError> {
        match Regex::new(src) {
            Ok(re) => Ok(AnyRegex::Plain(re)),
            Err(plain_err) => match fancy_regex::Regex::new(src) {
                Ok(re) => Ok(AnyRegex::Fancy(re)),
                Err(_) => Err(GrammarError::InvalidRegex {
                    pattern: src.to_string(),
                    reason: plain_err.to_string(),
                }),
            },
        }
    }

    /// Whether this pattern needed the backtracking engine (i.e. uses lookaround).
    fn is_fancy(&self) -> bool {
        matches!(self, AnyRegex::Fancy(_))
    }

    /// End offset of a non-empty match beginning *exactly* at `pos`, or `None`.
    /// The full `text` (not a suffix) is passed so a lookbehind can see the bytes
    /// before `pos`.
    fn match_end_at(&self, text: &str, pos: usize) -> Option<usize> {
        match self {
            AnyRegex::Plain(re) => {
                let m = re.find_at(text, pos);
                record_scan_skip(pos, m.as_ref().map(|m| m.start()));
                let m = m?;
                (m.start() == pos && m.end() > pos).then_some(m.end())
            }
            AnyRegex::Fancy(re) => {
                let m = re.find_from_pos(text, pos).ok().flatten();
                record_scan_skip(pos, m.as_ref().map(|m| m.start()));
                let m = m?;
                (m.start() == pos && m.end() > pos).then_some(m.end())
            }
        }
    }

    /// End offset of a non-empty match anchored at the start of `sub`, or `None`.
    fn match_end_in(&self, sub: &str) -> Option<usize> {
        match self {
            AnyRegex::Plain(re) => {
                let m = re.find(sub)?;
                (m.start() == 0 && m.end() > 0).then_some(m.end())
            }
            AnyRegex::Fancy(re) => {
                let m = re.find(sub).ok()??;
                (m.start() == 0 && m.end() > 0).then_some(m.end())
            }
        }
    }

    /// Whether the pattern matches `text` in full (used by `unless` retyping, where
    /// `src` is already anchored with `^…$`).
    fn is_full_match(&self, text: &str) -> bool {
        match self {
            AnyRegex::Plain(re) => re.is_match(text),
            AnyRegex::Fancy(re) => re.is_match(text).unwrap_or(false),
        }
    }
}

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

/// Which engine backs the per-position match (`Scanner::match_at`). Selects between
/// the two combined-scanner implementations behind the single [`ScannerBackend`]
/// seam, with no behavioral difference — both reproduce Lark's leftmost-first
/// selection, `unless` retyping, and lookaround side-probes byte-for-byte (the L0
/// differential oracle in `tests/test_scanner_differential.rs` is the contract).
///
///   * [`Regex`](LexerBackend::Regex) — the original `regex`-crate combined
///     alternation with capture groups (the default; see [`Scanner`]).
///   * [`Dfa`](LexerBackend::Dfa) — a `regex-automata` multi-pattern DFA
///     (`docs/LEXER_DFA_PLAN.md`, phase L1; see [`DfaScanner`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LexerBackend {
    /// The `regex`-crate combined-alternation scanner (today's engine).
    #[default]
    Regex,
    /// The `regex-automata` multi-pattern DFA scanner (phase L1).
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
    /// the original `regex`-crate [`Scanner`]; the DFA backend is opt-in.
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
    /// `regex`-crate [`Scanner`]; choosing [`LexerBackend::Dfa`] swaps in the
    /// `regex-automata` engine for the lookaround-free terminals without changing
    /// any lexing semantics.
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

// ─── Scanner plan (shared with the standalone generator) ──────────────────────

/// The deterministic recipe for a combined scanner: the global-flag prefix, the
/// alternation members in order (each terminal id paired with its inline regex
/// source), and the `unless` keyword-retype map.
///
/// [`Scanner::build`] consumes this to compile a runtime scanner; the standalone
/// parser generator (`crate::standalone`) bakes the very same plan into `const`
/// data, so a generated parser's lexer is byte-identical to the in-process one.
#[derive(Debug, Clone)]
pub struct ScannerPlan {
    /// Leading inline-flag group for `g_regex_flags` (e.g. `(?i)`), or empty.
    pub global_prefix: String,
    /// `(terminal id, inline regex source)`, in alternation order.
    pub groups: Vec<(SymbolId, String)>,
    /// regex-terminal-id → (matched-text → keyword-terminal-id).
    pub unless: HashMap<SymbolId, HashMap<String, SymbolId>>,
}

/// Compute the [`ScannerPlan`] for a candidate terminal set, applying exactly the
/// selection, ordering and `unless`-embedding rules [`Scanner::build`] relies on.
/// Factored out so the runtime lexer and the standalone code generator agree by
/// construction.
pub fn scanner_plan(
    terminals: &[(SymbolId, &TerminalDef)],
    global_flags: u32,
) -> Result<ScannerPlan, GrammarError> {
    let mut seen = HashSet::new();
    let terms: Vec<(SymbolId, &TerminalDef)> = terminals
        .iter()
        .copied()
        .filter(|(id, _)| seen.insert(*id))
        .collect();

    // unless: embed string terminals fully matched by a same-priority regex
    // terminal, and record the retype.
    let unless = compute_unless(&terms, global_flags)?;
    let embedded: HashSet<SymbolId> = unless.values().flat_map(|m| m.values().copied()).collect();

    // Scanner terminals = everything not embedded, sorted Python-style.
    let mut scan: Vec<(SymbolId, &TerminalDef)> = terms
        .iter()
        .copied()
        .filter(|(id, _)| !embedded.contains(id))
        .collect();
    sort_terminals(&mut scan);

    let groups = scan
        .iter()
        .map(|(id, term)| (*id, term.pattern.to_inline_regex()))
        .collect();

    Ok(ScannerPlan {
        global_prefix: global_flag_prefix(global_flags),
        groups,
        unless,
    })
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
///
/// Two allocation-avoidance measures (profiling spike, 2026-06-04 — both shared
/// by the future Earley engine, which scans through this same `Scanner`):
///
///   * each terminal's capture-group *index* is resolved once at build time, so
///     `match_at` reads the winning group by number instead of hashing a group
///     *name* per token (the SipHash cost the profiler flagged);
///   * a single [`CaptureLocations`] scratch buffer is reused across matches
///     (`captures_read_at`) rather than allocating a fresh `Captures` per token.
struct Scanner {
    /// Combined alternation over every *plain* (`regex`-crate) terminal, or `None`
    /// when this scanner's terminals are all lookaround terminals. Returns the
    /// lowest-rank plain terminal matching at a position (leftmost-first).
    re: Option<Regex>,
    /// (terminal id, capture-group index, rank), in alternation order. `rank` is the
    /// terminal's index in the fully-sorted candidate list, so a plain match can be
    /// compared against a fancy match by who Python's combined alternation would
    /// reach first.
    groups: Vec<(SymbolId, usize, usize)>,
    /// Lookaround terminals (`fancy-regex`), each matched individually. Stored in
    /// ascending `rank` order, so the first one that matches is the lowest-rank
    /// fancy candidate. Empty for the overwhelming common case (no lookaround).
    fancy: Vec<(usize, SymbolId, AnyRegex)>,
    /// regex-terminal-id → (matched-text → keyword-terminal-id).
    unless: HashMap<SymbolId, HashMap<String, SymbolId>>,
    /// Reused match-location scratch, sized for `re`. `RefCell` because the hot
    /// `match_at` runs behind `&self` (the contextual lexer's per-token path).
    locs: Option<RefCell<CaptureLocations>>,
}

impl Scanner {
    /// Build a scanner from candidate terminals (deduplicated by id).
    ///
    /// `global_flags` is Lark's `g_regex_flags`: a flag bitset applied to the
    /// whole combined regex (and to the `unless` membership tests) so that, e.g.,
    /// `IGNORECASE` makes every terminal — string literals included — match
    /// case-insensitively, without mutating the individual `TerminalDef`s.
    fn build(
        terminals: &[(SymbolId, &TerminalDef)],
        global_flags: u32,
    ) -> Result<Scanner, GrammarError> {
        // The selection + ordering + unless retyping is shared with the standalone
        // generator (`scanner_plan`) so a baked scanner is byte-identical to this
        // runtime one.
        let plan = scanner_plan(terminals, global_flags)?;
        let unless = plan.unless;
        let prefix = plan.global_prefix;

        // Split the plan's (rank-ordered) terminals into *plain* terminals — which
        // go into the single fast combined `regex` alternation — and *lookaround*
        // terminals, which the `regex` crate cannot compile and so are matched
        // individually via `fancy-regex` (issue #40). `rank` is the index in the
        // plan's sorted order: Python builds the alternation in this order and takes
        // the first branch that matches, so the lowest rank wins ties. We preserve it
        // to merge the two engines' candidates at match time.
        let mut parts = Vec::new();
        let mut group_names = Vec::new();
        let mut fancy: Vec<(usize, SymbolId, AnyRegex)> = Vec::new();
        for (rank, (id, inline)) in plan.groups.iter().enumerate() {
            // `to_inline_regex` (used by `scanner_plan`) keeps per-terminal flags
            // (e.g. `(?i:…)` for a case-insensitive terminal) scoped to this group.
            let compiled = AnyRegex::compile(&format!("{prefix}{inline}"))?;
            if compiled.is_fancy() {
                // Anchor the per-position fancy match to `pos` with `\G` (start-of-
                // search anchor). Without it, `fancy-regex`'s `find_from_pos` scans
                // *forward* to the next match, so trying a sparse lookaround terminal
                // (e.g. python.lark's `STRING`) at every position is O(n²) over the
                // input. `\G` makes the search fail immediately when nothing matches
                // at `pos`. Recompiled separately because the `regex` crate cannot
                // parse `\G`, so this pattern stays on the `fancy-regex` engine.
                let anchored = AnyRegex::compile(&format!("{prefix}\\G{inline}"))?;
                fancy.push((rank, *id, anchored));
            } else {
                let group = format!("g{}", id.0);
                parts.push(format!("(?P<{group}>{inline})"));
                group_names.push((*id, group, rank));
            }
        }

        // The combined regex over plain terminals (skipped entirely when every
        // candidate is a lookaround terminal).
        let (re, groups, locs) = if parts.is_empty() {
            (None, Vec::new(), None)
        } else {
            let pattern = format!("{}{}", prefix, parts.join("|"));
            let re = Regex::new(&pattern).map_err(|e| GrammarError::InvalidRegex {
                pattern: pattern.clone(),
                reason: e.to_string(),
            })?;
            // Resolve each terminal's named group to its capture *index* once. A
            // terminal pattern may itself contain capturing groups, so the index is
            // not the alternation position — read it from `capture_names` (which
            // enumerates every group in index order, unnamed ones as `None`).
            let name_to_idx: HashMap<&str, usize> = re
                .capture_names()
                .enumerate()
                .filter_map(|(i, n)| n.map(|n| (n, i)))
                .collect();
            let groups = group_names
                .iter()
                .map(|(id, name, rank)| (*id, name_to_idx[name.as_str()], *rank))
                .collect();
            let locs = Some(RefCell::new(re.capture_locations()));
            (Some(re), groups, locs)
        };

        Ok(Scanner {
            re,
            groups,
            fancy,
            unless,
            locs,
        })
    }

    /// Match a single token starting exactly at `pos`. Returns `(terminal id,
    /// value)`, with keyword retyping already applied. `None` means nothing
    /// matched here.
    ///
    /// The winner is the lowest-`rank` terminal that matches at `pos` — exactly the
    /// branch Python's combined `(A)|(B)|…` alternation reaches first. We get the
    /// lowest-rank *plain* terminal from the combined regex and the lowest-rank
    /// *fancy* terminal from the (rank-sorted) lookaround list, then keep whichever
    /// has the smaller rank.
    fn match_at<'t>(&self, text: &'t str, pos: usize) -> Option<(SymbolId, &'t str)> {
        // Lowest-rank plain candidate.
        let mut best: Option<(usize, SymbolId, &'t str)> = None;
        if let (Some(re), Some(locs)) = (&self.re, &self.locs) {
            let mut locs = locs.borrow_mut();
            let m0 = re.captures_read_at(&mut locs, text, pos);
            record_scan_skip(pos, m0.as_ref().map(|m| m.start()));
            if let Some(m0) = m0 {
                // Accept only a non-empty match beginning exactly at pos.
                if m0.start() == pos && m0.end() != pos {
                    let value = m0.as_str();
                    for (id, idx, rank) in &self.groups {
                        if locs.get(*idx).is_some() {
                            best = Some((*rank, *id, value));
                            break;
                        }
                    }
                }
            }
        }
        // Lowest-rank fancy candidate (the list is rank-sorted, so the first match
        // wins); keep it only if it out-ranks the plain candidate.
        for (rank, id, re) in &self.fancy {
            if best.is_some_and(|(b, _, _)| *rank > b) {
                break;
            }
            if let Some(end) = re.match_end_at(text, pos) {
                best = Some((*rank, *id, &text[pos..end]));
                break;
            }
        }

        let (_, id, value) = best?;
        let ty = self
            .unless
            .get(&id)
            .and_then(|m| m.get(value))
            .copied()
            .unwrap_or(id);
        Some((ty, value))
    }
}

// ─── DfaScanner: the combined scanner on a regex-automata multi-pattern DFA ────

/// The L1 combined scanner (`docs/LEXER_DFA_PLAN.md`). Same contract and selection
/// rules as [`Scanner`] — leftmost-first ranking, `unless` retyping, `fancy-regex`
/// side-probes — but the *plain* (lookaround-free) terminals are matched by one
/// [`regex_automata::meta::Regex`] over all of them (`new_many`, returning a
/// `PatternID`) instead of the `regex`-crate alternation-with-capture-groups trick.
///
/// Why this is byte-identical to [`Scanner`]:
///
///   * The plain patterns are handed to `new_many` **in rank order**, so `PatternID`
///     *is* the rank. Configured `MatchKind::LeftmostFirst`, the meta engine resolves
///     a same-start tie by **pattern order** (lowest `PatternID` wins) with that
///     pattern's own greedy length — exactly Python-`re`'s leftmost-first, the same
///     semantics the `regex`-crate alternation gives (verified: `[ab|abc]` at `"abc"`
///     picks pattern 0, length 2, *not* the longest match).
///   * The match is **anchored** at `pos` (`Anchored::Yes` over `pos..len`), so it
///     can only begin exactly at `pos` and never forward-scans — strictly safer than
///     the `regex`-crate `captures_read_at` (which is leftmost and relies on a
///     `start() == pos` reject). A zero-width match is rejected, mirroring `Scanner`.
///   * The `fancy` list, the rank-merge between the plain and fancy candidates, and
///     the `unless` retype are **copied verbatim** from `Scanner`: only the
///     plain-terminal engine changes.
///
/// The `regex` crate's combined alternation came with a free literal prefilter; an
/// *anchored* search makes the meta engine's own (forward-scanning) prefilter
/// dormant, so we re-add an explicit **start-byte prefilter** (`start_bytes`): the
/// set of bytes any plain terminal can begin with, computed once from a lazy DFA
/// over the plain union. When the byte at `pos` isn't in it we skip the plain engine
/// entirely. It is an over-approximation by construction (a possible start byte is
/// never dropped), so it can only ever *save* an engine call, never change a match —
/// the L0 differential oracle is the proof.
struct DfaScanner {
    /// Multi-pattern leftmost-first engine over the plain terminals, or `None` when
    /// every candidate is a lookaround terminal. `PatternID i` indexes `plain[i]`.
    re: Option<MetaRegex>,
    /// `PatternID` → (terminal id, rank), parallel to the patterns given to
    /// `new_many` (rank order), so a plain match's rank can be compared against a
    /// per-terminal probe's.
    plain: Vec<(SymbolId, usize)>,
    /// Start-byte prefilter for the plain engine (see the struct docs). `None`
    /// disables it (always run the engine) — the safe default if it can't be built.
    start_bytes: Option<Box<[bool; 256]>>,
    /// Per-terminal probes for the lookaround terminals, **rank-sorted** so the first
    /// one that matches is the lowest-rank lookaround candidate. Each is either a
    /// **lowered** matcher (a landed L2 shape — `regex-automata` only, no
    /// `fancy-regex`) or a `fancy-regex` side-probe (a shape whose lowering has not
    /// landed, or an unsupported assertion the grammar still uses). The split is keyed
    /// on [`lower_terminal`], so a terminal flips from `Fancy` to `Lowered` the moment
    /// its shape lands — the same gate the differential oracle reads.
    probes: Vec<(usize, SymbolId, Probe)>,
    /// regex-terminal-id → (matched-text → keyword-terminal-id) — identical retype.
    unless: HashMap<SymbolId, HashMap<String, SymbolId>>,
}

/// One per-terminal lookaround probe: a lowered (landed-shape) matcher or a
/// `fancy-regex` side-probe. Both answer "non-empty match ending at, exactly
/// beginning at, `pos`", so the rank-merge in [`DfaScanner::match_at`] treats them
/// uniformly.
enum Probe {
    /// A landed L2 shape, lowered to lookaround-free `regex-automata` matching.
    Lowered(LoweredMatcher),
    /// A not-yet-lowered (or unsupported) lookaround terminal on `fancy-regex`,
    /// anchored with `\G` exactly as [`Scanner`] does.
    Fancy(AnyRegex),
}

impl Probe {
    /// End offset of a non-empty match beginning exactly at `pos`, or `None`. The
    /// lexer rejects a zero-width match (mirroring [`AnyRegex::match_end_at`]), so a
    /// lowered matcher's raw zero-width prefix is dropped here too.
    fn match_end_at(&self, text: &str, pos: usize) -> Option<usize> {
        match self {
            Probe::Lowered(m) => {
                // An anchored probe: bounded by the matched token, charged a flat 1
                // to the scan counter like `record_scan_skip(pos, Some(pos))`.
                crate::perf::add_lexer_scan_steps(1);
                m.match_prefix(text, pos).filter(|&end| end > pos)
            }
            Probe::Fancy(re) => re.match_end_at(text, pos),
        }
    }
}

impl DfaScanner {
    /// Build a DFA scanner from candidate terminals (deduplicated by id). Consumes
    /// the same [`ScannerPlan`] as [`Scanner::build`], so selection / ordering /
    /// `unless` are shared by construction — only the plain engine differs.
    fn build(
        terminals: &[(SymbolId, &TerminalDef)],
        global_flags: u32,
    ) -> Result<DfaScanner, GrammarError> {
        let plan = scanner_plan(terminals, global_flags)?;
        let unless = plan.unless;
        let prefix = plan.global_prefix;

        // id → terminal definition, so a lookaround terminal can be classified and
        // lowered from its *own* (un-prefixed) regex source — the same string the
        // differential oracle gates `lower_terminal` on.
        let by_id: HashMap<SymbolId, &TerminalDef> =
            terminals.iter().map(|(id, t)| (*id, *t)).collect();

        // Split the rank-ordered plan into plain terminals (one combined meta DFA)
        // and lookaround terminals (per-terminal probes), exactly as `Scanner::build`
        // does. `rank` is the index in the plan's sorted order. A lookaround terminal
        // whose shape has **landed** lowers to a `regex-automata`-only `Probe::Lowered`
        // (no `fancy-regex`); the rest stay on the `fancy-regex` side-probe.
        let mut patterns: Vec<String> = Vec::new();
        let mut inlines: Vec<&str> = Vec::new();
        let mut plain: Vec<(SymbolId, usize)> = Vec::new();
        let mut probes: Vec<(usize, SymbolId, Probe)> = Vec::new();
        for (rank, (id, inline)) in plan.groups.iter().enumerate() {
            let src = format!("{prefix}{inline}");
            // Same fancy-detection as `Scanner`: a pattern is fancy iff the `regex`
            // crate (a subset of `fancy-regex`) rejects it.
            let compiled = AnyRegex::compile(&src)?;
            if compiled.is_fancy() {
                // A lookaround terminal. If its shape has landed (and no global flags
                // are in play — the lowered fragments do not carry them yet), lower it
                // to a `regex-automata`-only matcher; otherwise fall back to the
                // `\G`-anchored `fancy-regex` side-probe, exactly as today.
                let name = by_id.get(id).map(|t| t.name.as_str()).unwrap_or("");
                let raw = by_id
                    .get(id)
                    .map(|t| t.pattern.to_inline_regex())
                    .unwrap_or_else(|| inline.to_string());
                let lowered = if global_flags == 0 {
                    match lower_terminal(name, &raw) {
                        Ok(low @ Lowered::Trailing(_)) => Some(LoweredMatcher::build(&low)?),
                        _ => None,
                    }
                } else {
                    None
                };
                match lowered {
                    Some(m) => probes.push((rank, *id, Probe::Lowered(m))),
                    None => {
                        let anchored = AnyRegex::compile(&format!("{prefix}\\G{inline}"))?;
                        probes.push((rank, *id, Probe::Fancy(anchored)));
                    }
                }
            } else {
                plain.push((*id, rank));
                inlines.push(inline);
                patterns.push(src);
            }
        }
        // Rank-sort the probes so the first match is the lowest-rank lookaround
        // candidate (mixing lowered and fancy probes, which can interleave by rank).
        probes.sort_by_key(|(rank, _, _)| *rank);

        let (re, start_bytes) = if patterns.is_empty() {
            (None, None)
        } else {
            // Each plain terminal is its own pattern; the global-flag prefix sits at
            // the front of every pattern (an inline `(?i)` covers the whole pattern),
            // mirroring the prefix the `regex`-crate alternation places once at the
            // front of `(?i)(g0)|(g1)|…`.
            let re = MetaRegex::builder()
                .configure(MetaRegex::config().match_kind(MatchKind::LeftmostFirst))
                .build_many(&patterns)
                .map_err(|e| GrammarError::InvalidRegex {
                    pattern: patterns.join("|"),
                    reason: e.to_string(),
                })?;
            // Start-byte prefilter over the plain union `{prefix}(?:i0|i1|…)`.
            let union = format!("{prefix}(?:{})", inlines.join("|"));
            (Some(re), plain_start_bytes(&union))
        };

        Ok(DfaScanner {
            re,
            plain,
            start_bytes,
            probes,
            unless,
        })
    }

    /// Match a single token starting exactly at `pos` — the same contract as
    /// [`Scanner::match_at`], so the two are byte-for-byte interchangeable.
    fn match_at<'t>(&self, text: &'t str, pos: usize) -> Option<(SymbolId, &'t str)> {
        // Lowest-rank plain candidate, from the anchored multi-pattern search.
        let mut best: Option<(usize, SymbolId, &'t str)> = None;
        if let Some(re) = &self.re {
            // Start-byte prefilter: skip the engine when no plain terminal can begin
            // with the byte at `pos`. Over-approximated, so this never hides a match.
            let runnable = match &self.start_bytes {
                Some(set) => text.as_bytes().get(pos).is_some_and(|b| set[*b as usize]),
                None => true,
            };
            if runnable {
                // Anchored at `pos`: the match (if any) begins exactly here and the
                // search never scans forward — so the per-position cost is bounded by
                // the matched token's length (charged as a flat 1 to the scan
                // counter, like an anchored probe).
                record_scan_skip(pos, Some(pos));
                let input = Input::new(text)
                    .span(pos..text.len())
                    .anchored(Anchored::Yes);
                if let Some(m) = re.find(input) {
                    // Reject a zero-width match (no terminal is nullable, but keep the
                    // guard so the loop can never stall) — mirrors `Scanner`.
                    if m.end() > pos {
                        let (id, rank) = self.plain[m.pattern().as_usize()];
                        best = Some((rank, id, &text[pos..m.end()]));
                    }
                }
            }
        }
        // Lowest-rank lookaround candidate (lowered or fancy), rank-sorted so the
        // first match wins; keep it only if it out-ranks the plain candidate. Same
        // shape as `Scanner::match_at`'s fancy loop.
        for (rank, id, probe) in &self.probes {
            if best.is_some_and(|(b, _, _)| *rank > b) {
                break;
            }
            if let Some(end) = probe.match_end_at(text, pos) {
                best = Some((*rank, *id, &text[pos..end]));
                break;
            }
        }

        let (_, id, value) = best?;
        let ty = self
            .unless
            .get(&id)
            .and_then(|m| m.get(value))
            .copied()
            .unwrap_or(id);
        Some((ty, value))
    }
}

/// Match the single terminal `pattern` (named `name`) at the **start** of `input`
/// under the L2 lowering, returning the **raw** matched-prefix length in *characters*
/// (a zero-width match returns `Some(0)`, mirroring `fancy-regex`'s anchored `find`),
/// or `None` if it does not match at offset 0. `Err` if the terminal does not lower
/// to a landed shape — so the terminal-level equivalence/proof harness can never pass
/// by silently comparing `fancy-regex` against itself.
///
/// This is the lowered-matcher hook `tests/common/lowering.rs::lowered_prefix` calls:
/// it keeps the comparison at the engine boundary, lowered-path only.
pub fn lowered_match_prefix(
    name: &str,
    pattern: &str,
    input: &str,
) -> Result<Option<usize>, GrammarError> {
    use std::rc::Rc;

    thread_local! {
        // Compile once per pattern, reuse across inputs — the equivalence/proof
        // harness calls this for every string in an exhaustive corpus, so rebuilding
        // the matcher (a dense DFA + regexes) per call would be quadratically slow.
        static CACHE: RefCell<HashMap<String, Rc<LoweredMatcher>>> = RefCell::new(HashMap::new());
    }

    let cached = CACHE.with(|c| c.borrow().get(pattern).cloned());
    let matcher = match cached {
        Some(m) => m,
        None => {
            // Only a genuinely-lowered (landed) shape; a plain or pending/unsupported
            // pattern is an error (the harness must not measure a non-lowered term).
            let lowered = lower_terminal(name, pattern)?;
            if !matches!(lowered, Lowered::Trailing(_)) {
                return Err(GrammarError::Other {
                    msg: format!("terminal `{name}`: {pattern:?} is not a lowered shape"),
                });
            }
            // Validate the pattern compiles as a terminal too (parity with the build
            // path; surfaces a malformed pattern as an error, not a panic).
            PatternRe::new(pattern, 0)?;
            let m = Rc::new(LoweredMatcher::build(&lowered)?);
            CACHE.with(|c| c.borrow_mut().insert(pattern.to_string(), m.clone()));
            m
        }
    };
    Ok(matcher
        .match_prefix(input, 0)
        .map(|end| input[..end].chars().count()))
}

/// The set of bytes any branch of the plain union `src` can begin a match with, or
/// `None` if it cannot be computed (so the prefilter is disabled — always run the
/// engine). Built from a **lazy** (hybrid) DFA so only the start state and its 256
/// transitions are realized — no full determinization, hence no blow-up on a large
/// terminal set. A byte is "possible" iff the anchored start state does not go dead
/// on it; non-accepting live transitions are kept too, so the set is an
/// over-approximation (the safe direction: it never drops a real start byte).
fn plain_start_bytes(src: &str) -> Option<Box<[bool; 256]>> {
    let dfa = LazyDfa::new(src).ok()?;
    let mut cache = dfa.create_cache();
    let anchored = Input::new("").anchored(Anchored::Yes);
    let start = dfa.start_state_forward(&mut cache, &anchored).ok()?;
    let mut set = Box::new([false; 256]);
    for b in 0u8..=255 {
        let next = dfa.next_state(&mut cache, start, b).ok()?;
        if !next.is_dead() && !next.is_quit() {
            set[b as usize] = true;
        }
    }
    Some(set)
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

/// For each regex terminal, find the same-priority string terminals it fully
/// matches; those strings are embedded (dropped from the alternation) and
/// retyped after the fact. Mirrors Python Lark's `_create_unless`.
fn compute_unless(
    terms: &[(SymbolId, &TerminalDef)],
    global_flags: u32,
) -> Result<HashMap<SymbolId, HashMap<String, SymbolId>>, GrammarError> {
    let res: Vec<&(SymbolId, &TerminalDef)> = terms
        .iter()
        .filter(|(_, t)| matches!(t.pattern, Pattern::Re(_)))
        .collect();
    let strs: Vec<&(SymbolId, &TerminalDef)> = terms
        .iter()
        .filter(|(_, t)| matches!(t.pattern, Pattern::Str(_)))
        .collect();
    if res.is_empty() || strs.is_empty() {
        return Ok(HashMap::new());
    }

    let mut unless: HashMap<SymbolId, HashMap<String, SymbolId>> = HashMap::new();
    for (re_id, re_t) in &res {
        let full_src = format!(
            "{}^(?:{})$",
            global_flag_prefix(global_flags),
            re_t.pattern.to_inline_regex()
        );
        // A lookaround terminal (issue #40) cannot compile under the `regex` crate;
        // `AnyRegex` falls back to `fancy-regex` so keyword embedding still works.
        let full = AnyRegex::compile(&full_src)?;
        for (s_id, s_t) in &strs {
            if s_t.priority != re_t.priority {
                continue;
            }
            let value = match &s_t.pattern {
                Pattern::Str(p) => &p.value,
                Pattern::Re(_) => continue,
            };
            if full.is_full_match(value) {
                unless
                    .entry(*re_id)
                    .or_default()
                    .insert(value.clone(), *s_id);
            }
        }
    }
    Ok(unless)
}

/// The leading inline-flag group (`(?i)`, `(?im)`, …) for Lark's `g_regex_flags`,
/// or an empty string when no global flags are set. Placed at the very start of a
/// pattern it applies to the entire combined regex (every alternation branch),
/// mirroring `re.compile(pattern, flags=g_regex_flags)`.
fn global_flag_prefix(global_flags: u32) -> String {
    let letters = crate::grammar::terminal::flag_letters(global_flags);
    if letters.is_empty() {
        String::new()
    } else {
        format!("(?{letters})")
    }
}

// ─── Strict-mode regex-collision detection (issue #35) ───────────────────────
//
// Python Lark delegates this to `interegular` (`lexer.py::_check_regex_collisions`):
// it groups the *regex* terminals by priority, compiles each to an FSM, and reports
// a collision when two same-priority regexes can match a common string — raising a
// `LexError` under `strict=True` (a warning otherwise).
//
// The `regex` crate offers no intersection/emptiness test, so we drop to its
// `regex-automata` layer. Each terminal's regex is compiled to a **whole-match**
// DFA (anchored at the start; acceptance is checked only at the end-of-input
// transition, so the DFA accepts exactly the strings the terminal matches in
// full). Two terminals collide iff the *product* of their DFAs has a reachable
// state that is accepting in both — classic product-construction emptiness. A BFS
// over byte-labelled state pairs both decides emptiness and yields the shortest
// witness string, which we surface in the error the way interegular surfaces its
// example overlap.
//
// We only ever act in strict mode (Lark's non-strict path just logs a warning,
// and lark-rs has no warning channel), so this never runs on the default build
// path — there is zero cost unless the user opts into `strict=True`.

/// Cap on distinct product states explored per terminal pair. A pathological
/// pair (huge unicode classes, bounded repetitions) could otherwise make the BFS
/// run for a long time; like Python Lark's `max_time` budget we'd rather *under*-
/// report a collision than hang. Generous enough that real terminal overlaps —
/// which are tiny — are always found.
const MAX_PRODUCT_STATES: usize = 200_000;

/// Size ceiling (bytes) for a single terminal's DFA — and for the intermediate
/// determinization. A wide-class bounded repetition like `\w{1,200}` compiles to a
/// very large DFA whose *eager* construction alone can take seconds; capping it
/// makes the build fail fast (returning `None` here) so the pair is skipped. This
/// deliberately trades a missed collision (the safe, *under*-reporting direction —
/// see [`whole_match_dfa`]) for a bounded build time, mirroring interegular's own
/// time budget. Real terminals are tiny and never approach this.
const MAX_DFA_BYTES: usize = 1 << 20;

/// Build an anchored whole-match DFA for a terminal's regex source, or `None` if
/// the automaton cannot be built (too large, or an unsupported feature). A failed
/// build means we silently skip the pair rather than over-reject a valid grammar.
fn whole_match_dfa(src: &str) -> Option<dense::DFA<Vec<u32>>> {
    dense::Builder::new()
        .configure(
            dense::Config::new()
                .start_kind(StartKind::Anchored)
                .dfa_size_limit(Some(MAX_DFA_BYTES))
                .determinize_size_limit(Some(MAX_DFA_BYTES)),
        )
        .build(src)
        .ok()
}

/// If the languages of `a` and `b` (each matched in full) share a string, return
/// the shortest such witness as raw bytes; otherwise `None`. Walks the product
/// automaton breadth-first so the witness is minimal, recording a back-pointer per
/// product state to reconstruct it.
fn intersection_witness(a: &dense::DFA<Vec<u32>>, b: &dense::DFA<Vec<u32>>) -> Option<Vec<u8>> {
    let anchored = Input::new("").anchored(Anchored::Yes);
    let start = (
        a.start_state_forward(&anchored).ok()?,
        b.start_state_forward(&anchored).ok()?,
    );

    // back-pointer: product state → (predecessor, byte that led here).
    let mut parent: HashMap<(StateID, StateID), ((StateID, StateID), u8)> = HashMap::new();
    let mut visited: HashSet<(StateID, StateID)> = HashSet::new();
    let mut queue: VecDeque<(StateID, StateID)> = VecDeque::new();
    visited.insert(start);
    queue.push_back(start);

    while let Some(cur) = queue.pop_front() {
        let (ca, cb) = cur;
        // Accepting in both at end-of-input ⇒ the path to here is a common string.
        if a.is_match_state(a.next_eoi_state(ca)) && b.is_match_state(b.next_eoi_state(cb)) {
            return Some(reconstruct_witness(&parent, start, cur));
        }
        if visited.len() > MAX_PRODUCT_STATES {
            return None;
        }
        for byte in 0u8..=255 {
            let na = a.next_state(ca, byte);
            if a.is_dead_state(na) || a.is_quit_state(na) {
                continue;
            }
            let nb = b.next_state(cb, byte);
            if b.is_dead_state(nb) || b.is_quit_state(nb) {
                continue;
            }
            let next = (na, nb);
            if visited.insert(next) {
                parent.insert(next, (cur, byte));
                queue.push_back(next);
            }
        }
    }
    None
}

/// Walk the back-pointers from `target` to `start`, collecting the bytes in order.
fn reconstruct_witness(
    parent: &HashMap<(StateID, StateID), ((StateID, StateID), u8)>,
    start: (StateID, StateID),
    target: (StateID, StateID),
) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut cur = target;
    while cur != start {
        let (prev, byte) = parent[&cur];
        bytes.push(byte);
        cur = prev;
    }
    bytes.reverse();
    bytes
}

/// Strict-mode collision check (Python Lark's `_check_regex_collisions`). Groups
/// the configured *regex* terminals by priority and, within each group, reports
/// the first same-priority pair whose languages overlap. A no-op unless `strict`.
///
/// `states` selects the scoping, mirroring how Python attaches the check to a
/// lexer:
///
///   * `None` — the **basic** lexer: every terminal is compiled into one scanner,
///     so all terminals are compared together (one global group set).
///   * `Some(state→ids)` — the **contextual** lexer: Python builds one
///     `BasicLexer` per parser state and checks each over *that state's* terminals
///     (sharing a comparator so a pair is reported once). Two overlapping
///     terminals that never share a state are therefore *not* a collision —
///     checking globally here would over-reject grammars Python accepts. The
///     `%ignore` terminals are always-accepted, so they join every state's set.
///
/// String terminals are excluded — exactly as Python only feeds `pattern.type ==
/// "re"` terminals to interegular; string/keyword overlaps are handled by the
/// `unless` retyping, not flagged as collisions.
pub fn check_regex_collisions(
    conf: &LexerConf,
    strict: bool,
    states: Option<&HashMap<usize, Vec<SymbolId>>>,
) -> Result<(), GrammarError> {
    if !strict {
        return Ok(());
    }
    let prefix = global_flag_prefix(conf.global_flags);
    let by_id: HashMap<SymbolId, &TerminalDef> =
        conf.terminals.iter().map(|(id, t)| (*id, t)).collect();

    // The terminal-id sets to check *together*: one global set for the basic lexer,
    // or each parser state's accept set (plus the always-accepted `%ignore`s) for
    // the contextual lexer.
    let id_sets: Vec<Vec<SymbolId>> = match states {
        None => vec![conf.terminals.iter().map(|(id, _)| *id).collect()],
        Some(map) => map
            .values()
            .map(|ids| {
                ids.iter()
                    .copied()
                    .chain(conf.ignore.iter().copied())
                    .collect()
            })
            .collect(),
    };

    // DFA cache and an already-reported set, both shared across the (possibly many)
    // state sets — Python shares one comparator with `skip_marked` for the same
    // reason: never compile or compare a pair twice.
    let mut dfa_cache: HashMap<String, Option<dense::DFA<Vec<u32>>>> = HashMap::new();
    let mut checked: HashSet<(SymbolId, SymbolId)> = HashSet::new();
    let dfa_for = |src: &str, cache: &mut HashMap<String, Option<dense::DFA<Vec<u32>>>>| {
        cache
            .entry(src.to_string())
            .or_insert_with(|| whole_match_dfa(src))
            .clone()
    };

    for ids in id_sets {
        // Regex terminals only (Python feeds interegular just `pattern.type ==
        // "re"`), deduplicated, sorted by name so the reported pair is
        // deterministic. `string_type` flags the terminals Python would represent
        // as a `PatternStr`; lark-rs compiles those to a regex too, but they must
        // be excluded here or a keyword like `IF: "if"` would be wrongly reported
        // as colliding with `/[a-z]+/`.
        let mut seen = HashSet::new();
        let mut res: Vec<(SymbolId, &TerminalDef)> = ids
            .into_iter()
            .filter_map(|id| by_id.get(&id).map(|t| (id, *t)))
            .filter(|(id, t)| {
                matches!(t.pattern, Pattern::Re(_)) && !t.string_type && seen.insert(*id)
            })
            .collect();
        res.sort_by(|(_, x), (_, y)| x.name.cmp(&y.name));

        // Group by priority; compare only within a group (Lark's `classify`).
        let mut by_priority: HashMap<i32, Vec<(SymbolId, &TerminalDef)>> = HashMap::new();
        for (id, t) in res {
            by_priority.entry(t.priority).or_default().push((id, t));
        }

        let mut priorities: Vec<i32> = by_priority.keys().copied().collect();
        priorities.sort_unstable();
        for p in priorities {
            let group = &by_priority[&p];
            for i in 0..group.len() {
                let (a_id, a) = group[i];
                let a_src = format!("{}{}", prefix, a.pattern.to_inline_regex());
                for &(b_id, b) in group.iter().skip(i + 1) {
                    // Normalise the pair key so a collision found in one state is
                    // not re-reported in another.
                    let key = if a_id.0 <= b_id.0 {
                        (a_id, b_id)
                    } else {
                        (b_id, a_id)
                    };
                    if !checked.insert(key) {
                        continue;
                    }
                    let Some(da) = dfa_for(&a_src, &mut dfa_cache) else {
                        continue;
                    };
                    let b_src = format!("{}{}", prefix, b.pattern.to_inline_regex());
                    let Some(db) = dfa_for(&b_src, &mut dfa_cache) else {
                        continue;
                    };
                    if let Some(bytes) = intersection_witness(&da, &db) {
                        let example = String::from_utf8_lossy(&bytes);
                        return Err(GrammarError::Other {
                            msg: format!(
                                "Collision between Terminals {} and {}.\n\
                                 Both match the string {:?}",
                                a.name, b.name, example
                            ),
                        });
                    }
                }
            }
        }
    }
    Ok(())
}

/// Reject zero-width terminals — those whose pattern can match the empty string —
/// exactly as Python Lark's `BasicLexer` sanitization does (`pattern.min_width ==
/// 0`). A nullable terminal would let the scanner make no progress, so Lark
/// forbids it at construction time, in *every* mode (not just `strict`) and before
/// the collision check. Runs on the basic/contextual lexer paths (the dynamic
/// Earley lexer has its own scanning model and does not apply this guard).
pub fn check_zero_width_terminals(conf: &LexerConf) -> Result<(), GrammarError> {
    let prefix = global_flag_prefix(conf.global_flags);
    for (_, t) in &conf.terminals {
        let src = format!("{}{}", prefix, t.pattern.to_inline_regex());
        // The pattern was already validated when the `TerminalDef` was built, so a
        // compile error here is not expected; treat it as "not zero-width" rather
        // than masking the real diagnostic.
        if let Ok(re) = Regex::new(&src) {
            if re.is_match("") {
                return Err(GrammarError::Other {
                    msg: format!(
                        "Lexer does not allow zero-width terminals. ({}: {})",
                        t.name,
                        t.pattern.to_inline_regex()
                    ),
                });
            }
        }
    }
    Ok(())
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
            .then_with(|| {
                b.pattern
                    .as_regex_str()
                    .len()
                    .cmp(&a.pattern.as_regex_str().len())
            })
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a_id.cmp(b_id))
    });
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
                        if ch == '\n' {
                            line += 1;
                            col = 1;
                        } else {
                            col += 1;
                        }
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
                        ch,
                        line,
                        col,
                        pos,
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
    state_scanners: HashMap<usize, ScannerBackend>,
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
            state_scanners.insert(
                *state_id,
                ScannerBackend::build(&terms, conf.global_flags, conf.backend)?,
            );
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
        let scanner = match self
            .state_scanners
            .get(&state)
            .or_else(|| self.state_scanners.get(&0))
        {
            Some(s) => s,
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

// ─── DynamicMatcher: per-terminal regexes for the Earley dynamic lexer ────────

/// A matcher for Earley's **dynamic lexer** (Phase 2, Sprint 5).
///
/// Unlike the [`Scanner`], which scans one combined alternation left-to-right and
/// hands the parser a fixed token stream, the dynamic lexer matches a *specific*
/// terminal — the one an Earley item predicts — at a given position, integrating
/// scanning into the parse loop. Each terminal therefore gets its own compiled
/// regex, anchored at the query position via [`Regex::find_at`] (a match is
/// accepted only if it begins exactly at `pos`).
///
/// There is **no `unless` keyword retyping** here: the parser context (which items
/// sit in the scan set) already decides which terminals to try, so `if`-vs-`iffy`
/// is resolved by the grammar, not by a lexer tie-break. Per-terminal flags
/// (`(?i:…)`) and `g_regex_flags` are preserved exactly as the basic lexer does.
pub struct DynamicMatcher {
    res: HashMap<SymbolId, AnyRegex>,
    ignore: Vec<SymbolId>,
    names: HashMap<SymbolId, String>,
}

impl DynamicMatcher {
    /// Build a matcher from the same [`LexerConf`] the basic lexer uses, so both
    /// engines honour identical terminal patterns and global flags. A lookaround
    /// terminal (issue #40) compiles via `fancy-regex`, exactly as in the basic
    /// scanner, so the dynamic lexer matches the same language.
    pub fn new(conf: &LexerConf) -> Result<Self, GrammarError> {
        let prefix = global_flag_prefix(conf.global_flags);
        let mut res = HashMap::new();
        for (id, term) in &conf.terminals {
            let src = format!("{}{}", prefix, term.pattern.to_inline_regex());
            res.insert(*id, AnyRegex::compile(&src)?);
        }
        Ok(DynamicMatcher {
            res,
            ignore: conf.ignore.clone(),
            names: conf.names(),
        })
    }

    /// Match terminal `id` starting exactly at byte `pos` in `text`. Returns the
    /// matched slice, or `None` if the terminal does not match there (or matches
    /// empty — a nullable terminal can never advance the scan).
    pub fn match_at<'t>(&self, id: SymbolId, text: &'t str, pos: usize) -> Option<&'t str> {
        let end = self.res.get(&id)?.match_end_at(text, pos)?;
        Some(&text[pos..end])
    }

    /// Match terminal `id` against the whole sub-slice `sub` (anchored at its
    /// start). Used by `dynamic_complete` to explore shorter tokenizations, which
    /// Python Lark does by re-matching against a truncated string `s[:-j]`.
    pub fn match_in<'t>(&self, id: SymbolId, sub: &'t str) -> Option<&'t str> {
        let end = self.res.get(&id)?.match_end_in(sub)?;
        Some(&sub[..end])
    }

    /// The `%ignore` terminal ids, tried between tokens by the dynamic scanner.
    pub fn ignore(&self) -> &[SymbolId] {
        &self.ignore
    }

    /// Display name of a terminal id (for the token's `type_`).
    pub fn name(&self, id: SymbolId) -> &str {
        self.names.get(&id).map(String::as_str).unwrap_or("")
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

// ─── DfaScanner ≡ Scanner: focused parity unit tests (L1) ─────────────────────
//
// The L0 differential oracle (tests/test_scanner_differential.rs) is the broad
// contract. These pin the load-bearing edge cases directly, in-crate, so a
// regression localizes to `match_at` without a corpus run — chiefly the
// multi-pattern leftmost-first **tie-break** the plan flags as the one real risk.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grammar::terminal::{PatternRe, PatternStr};

    fn re_term(id: u32, name: &str, pat: &str, prio: i32) -> (SymbolId, TerminalDef) {
        let p = Pattern::Re(PatternRe::new(pat, 0).unwrap());
        (SymbolId(id), TerminalDef::new(name, p, prio))
    }
    fn str_term(id: u32, name: &str, val: &str, prio: i32) -> (SymbolId, TerminalDef) {
        let p = Pattern::Str(PatternStr::new(val));
        (
            SymbolId(id),
            TerminalDef::new(name, p, prio).with_string_type(true),
        )
    }

    fn both(terms: &[(SymbolId, TerminalDef)]) -> (Scanner, DfaScanner) {
        let refs: Vec<(SymbolId, &TerminalDef)> = terms.iter().map(|(i, t)| (*i, t)).collect();
        (
            Scanner::build(&refs, 0).unwrap(),
            DfaScanner::build(&refs, 0).unwrap(),
        )
    }

    /// Assert the two engines pick the byte-identical `(id, value)` at **every**
    /// position of each input — the L1 contract, in miniature.
    fn assert_agree(terms: &[(SymbolId, TerminalDef)], inputs: &[&str]) {
        let (s, d) = both(terms);
        for inp in inputs {
            for pos in 0..=inp.len() {
                assert_eq!(
                    s.match_at(inp, pos),
                    d.match_at(inp, pos),
                    "engines diverged on {inp:?} at pos {pos}"
                );
            }
        }
    }

    #[test]
    fn dfa_tiebreak_same_start_picks_lowest_rank_not_longest() {
        // Two regex terminals matching at the same start with different lengths.
        // `sort_terminals` orders by (priority, max_width, pattern-len, name); both
        // are unbounded-width regexes of the same priority, so the longer *source*
        // (`abc`) ranks first. Leftmost-first then takes that branch's own greedy
        // length — and crucially, where only the shorter (`ab`) ranks first, it must
        // win with length 2 even though `abc` would match longer. Both engines agree.
        assert_agree(
            &[re_term(1, "AB", "ab", 0), re_term(2, "ABC", "abc", 0)],
            &["abc", "ab", "abz", "a", "abcd", "x", ""],
        );
        // The decisive direction: make the *shorter* pattern rank first by source
        // length (`a.` is 2 chars, `abc` is 3 → `abc` first; use `ab?` vs `abcd`).
        let (_, d) = both(&[re_term(1, "SHORT", "ab", 5), re_term(2, "LONG", "abcd", 0)]);
        // SHORT has higher priority, so it ranks first and wins at "abcd" with len 2,
        // NOT the longest match (len 4). This is the Python-re leftmost-first tie-break.
        assert_eq!(d.match_at("abcd", 0), Some((SymbolId(1), "ab")));
    }

    #[test]
    fn dfa_keyword_unless_retype_matches_regex_scanner() {
        let terms = [str_term(1, "IF", "if", 0), re_term(2, "NAME", "[a-z]+", 0)];
        assert_agree(&terms, &["if", "iffy", "if x", "i", "z", "if2"]);
        // Pin the engine-independent outcome too: the keyword retypes to IF (id 1),
        // a longer identifier stays NAME (id 2).
        let (_, d) = both(&terms);
        assert_eq!(d.match_at("if", 0), Some((SymbolId(1), "if")));
        assert_eq!(d.match_at("iffy", 0), Some((SymbolId(2), "iffy")));
    }

    #[test]
    fn dfa_priority_and_width_ordering_matches_regex_scanner() {
        // OCT (priority 2) must beat INT at "0o777"; agreement across the boundary
        // and over a punctuation terminal that shares no start byte.
        assert_agree(
            &[
                re_term(1, "OCT", "0[oO][0-7]+", 2),
                re_term(2, "INT", "[0-9]+", 0),
                str_term(3, "PLUS", "+", 0),
            ],
            &["0o777", "0777", "123", "0", "+", "0o", "12+34", "0o+1"],
        );
    }

    #[test]
    fn dfa_start_byte_prefilter_never_hides_a_match() {
        // Scan every position of a mixed string: the start-byte prefilter must skip
        // the engine only where no terminal could match, never where one does.
        assert_agree(
            &[
                re_term(1, "WORD", "[a-z]+", 0),
                re_term(2, "NUM", "[0-9]+", 0),
            ],
            &["abc123 def", "   x", "9z9z", "...."],
        );
    }

    #[test]
    fn dfa_all_lookaround_terminals_has_no_plain_engine() {
        // A scanner whose only terminal is a lookaround pattern builds with `re ==
        // None` (no plain engine) and still matches via the fancy side-probe,
        // identically to `Scanner`.
        let terms = [re_term(1, "STR", "\"(?!\")[^\"]*\"", 0)];
        let (s, d) = both(&terms);
        assert!(d.re.is_none(), "all-lookaround scanner has no plain engine");
        for inp in ["\"ab\"", "\"\"", "\"\"\"\"", "x"] {
            assert_eq!(
                s.match_at(inp, 0),
                d.match_at(inp, 0),
                "diverged on {inp:?}"
            );
        }
    }
}
