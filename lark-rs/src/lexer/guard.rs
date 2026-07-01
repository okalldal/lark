//! Compiled lowered-lookaround guards and their compilation context.
//!
//! A lowered branch's assertions become *guards* evaluated beside the DFA match:
//! a boundary [`Guard`] checked at the match start or end, and a bounded
//! [`LookbehindGuardC`] read backward from a fixed char-offset. Compilation is
//! funneled through [`GuardContext`] so every guard of one terminal is provably
//! built under the same global-flag prefix and merged flag bitset as the branch
//! it guards — the invariant the old closure-captured compilation kept implicit.

use std::cell::RefCell;

use regex_automata::{
    dfa::{dense, Automaton, OverlappingState, StartKind},
    hybrid::dfa::{Cache as LazyCache, DFA as LazyDfa},
    nfa::thompson,
    Anchored, Input, MatchKind,
};

use super::dfa::DENSE_PER_SOURCE_BUDGET;
use super::pattern::wrap_flags;
use crate::error::GrammarError;
use crate::lookaround::lower::{GuardSpec, LookbehindGuard};

/// A guard body's compiled automaton, either **eagerly determinized** (the dense
/// default) or **lazily** (the hybrid fallback for an over-budget body — issue #568,
/// the guard-body analog of the [`dfa`](super::dfa)-engine H4-12 fallback, ADR-0037).
///
/// A guard body `S` in `(?=S)` / `(?!S)` / `(?<=S)` / `(?<!S)` is a user-authored regex
/// carried **verbatim** into the anchored DFA build (`GuardSpec::set` / `LookbehindGuard::set`
/// are `body.to_source()` — no width bound). A leading lookahead body may be unbounded
/// (`classify.rs` admits `(?![01]*1[01]{N})…`), so the classic `.*a.{N}` determinization
/// blow-up (`2^(N+1)` states) is reachable through a guard body exactly as it was through a
/// main-engine terminal. This enum bounds the eager build under [`DENSE_PER_SOURCE_BUDGET`]
/// (the same budget `dfa.rs` uses) and routes an over-budget body to the lazy/hybrid DFA,
/// which realizes states on demand and so stays *flat* in build cost. The hybrid DFA
/// produces byte-identical matches, so oracle parity is preserved — the guard still
/// evaluates exactly as Python's backtracking assertion does; only the eager
/// determinization is skipped.
///
/// The lazy DFA needs a mutable scratch [`LazyCache`]; `Lark` is already `!Sync`, so the
/// `RefCell<LazyCache>` here is consistent with the `dfa.rs` `CombinedDfa::Hybrid`.
enum GuardDfa {
    /// The eager determinization — used for every guard body that fits the size budget.
    Dense(dense::DFA<Vec<u32>>),
    /// The lazy fallback — used only for an over-budget body (#568). The cache is reused
    /// across `holds` calls (interior-mutable; the scanner is `!Sync`).
    Hybrid {
        dfa: LazyDfa,
        cache: RefCell<LazyCache>,
    },
}

impl GuardDfa {
    /// Anchored leftmost-first "does `S` match at the span start?" — the dense DFA's
    /// `try_search_fwd`, or the lazy DFA's with a borrowed cache. A run/search error
    /// (only the lazy cache's giving-up path) reads as "no match", matching the dense
    /// path's `Ok(None)`.
    fn is_match(&self, input: &Input<'_>) -> bool {
        match self {
            GuardDfa::Dense(dfa) => matches!(dfa.try_search_fwd(input), Ok(Some(_))),
            GuardDfa::Hybrid { dfa, cache } => {
                matches!(
                    dfa.try_search_fwd(&mut cache.borrow_mut(), input),
                    Ok(Some(_))
                )
            }
        }
    }

