//! Strict-mode regex-terminal collision detection (Python Lark's `interegular`
//! check, `lexer.py::_check_regex_collisions`).
//!
//! Under `strict=True`, Lark rejects a grammar in which two same-priority *regex*
//! terminals can both fully match a common string — the lexer would otherwise have
//! to pick between them arbitrarily. Python delegates this to the `interegular`
//! library: regex → FSM → product construction → intersection-emptiness, with a
//! concrete example of the overlap.
//!
//! lark-rs has no FSM layer of its own, so we reuse `regex-automata`'s dense DFAs
//! (already a transitive dependency of the `regex` crate) and step two of them in
//! lock-step over the product automaton. A both-ends-anchored (full-string) match
//! in both DFAs at the same input position witnesses a collision.
//!
//! Only `strict` grammars ever run this; the common (non-strict) path is untouched.

use regex_automata::{
    dfa::{dense, Automaton, StartKind},
    util::primitives::StateID,
    Anchored, Input,
};
use std::collections::{HashMap, VecDeque};

use crate::error::GrammarError;
use crate::grammar::terminal::{Pattern, TerminalDef};

/// Upper bound on product states we explore before giving up. Bounds pathological
/// blow-ups (e.g. two large bounded-repetition regexes). Exhausting it returns
/// "no collision" — strictly safer to *under*-report than to over-reject a valid
/// grammar (the project's stated discipline; see COMPLIANCE_PARITY.md M7b).
const MAX_VISITED: usize = 200_000;

/// `Some(example)` if the two regexes share a common fully-matched (anchored at
/// both ends), non-empty string; `None` if their intersection is empty. The
/// example is the shortest such string (BFS over the product automaton).
fn regex_intersection_example(
    re_a: &str,
    re_b: &str,
) -> Result<Option<Vec<u8>>, dense::BuildError> {
    // Anchored start keeps each DFA's accepted language to strings matched in
    // their *entirety* (no implicit leading `.*?`), and keeps the DFAs small.
    let cfg = dense::Config::new().start_kind(StartKind::Anchored);
    let dfa_a = dense::Builder::new().configure(cfg.clone()).build(re_a)?;
    let dfa_b = dense::Builder::new().configure(cfg).build(re_b)?;

    let input = Input::new(b"").anchored(Anchored::Yes);
    let (start_a, start_b) = match (
        dfa_a.start_state_forward(&input),
        dfa_b.start_state_forward(&input),
    ) {
        (Ok(a), Ok(b)) => (a, b),
        _ => return Ok(None),
    };

    // Reduced alphabet: the union of both DFAs' byte-class representatives. A
    // product transition can only change behaviour on a class boundary of one of
    // the two DFAs, so the union is both sound and complete — far cheaper than
    // iterating all 256 byte values per state.
    let mut alphabet: Vec<u8> = Vec::new();
    let mut seen = [false; 256];
    for dfa in [&dfa_a, &dfa_b] {
        for unit in dfa.byte_classes().representatives(0u8..=255u8) {
            if let Some(b) = unit.as_u8() {
                if !seen[b as usize] {
                    seen[b as usize] = true;
                    alphabet.push(b);
                }
            }
        }
    }

    type Key = (StateID, StateID);

    // A dead state can never reach a match; a quit state means the DFA gave up
    // (e.g. a Unicode word boundary). Pruning a quit state is conservative — it
    // could in principle hide an overlap, so we under-report rather than guess.
    let is_live =
        |dfa: &dense::DFA<Vec<u32>>, s: StateID| !dfa.is_dead_state(s) && !dfa.is_quit_state(s);
    if !is_live(&dfa_a, start_a) || !is_live(&dfa_b, start_b) {
        return Ok(None);
    }

    let root: Key = (start_a, start_b);
    let mut prev: HashMap<Key, Option<(Key, u8)>> = HashMap::new();
    prev.insert(root, None);
    let mut queue: VecDeque<Key> = VecDeque::new();
    queue.push_back(root);

    // Full-string match: feeding end-of-input from this state lands in a match
    // state in *both* DFAs. This is language equivalence (the set of strings each
    // regex matches whole), not leftmost-search — exactly interegular's test.
    let accepting = |a: StateID, b: StateID| {
        dfa_a.is_match_state(dfa_a.next_eoi_state(a))
            && dfa_b.is_match_state(dfa_b.next_eoi_state(b))
    };
    let reconstruct = |prev: &HashMap<Key, Option<(Key, u8)>>, mut k: Key| {
        let mut bytes = Vec::new();
        while let Some(Some((pk, byte))) = prev.get(&k) {
            bytes.push(*byte);
            k = *pk;
        }
        bytes.reverse();
        bytes
    };

    while let Some(cur @ (sa, sb)) = queue.pop_front() {
        if prev.len() > MAX_VISITED {
            return Ok(None);
        }
        // We never accept the root (empty string): acceptance is only tested on a
        // state reached via >= 1 byte. Callers only pass terminals with min-width
        // >= 1, so an empty overlap is meaningless anyway.
        for &byte in &alphabet {
            let na = dfa_a.next_state(sa, byte);
            let nb = dfa_b.next_state(sb, byte);
            if !is_live(&dfa_a, na) || !is_live(&dfa_b, nb) {
                continue;
            }
            let nk = (na, nb);
            if prev.contains_key(&nk) {
                continue;
            }
            prev.insert(nk, Some((cur, byte)));
            if accepting(na, nb) {
                return Ok(Some(reconstruct(&prev, nk)));
            }
            queue.push_back(nk);
        }
    }

    Ok(None)
}

