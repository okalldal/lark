//! The **runtime** of the L2 bounded-lookaround lowering — the lookaround-free
//! matcher a classified terminal lowers to (`docs/LEXER_DFA_PLAN.md`, "How the
//! lowering works"). [`classify`](super::classify) decides *what* a terminal lowers
//! to (a [`Lowered`] value, all plain-regex strings, no engine); this module turns
//! that into a concrete matcher driven entirely by `regex-automata` — **no
//! `fancy-regex` at runtime** — and matched at a byte offset.
//!
//! ## Shape 1 — trailing boundary (`X(?!S)` / `X(?=S)`): the guarded accept
//!
//! The lookahead char belongs to the *next* token, so it is never consumed: it is a
//! **guard** the driver checks at the accept. The body `X` is a plain regular
//! language; the guard `S` is a plain regular language one position past the match.
//! For a greedy body, fancy-regex tries the longest accept of `X` first and
//! backtracks to a shorter one only when the guard fails — so the lowered match is
//! the **longest accept of `X` at which the guard holds**. We realise that with no
//! backtracking engine: enumerate the body's accept offsets (the per-state accept
//! set, via the `Automaton` trait's overlapping search) and walk them longest-first,
//! returning the first at which the guard holds. The length-changing case
//! (`DEC_NUMBER` `0001`→`00`) falls straight out of "last accept where the guard
//! held."
//!
//! A terminal is an ordered alternation of branches (the regex `|`), and only some
//! branches carry a trailing guard (`lark`'s `OP` = `[+*]|[?](?![a-z])`). The match
//! is the **first branch, in source order, that matches** — exactly the regex
//! engine's ordered-alternation semantics — so we try the branches in order and take
//! the first that yields a match (including a zero-width one, which the lexer then
//! rejects, mirroring `fancy-regex`).

use regex_automata::{
    dfa::{dense, Automaton, OverlappingState, StartKind},
    meta::Regex as MetaRegex,
    Anchored, Input, MatchKind,
};

use super::classify::{Lowered, LoweredBranch, TrailingGuard};
use crate::error::GrammarError;

/// A compiled, lookaround-free matcher for one lowered terminal. Built from a
/// [`Lowered`] (plain-regex strings) and matched at a byte offset against the full
/// text. The engine is `regex-automata` only.
pub struct LoweredMatcher {
    kind: Kind,
}

enum Kind {
    /// A trailing-boundary terminal: ordered branches, tried in source order.
    Trailing(Vec<BranchMatcher>),
}

struct BranchMatcher {
    /// Leftmost-first matcher for an *unguarded* branch body — reproduces the body's
    /// own greedy/lazy preference exactly. `None` for a guarded branch.
    body_leftmost: Option<MetaRegex>,
    /// Accept-set DFA for a *guarded* branch body (`MatchKind::All`, anchored), so
    /// the driver can see every accept offset and walk them longest-first. `None`
    /// for an unguarded branch.
    body_accepts: Option<dense::DFA<Vec<u32>>>,
    /// The trailing guard, if any.
    guard: Option<GuardMatcher>,
}

struct GuardMatcher {
    /// `true` for `(?!S)` (the next chars must *not* start `S`), `false` for `(?=S)`.
    neg: bool,
    /// Anchored matcher for the guard's plain regex `S`.
    re: MetaRegex,
}

fn other(msg: String) -> GrammarError {
    GrammarError::Other { msg }
}

impl LoweredMatcher {
    /// Build a runtime matcher for a [`Lowered`] terminal. Errors only if a lowered
    /// fragment fails to compile under `regex-automata` (not expected — every
    /// fragment is a plain regex the classifier already accepted).
    pub fn build(lowered: &Lowered) -> Result<LoweredMatcher, GrammarError> {
        match lowered {
            Lowered::Plain => Err(other(
                "a plain terminal needs no lowered matcher (internal error)".into(),
            )),
            Lowered::Trailing(branches) => {
                let mut out = Vec::with_capacity(branches.len());
                for b in branches {
                    out.push(BranchMatcher::build(b)?);
                }
                Ok(LoweredMatcher {
                    kind: Kind::Trailing(out),
                })
            }
        }
    }