    /// Whether the anchored **overlapping** (all-matches) search yields an accept whose
    /// end is exactly `full_len` — the full-slice match test the lookbehind window uses.
    /// The dense and lazy DFAs carry *different* `OverlappingState` types, so each owns
    /// its own state-threaded loop (mirrors `dfa.rs::CombinedDfa::for_each_overlapping`).
    fn full_match(&self, input: &Input<'_>, full_len: usize) -> bool {
        match self {
            GuardDfa::Dense(dfa) => {
                let mut state = OverlappingState::start();
                loop {
                    if dfa.try_search_overlapping_fwd(input, &mut state).is_err() {
                        return false;
                    }
                    match state.get_match() {
                        Some(hm) if hm.offset() == full_len => return true,
                        Some(_) => continue,
                        None => return false,
                    }
                }
            }
            GuardDfa::Hybrid { dfa, cache } => {
                let cache = &mut cache.borrow_mut();
                let mut state = regex_automata::hybrid::dfa::OverlappingState::start();
                loop {
                    if dfa
                        .try_search_overlapping_fwd(cache, input, &mut state)
                        .is_err()
                    {
                        return false;
                    }
                    match state.get_match() {
                        Some(hm) if hm.offset() == full_len => return true,
                        Some(_) => continue,
                        None => return false,
                    }
                }
            }
        }
    }
}

/// A compiled boundary guard. The driver records an accept of the guarded
/// sub-pattern only when this holds at its position (start for leading, end for
/// trailing) — so the peeked char, which belongs to a neighbouring token, is
/// consulted but never consumed.
pub(super) struct Guard {
    /// `true` for `(?!S)` (must **not** match `S`), `false` for `(?=S)`.
    neg: bool,
    /// Anchored DFA for the assertion body `S`, matched at the guard position. Dense
    /// (in-budget) or lazy/hybrid (over-budget body — #568).
    dfa: GuardDfa,
}

impl Guard {
    /// Whether the guard is satisfied at byte offset `at` in `text`. At end-of-input
    /// (`at == text.len()`) `S` cannot match (no chars follow), so a negative guard
    /// `(?!S)` holds and a positive guard `(?=S)` fails — exactly Python's
    /// trailing-assertion-at-EOF semantics.
    ///
    /// Cost note: the DFA run stops where `S`'s automaton dies, so a *bounded*
    /// body keeps a guard run O(width). A LEADING guard may carry an unbounded
    /// body (`classify.rs` admits e.g. `(?!\[=*\[)`), for which the run is
    /// bounded only by the remaining input — the same per-attempt worst case
    /// Python `re` pays evaluating the identical assertion, so oracle parity
    /// holds, but such a terminal is not linear-by-construction.
    pub(super) fn holds(&self, text: &str, at: usize) -> bool {
        let input = Input::new(text)
            .span(at..text.len())
            .anchored(Anchored::Yes);
        let s_matches = self.dfa.is_match(&input);
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
pub(super) struct LookbehindGuardC {
    /// `true` for `(?<!S)` (the window must **not** match `S`), `false` for `(?<=S)`.
    neg: bool,
    /// All-matches anchored DFA for the body `S`, so a window is tested for an *exact*
    /// (full-slice) match regardless of `S`'s internal alternation order. Dense
    /// (in-budget) or lazy/hybrid (over-budget body — #568).
    dfa: GuardDfa,
    /// Char offset from the match start to the lookbehind point.
    offset_chars: usize,
    /// Maximum char width of `S` — the driver tries window lengths `1..=width`.
    width: usize,
}

impl LookbehindGuardC {
    /// Whether the lookbehind holds for a match starting at byte `pos` in `text`.
    pub(super) fn holds(&self, text: &str, pos: usize) -> bool {
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
        self.dfa.full_match(&input, slice.len())
    }
}

/// The compile-time context every guard of one terminal shares: the global-flag
/// prefix and the terminal's merged flag bitset (recovered by the whole-pattern
/// wrapper strip on the routing seam). Threaded as a value — not closure
/// captures — so the invariant is visible in the signature: **every** guard DFA
/// is built under the same `prefix` + `flags` as the lowered branch it guards.
/// Shared by the combined [`DfaScanner`](super::dfa::DfaScanner) build and
/// `compute_unless`'s full-match probes, so the two cannot drift.
pub(super) struct GuardContext<'a> {
    pub(super) prefix: &'a str,
    pub(super) flags: u32,
}

impl GuardContext<'_> {
    /// Compile one boundary guard under this context.
    pub(super) fn compile_guard(&self, g: &GuardSpec) -> Result<Guard, GrammarError> {
        let src = format!("{}{}", self.prefix, wrap_flags(self.flags, &g.set));
        Ok(Guard {
            neg: g.neg,
            dfa: build_anchored_dfa(&src)?,
        })
    }