/// Strict-mode regex collision check over a grammar's terminals. Errors on the
/// first same-priority *regex*-terminal pair that share a fully-matched string.
///
/// Mirrors `lexer.py::_check_regex_collisions`: string terminals are excluded,
/// terminals are grouped by priority, and only same-priority pairs are compared.
pub fn check_regex_collisions(terminals: &[TerminalDef]) -> Result<(), GrammarError> {
    // Regex terminals only, paired with their inline regex (keeps scoped flags
    // like `(?i:…)`). String/literal terminals are not part of this check.
    let regex_terms: Vec<(&TerminalDef, String)> = terminals
        .iter()
        .filter(|t| matches!(t.pattern, Pattern::Re(_)))
        .map(|t| (t, t.pattern.to_inline_regex()))
        .collect();

    // Pairwise within equal priority, in terminal order for a deterministic
    // message. The grammars that reach here are tiny (strict mode is opt-in), so
    // an O(n²) scan over same-priority pairs is fine.
    for i in 0..regex_terms.len() {
        let (ti, ri) = &regex_terms[i];
        for (tj, rj) in regex_terms.iter().skip(i + 1) {
            if ti.priority != tj.priority {
                continue;
            }
            let overlap = regex_intersection_example(ri, rj).map_err(|e| GrammarError::Other {
                msg: format!("could not build DFA for collision check: {e}"),
            })?;
            if let Some(example) = overlap {
                let example = String::from_utf8_lossy(&example);
                return Err(GrammarError::Collision {
                    report: format!(
                        "Collision between Terminals {} and {}. Both match {:?}. [strict-mode]",
                        ti.name, tj.name, example
                    ),
                });
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlapping_regexes_report_an_example() {
        // /e?rez/ matches {rez, erez}; /erez?/ matches {erez, ere}. Overlap: erez.
        let ex = regex_intersection_example("e?rez", "erez?").unwrap();
        assert_eq!(ex.as_deref(), Some(&b"erez"[..]));
    }

    #[test]
    fn disjoint_regexes_have_no_overlap() {
        assert_eq!(regex_intersection_example("abc", "xyz").unwrap(), None);
        // Prefix vs longer string: {a} vs {ab} share no full match.
        assert_eq!(regex_intersection_example("a", "ab").unwrap(), None);
    }
}
