//! Bounded-lookaround **lowering** — the L2 feature's first shape
//! (`docs/LEXER_DFA_PLAN.md`, "How the lowering works").
//!
//! This module turns a *supported* lookaround terminal into a lookaround-free
//! matcher on `regex-automata`, so the terminal joins the DFA-backed scanner and no
//! longer needs the `fancy-regex` side-probe. **Only the trailing-boundary shape is
//! implemented here** — the self-contained guarded accept, the plan's first shape.
//! Leading-boundary and bounded-lookbehind still route to `fancy-regex` until their
//! sessions land; [`build_trailing`] returns `Ok(None)` for them (the safe fall-back).
//!
//! ## Trailing boundary as a guarded accept
//!
//! A trailing-boundary terminal is an alternation whose branches each look like
//! `BODY` or `BODY(?=S)` / `BODY(?!S)`, where the assertion is the **last** element
//! of the branch (the classifier guarantees this; see [`classify`](super::classify)).
//! The body is an ordinary regular language, so we:
//!
//!   1. strip the trailing assertion off each branch → a plain `BODY_i` pattern and a
//!      per-branch *guard* `(S_i, neg_i)` (or no guard);
//!   2. compile all the `BODY_i` into **one** anchored multi-pattern dense DFA under
//!      `MatchKind::All`, so an overlapping search enumerates *every* accept end of
//!      *every* branch at a position — the "accept set" the plan's guarded-longest
//!      accumulator needs;
//!   3. at match time, for each branch keep the **longest** accept end whose guard
//!      holds (the next char ∈/∉ `S_i`), then pick the **lowest-index** branch that
//!      has one — Python-`re`'s leftmost-first alternation, with that branch's own
//!      greedy length. The length-changing case (`[0-9]+(?![a-z])` on `"12a"` → `"1"`)
//!      falls out of "longest accept where the guard holds", with no backtracking
//!      engine: a failed guard just rules that accept out and a shorter one wins.
//!
//! The guard is evaluated by a tiny anchored `regex-automata` match of `S_i` at the
//! accept position (EOF ⇒ `S_i` cannot match, so a negative guard holds and a positive
//! guard fails). The whole matcher is `regex-automata`-only — no `fancy-regex`.
//!
//! ## Greedy-body assumption
//!
//! "Longest accept where the guard holds" reproduces `fancy-regex`'s backtracking
//! **for greedy bodies** — the bundled `OP`/`DEC_NUMBER` and every generated
//! trailing terminal. A body whose leftmost-first match is a *proper prefix* of a
//! longer accept (an explicit `x|xy` alternation, or a lazy `*?`) is the one shape
//! this picks differently; none occur in the supported set, and the scanner
//! differential vs `fancy-regex` (`tests/test_scanner_differential.rs`) is the net
//! that would catch one if it were ever added.

use regex_automata::{
    dfa::{dense, Automaton, OverlappingState, StartKind},
    meta::Regex as MetaRegex,
    util::syntax,
    Anchored, Input, MatchKind,
};

use super::classify::{classify, ShapeClass, Verdict};
use super::{Look, Node};
use crate::error::GrammarError;

/// A per-branch trailing guard: the next char must (not) start a match of `re`.
struct Guard {
    re: MetaRegex,
    /// `true` for `(?!S)` (the char must **not** match `S`), `false` for `(?=S)`.
    neg: bool,
}

/// A lowered trailing-boundary terminal: one multi-pattern body DFA plus a parallel
/// per-branch guard table. Built once per terminal, queried per position.
pub struct LoweredTrailing {
    /// Anchored multi-pattern dense DFA over the branch bodies, `MatchKind::All` so an
    /// overlapping search yields every accept end of every branch. `PatternID i`
    /// indexes `guards[i]`.
    dfa: dense::DFA<Vec<u32>>,
    /// `PatternID` → optional trailing guard. `None` is a branch with no assertion.
    guards: Vec<Option<Guard>>,
}