    /// The **raw** matched-prefix byte length of the lowered terminal beginning
    /// exactly at `pos`, or `None` if it does not match there. A zero-width match
    /// returns `Some(pos)` — the lexer rejects that itself (mirroring `fancy-regex`'s
    /// `find` returning an empty match, which `match_end_at` then drops). The
    /// terminal-level equivalence oracle compares this against `fancy-regex` and so
    /// needs the raw length (zero-width included).
    pub fn match_prefix(&self, text: &str, pos: usize) -> Option<usize> {
        match &self.kind {
            Kind::Trailing(branches) => {
                // Ordered alternation: the first branch (in source order) that
                // matches at `pos` wins, exactly as the regex engine's `|` resolves.
                for b in branches {
                    if let Some(end) = b.match_at(text, pos) {
                        return Some(end);
                    }
                }
                None
            }
        }
    }
}

impl BranchMatcher {
    fn build(branch: &LoweredBranch) -> Result<BranchMatcher, GrammarError> {
        let guard = match &branch.guard {
            Some(g) => Some(GuardMatcher::build(g)?),
            None => None,
        };
        if guard.is_some() {
            // Guarded: build the accept-set DFA so the driver can walk the body's
            // accept offsets longest-first.
            let dfa = build_accept_dfa(&branch.body)?;
            Ok(BranchMatcher {
                body_leftmost: None,
                body_accepts: Some(dfa),
                guard,
            })
        } else {
            // Unguarded: the body matches with its own leftmost-first preference.
            let re = MetaRegex::new(&branch.body).map_err(|e| {
                other(format!(
                    "lowered body {:?} failed to compile: {e}",
                    branch.body
                ))
            })?;
            Ok(BranchMatcher {
                body_leftmost: Some(re),
                body_accepts: None,
                guard: None,
            })
        }
    }

    /// Match this branch at `pos`, returning the matched byte end (possibly `pos` for
    /// a zero-width match), or `None`.
    fn match_at(&self, text: &str, pos: usize) -> Option<usize> {
        match (&self.guard, &self.body_leftmost, &self.body_accepts) {
            // Unguarded branch: the body's own leftmost-first match.
            (None, Some(re), _) => {
                let input = Input::new(text)
                    .span(pos..text.len())
                    .anchored(Anchored::Yes);
                re.find(input).map(|m| m.end())
            }
            // Guarded branch: longest body accept at which the guard holds.
            (Some(guard), _, Some(dfa)) => {
                let accepts = accept_offsets(dfa, text, pos);
                // Longest-first: a greedy body prefers the longest accept, and
                // fancy-regex backtracks to a shorter one only when the guard fails.
                accepts
                    .iter()
                    .rev()
                    .copied()
                    .find(|&e| guard.holds(text, e))
            }
            _ => None,
        }
    }
}

impl GuardMatcher {
    fn build(g: &TrailingGuard) -> Result<GuardMatcher, GrammarError> {
        let re = MetaRegex::new(&g.guard).map_err(|e| {
            other(format!(
                "lowered guard {:?} failed to compile: {e}",
                g.guard
            ))
        })?;
        Ok(GuardMatcher { neg: g.neg, re })
    }

    /// Whether the trailing guard holds when the match ends at byte offset `at`: does
    /// `S` match a prefix of the text starting at `at` (positive guard wants yes,
    /// negative wants no)? At end-of-input `S` cannot match, so a negative guard
    /// holds and a positive one fails — exactly `fancy-regex`'s `(?=S)` / `(?!S)`.
    fn holds(&self, text: &str, at: usize) -> bool {
        let input = Input::new(text)
            .span(at..text.len())
            .anchored(Anchored::Yes);
        let matched = self.re.is_match(input);
        if self.neg {
            !matched
        } else {
            matched
        }
    }
}

/// Build the anchored, all-matches accept DFA for a plain body regex. `MatchKind::All`
/// keeps every accept state live (no leftmost-first short-circuit), so an overlapping
/// search enumerates the body's full accept set at a position.
fn build_accept_dfa(body: &str) -> Result<dense::DFA<Vec<u32>>, GrammarError> {
    dense::Builder::new()
        .configure(
            dense::Config::new()
                .match_kind(MatchKind::All)
                .start_kind(StartKind::Anchored),
        )
        .build(body)
        .map_err(|e| other(format!("lowered body {body:?} failed to determinize: {e}")))
}

