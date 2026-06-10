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
    dfa::{dense, Automaton, OverlappingState, StartKind},
    hybrid::dfa::DFA as LazyDfa,
    nfa::thompson,
    util::primitives::StateID,
    Anchored, Input, MatchKind,
};

use crate::error::{GrammarError, ParseError};
use crate::grammar::intern::SymbolId;
use crate::grammar::terminal::{Pattern, TerminalDef};
use crate::tree::Token;

// ─── SideProbe: a per-terminal matcher for a regex-crate-rejected terminal ────
//
// The combined scanner is built on the linear-time `regex` crate, which has no
// lookahead/lookbehind. A terminal the `regex` crate rejects either LOWERS into its
// own single-terminal DFA ([`LoweredTerminalMatcher`], the default build) or — under
// the TEST-ONLY `fancy-oracle` feature — is matched by the historical `\G`-anchored
// `fancy-regex` probe, so the `Regex` reference backend stays an independent oracle
// for the whole-lexer differential. There is no runtime fallback engine, and the
// feature never widens the accepted grammar set: BOTH builds route every
// regex-rejected terminal through THE refusal seam (`route_fancy_only_terminal`)
// first, so a terminal the lowering refuses fails the grammar build with the same
// categorized scope error (`docs/LOOKAROUND_SCOPE.md`) with and without the feature.
// The feature only swaps the *matcher* for terminals that lower.

enum SideProbe {
    /// The lowered single-terminal DFA — the default build (and the engine whose
    /// semantics the combined `DfaScanner` shares by construction). Under
    /// `fancy-oracle` every lowered terminal gets a [`Self::Fancy`] probe instead,
    /// so this variant is never constructed there — it exists so the
    /// single-variant default build is the same type.
    #[cfg_attr(feature = "fancy-oracle", allow(dead_code))]
    Lowered(LoweredTerminalMatcher),
    /// TEST-ONLY historical reference: the per-position `\G`-anchored fancy probe.
    /// (`\G` makes `find_from_pos` fail immediately when nothing matches at `pos`
    /// instead of forward-scanning — the linearity fix; see the module docs.)
    #[cfg(feature = "fancy-oracle")]
    Fancy(fancy_regex::Regex),
}