    /// Compile one bounded-lookbehind guard under this context.
    pub(super) fn compile_lookbehind(
        &self,
        g: &LookbehindGuard,
    ) -> Result<LookbehindGuardC, GrammarError> {
        let src = format!("{}{}", self.prefix, wrap_flags(self.flags, &g.set));
        Ok(LookbehindGuardC {
            neg: g.neg,
            dfa: build_anchored_all_dfa(&src)?,
            offset_chars: g.offset_chars,
            width: g.width,
        })
    }
}

/// Build an anchored DFA for a guard body `src` (leftmost-first; we only need a yes/no
/// "does `S` match here").
fn build_anchored_dfa(src: &str) -> Result<GuardDfa, GrammarError> {
    build_guard_dfa(src, MatchKind::LeftmostFirst)
}

/// Build an anchored **all-matches** DFA for a lookbehind body `src`, so the driver can
/// test whether `S` matches a window *exactly* (every accept length is surfaced via an
/// overlapping search, not just the leftmost-first one).
fn build_anchored_all_dfa(src: &str) -> Result<GuardDfa, GrammarError> {
    build_guard_dfa(src, MatchKind::All)
}

/// Build the anchored [`GuardDfa`] for a guard body `src` under `match_kind`, **bounded**
/// by [`DENSE_PER_SOURCE_BUDGET`] (#568). Mirrors `dfa.rs::build_partitioned_dfa`'s
/// per-source probe, but for a *single* source: compile `src` to a Thompson NFA, try the
/// eager dense determinization under the size limit, and — only on a genuine
/// `is_size_limit_exceeded()` overflow (the `.*a.{N}` guard-body blow-up) — fall back to a
/// lazy/hybrid DFA over the same NFA. Any *other* dense error (e.g. an unsupported Unicode
/// word boundary) is a real build error attributed to this exact body, never silently
/// rerouted — the same asymmetry ADR-0037 keeps, so the hybrid path never accepts a body
/// the eager path would have rejected.
fn build_guard_dfa(src: &str, match_kind: MatchKind) -> Result<GuardDfa, GrammarError> {
    let nfa = thompson::NFA::compiler()
        .configure(thompson::Config::new().which_captures(thompson::WhichCaptures::None))
        .build(src)
        .map_err(|e| GrammarError::InvalidRegex {
            pattern: src.to_string(),
            reason: e.to_string(),
        })?;

    match dense::Builder::new()
        .configure(
            dense::Config::new()
                .match_kind(match_kind)
                .start_kind(StartKind::Anchored)
                .dfa_size_limit(Some(DENSE_PER_SOURCE_BUDGET))
                .determinize_size_limit(Some(DENSE_PER_SOURCE_BUDGET)),
        )
        .build_from_nfa(&nfa)
    {
        Ok(dfa) => {
            crate::perf::add_dense_build_bytes(dfa.memory_usage() as u64);
            Ok(GuardDfa::Dense(dfa))
        }
        // Over-budget guard body → lazy/hybrid fallback (the #568 case). `is_size_limit_exceeded()`
        // covers both the DFA- and determinize-size limits and the too-many-states overflow.
        Err(e) if e.is_size_limit_exceeded() => {
            let dfa = LazyDfa::builder()
                .configure(
                    LazyDfa::config()
                        .match_kind(match_kind)
                        // Skip the cache-capacity sanity check: a deliberately large NFA
                        // (the .*a.{N} family) has a big worst-case state, but the realized
                        // cache stays small in practice. The lazy DFA bounds memory itself.
                        .skip_cache_capacity_check(true),
                )
                .build_from_nfa(nfa)
                .map_err(|e| GrammarError::InvalidRegex {
                    pattern: src.to_string(),
                    reason: e.to_string(),
                })?;
            let cache = RefCell::new(dfa.create_cache());
            // The lazy DFA realizes states on demand, so its build-time memory is a small
            // fixed cache — counting it keeps the scaling gate honest (it stays ~flat).
            crate::perf::add_dense_build_bytes(cache.borrow().memory_usage() as u64);
            Ok(GuardDfa::Hybrid { dfa, cache })
        }
        // A non-size dense error (e.g. an unsupported feature) is a real build error for
        // this exact body — surface it, never reroute (would misattribute or, worse,
        // accept a body the eager path rejected).
        Err(e) => Err(GrammarError::InvalidRegex {
            pattern: src.to_string(),
            reason: e.to_string(),
        }),
    }
}