/// Try to lower `pattern` (a terminal regex's inline source) as a trailing-boundary
/// terminal. Returns:
///
///   * `Ok(Some(matcher))` — every assertion is a top-level trailing boundary and the
///     lowering built; the caller drops the `fancy-regex` probe for this terminal.
///   * `Ok(None)` — not a (fully) trailing-boundary terminal, or the automaton could
///     not be built. The caller keeps the existing routing (`fancy-regex`) — the safe
///     fall-back, so an unhandled shape is never silently mis-lowered.
///   * `Err` — the front-end could not parse the pattern (the regex engines reject it
///     too).
///
/// `global_flags` is Lark's `g_regex_flags`; it is prepended to every body and guard
/// pattern so a case-insensitive grammar lowers case-insensitively, exactly as the
/// combined `regex` alternation carries the flag prefix.
pub fn build_trailing(
    pattern: &str,
    global_flags: u32,
) -> Result<Option<LoweredTrailing>, GrammarError> {
    // Gate: there must be at least one assertion and *every* assertion must classify
    // as a trailing boundary. A mixed terminal (a leading or lookbehind assertion too)
    // is left for those shapes' sessions — fall back to `fancy-regex`.
    let cls = classify(pattern)?;
    if cls.is_plain() {
        return Ok(None);
    }
    let all_trailing = cls
        .assertions
        .iter()
        .all(|a| a.verdict() == Verdict::Supported(ShapeClass::TrailingBoundary));
    if !all_trailing {
        return Ok(None);
    }

    let node = super::parse(pattern)?;
    let branches = top_level_branches(&node);
    let prefix = crate::lexer::global_flag_prefix(global_flags);

    let mut bodies: Vec<String> = Vec::new();
    let mut guards: Vec<Option<Guard>> = Vec::new();
    for b in &branches {
        let Some((body_src, guard)) = split_trailing(b) else {
            // A branch whose structure we don't recognize (an assertion not cleanly at
            // the branch end) — bail to the fall-back rather than guess.
            return Ok(None);
        };
        bodies.push(format!("{prefix}{body_src}"));
        match guard {
            Some((s, neg)) => {
                let re = match MetaRegex::new(&format!("{prefix}(?:{s})")) {
                    Ok(re) => re,
                    Err(_) => return Ok(None),
                };
                guards.push(Some(Guard { re, neg }));
            }
            None => guards.push(None),
        }
    }

    // The body DFA: anchored start, `MatchKind::All` for the overlapping accept-set
    // enumeration. `utf8(false)` lets an empty (nullable-body) match be reported at a
    // byte position without the UTF-8 empty-split handling, and keeps the DFA byte
    // oriented like the rest of the scanner.
    let dfa = match dense::Builder::new()
        .configure(
            dense::Config::new()
                .match_kind(MatchKind::All)
                .start_kind(StartKind::Anchored),
        )
        .syntax(syntax::Config::new().utf8(false))
        .build_many(&bodies)
    {
        Ok(dfa) => dfa,
        Err(_) => return Ok(None),
    };

    Ok(Some(LoweredTrailing { dfa, guards }))
}

impl LoweredTrailing {
    /// The **raw** lowered match: the end byte offset of the winning match beginning
    /// exactly at `pos`, or `None` if no branch matches. A zero-width match (a nullable
    /// body) is reported as `Some(pos)` — the scanner applies its own non-empty rule on
    /// top, but the terminal-level equivalence oracle compares this raw length against
    /// `fancy-regex`, which also reports the empty match.
    pub fn match_end_at(&self, text: &str, pos: usize) -> Option<usize> {
        // Longest guard-satisfying accept end per branch (in `PatternID` / branch order).
        let mut best_per: Vec<Option<usize>> = vec![None; self.guards.len()];
        let input = Input::new(text)
            .span(pos..text.len())
            .anchored(Anchored::Yes);
        let mut state = OverlappingState::start();
        loop {
            // The DFA is built from in-memory patterns and the input is in range, so a
            // search error is not expected; treat one as "no match" defensively.
            if self
                .dfa
                .try_search_overlapping_fwd(&input, &mut state)
                .is_err()
            {
                return None;
            }
            let Some(hm) = state.get_match() else { break };
            let pid = hm.pattern().as_usize();
            let end = hm.offset();
            if self.guard_holds(pid, text, end) {
                let slot = &mut best_per[pid];
                if slot.map_or(true, |e| end > e) {
                    *slot = Some(end);
                }
            }
        }
        // Leftmost-first across branches: the lowest-index branch with a satisfying
        // accept wins, using that branch's (greedy, longest) end.
        best_per.into_iter().flatten().next()
    }

    /// Whether branch `pid`'s trailing guard holds at byte offset `end`.
    fn guard_holds(&self, pid: usize, text: &str, end: usize) -> bool {
        match &self.guards[pid] {
            None => true,
            Some(g) => {
                let gi = Input::new(text)
                    .span(end..text.len())
                    .anchored(Anchored::Yes);
                let matched = g.re.find(gi).is_some();
                if g.neg {
                    !matched
                } else {
                    matched
                }
            }
        }
    }

    /// Matched-prefix length in **characters** at the start of `text` (offset 0), or
    /// `None` for no match — the terminal-level equivalence oracle's view (allows the
    /// zero-width nullable-body match, like `fancy-regex`).
    pub fn match_len_chars(&self, text: &str) -> Option<usize> {
        let end = self.match_end_at(text, 0)?;
        Some(text[..end].chars().count())
    }
}

/// The top-level alternation branches of a parsed terminal: an `Alt`'s branches, or
/// the whole node as a single branch.
fn top_level_branches(node: &Node) -> Vec<&Node> {
    match node {
        Node::Alt(branches) => branches.iter().collect(),
        other => vec![other],
    }
}