/// Every byte offset `e ≥ pos` at which the body DFA accepts the prefix `text[pos..e]`,
/// in ascending order (deduplicated). Driven through the `Automaton` trait's
/// overlapping search, which reports each accept end exactly once.
fn accept_offsets(dfa: &dense::DFA<Vec<u32>>, text: &str, pos: usize) -> Vec<usize> {
    let input = Input::new(text)
        .span(pos..text.len())
        .anchored(Anchored::Yes);
    let mut state = OverlappingState::start();
    let mut out = Vec::new();
    loop {
        if dfa.try_search_overlapping_fwd(&input, &mut state).is_err() {
            break;
        }
        match state.get_match() {
            Some(hm) => out.push(hm.offset()),
            None => break,
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lookaround::classify::lower_terminal;

    fn dfa(body: &str) -> dense::DFA<Vec<u32>> {
        build_accept_dfa(body).unwrap()
    }

    #[test]
    fn accept_offsets_enumerate_every_prefix_match() {
        assert_eq!(accept_offsets(&dfa("[0-9]+"), "0001", 0), vec![1, 2, 3, 4]);
        assert_eq!(accept_offsets(&dfa("0"), "0", 0), vec![1]);
        assert_eq!(accept_offsets(&dfa("[0-9]*"), "ab", 0), vec![0]);
        assert_eq!(accept_offsets(&dfa("[a-z]*"), "ab", 0), vec![0, 1, 2]);
        // From a non-zero offset.
        assert_eq!(accept_offsets(&dfa("[0-9]+"), "x012", 1), vec![2, 3, 4]);
    }

    /// The length-changing guard: `[0-9]+(?![0-9])` over a digit run with a trailing
    /// digit takes the *whole* run (greedy), while a guarded shorter accept is taken
    /// only when the greedy one's guard fails — here it never does (greedy eats them
    /// all), so this pins the greedy case.
    #[test]
    fn trailing_guard_greedy_then_guarded() {
        let lo = lower_terminal("T", "[0-9]+(?![0-9])").unwrap();
        let m = LoweredMatcher::build(&lo).unwrap();
        assert_eq!(m.match_prefix("123", 0), Some(3)); // greedy, guard holds at EOF
        assert_eq!(m.match_prefix("12a", 0), Some(2)); // guard holds before 'a'
        assert_eq!(m.match_prefix("a", 0), None); // body needs a digit
    }

    /// `0(?![1-9])` — the bundled `DEC_NUMBER` leading-zero guard. `0` matches, then
    /// the next char must not be `1..9`.
    #[test]
    fn dec_number_zero_guard() {
        let lo = lower_terminal("DEC", "0(?![1-9])").unwrap();
        let m = LoweredMatcher::build(&lo).unwrap();
        assert_eq!(m.match_prefix("0", 0), Some(1));
        assert_eq!(m.match_prefix("0a", 0), Some(1));
        assert_eq!(m.match_prefix("00", 0), Some(1)); // '0' then '0' (not 1-9): holds
        assert_eq!(m.match_prefix("01", 0), None); // '0' then '1': guard fails
    }

    /// `[+*]|[?](?![a-z])` — the bundled `lark` `OP`: an ordered alternation where
    /// only the second branch is guarded.
    #[test]
    fn ordered_alternation_first_branch_wins() {
        let lo = lower_terminal("OP", "[+*]|[?](?![a-z])").unwrap();
        let m = LoweredMatcher::build(&lo).unwrap();
        assert_eq!(m.match_prefix("+", 0), Some(1)); // branch 0
        assert_eq!(m.match_prefix("?", 0), Some(1)); // branch 1, guard at EOF holds
        assert_eq!(m.match_prefix("?a", 0), None); // branch 1, guard fails
        assert_eq!(m.match_prefix("?1", 0), Some(1)); // '1' not [a-z]: holds
    }
}
