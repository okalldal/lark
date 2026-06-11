//! Compiled lowered-lookaround guards and their compilation context.
//!
//! A lowered branch's assertions become *guards* evaluated beside the DFA match:
//! a boundary [`Guard`] checked at the match start or end, and a bounded
//! [`LookbehindGuardC`] read backward from a fixed char-offset. Compilation is
//! funneled through [`GuardContext`] so every guard of one terminal is provably
//! built under the same global-flag prefix and merged flag bitset as the branch
//! it guards — the invariant the old closure-captured compilation kept implicit.

use regex_automata::{
    dfa::{dense, Automaton, OverlappingState, StartKind},
    Anchored, Input, MatchKind,
};

use super::pattern::wrap_flags;
use crate::error::GrammarError;
use crate::lookaround::lower::{GuardSpec, LookbehindGuard};

/// A compiled boundary guard. The driver records an accept of the guarded
/// sub-pattern only when this holds at its position (start for leading, end for
/// trailing) — so the peeked char, which belongs to a neighbouring token, is
/// consulted but never consumed.
pub(super) struct Guard {
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
    pub(super) fn holds(&self, text: &str, at: usize) -> bool {
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
pub(super) struct LookbehindGuardC {
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