/// Split a branch into its plain body source and an optional trailing guard
/// `(S_source, neg)`. Returns `None` if the branch holds an assertion anywhere other
/// than cleanly at its end (so the caller bails to the `fancy-regex` fall-back).
fn split_trailing(branch: &Node) -> Option<(String, Option<(String, bool)>)> {
    match branch {
        // A bare trailing assertion: `(?![1-9])` — empty body, a guard.
        Node::Assertion {
            neg,
            look: Look::Ahead,
            body,
            quant,
        } if quant.is_empty() => Some((String::new(), Some((body.to_source(), *neg)))),
        Node::Concat(parts) => {
            // The last element may be the trailing assertion; no other element may be.
            let n = parts.len();
            let (last, head) = parts.split_last()?;
            if head.iter().any(Node::has_assertion) {
                return None;
            }
            let body_src: String = head.iter().map(Node::to_source).collect();
            match last {
                Node::Assertion {
                    neg,
                    look: Look::Ahead,
                    body,
                    quant,
                } if quant.is_empty() => Some((body_src, Some((body.to_source(), *neg)))),
                // Last element is not an assertion → the whole branch is the body, but
                // only if it carries no assertion at all.
                other if !other.has_assertion() => {
                    let _ = n;
                    Some((branch.to_source(), None))
                }
                _ => None,
            }
        }
        // A plain branch (atom / group) with no assertion at all.
        other if !other.has_assertion() => Some((other.to_source(), None)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Matched-prefix length in characters at offset 0, asserting the pattern lowers.
    fn m(pattern: &str, input: &str) -> Option<usize> {
        build_trailing(pattern, 0)
            .unwrap()
            .unwrap_or_else(|| panic!("{pattern:?} should lower as trailing"))
            .match_len_chars(input)
    }

    #[test]
    fn bundled_op_and_dec_number_guards() {
        // lark OP's `[?](?![a-z])`.
        assert_eq!(m("[?](?![a-z])", "?"), Some(1));
        assert_eq!(m("[?](?![a-z])", "?a"), None);
        // DEC_NUMBER's leading-zero guard.
        assert_eq!(m("0(?![1-9])", "0"), Some(1));
        assert_eq!(m("0(?![1-9])", "00"), Some(1));
        assert_eq!(m("0(?![1-9])", "01"), None);
    }

    #[test]
    fn length_changing_backtrack_falls_out_of_longest_guarded_accept() {
        // `[0-9]+(?![a-z])` on "12a": the greedy "12" fails the guard (next is 'a'),
        // so the shorter "1" (next is '2', a digit) wins — no backtracking engine.
        assert_eq!(m("[0-9]+(?![a-z])", "12a"), Some(1));
        assert_eq!(m("[0-9]+(?![a-z])", "12"), Some(2));
        assert_eq!(m("[0-9]+(?![a-z])", "1a"), None);
    }

    #[test]
    fn nullable_body_reports_zero_width_like_fancy_regex() {
        // `[0-9]*(?![0-9])` matches the empty string when the next char is not a digit.
        assert_eq!(m("[0-9]*(?![0-9])", "a"), Some(0));
        assert_eq!(m("[0-9]*(?![0-9])", "12a"), Some(2));
    }

    #[test]
    fn positive_lookahead_fails_at_eof() {
        // `(?=S)` needs a following char in S; end-of-input has none.
        assert_eq!(m("[0-9]+(?=[a-z])", "12"), None);
        assert_eq!(m("[0-9]+(?=[a-z])", "12a"), Some(2));
    }

    #[test]
    fn alternation_branch_with_guard_is_leftmost_first() {
        // `[+*]|[?](?![a-z])`: the unguarded branch and the guarded branch coexist.
        assert_eq!(m("[+*]|[?](?![a-z])", "+"), Some(1));
        assert_eq!(m("[+*]|[?](?![a-z])", "?"), Some(1));
        assert_eq!(m("[+*]|[?](?![a-z])", "?a"), None);
    }

    #[test]
    fn delimiter_and_utf8_boundary_guards() {
        // A closed string not followed by another quote.
        assert_eq!(m(r#""[^"]*"(?!")"#, r#""""#), Some(2));
        assert_eq!(m(r#""[^"]*"(?!")"#, r#"""""#), None);
        // A guard adjacent to a multi-byte char (byte-level DFA, char-level terminal).
        assert_eq!(m("é(?![a-z])", "é"), Some(1));
        assert_eq!(m("é(?![a-z])", "éx"), None);
    }

    #[test]
    fn non_trailing_shapes_do_not_lower_here() {
        // Leading boundary, bounded lookbehind, and plain terminals all return None
        // (the caller keeps their existing routing).
        assert!(build_trailing("(?!--)[a-z]+", 0).unwrap().is_none());
        assert!(build_trailing("(?<!_)/", 0).unwrap().is_none());
        assert!(build_trailing("[a-z]+", 0).unwrap().is_none());
        // A terminal mixing a trailing guard with a leading one is not yet lowerable.
        assert!(build_trailing("(?!x)[a-z]+(?![0-9])", 0).unwrap().is_none());
    }

    #[test]
    fn global_ignorecase_flag_is_threaded_through() {
        // With IGNORECASE the body and the guard both fold case.
        use crate::grammar::terminal::flags;
        let lowered = build_trailing("[a-z]+(?![a-z])", flags::IGNORECASE)
            .unwrap()
            .expect("lowers");
        // "AB" is all letters, so the guard (?![a-z]) holds only at end-of-input.
        assert_eq!(lowered.match_len_chars("AB"), Some(2));
        assert_eq!(lowered.match_len_chars("ABc"), Some(3));
    }
}