impl SideProbe {
    /// End offset of a non-empty match beginning *exactly* at `pos`, or `None`.
    /// The full `text` (not a suffix) is passed so a lookbehind can see the bytes
    /// before `pos`.
    fn match_end_at(&self, text: &str, pos: usize) -> Option<usize> {
        match self {
            SideProbe::Lowered(m) => m.match_end_at(text, pos),
            #[cfg(feature = "fancy-oracle")]
            SideProbe::Fancy(re) => {
                let m = re.find_from_pos(text, pos).ok().flatten();
                record_scan_skip(pos, m.as_ref().map(|m| m.start()));
                let m = m?;
                (m.start() == pos && m.end() > pos).then_some(m.end())
            }
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
///     alternation with capture groups (see [`Scanner`]).
///   * [`Dfa`](LexerBackend::Dfa) — a `regex-automata` multi-pattern DFA
///     (`docs/LEXER_DFA_PLAN.md`, phase L1; see [`DfaScanner`]). This is now the
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
    /// the `regex-automata` [`DfaScanner`]; the original `regex` Scanner is opt-in.
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
    /// `regex-automata` [`DfaScanner`]; choosing [`LexerBackend::Regex`] swaps back
    /// to the original `regex`-crate [`Scanner`] without changing any lexing
    /// semantics. Both refuse the same patterns with the same categorized scope
    /// errors (`docs/LOOKAROUND_SCOPE.md`); a lowered lookaround terminal rides the
    /// shared DFA branches there and a per-terminal [`SideProbe`] here (the
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
    /// Lookaround terminals, each matched individually by its [`SideProbe`]
    /// (lowered by default; the historical fancy probe under the TEST-ONLY
    /// `fancy-oracle` feature). Stored in ascending `rank` order, so the first one
    /// that matches is the lowest-rank side candidate. Empty for the overwhelming
    /// common case (no lookaround).
    side: Vec<(usize, SymbolId, SideProbe)>,
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
        // terminals (the `regex` crate rejects them), which are matched individually
        // via a [`SideProbe`]. `rank` is the index in the plan's sorted order: Python
        // builds the alternation in this order and takes the first branch that
        // matches, so the lowest rank wins ties. We preserve it to merge the two
        // engines' candidates at match time.
        let by_id: HashMap<SymbolId, &TerminalDef> =
            terminals.iter().map(|(id, t)| (*id, *t)).collect();
        let mut parts = Vec::new();
        let mut group_names = Vec::new();
        let mut side: Vec<(usize, SymbolId, SideProbe)> = Vec::new();
        for (rank, (id, inline)) in plan.groups.iter().enumerate() {
            // `to_inline_regex` (used by `scanner_plan`) keeps per-terminal flags
            // (e.g. `(?i:…)` for a case-insensitive terminal) scoped to this group.
            match Regex::new(&format!("{prefix}{inline}")) {
                Ok(_) => {
                    let group = format!("g{}", id.0);
                    parts.push(format!("(?P<{group}>{inline})"));
                    group_names.push((*id, group, rank));
                }
                #[cfg(feature = "fancy-oracle")]
                Err(e) => {
                    // Acceptance is decided by THE refusal seam FIRST, exactly as the
                    // default build below decides it — so the TEST-ONLY feature can
                    // never change what a grammar build accepts (the Cargo.toml
                    // contract). Only a terminal that *lowers* proceeds to the probe.
                    route_fancy_only_terminal(by_id[id], global_flags, &e.to_string())?;
                    // The historical reference matcher for the lowered terminal: the
                    // `\G`-anchored fancy probe (`\G` anchors `find_from_pos` to
                    // `pos` so a sparse terminal stays linear; the `regex` crate
                    // cannot parse `\G`, so this pattern lives on the fancy engine
                    // only). This is what makes the feature build an independent
                    // oracle for the differential.
                    let src = format!("{prefix}\\G{inline}");
                    let anchored =
                        fancy_regex::Regex::new(&src).map_err(|e| GrammarError::InvalidRegex {
                            pattern: src.clone(),
                            reason: e.to_string(),
                        })?;
                    side.push((rank, *id, SideProbe::Fancy(anchored)));
                }
                #[cfg(not(feature = "fancy-oracle"))]
                Err(e) => {
                    // Default build: lower the terminal through THE refusal seam —
                    // its own single-terminal DFA, or the categorized scope error
                    // (identical to the `DfaScanner` backend's policy).
                    let m = LoweredTerminalMatcher::build(
                        *id,
                        by_id[id],
                        global_flags,
                        &e.to_string(),
                    )?;
                    side.push((rank, *id, SideProbe::Lowered(m)));
                }
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
            side,
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
    /// *side-probe* terminal from the (rank-sorted) lookaround list, then keep
    /// whichever has the smaller rank.
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
        // Lowest-rank side candidate (the list is rank-sorted, so the first match
        // wins); keep it only if it out-ranks the plain candidate.
        for (rank, id, re) in &self.side {
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

/// The combined scanner (`docs/LEXER_DFA_PLAN.md`). Same contract and selection
/// rules as [`Scanner`] — leftmost-first ranking, `unless` retyping — but the
/// *plain* (lookaround-free) terminals are matched by one
/// hand-built `regex-automata` **dense DFA** over all of them, returning a
/// `PatternID`, instead of the `regex`-crate alternation-with-capture-groups trick.
///
/// **M0 re-platform (`docs/LEXER_DFA_PLAN.md`, "L2 re-platforms the engine").** L1's
/// `DfaScanner` was a `meta::Regex::new_many`, whose only input is *pattern strings*
/// and which categorically cannot host a lowered `(?!…)` fragment or expose the
/// per-state accept-set the guarded-accept driver needs. So the engine is rebuilt on
/// the lower layer: each plain terminal is compiled to a Thompson NFA, the terminals
/// are **unioned into one multi-pattern NFA** (`build_many`, `PatternID == rank`),
/// and that NFA is determinized to a `dense::DFA` we drive ourselves through the
/// [`Automaton`] trait. This is the seam M1+ extend: hand-assembled lowered fragments
/// join the same NFA, and the dense DFA exposes `match_pattern`/`match_len` for the
/// accept-set accumulator.
///
/// Why this is byte-identical to [`Scanner`]:
///
///   * The plain patterns are unioned **in rank order**, so `PatternID` *is* the rank.
///     Built with `MatchKind::LeftmostFirst`, the dense DFA resolves a same-start tie
///     by **pattern order** (lowest `PatternID` wins) with that pattern's own greedy
///     length — exactly Python-`re`'s leftmost-first, the same semantics the
///     `regex`-crate alternation gives (verified: `[ab|abc]` at `"abc"` picks pattern
///     0, length 2, *not* the longest match).
///   * The search is **anchored** at `pos` (`Anchored::Yes` over `pos..len`), so it
///     can only begin exactly at `pos` and never forward-scans. A zero-width match is
///     rejected, mirroring `Scanner`.
///   * The `unless` retype is **copied verbatim** from `Scanner`: only the
///     plain-terminal engine changes.
///
/// **There is no fallback engine (L4).** Every terminal either compiles plain, lowers
/// into the DFA (M1/M2/M3/M4 + the Stage-B idioms — all bundled lookaround terminals),
/// or **fails the build** with the categorized scope error
/// (`route_fancy_only_terminal`, `docs/LOOKAROUND_SCOPE.md`). The historical
/// `fancy-regex` side-probe and its `push_fancy_fallback` compatibility seam are gone;
/// the scanner is fully self-contained `regex-automata` data — the L5 bake target.
///
/// The `regex` crate's combined alternation came with a free literal prefilter; an
/// *anchored* search runs no prefilter of its own, so we re-add an explicit
/// **start-byte prefilter** (`start_bytes`): the set of bytes any plain terminal can
/// begin with, **re-derived from the new union** (a lazy DFA over the plain union).
/// When the byte at `pos` isn't in it we skip the plain engine entirely. It is an
/// over-approximation by construction (a possible start byte is never dropped), so it
/// can only ever *save* an engine call, never change a match — the L0 differential
/// oracle is the proof.
struct DfaScanner {
    /// Leftmost-first DFA over the **unguarded** sub-patterns (plain terminals and the
    /// unguarded branches of boundary terminals). `None` when there are none. This is
    /// the M0 engine: it reproduces Python-`re` leftmost-first *exactly*, including a
    /// terminal's own order-sensitive internal alternation (`/ab|abc/` → `"ab"`), so a
    /// sibling guard never disturbs a plain terminal.
    plain: Option<PlainEngine>,
    /// All-matches DFA over the **guarded** sub-patterns (branches carrying a leading
    /// and/or trailing boundary guard). `None` when there are none. Driven by the
    /// guarded-accept accumulator (`docs/LEXER_DFA_PLAN.md`, "guarded accept ×
    /// multi-pattern priority").
    guarded: Option<GuardedEngine>,
    /// Start-byte prefilter over the base union of both engines (see the struct docs).
    /// `None` disables it (always run the engines).
    start_bytes: Option<Box<[bool; 256]>>,
    /// regex-terminal-id → (matched-text → keyword-terminal-id) — identical retype.
    unless: HashMap<SymbolId, HashMap<String, SymbolId>>,
}

/// Leftmost-first DFA over the unguarded sub-patterns. Sub-patterns are ordered by
/// `(rank, branch_order)`, so the lowest `PatternID` is the leftmost-first winner with
/// its own (order-sensitive) match length — byte-identical to M0.
struct PlainEngine {
    dfa: dense::DFA<Vec<u32>>,
    /// `PatternID` → (terminal id, rank, branch_order).
    map: Vec<(SymbolId, usize, usize)>,
}

/// All-matches DFA over the guarded sub-patterns + their guards.
struct GuardedEngine {
    dfa: dense::DFA<Vec<u32>>,
    /// Indexed by `PatternID`.
    subs: Vec<SubPattern>,
}

// (the former `PlainDfa` enum is replaced by the split `PlainEngine` / `GuardedEngine`)

/// One lowered sub-pattern fed to the combined NFA. A plain terminal contributes one
/// unguarded sub-pattern; a boundary terminal contributes one per top-level
/// alternation branch (some carrying a leading and/or trailing guard — `lark.OP`).
struct SubPattern {
    id: SymbolId,
    /// The terminal's rank in the sorted plan — cross-terminal leftmost-first.
    rank: usize,
    /// The branch's index within its terminal — within-terminal leftmost-first.
    branch_order: usize,
    /// A guard checked at the match **start** (`pos`) — a leading boundary `(?!S)X`.
    leading: Option<Guard>,
    /// A guard checked at the match **end** — a trailing boundary `X(?!S)`.
    trailing: Option<Guard>,
    /// Bounded-lookbehind guards, each checked *backward* at a fixed char-offset from
    /// the match start (M3). Empty for a branch with no lookbehind.
    lookbehind: Vec<LookbehindGuardC>,
}

/// A compiled boundary guard. The driver records an accept of the guarded
/// sub-pattern only when this holds at its position (start for leading, end for
/// trailing) — so the peeked char, which belongs to a neighbouring token, is
/// consulted but never consumed.
struct Guard {
    /// `true` for `(?!S)` (must **not** match `S`), `false` for `(?=S)`.
    neg: bool,
    /// Anchored DFA for the assertion body `S`, matched at the guard position.
    dfa: dense::DFA<Vec<u32>>,
}

impl Guard {
    /// Whether the guard is satisfied at byte offset `at` in `text`. At end-of-input
    /// (`at == text.len()`) `S` cannot match (no chars follow), so a negative guard
    /// `(?!S)` holds and a positive guard `(?=S)` fails — exactly Python's
    /// trailing-assertion-at-EOF semantics.
    fn holds(&self, text: &str, at: usize) -> bool {
        let input = Input::new(text)
            .span(at..text.len())
            .anchored(Anchored::Yes);
        let s_matches = matches!(self.dfa.try_search_fwd(&input), Ok(Some(_)));
        if self.neg {
            !s_matches
        } else {
            s_matches
        }
    }
}

/// A compiled **bounded-lookbehind** guard (`docs/LEXER_DFA_PLAN.md`, M3). The driver
/// records an accept of the sub-pattern only when the ≤`width` chars *ending* at the
/// lookbehind point — byte offset `pos` advanced by `offset_chars` characters —
/// do/don't match `S`. The offset is fixed (the lowering declines a variable-offset
/// lookbehind), so this is a uniform precondition evaluated once, like a leading guard
/// but read *backward* from a fixed point inside (or at the start of) the match.
struct LookbehindGuardC {
    /// `true` for `(?<!S)` (the window must **not** match `S`), `false` for `(?<=S)`.
    neg: bool,
    /// All-matches anchored DFA for the body `S`, so a window is tested for an *exact*
    /// (full-slice) match regardless of `S`'s internal alternation order.
    dfa: dense::DFA<Vec<u32>>,
    /// Char offset from the match start to the lookbehind point.
    offset_chars: usize,
    /// Maximum char width of `S` — the driver tries window lengths `1..=width`.
    width: usize,
}

impl LookbehindGuardC {
    /// Whether the lookbehind holds for a match starting at byte `pos` in `text`.
    fn holds(&self, text: &str, pos: usize) -> bool {
        // Walk `offset_chars` characters forward from `pos` to the lookbehind point.
        let mut point = pos;
        for _ in 0..self.offset_chars {
            match text[point..].chars().next() {
                Some(c) => point += c.len_utf8(),
                // Not enough chars consumed before the lookbehind point: the base can't
                // match here anyway, so the value is moot. Treat the window as absent.
                None => return self.neg,
            }
        }
        // Does `S` match some suffix (1..=width chars) ending exactly at `point`?
        let mut start = point;
        let mut matched = false;
        for _ in 0..self.width {
            match text[..start].chars().next_back() {
                Some(c) => start -= c.len_utf8(),
                None => break, // ran off the front of the haystack — window absent
            }
            if self.window_full_match(&text[start..point]) {
                matched = true;
                break;
            }
        }
        if self.neg {
            !matched
        } else {
            matched
        }
    }

    /// Whether `S` matches the whole `slice` (anchored at both ends). Uses the
    /// all-matches DFA's overlapping search and checks for an accept whose end is the
    /// slice end — so an order-sensitive `S` (`a|ab`) that leftmost-first would stop
    /// short still registers its full-slice match.
    fn window_full_match(&self, slice: &str) -> bool {
        let input = Input::new(slice).anchored(Anchored::Yes);
        let mut state = OverlappingState::start();
        loop {
            if self
                .dfa
                .try_search_overlapping_fwd(&input, &mut state)
                .is_err()
            {
                return false;
            }
            match state.get_match() {
                Some(hm) if hm.offset() == slice.len() => return true,
                Some(_) => continue,
                None => return false,
            }
        }
    }
}

/// Wrap `src` in a flag-scoped group `(?flags:src)` for a terminal's own regex flags,
/// or return it unchanged when the terminal has none. Mirrors
/// [`Pattern::to_inline_regex`](crate::grammar::terminal::Pattern::to_inline_regex)
/// so a lowered branch's flags scope exactly as the un-split terminal's did.
fn wrap_flags(flags: u32, src: &str) -> String {
    let letters = crate::grammar::terminal::flag_letters(flags);
    if letters.is_empty() {
        src.to_string()
    } else {
        format!("(?{letters}:{src})")
    }
}

/// Strip a **whole-pattern flag wrapper** `(?ims:…)` back into the flag bitset —
/// the inverse of what the grammar loader bakes in. The loader converts a terminal's
/// `/…/is`-style flags into one flag-scoped group around the entire pattern
/// (`Pattern::to_inline_regex`) and stores `PatternRe.flags = 0`, so without this
/// step the lowering router would see every assertion nested inside a `Group` (an
/// instant decline/reject) and the bundled `python.STRING` / `python.LONG_STRING`
/// idioms would silently ride the fancy fallback — with their flags lost if a
/// recognizer peeled the group instead (the dotall mis-lowering the
/// `g_regex_flags_dotall_long_string` / `newline_dotall_body` seam fixtures pin).
///
/// Returns the inner pattern + the merged flags. Conservative: on anything but a
/// single unquantified positive-`ims` flag group spanning the whole pattern (an `x`
/// VERBOSE wrapper — see the inline note, a `-` clear, an unknown letter, a
/// quantifier, a bare `(?:`, a parse failure) the input is returned unchanged, so
/// the route behaves exactly as before. Loops so a nested `(?i:(?s:…))` (not
/// produced by the loader, but cheap to honor) fully unwraps.
fn strip_whole_pattern_flag_wrapper(raw: &str, flags: u32) -> (String, u32) {
    use crate::grammar::terminal::flags as f;
    let mut pattern = raw.to_string();
    let mut flags = flags;
    loop {
        let Ok(crate::lookaround::Node::Group { open, body, quant }) =
            crate::lookaround::parse(&pattern)
        else {
            return (pattern, flags);
        };
        if !quant.is_empty() {
            return (pattern, flags);
        }
        let Some(letters) = open
            .strip_prefix("(?")
            .and_then(|s| s.strip_suffix(':'))
            .filter(|s| !s.is_empty())
        else {
            return (pattern, flags); // a capturing `(` or bare `(?:` — not a flag wrapper
        };
        let mut add = 0u32;
        for c in letters.chars() {
            add |= match c {
                'i' => f::IGNORECASE,
                'm' => f::MULTILINE,
                's' => f::DOTALL,
                // `x` (VERBOSE) is deliberately NOT stripped: the lookaround
                // parser and its width/offset analysis are not verbose-aware, so a
                // stripped `(?x:…)` body would have its whitespace/comments counted
                // as literal width while the re-wrapped branch ignores them — a
                // fixed-offset lookbehind could lower with a wrong offset (a
                // false-accept). Left wrapped, the pattern is refused with the
                // honest categorized NYI error (`DeclineReason::VerboseMode`,
                // via `is_verbose_wrapped_lookaround`) — the reject-when-unsure
                // direction. Pinned by
                // `verbose_flag_wrapper_is_not_stripped_into_lowering`.
                //
                // A flag-clear (`-`) or any unknown letter likewise leaves the
                // pattern alone. Named groups (`(?P<n>…`, `(?<n>…`) never get here —
                // their opens end with `>`, not `:`.
                _ => return (pattern, flags),
            };
        }
        flags |= add;
        pattern = body.to_source();
    }
}

/// True when `raw` is a whole-pattern flag wrapper whose letters include `x` (VERBOSE)
/// **and** whose body contains a lookaround assertion — the shape
/// [`strip_whole_pattern_flag_wrapper`] deliberately refuses to strip (the lookaround
/// analyzer's width/offset arithmetic is not verbose-aware). Detected *before*
/// classification so the refusal surfaces as the honest
/// [`DeclineReason::VerboseMode`] (NotYetImplemented) instead of the classifier
/// mislabeling the group-nested assertion as out-of-scope internal lookahead. An
/// `x`-wrapped pattern with **no** assertion never reaches the routing seam (the
/// `regex` crate supports verbose mode and compiles it plain), and one that is
/// regex-rejected for a non-lookaround reason (e.g. a backref) falls through to the
/// `BacktrackingOnlySyntax` triage.
fn is_verbose_wrapped_lookaround(raw: &str) -> bool {
    let Ok(crate::lookaround::Node::Group { open, body, quant }) = crate::lookaround::parse(raw)
    else {
        return false;
    };
    let Some(letters) = open.strip_prefix("(?").and_then(|s| s.strip_suffix(':')) else {
        return false;
    };
    quant.is_empty()
        && !letters.is_empty()
        && letters.chars().all(|c| matches!(c, 'i' | 'm' | 's' | 'x'))
        && letters.contains('x')
        && body.has_assertion()
}

/// True when the lookaround frontend parses `raw` and finds any assertion in it.
/// Used by the routing seam's verbose-mode gate: under VERBOSE the analyzer's
/// width/offset arithmetic would be wrong, and the only route that could *lower*
/// such a pattern is one with assertions — so refusing exactly these closes the
/// false-accept class. A pattern the frontend cannot parse returns `false` and is
/// still refused downstream (`DeclineReason::FrontendParse` / `LoweringRoute::Plain`'s
/// `BacktrackingOnlySyntax` triage), never lowered.
fn pattern_contains_assertion(raw: &str) -> bool {
    crate::lookaround::parse(raw).is_ok_and(|n| n.has_assertion())
}

/// **THE single refusal seam** (L4): route one `regex`-crate-rejected terminal through
/// the typed lowering and either return its lowered branches (+ the merged flag bitset
/// to re-wrap them with), or the **categorized scope build error**
/// (`GrammarError::LookaroundScope`, `docs/LOOKAROUND_SCOPE.md`). The successor of the
/// historical `push_fancy_fallback` compatibility seam: every refusal — a per-instance
/// decline, an out-of-shape rejection, or backtracking-only syntax — funnels through
/// exactly this function, on every engine (`DfaScanner`, the `Scanner` reference
/// backend's default build, the Earley `DynamicMatcher`, and `compute_unless`), so the
/// categorized error is produced in one auditable place.
///
/// `compile_err` is the `regex` crate's rejection message for the full source — quoted
/// in the backtracking-only triage so the user sees the engine's own reason.
fn route_fancy_only_terminal(
    def: &TerminalDef,
    global_flags: u32,
    compile_err: &str,
) -> Result<(Vec<crate::lookaround::lower::LoweredBranch>, u32), GrammarError> {
    use crate::lookaround::classify::{
        route_terminal_dotall, scope_message, DeclineReason, LookaroundIssue, LoweringRoute,
    };
    let (raw, flags) = match &def.pattern {
        Pattern::Re(p) => (p.pattern.as_str(), p.flags),
        // A string literal compiles as an escaped plain pattern and never reaches this
        // seam; error defensively rather than panicking.
        Pattern::Str(_) => {
            return Err(GrammarError::InvalidRegex {
                pattern: def.name.clone(),
                reason: format!("string terminal failed to compile: {compile_err}"),
            });
        }
    };
    // The loader bakes terminal-level `/…/is` flags into the pattern as one
    // whole-pattern wrapper (`(?is:…)`, `PatternRe.flags = 0`); strip it back into the
    // flag bitset so the lowering sees the assertions at top level. The caller re-wraps
    // every lowered branch/guard with the returned `flags`.
    let (raw, flags) = strip_whole_pattern_flag_wrapper(raw, flags);
    let raw = raw.as_str();
    // VERBOSE mode makes the lookaround analyzer's arithmetic wrong (whitespace and
    // comments are counted as literal width while the compiled branch ignores them —
    // the false-accept class), in either of its two spellings:
    //   * a whole-pattern `(?x:…)` wrapper — the strip refused it (VERBOSE bodies
    //     are not analyzable), caught before the classifier can mislabel the
    //     group-nested assertion as internal lookahead;
    //   * the global `g_regex_flags` VERBOSE bit (or a terminal-level `x` flag bit) —
    //     the raw pattern looks plain to the analyzer but compiles under `(?x)`,
    //     so any pattern containing an assertion must be refused before routing.
    // Both refuse with the honest categorized NYI reason.
    let verbose_mode = ((flags | global_flags) & crate::grammar::terminal::flags::VERBOSE) != 0;
    if is_verbose_wrapped_lookaround(raw) || (verbose_mode && pattern_contains_assertion(raw)) {
        let reason = DeclineReason::VerboseMode;
        return Err(GrammarError::LookaroundScope {
            terminal: def.name.clone(),
            subject: raw.to_string(),
            scope: reason.scope(),
            issue: LookaroundIssue::Declined(reason),
            msg: scope_message(
                &def.name,
                raw,
                LookaroundIssue::Declined(reason),
                reason.explain(),
            ),
        });
    }
    // `dotall` must reflect the terminal's own flags *or* the global `(?s…)` prefix —
    // both end up wrapped around every lowered branch source.
    let dotall = ((flags | global_flags) & crate::grammar::terminal::flags::DOTALL) != 0;
    match route_terminal_dotall(&def.name, raw, dotall) {
        LoweringRoute::Lowered(branches) => Ok((branches, flags)),
        LoweringRoute::Declined { reason, message } => Err(GrammarError::LookaroundScope {
            terminal: def.name.clone(),
            subject: raw.to_string(),
            scope: reason.scope(),
            issue: LookaroundIssue::Declined(reason),
            msg: message,
        }),
        LoweringRoute::Unsupported {
            assertion,
            rejection,
            message,
        } => Err(GrammarError::LookaroundScope {
            terminal: def.name.clone(),
            subject: assertion,
            scope: rejection.scope(),
            issue: LookaroundIssue::Rejected(rejection),
            msg: message,
        }),
        // No lookaround at all, yet the `regex` crate rejected the pattern:
        // backtracking-only syntax (a top-level backreference, an atomic group, a
        // possessive quantifier) — a by-design non-goal (and, for backrefs, the one
        // named parity break with Python Lark's backtracking engine).
        LoweringRoute::Plain => {
            let reason = DeclineReason::BacktrackingOnlySyntax;
            Err(GrammarError::LookaroundScope {
                terminal: def.name.clone(),
                subject: raw.to_string(),
                scope: reason.scope(),
                issue: LookaroundIssue::Declined(reason),
                msg: scope_message(
                    &def.name,
                    raw,
                    LookaroundIssue::Declined(reason),
                    &format!(
                        "{} (the regex engine said: {compile_err})",
                        reason.explain()
                    ),
                ),
            })
        }
        LoweringRoute::Invalid { message } => Err(GrammarError::Other { msg: message }),
    }
}

/// Build an anchored dense DFA for a guard body `src` (leftmost-first; we only need a
/// yes/no "does `S` match here").
fn build_anchored_dfa(src: &str) -> Result<dense::DFA<Vec<u32>>, GrammarError> {
    let dfa = dense::Builder::new()
        .configure(dense::Config::new().start_kind(StartKind::Anchored))
        .build(src)
        .map_err(|e| GrammarError::InvalidRegex {
            pattern: src.to_string(),
            reason: e.to_string(),
        })?;
    crate::perf::add_dense_build_bytes(dfa.memory_usage() as u64);
    Ok(dfa)
}

/// Build an anchored **all-matches** dense DFA for a lookbehind body `src`, so the
/// driver can test whether `S` matches a window *exactly* (every accept length is
/// surfaced via an overlapping search, not just the leftmost-first one).
fn build_anchored_all_dfa(src: &str) -> Result<dense::DFA<Vec<u32>>, GrammarError> {
    let dfa = dense::Builder::new()
        .configure(
            dense::Config::new()
                .match_kind(MatchKind::All)
                .start_kind(StartKind::Anchored),
        )
        .build(src)
        .map_err(|e| GrammarError::InvalidRegex {
            pattern: src.to_string(),
            reason: e.to_string(),
        })?;
    crate::perf::add_dense_build_bytes(dfa.memory_usage() as u64);
    Ok(dfa)
}

/// Compile `srcs` to one Thompson NFA (`build_many`, `PatternID` = index), then
/// determinize one anchored dense DFA under `match_kind`. The NFA is
/// match-kind-agnostic — `MatchKind` lives on the determinizer (leftmost-first keeps
/// the NFA's alternation priority; all surfaces every overlapping match). Captures are
/// dropped — the winning sub-pattern is read from `PatternID`.
fn build_combined_dfa(
    srcs: &[&str],
    match_kind: MatchKind,
) -> Result<dense::DFA<Vec<u32>>, GrammarError> {
    let nfa = thompson::NFA::compiler()
        .configure(thompson::Config::new().which_captures(thompson::WhichCaptures::None))
        .build_many(srcs)
        .map_err(|e| GrammarError::InvalidRegex {
            pattern: srcs.join("|"),
            reason: e.to_string(),
        })?;
    let dfa = dense::Builder::new()
        .configure(
            dense::Config::new()
                .match_kind(match_kind)
                .start_kind(StartKind::Anchored),
        )
        .build_from_nfa(&nfa)
        .map_err(|e| GrammarError::InvalidRegex {
            pattern: srcs.join("|"),
            reason: e.to_string(),
        })?;
    crate::perf::add_dense_build_bytes(dfa.memory_usage() as u64);
    Ok(dfa)
}

// (the greedy-monotone realizability check now lives in `crate::lookaround::lower`,
// so `lower_terminal` itself declines a non-greedy-monotone guarded base.)

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
        let by_id: HashMap<SymbolId, &TerminalDef> =
            terminals.iter().map(|(id, t)| (*id, *t)).collect();

        // Walk the rank-ordered plan, classifying each terminal's lowered branches
        // into **unguarded** sub-patterns (plain terminals + the unguarded branches of
        // boundary terminals) and **guarded** sub-patterns (branches with a leading
        // and/or trailing guard). The two go to two different engines:
        //   * unguarded → one leftmost-first DFA (M0 semantics, exact within-pattern
        //     order — a sibling guard never disturbs `/ab|abc/`);
        //   * guarded → one all-matches DFA driven by the guarded-accept accumulator.
        // A lookaround terminal whose guarded base is not guard-realizable (see
        // `is_guard_realizable`), or whose lookbehind sits at a variable offset outside
        // a recognized idiom, FAILS THE BUILD with the categorized scope error (L4 —
        // there is no fallback engine). M1/M2/M3 boundary+lookbehind, the M4 STRING
        // splice, and the Stage-B `lark.REGEXP` / `python.LONG_STRING` delimited-token
        // idioms all lower, so every bundled grammar builds.
        let mut plain_subs: Vec<SubPattern> = Vec::new();
        let mut plain_srcs: Vec<String> = Vec::new();
        let mut guarded_subs: Vec<SubPattern> = Vec::new();
        let mut guarded_srcs: Vec<String> = Vec::new();
        let mut base_inlines: Vec<String> = Vec::new(); // for the start-byte union

        for (rank, (id, inline)) in plan.groups.iter().enumerate() {
            let src = format!("{prefix}{inline}");
            // A pattern the `regex` crate compiles is plain; everything else must
            // lower or refuse with the categorized scope error — there is no runtime
            // fallback engine any more (L4).
            let compile_err = match Regex::new(&src) {
                Ok(_) => {
                    plain_subs.push(SubPattern {
                        id: *id,
                        rank,
                        branch_order: 0,
                        leading: None,
                        trailing: None,
                        lookbehind: Vec::new(),
                    });
                    plain_srcs.push(src);
                    base_inlines.push(inline.clone());
                    continue;
                }
                Err(e) => e.to_string(),
            };

            // A lookaround (or otherwise regex-rejected) terminal — lower it through
            // THE single refusal seam, or fail the build with the categorized scope
            // error. The seam strips the loader's whole-pattern flag wrapper and
            // returns the merged flag bitset; `wrap_flags(flags, …)` re-applies the
            // same scoping to every lowered branch and guard below.
            let def = by_id[id];
            let (lowered, flags) = route_fancy_only_terminal(def, global_flags, &compile_err)?;
            let compile_guard =
                |g: &crate::lookaround::lower::GuardSpec| -> Result<Guard, GrammarError> {
                    let gsrc = format!("{prefix}{}", wrap_flags(flags, &g.set));
                    Ok(Guard {
                        neg: g.neg,
                        dfa: build_anchored_dfa(&gsrc)?,
                    })
                };
            let compile_lookbehind = |g: &crate::lookaround::lower::LookbehindGuard|
             -> Result<LookbehindGuardC, GrammarError> {
                let gsrc = format!("{prefix}{}", wrap_flags(flags, &g.set));
                Ok(LookbehindGuardC {
                    neg: g.neg,
                    dfa: build_anchored_all_dfa(&gsrc)?,
                    offset_chars: g.offset_chars,
                    width: g.width,
                })
            };

            for (bo, br) in lowered.iter().enumerate() {
                let inline_br = wrap_flags(flags, &br.regex);
                let nfa_src = format!("{prefix}{inline_br}");
                base_inlines.push(inline_br);
                if br.leading.is_none() && br.trailing.is_none() && br.lookbehind.is_empty() {
                    // An unguarded branch (e.g. `lark.OP`'s `[+*]`) is plain — it joins
                    // the leftmost-first engine so its priority is exact.
                    plain_subs.push(SubPattern {
                        id: *id,
                        rank,
                        branch_order: bo,
                        leading: None,
                        trailing: None,
                        lookbehind: Vec::new(),
                    });
                    plain_srcs.push(nfa_src);
                } else {
                    let leading = br.leading.as_ref().map(&compile_guard).transpose()?;
                    let trailing = br.trailing.as_ref().map(&compile_guard).transpose()?;
                    let lookbehind = br
                        .lookbehind
                        .iter()
                        .map(&compile_lookbehind)
                        .collect::<Result<Vec<_>, _>>()?;
                    guarded_subs.push(SubPattern {
                        id: *id,
                        rank,
                        branch_order: bo,
                        leading,
                        trailing,
                        lookbehind,
                    });
                    guarded_srcs.push(nfa_src);
                }
            }
        }

        // The leftmost-first plain engine: order the sub-patterns by `(rank,
        // branch_order)` so the lowest `PatternID` is the leftmost-first winner.
        let plain = if plain_srcs.is_empty() {
            None
        } else {
            let mut order: Vec<usize> = (0..plain_subs.len()).collect();
            order.sort_by_key(|&i| (plain_subs[i].rank, plain_subs[i].branch_order));
            let ordered_srcs: Vec<&str> = order.iter().map(|&i| plain_srcs[i].as_str()).collect();
            let map: Vec<(SymbolId, usize, usize)> = order
                .iter()
                .map(|&i| {
                    (
                        plain_subs[i].id,
                        plain_subs[i].rank,
                        plain_subs[i].branch_order,
                    )
                })
                .collect();
            let dfa = build_combined_dfa(&ordered_srcs, MatchKind::LeftmostFirst)?;
            Some(PlainEngine { dfa, map })
        };

        // The all-matches guarded engine.
        let guarded = if guarded_srcs.is_empty() {
            None
        } else {
            let srcs: Vec<&str> = guarded_srcs.iter().map(String::as_str).collect();
            let dfa = build_combined_dfa(&srcs, MatchKind::All)?;
            Some(GuardedEngine {
                dfa,
                subs: guarded_subs,
            })
        };

        // Start-byte prefilter over the base union of both engines (the lowered bases
        // over-approximate the guarded languages, so it never drops a real start byte).
        let start_bytes = if base_inlines.is_empty() {
            None
        } else {
            let union = format!("{prefix}(?:{})", base_inlines.join("|"));
            plain_start_bytes(&union)
        };

        Ok(DfaScanner {
            plain,
            guarded,
            start_bytes,
            unless,
        })
    }

    /// Match a single token starting exactly at `pos` — the same contract as
    /// [`Scanner::match_at`], so the two are byte-for-byte interchangeable. Consults the
    /// leftmost-first **plain** engine and the all-matches **guarded** engine and keeps
    /// the lower `(rank, branch_order)` candidate.
    fn match_at<'t>(&self, text: &'t str, pos: usize) -> Option<(SymbolId, &'t str)> {
        let mut best: Option<(usize, usize, SymbolId, &'t str)> = None;
        let runnable = match &self.start_bytes {
            Some(set) => text.as_bytes().get(pos).is_some_and(|b| set[*b as usize]),
            None => true,
        };
        if runnable && (self.plain.is_some() || self.guarded.is_some()) {
            record_scan_skip(pos, Some(pos));
            // Plain engine: the leftmost-first winner over the unguarded sub-patterns,
            // with its own (order-sensitive) length — never disturbed by a sibling guard.
            if let Some(p) = &self.plain {
                let input = Input::new(text)
                    .span(pos..text.len())
                    .anchored(Anchored::Yes);
                if let Ok(Some(hm)) = p.dfa.try_search_fwd(&input) {
                    if hm.offset() > pos {
                        let (id, rank, bo) = p.map[hm.pattern().as_usize()];
                        best = Some((rank, bo, id, &text[pos..hm.offset()]));
                    }
                }
            }
            // Guarded engine: the guarded-accept accumulator's winner.
            if let Some(g) = &self.guarded {
                if let Some(cand) = guarded_best(&g.dfa, &g.subs, text, pos) {
                    if best.is_none_or(|(r, b, _, _)| (cand.0, cand.1) < (r, b)) {
                        best = Some(cand);
                    }
                }
            }
        }
        let (_, _, id, value) = best?;
        let ty = self
            .unless
            .get(&id)
            .and_then(|m| m.get(value))
            .copied()
            .unwrap_or(id);
        Some((ty, value))
    }
}

/// Drive the guarded all-matches DFA over `text` from `pos`: enumerate every
/// `(sub-pattern, end)` accept via an **overlapping** anchored search, keep per
/// sub-pattern the **longest accept where its guard holds**, then select Lark's
/// leftmost-first winner across the survivors by `(rank, branch_order)`. Returns the
/// winning `(rank, branch_order, terminal id, matched slice)`, or `None`.
///
/// The overlapping search is the `regex-automata`-blessed way to read the full
/// accept-set out of a `MatchKind::All` DFA (it reports each distinct `(pattern,
/// end)` once, including multiple ends for one pattern — `[0-9]+` accepting at every
/// length). It is anchored at `pos` and stops when the DFA dies, so it is linear in
/// the matched token's length, never forward-scanning.
fn guarded_best<'t>(
    dfa: &dense::DFA<Vec<u32>>,
    subs: &[SubPattern],
    text: &'t str,
    pos: usize,
) -> Option<(usize, usize, SymbolId, &'t str)> {
    let input = Input::new(text)
        .span(pos..text.len())
        .anchored(Anchored::Yes);

    // A leading guard — and every bounded-lookbehind guard — is a precondition on the
    // match start (`pos`), identical for every accept of that sub-pattern, so evaluate
    // it once. `false` = some precondition failed, so the sub-pattern is out entirely.
    let leading_ok: Vec<bool> = subs
        .iter()
        .map(|s| {
            let lead = match &s.leading {
                None => true,
                Some(g) => g.holds(text, pos),
            };
            lead && s.lookbehind.iter().all(|lb| lb.holds(text, pos))
        })
        .collect();

    // Longest accept end (exclusive) per sub-pattern where both guards held.
    let mut longest: Vec<Option<usize>> = vec![None; subs.len()];
    let mut state = OverlappingState::start();
    loop {
        dfa.try_search_overlapping_fwd(&input, &mut state).ok()?;
        let Some(hm) = state.get_match() else { break };
        let pid = hm.pattern().as_usize();
        let end = hm.offset();
        if end <= pos {
            continue; // reject a zero-width accept
        }
        if !leading_ok[pid] {
            continue; // leading precondition failed → this sub-pattern can't match
        }
        let trailing_ok = match &subs[pid].trailing {
            None => true,
            Some(g) => g.holds(text, end),
        };
        if trailing_ok && longest[pid].is_none_or(|cur| end > cur) {
            longest[pid] = Some(end);
        }
    }

    // Lark's leftmost-first selection across the survivors: lowest terminal rank,
    // then lowest branch order within a terminal; the winner keeps its own longest
    // guard-held length.
    let mut best: Option<(usize, usize, SymbolId, usize)> = None;
    for (pid, end_opt) in longest.iter().enumerate() {
        let Some(end) = *end_opt else { continue };
        let s = &subs[pid];
        let key = (s.rank, s.branch_order);
        if best.is_none_or(|(r, b, _, _)| key < (r, b)) {
            best = Some((s.rank, s.branch_order, s.id, end));
        }
    }
    best.map(|(rank, bo, id, end)| (rank, bo, id, &text[pos..end]))
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

    // The whole-string ("full match") membership test for one regex terminal: the
    // anchored `regex` crate for the plain common case; a lookaround terminal is
    // routed through THE refusal seam and full-matched via its lowered branches
    // (each `^(?:branch)$` is lookaround-free, so `is_match` under the anchors is
    // pure language membership — greedy/lazy is irrelevant — plus the branch's
    // guards evaluated within the keyword value: leading at 0, trailing at the end
    // [EOI semantics, matching the assertion's view under `^…$`], lookbehinds at
    // their fixed offsets). A terminal the seam REFUSES is skipped silently here:
    // the engine build that follows reports the one canonical categorized error
    // (`docs/LOOKAROUND_SCOPE.md`), so no duplicate/diverging message is produced.
    enum FullMatcher {
        Plain(Regex),
        Lowered(Vec<(Regex, Option<Guard>, Option<Guard>, Vec<LookbehindGuardC>)>),
        Refused,
    }
    impl FullMatcher {
        fn is_full(&self, value: &str) -> bool {
            match self {
                FullMatcher::Plain(re) => re.is_match(value),
                FullMatcher::Lowered(branches) => {
                    branches.iter().any(|(re, leading, trailing, behinds)| {
                        re.is_match(value)
                            && leading.as_ref().is_none_or(|g| g.holds(value, 0))
                            && trailing
                                .as_ref()
                                .is_none_or(|g| g.holds(value, value.len()))
                            && behinds.iter().all(|g| g.holds(value, 0))
                    })
                }
                FullMatcher::Refused => false,
            }
        }
    }

    let prefix = global_flag_prefix(global_flags);
    let mut unless: HashMap<SymbolId, HashMap<String, SymbolId>> = HashMap::new();
    for (re_id, re_t) in &res {
        let full_src = format!("{}^(?:{})$", prefix, re_t.pattern.to_inline_regex());
        let full = match Regex::new(&full_src) {
            Ok(re) => FullMatcher::Plain(re),
            Err(e) => match route_fancy_only_terminal(re_t, global_flags, &e.to_string()) {
                Ok((branches, flags)) => {
                    let mut compiled = Vec::new();
                    for br in &branches {
                        let re_src = format!("{prefix}^(?:{})$", wrap_flags(flags, &br.regex));
                        let re = Regex::new(&re_src).map_err(|e| GrammarError::InvalidRegex {
                            pattern: re_src.clone(),
                            reason: e.to_string(),
                        })?;
                        let guard = |g: &crate::lookaround::lower::GuardSpec| {
                            Ok::<_, GrammarError>(Guard {
                                neg: g.neg,
                                dfa: build_anchored_dfa(&format!(
                                    "{prefix}{}",
                                    wrap_flags(flags, &g.set)
                                ))?,
                            })
                        };
                        let leading = br.leading.as_ref().map(&guard).transpose()?;
                        let trailing = br.trailing.as_ref().map(&guard).transpose()?;
                        let behinds = br
                            .lookbehind
                            .iter()
                            .map(|g| {
                                Ok::<_, GrammarError>(LookbehindGuardC {
                                    neg: g.neg,
                                    dfa: build_anchored_all_dfa(&format!(
                                        "{prefix}{}",
                                        wrap_flags(flags, &g.set)
                                    ))?,
                                    offset_chars: g.offset_chars,
                                    width: g.width,
                                })
                            })
                            .collect::<Result<Vec<_>, _>>()?;
                        compiled.push((re, leading, trailing, behinds));
                    }
                    FullMatcher::Lowered(compiled)
                }
                Err(_) => FullMatcher::Refused,
            },
        };
        for (s_id, s_t) in &strs {
            if s_t.priority != re_t.priority {
                continue;
            }
            let value = match &s_t.pattern {
                Pattern::Str(p) => &p.value,
                Pattern::Re(_) => continue,
            };
            if full.is_full(value) {
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
/// LALR parser state. States with the same terminal set share one [`Scanner`]
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
/// **One lookaround terminal, matched alone** — the per-terminal analogue of the
/// combined [`DfaScanner`], for engines that match terminals individually: the Earley
/// dynamic lexer ([`DynamicMatcher`]) and the `Scanner` reference backend's lowered
/// side-probes. Internally it *is* a single-terminal `DfaScanner` built through the
/// same plan/routing/guard machinery, so its semantics (greedy/lazy match end, guard
/// evaluation, lookbehind windows over the surrounding text) are the combined
/// scanner's by construction — one lowering, not two.
///
/// Build cost: only terminals the `regex` crate rejects pay for it (one small dense
/// DFA each); plain terminals never come near this type.
pub(crate) struct LoweredTerminalMatcher {
    scanner: DfaScanner,
}

impl LoweredTerminalMatcher {
    /// Build the matcher for one terminal, or fail with the same categorized scope
    /// error the combined build produces (`route_fancy_only_terminal` runs inside
    /// `DfaScanner::build`). `compile_err` is the `regex` crate's rejection message,
    /// quoted by the backtracking-only triage.
    fn build(
        id: SymbolId,
        def: &TerminalDef,
        global_flags: u32,
        compile_err: &str,
    ) -> Result<Self, GrammarError> {
        // `compile_err` is only consumed on the refusal path; pre-check the routing so
        // the error carries the engine's own message (DfaScanner::build re-derives the
        // same answer from the plan source, which includes the global prefix).
        route_fancy_only_terminal(def, global_flags, compile_err)?;
        let scanner = DfaScanner::build(&[(id, def)], global_flags)?;
        Ok(LoweredTerminalMatcher { scanner })
    }

    /// End of the non-empty match starting exactly at `pos` (the full `text` is
    /// passed so a lookbehind guard can see the bytes before `pos`).
    fn match_end_at(&self, text: &str, pos: usize) -> Option<usize> {
        let (_, value) = self.scanner.match_at(text, pos)?;
        Some(pos + value.len())
    }

    /// End offset of a non-empty match anchored at the start of `sub`.
    fn match_end_in(&self, sub: &str) -> Option<usize> {
        let (_, value) = self.scanner.match_at(sub, 0)?;
        Some(value.len())
    }
}

pub struct DynamicMatcher {
    res: HashMap<SymbolId, TermRegex>,
    ignore: Vec<SymbolId>,
    names: HashMap<SymbolId, String>,
}

/// One terminal's per-terminal matcher for the dynamic lexer: the `regex` crate for
/// the plain common case, the lowered single-terminal DFA for a lookaround terminal.
enum TermRegex {
    Plain(Regex),
    Lowered(LoweredTerminalMatcher),
}

impl TermRegex {
    /// End of the non-empty match starting exactly at `pos`, or `None` — the
    /// contract `AnyRegex::match_end_at` had. The full `text` (not a suffix) is
    /// passed so a lookbehind can see the bytes before `pos`, exactly as the
    /// historical fancy probe could.
    fn match_end_at(&self, text: &str, pos: usize) -> Option<usize> {
        match self {
            TermRegex::Plain(re) => {
                let m = re.find_at(text, pos);
                record_scan_skip(pos, m.as_ref().map(|m| m.start()));
                let m = m?;
                (m.start() == pos && m.end() > pos).then_some(m.end())
            }
            TermRegex::Lowered(m) => m.match_end_at(text, pos),
        }
    }

    /// End offset of a non-empty match anchored at the start of `sub`, or `None` —
    /// the contract `AnyRegex::match_end_in` had (used by the `dynamic_complete`
    /// scan, which re-matches against a truncated haystack).
    fn match_end_in(&self, sub: &str) -> Option<usize> {
        match self {
            TermRegex::Plain(re) => {
                let m = re.find(sub)?;
                (m.start() == 0 && m.end() > 0).then_some(m.end())
            }
            TermRegex::Lowered(m) => m.match_end_in(sub),
        }
    }
}

impl DynamicMatcher {
    /// Build a matcher from the same [`LexerConf`] the basic lexer uses, so both
    /// engines honour identical terminal patterns and global flags. A lookaround
    /// terminal lowers to its own single-terminal [`DfaScanner`]
    /// ([`LoweredTerminalMatcher`]); a terminal the lowering refuses fails the build
    /// with the **same categorized scope error** the basic-lexer path produces
    /// (`docs/LOOKAROUND_SCOPE.md`), so the dynamic lexer accepts exactly the same
    /// grammars.
    pub fn new(conf: &LexerConf) -> Result<Self, GrammarError> {
        let prefix = global_flag_prefix(conf.global_flags);
        let mut res = HashMap::new();
        for (id, term) in &conf.terminals {
            let src = format!("{}{}", prefix, term.pattern.to_inline_regex());
            let compiled = match Regex::new(&src) {
                Ok(re) => TermRegex::Plain(re),
                Err(e) => TermRegex::Lowered(LoweredTerminalMatcher::build(
                    *id,
                    term,
                    conf.global_flags,
                    &e.to_string(),
                )?),
            };
            res.insert(*id, compiled);
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

    /// Assert `DfaScanner::build` refuses `terms` with the expected categorized scope
    /// error (`docs/LOOKAROUND_SCOPE.md`) — the L4 contract: no fallback engine, a
    /// clean typed refusal instead.
    fn assert_dfa_scope_error(
        terms: &[(SymbolId, TerminalDef)],
        scope: crate::lookaround::classify::Scope,
        issue: crate::lookaround::classify::LookaroundIssue,
    ) {
        let refs: Vec<(SymbolId, &TerminalDef)> = terms.iter().map(|(i, t)| (*i, t)).collect();
        match DfaScanner::build(&refs, 0) {
            Err(GrammarError::LookaroundScope {
                scope: got_scope,
                issue: got_issue,
                ..
            }) => {
                assert_eq!(got_scope, scope);
                assert_eq!(got_issue, issue);
            }
            Err(other) => panic!("expected a LookaroundScope error, got {other:?}"),
            Ok(_) => panic!("expected the build to refuse, but it succeeded"),
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

    /// `unless` keyword retyping over a LOWERED terminal: the keyword's full-match
    /// test runs on the lowered branches + guards (`compute_unless`'s `FullMatcher`),
    /// not on any fallback engine. `T=/ab(?!c)|q/` overlaps the keyword `"ab"` (its
    /// trailing guard at the end of the value sees EOI → `(?!c)` holds, exactly as
    /// the historical `^(?:…)$` full-match saw it), so `"ab"` retypes to `K`; the
    /// guard still bites at scan time (`"abc"` is no `T` at 0).
    #[test]
    fn dfa_unless_retype_works_over_lowered_terminal() {
        let terms = [re_term(1, "T", "ab(?!c)|q", 0), str_term(2, "K", "ab", 0)];
        let refs: Vec<(SymbolId, &TerminalDef)> = terms.iter().map(|(i, t)| (*i, t)).collect();
        let d = DfaScanner::build(&refs, 0).expect("lowered terminal + keyword builds");
        assert_eq!(
            d.match_at("ab", 0),
            Some((SymbolId(2), "ab")),
            "retyped to K"
        );
        assert_eq!(d.match_at("q", 0), Some((SymbolId(1), "q")), "stays T");
        assert_eq!(d.match_at("abc", 0), None, "the trailing guard still bites");
    }

    #[test]
    fn dfa_guarded_order_sensitive_base_is_a_categorized_nyi_error() {
        // A trailing guard over a base whose internal alternation is order-sensitive
        // (`(ab|abc)`) is NOT greedy-monotone: "longest accept where the guard holds"
        // would pick "abc" where leftmost-first wants "ab". `is_greedy_monotone` keeps
        // it off the accumulator; since L4 there is no fallback engine, so the build
        // refuses with the categorized NotYetImplemented error — never a mis-lowering.
        use crate::lookaround::classify::{DeclineReason, LookaroundIssue, Scope};
        let terms = [re_term(1, "T", "(ab|abc)(?!z)", 0)];
        assert_dfa_scope_error(
            &terms,
            Scope::NotYetImplemented,
            LookaroundIssue::Declined(DeclineReason::NonRealizableGuardedBase),
        );
    }

    #[test]
    fn dfa_sibling_guard_does_not_demote_plain_alternation() {
        // Regression for the cross-terminal selection bug: a guarded terminal in the
        // same scanner as an *unguarded* order-sensitive alternation must NOT flip the
        // plain terminal from leftmost-first to longest-match. `AB=/ab|abc/` (plain)
        // stays leftmost-first ("ab") even though `B=/x(?!y)/` is guarded.
        let terms = [re_term(1, "AB", "ab|abc", 0), re_term(2, "B", "x(?!y)", 0)];
        let (s, d) = both(&terms);
        assert_eq!(d.match_at("abc", 0), Some((SymbolId(1), "ab")));
        assert_agree(&terms, &["abc", "ab", "x", "xy", "abx", "xab"]);
        let _ = s;
    }

    #[test]
    fn dfa_lazy_guarded_base_is_a_categorized_nyi_error() {
        // Regression for the lazy-body bug: a lazy quantifier in a guarded base
        // (`ab??(?!c)`) is not greedy-monotone — the longest-accept accumulator would
        // pick "ab" where leftmost-first (lazy) wants "a". The lowering declines it;
        // since L4 the build refuses with the categorized NotYetImplemented error.
        use crate::lookaround::classify::{DeclineReason, LookaroundIssue, Scope};
        let terms = [re_term(1, "T", "ab??(?!c)", 0)];
        assert_dfa_scope_error(
            &terms,
            Scope::NotYetImplemented,
            LookaroundIssue::Declined(DeclineReason::NonRealizableGuardedBase),
        );
    }

    /// **The engine-path pin for the bundled idioms.** The grammar loader delivers a
    /// terminal's `/…/is`-style flags **baked into the pattern** as one flag-scoped
    /// wrapper (`(?is:…)`, `PatternRe.flags = 0`) — exactly what `re_term` models here.
    /// `DfaScanner::build` must strip that wrapper back into the flag bitset
    /// (`strip_whole_pattern_flag_wrapper`) so the bundled `python.STRING` /
    /// `python.LONG_STRING` / `lark.REGEXP` idioms genuinely lower **on the engine
    /// path**: the built scanner has NO fancy side-probe at all. (Before the strip,
    /// the wrapped STRING silently rode the `Unsupported` compatibility fallback and
    /// the wrapped LONG_STRING the decline route — invisible to the differential,
    /// which the fancy reference backend matched anyway.) Behaviour is then pinned
    /// against `Scanner` on flag-sensitive inputs: a multi-line docstring (DOTALL)
    /// and case-folded prefixes (IGNORECASE).
    #[test]
    fn dfa_bundled_lookaround_terminals_lower_with_no_fancy_probe() {
        let terms = [
            re_term(
                1,
                "STRING",
                r#"(?i:([ubf]?r?|r[ubf])("(?!"").*?(?<!\\)(\\\\)*?"|'(?!'').*?(?<!\\)(\\\\)*?'))"#,
                0,
            ),
            re_term(
                2,
                "LONG_STRING",
                r#"(?is:([ubf]?r?|r[ubf])(""".*?(?<!\\)(\\\\)*?"""|'''.*?(?<!\\)(\\\\)*?'''))"#,
                1,
            ),
            re_term(3, "REGEXP", r"\/(?!\/)(\\\/|\\\\|[^\/])*?\/[imslux]*", 0),
        ];
        let (_, d) = both(&terms);
        // Since L4 there is no fallback engine at all, so the *build succeeding* is
        // itself the pin (a refused terminal is a categorized build error); the
        // structural assert below shows the idioms genuinely populate the engines.
        assert!(
            d.plain.is_some() && d.guarded.is_some(),
            "the lowered idioms populate both engines (unguarded branches + STRING's \
             guarded empty arm)"
        );
        assert_agree(
            &terms,
            &[
                "\"\"\"a\nb\"\"\"",  // DOTALL: the docstring spans lines
                "R\"x\"",            // IGNORECASE: case-folded prefix (STRING)
                "RB\"\"\"x\n\"\"\"", // IGNORECASE+DOTALL prefix (LONG_STRING)
                "\"\"\"\"",          // the (?!"") canary: no STRING opens in the run
                "\"\"\"\"\"\"",      // six quotes: one empty LONG_STRING
                "/a\\/b/i",          // REGEXP with escaped slash + flag
                "\"a\" '''b'''",     // STRING then LONG_STRING
            ],
        );
    }

    /// **The model-vs-reality closure for the zero-probe pin.** The test above models
    /// the loader's flag-bake format by hand (`re_term` with `(?is:…)`-wrapped
    /// patterns); if `Pattern::to_inline_regex` ever changed its emitted form, that
    /// model could keep passing while the *real* import path silently regressed to the
    /// fancy probe — exactly the invisible rot this PR dug `python.STRING` out of. So
    /// this twin builds the scanner from the **real loader output**: a grammar that
    /// `%import`s all three bundled lookaround terminals, run through `load_grammar` →
    /// `lower` → `basic_lexer_conf`, must also build — and since L4 a successful build
    /// IS the zero-probe claim (a refused terminal is a categorized build error; no
    /// fallback engine exists).
    #[test]
    fn dfa_real_loader_bundled_imports_have_no_fancy_probe() {
        let grammar = "start: STRING | LONG_STRING | REGEXP\n\
                       %import python.STRING\n\
                       %import python.LONG_STRING\n\
                       %import lark.REGEXP\n";
        let g = crate::load_grammar(grammar, &["start".to_string()], false, false)
            .expect("grammar importing the three bundled lookaround terminals builds");
        let cg = crate::lower(&g);
        let conf = crate::basic_lexer_conf(&cg, 0);
        let refs: Vec<(SymbolId, &TerminalDef)> =
            conf.terminals.iter().map(|(i, t)| (*i, t)).collect();
        let d = DfaScanner::build(&refs, conf.global_flags).expect(
            "the REAL loader-imported bundled terminals must lower — a refusal here \
             means `to_inline_regex`'s bake format and the flag-wrapper strip drifted",
        );
        assert!(d.plain.is_some() && d.guarded.is_some());
    }

    /// **The VERBOSE conservatism pin.** `strip_whole_pattern_flag_wrapper` must NOT
    /// strip a `(?x:…)` wrapper: the lookaround parser's width/offset analysis is not
    /// verbose-aware, so a stripped `x`-body would count whitespace as literal width
    /// while the re-wrapped branch ignores it — a fixed-offset lookbehind could lower
    /// with a wrong offset (a false-accept). An `x`-wrapped lookaround terminal must
    /// never lower: the strip leaves the wrapper alone, and since L4 the build refuses
    /// it with the honest categorized NotYetImplemented `VerboseMode` error (not the
    /// classifier's mislabel of the group-nested assertion as out-of-scope internal
    /// lookahead).
    #[test]
    fn verbose_flag_wrapper_is_not_stripped_into_lowering() {
        use crate::lookaround::classify::{DeclineReason, LookaroundIssue, Scope};
        // Whitespace inside the verbose body is regex-insignificant at runtime but
        // would be width-significant to a naive strip + reparse.
        let terms = [re_term(1, "VX", r"(?x:[0-9]+ (?![0-9]))", 0)];
        assert_dfa_scope_error(
            &terms,
            Scope::NotYetImplemented,
            LookaroundIssue::Declined(DeclineReason::VerboseMode),
        );
        // The helper itself: an `x` anywhere in the wrapper letters refuses the strip
        // wholesale; the plain `i`/`s` strips still work.
        assert_eq!(
            strip_whole_pattern_flag_wrapper("(?x:a b)", 0),
            ("(?x:a b)".to_string(), 0)
        );
        assert_eq!(
            strip_whole_pattern_flag_wrapper("(?isx:a)", 0),
            ("(?isx:a)".to_string(), 0)
        );
        let f =
            crate::grammar::terminal::flags::IGNORECASE | crate::grammar::terminal::flags::DOTALL;
        assert_eq!(
            strip_whole_pattern_flag_wrapper("(?is:a)", 0),
            ("a".to_string(), f)
        );
    }

    /// **The global-VERBOSE conservatism pin** (PR #137 review, blocker 1). The
    /// verbose false-accept hazard is not only the explicit `(?x:…)` wrapper:
    /// `g_regex_flags = VERBOSE` compiles every terminal under a global `(?x)`
    /// prefix while the lookaround analyzer still counts whitespace/comments as
    /// literal width — the exact same class. The routing seam must refuse any
    /// lookaround pattern under global VERBOSE with the same categorized NYI
    /// `VerboseMode` error, on BOTH combined-scanner builds (and, via the seam,
    /// every other engine path). A verbose *plain* pattern never reaches the seam
    /// (the `regex` crate compiles `(?x)` natively) and must keep building.
    #[test]
    fn global_verbose_flag_refuses_lookaround_lowering() {
        use crate::grammar::terminal::flags;
        use crate::lookaround::classify::{DeclineReason, LookaroundIssue, Scope};
        // No wrapper: the pattern looks analyzable, but under (?x) the space before
        // the guard is ignored at runtime while the analyzer would count it.
        let terms = [re_term(1, "VG", r"[0-9]+ (?![0-9])", 0)];
        let refs: Vec<(SymbolId, &TerminalDef)> = terms.iter().map(|(i, t)| (*i, t)).collect();
        let assert_refused = |result: Result<(), GrammarError>, engine: &str| match result {
            Err(GrammarError::LookaroundScope { scope, issue, .. }) => {
                assert_eq!(scope, Scope::NotYetImplemented, "{engine}");
                assert_eq!(
                    issue,
                    LookaroundIssue::Declined(DeclineReason::VerboseMode),
                    "{engine}"
                );
            }
            Err(other) => panic!("{engine}: expected the VerboseMode scope error, got {other:?}"),
            Ok(()) => panic!(
                "{engine}: a global-VERBOSE lookaround terminal built — the false-accept class"
            ),
        };
        assert_refused(
            DfaScanner::build(&refs, flags::VERBOSE).map(drop),
            "DfaScanner",
        );
        // The Regex backend refuses identically — including under the TEST-ONLY
        // `fancy-oracle` feature, whose build routes the same seam first (PR #137
        // review, blocker 2: the feature must never widen the accepted grammar set).
        assert_refused(Scanner::build(&refs, flags::VERBOSE).map(drop), "Scanner");
        // Without an assertion there is no hazard: a verbose plain pattern compiles
        // on the `regex` crate and never reaches the routing seam.
        let plain = [re_term(1, "VP", r"[0-9]+ [a-z]+", 0)];
        let prefs: Vec<(SymbolId, &TerminalDef)> = plain.iter().map(|(i, t)| (*i, t)).collect();
        DfaScanner::build(&prefs, flags::VERBOSE)
            .expect("a plain pattern under global VERBOSE builds (the regex crate handles `x`)");
        Scanner::build(&prefs, flags::VERBOSE)
            .expect("a plain pattern under global VERBOSE builds (the regex crate handles `x`)");
    }

    #[test]
    fn dfa_all_lookaround_terminals_is_a_categorized_out_of_scope_error() {
        // A scanner whose only terminal is an *internal*-assertion lookaround pattern
        // (not a lowerable boundary shape, not a recognized idiom) refuses to build
        // with the categorized OutOfScope error — since L4 there is no fancy
        // side-probe to ride.
        use crate::lookaround::classify::Rejection;
        use crate::lookaround::classify::{LookaroundIssue, Scope};
        let terms = [re_term(1, "STR", "\"(?!\")[^\"]*\"", 0)];
        assert_dfa_scope_error(
            &terms,
            Scope::OutOfScope,
            LookaroundIssue::Rejected(Rejection::Internal),
        );
    }
}
