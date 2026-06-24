//! Construction-time terminal sanitization: the strict-mode regex-collision
//! check (issue #35) and the zero-width-terminal rejection.
//!
//! Python Lark delegates the collision check to `interegular`
//! (`lexer.py::_check_regex_collisions`): it groups the *regex* terminals by
//! priority, compiles each to an FSM, and reports a collision when two
//! same-priority regexes can match a common string — raising a `LexError` under
//! `strict=True` (a warning otherwise).
//!
//! The `regex` crate offers no intersection/emptiness test, so we drop to its
//! `regex-automata` layer. Each terminal's regex is compiled to a **whole-match**
//! DFA (anchored at the start; acceptance is checked only at the end-of-input
//! transition, so the DFA accepts exactly the strings the terminal matches in
//! full). Two terminals collide iff the *product* of their DFAs has a reachable
//! state that is accepting in both — classic product-construction emptiness. A BFS
//! over byte-labelled state pairs both decides emptiness and yields the shortest
//! witness string, which we surface in the error the way interegular surfaces its
//! example overlap.
//!
//! We only ever act in strict mode (Lark's non-strict path just logs a warning,
//! and lark-rs has no warning channel), so this never runs on the default build
//! path — there is zero cost unless the user opts into `strict=True`.

use std::collections::{HashMap, HashSet, VecDeque};

use regex::Regex;
use regex_automata::{
    dfa::{dense, Automaton, StartKind},
    util::primitives::StateID,
    Anchored, Input,
};

use super::plan::global_flag_prefix;
use super::LexerConf;
use crate::error::GrammarError;
use crate::grammar::intern::SymbolId;
use crate::grammar::terminal::{Pattern, TerminalDef};

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
        let mut by_priority: HashMap<i64, Vec<(SymbolId, &TerminalDef)>> = HashMap::new();
        for (id, t) in res {
            by_priority.entry(t.priority).or_default().push((id, t));
        }

        let mut priorities: Vec<i64> = by_priority.keys().copied().collect();
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
