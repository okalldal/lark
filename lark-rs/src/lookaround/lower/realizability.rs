//! Guard-realizability analysis for boundary lowering — the syntactic and
//! semantic gates that decide whether a guarded branch's base is reproducible by
//! the driver's longest-accept accumulator. Moved verbatim from the former single-file
//! `lower.rs` (issue #478 submodule split); behavior unchanged.

use super::*;

/// Whether a guarded branch's base is **greedy-monotone**: its leftmost-first match
/// always equals its longest match, so the driver's "longest accept where the guard
/// holds" coincides with Python's backtracking result. True for a base with no
/// alternation and no lazy/possessive quantifier. Conservative — the caller treats a
/// `false` as "route to fancy."
pub(super) fn is_greedy_monotone(base: &Node) -> bool {
    !node_has_alt(base) && !node_has_lazy(base)
}

pub(super) fn node_has_alt(n: &Node) -> bool {
    match n {
        Node::Alt(_) => true,
        Node::Concat(parts) => parts.iter().any(node_has_alt),
        Node::Group { body, .. } => node_has_alt(body),
        Node::Atom(_) | Node::Assertion { .. } => false,
    }
}

pub(super) fn node_has_lazy(n: &Node) -> bool {
    match n {
        Node::Atom(s) => atom_has_lazy(s),
        Node::Concat(parts) | Node::Alt(parts) => parts.iter().any(node_has_lazy),
        Node::Group { body, quant, .. } => quant.ends_with('?') || node_has_lazy(body),
        Node::Assertion { .. } => false,
    }
}

/// A lazy/possessive quantifier in a flat atom run: a `*` / `+` / `?` / `}` followed
/// by `?` (lazy) or `+` (possessive). Over-approximates (the safe direction).
pub(super) fn atom_has_lazy(atom: &str) -> bool {
    let lazy = ["*?", "+?", "??", "}?"];
    let possessive = ["*+", "++", "?+", "}+"];
    lazy.iter()
        .chain(possessive.iter())
        .any(|m| atom.contains(m))
}

/// Whether a guarded branch's base regex is **guard-realizable** — its leftmost-first
/// match priority is descending by length, so the driver's "longest accept where the
/// guard holds" accumulator coincides with Python's backtracking leftmost-first result.
/// Two independently-sufficient conditions, both decidable:
///
///   * **Greedy-monotone** ([`is_greedy_monotone`]) — no alternation, no lazy/possessive
///     quantifier, so the (single) greedy match is the longest and is tried first. Covers
///     `[0-9]+(?![0-9])`-style bases.
///   * **Prefix-free** ([`is_prefix_free`]) — at most one match length at any start
///     position, so there is a single candidate and "longest where guard holds" is
///     trivially that candidate. Covers a base with a bounded alternation prefix over a
///     fixed literal (`python.STRING`'s empty-arm base `([ubf]?r?|r[ubf])""`), which is
///     *not* greedy-monotone (it has alternation) yet is unambiguous in length because the
///     fixed `""` suffix immediately following pins the prefix length.
///
/// Conservative: a base meeting neither (nor the exact [`is_leftmost_longest`] decision)
/// is declined — since L4 a categorized NotYetImplemented build error. `dotall`
/// is the terminal's `s` flag — it changes what `.` matches and so the base's language,
/// so the prefix-free check must evaluate the base under the same flag the engine wraps.
pub(super) fn is_guard_realizable(base: &str, dotall: bool) -> bool {
    // The greedy-monotone test works on the parsed tree (it predates this routine), so
    // re-parse the base; on a parse failure fall back to "not realizable" (decline).
    match super::super::parse(base) {
        Ok(node) if is_greedy_monotone(&node) => true,
        _ => is_prefix_free(base, dotall) || is_leftmost_longest(base, dotall),
    }
}

/// **The exact (semantic) realizability decision**: whether the base's leftmost-first
/// match length equals its longest match length **on every input** — which is verbatim
/// the property the accumulator needs ("its leftmost-first match is always its
/// longest"), decided on the automata instead of approximated from the syntax. The two
/// syntactic fast paths above ([`is_greedy_monotone`], [`is_prefix_free`]) are sound
/// but incomplete: the bundled `python.DEC_NUMBER`'s guarded arm base `0(?:(?:_)?0)*`
/// fails both (it has an optional group, and `"0"` is a prefix of `"00"`), yet its
/// all-greedy preference order *is* descending by length — this check proves it.
///
/// **Decision procedure.** Build two anchored dense DFAs over the same base: `L`
/// (`MatchKind::LeftmostFirst` — the backtracking-preference result, the same
/// semantics the plain engine runs) and `A` (`MatchKind::All` — every accept length).
/// Walk their product from the anchored start over one representative byte per
/// *joint* byte-class (plus the EOI transition). For any input `w`, the leftmost-first
/// engine's report is the **deepest `L`-match state** along `w`'s walk, and the
/// longest accept is the **deepest `A`-match state**; `L`'s match is always one of
/// `A`'s accepts, so the two lengths are equal for every input **iff no reachable
/// product state has `A` matching where `L` does not** (such a state, taken as the end
/// of the input, witnesses `longest > leftmost-first`; conversely if every `A`-match
/// state is an `L`-match state, the deepest accepts coincide on every walk). Both DFAs
/// delay their match flag by one transition equally, so the per-state comparison is
/// depth-aligned by construction.
///
/// **Flags.** `dotall` is wrapped exactly. Unlike `is_prefix_free`'s one-directional
/// `(?i)` argument, leftmost-longest is **not** monotone under language enlargement in
/// either direction, so the check must pass for the base **both** bare and
/// `(?i)`-wrapped — whichever wrap the engine actually applies is then covered.
/// A nullable base, a compile/size-limit failure, or a quit state declines
/// (conservative).
pub(super) fn is_leftmost_longest(base: &str, dotall: bool) -> bool {
    let s = if dotall {
        format!("(?s:{base})")
    } else {
        base.to_string()
    };
    leftmost_longest_one(&s) && leftmost_longest_one(&format!("(?i:{s})"))
}

pub(super) fn leftmost_longest_one(base: &str) -> bool {
    use regex_automata::dfa::{dense, Automaton, StartKind};
    use regex_automata::util::primitives::StateID;
    use regex_automata::{Anchored, Input, MatchKind};

    const SIZE_LIMIT: usize = 10 * (1 << 20);
    let build = |kind: MatchKind| -> Option<dense::DFA<Vec<u32>>> {
        dense::Builder::new()
            .configure(
                dense::Config::new()
                    .match_kind(kind)
                    .start_kind(StartKind::Anchored)
                    .dfa_size_limit(Some(SIZE_LIMIT))
                    .determinize_size_limit(Some(SIZE_LIMIT)),
            )
            .build(base)
            .ok()
    };
    let (Some(l), Some(a)) = (build(MatchKind::LeftmostFirst), build(MatchKind::All)) else {
        return false;
    };
    let anchored_start = |dfa: &dense::DFA<Vec<u32>>| -> Option<StateID> {
        dfa.start_state_forward(&Input::new("").anchored(Anchored::Yes))
            .ok()
    };
    let (Some(ls), Some(as_)) = (anchored_start(&l), anchored_start(&a)) else {
        return false;
    };
    // A nullable base: decline conservatively, mirroring `is_prefix_free` (the lexer
    // forbids zero-width matches, and the accumulator's interplay with a nullable
    // base has no audited equivalence argument).
    if a.is_match_state(a.next_eoi_state(as_)) {
        return false;
    }
    // One representative byte per *joint* (L, A) byte-equivalence class.
    let reps: Vec<u8> = {
        let (cl, ca) = (l.byte_classes(), a.byte_classes());
        let mut seen = std::collections::HashSet::new();
        let mut v = Vec::new();
        for byte in 0u8..=0xFF {
            if seen.insert((cl.get(byte), ca.get(byte))) {
                v.push(byte);
            }
        }
        v
    };

    let mut seen = std::collections::HashSet::new();
    let mut stack = vec![(ls, as_)];
    seen.insert((ls, as_));
    while let Some((sl, sa)) = stack.pop() {
        if l.is_quit_state(sl) || a.is_quit_state(sa) {
            return false; // no equivalence argument through a quit — decline
        }
        if a.is_match_state(sa) && !l.is_match_state(sl) {
            return false; // a longer accept the leftmost-first engine won't report
        }
        if l.is_dead_state(sl) && a.is_dead_state(sa) {
            continue;
        }
        let mut push = |nl: StateID, na: StateID| {
            if seen.insert((nl, na)) {
                stack.push((nl, na));
            }
        };
        for &b in &reps {
            push(l.next_state(sl, b), a.next_state(sa, b));
        }
        push(l.next_eoi_state(sl), a.next_eoi_state(sa));
    }
    true
}

/// Whether the anchored language of `base` is **prefix-free**: no string it matches is a
/// proper prefix of another string it matches. Equivalently, at most one match length at
/// each start position. Decided over the anchored all-matches dense DFA: from every match
/// state, no match state may be reachable on a non-empty path (a reachable match state
/// would witness a string in `L` that extends a shorter one in `L`). Bytes are explored
/// one representative per equivalence class plus the EOI transition — sound because bytes
/// in one class are indistinguishable to the automaton.
///
/// Two safety guards beyond the reachability scan:
///   * **Nullability** — a base that matches the empty string is *not* prefix-free (`""`
///     is a prefix of every non-empty match), but the empty match's match-state is the
///     EOI state, which has no outgoing transitions, so the reachability scan alone would
///     miss it. We detect nullability explicitly (start → EOI is a match) and decline.
///     (This is the gate's own invariant, not a lean on the driver's separate zero-width
///     reject.)
///   * **Determinization size limits** — a pathological base declines (build error →
///     `false`) instead of blowing up the dense build, the L5 bake target.
///
/// A build/representation failure returns `false` (the conservative, decline-to-fancy
/// direction).
///
/// **Flags.** The engine wraps each lowered branch in the terminal's flags, so the
/// decided language must reflect them or the gate could false-accept:
///   * `dotall` wraps `(?s:…)` exactly (the actual flag) so `.` matches a newline as the
///     engine's wrap would.
///   * `(?i)` is applied **unconditionally** — case-folding can introduce a *new* prefix
///     relation among alternation arms (`(a|Add)dd` is prefix-free case-sensitively but
///     not under `/i`), and a guarded base lowered without seeing that would mis-pick its
///     length. Wrapping `(?i)` is sound for *both* a case-sensitive and a case-insensitive
///     terminal: case-folding only *enlarges* the language, and a subset of a prefix-free
///     language is prefix-free, so this never false-accepts (at worst it over-declines a
///     case-sensitive letter-alternation base to `fancy-regex` — the safe direction). The
///     check is built with the same `regex-automata` engine the lexer uses, so whatever
///     case-folding the runtime applies (length-preserving simple folding today, or any
///     future change) is reflected exactly.
pub(super) fn is_prefix_free(base: &str, dotall: bool) -> bool {
    use regex_automata::dfa::{dense, Automaton, StartKind};
    use regex_automata::util::primitives::StateID;
    use regex_automata::{Anchored, Input, MatchKind};

    // Decide the base under the engine's flag-wrap: DOTALL exactly, IGNORECASE
    // conservatively (see the doc above).
    let wrapped = if dotall {
        format!("(?si:{base})")
    } else {
        format!("(?i:{base})")
    };
    // ~10 MiB determinization budget: ample for any real terminal base, but a
    // pathological one errors out → decline rather than blow up the bake target.
    const SIZE_LIMIT: usize = 10 * (1 << 20);
    let Ok(dfa) = dense::Builder::new()
        .configure(
            dense::Config::new()
                .match_kind(MatchKind::All)
                .start_kind(StartKind::Anchored)
                .dfa_size_limit(Some(SIZE_LIMIT))
                .determinize_size_limit(Some(SIZE_LIMIT)),
        )
        .build(&wrapped)
    else {
        return false;
    };
    let Ok(start) = dfa.start_state_forward(&Input::new("").anchored(Anchored::Yes)) else {
        return false;
    };
    // Nullable base → empty match is a prefix of any non-empty match → not prefix-free.
    // The empty match's match-state is the EOI state (no outgoing edges), so the
    // reachability scan below would miss it; detect epsilon-membership explicitly via the
    // `regex` crate (an independent engine — `find("")` matches at 0..0 iff the language
    // contains the empty string). A compile failure (shouldn't happen — the dense DFA
    // built) is treated as nullable → decline, the conservative direction.
    let nullable = match regex::Regex::new(&wrapped) {
        Ok(re) => re.find("").is_some(), // matches the empty haystack ⇒ ε ∈ L
        Err(_) => true,                  // shouldn't happen; decline conservatively
    };
    if nullable {
        return false;
    }
    let classes = dfa.byte_classes();
    let reps: Vec<u8> = {
        let mut seen = std::collections::HashSet::new();
        let mut v = Vec::new();
        for byte in 0u8..=0xFF {
            if seen.insert(classes.get(byte)) {
                v.push(byte);
            }
        }
        v
    };

    // Successor states of `s` over every byte-class representative + the EOI transition.
    let succ = |s: StateID| -> Vec<StateID> {
        let mut out: Vec<StateID> = reps.iter().map(|&b| dfa.next_state(s, b)).collect();
        out.push(dfa.next_eoi_state(s));
        out
    };
    // From `from`, is any match state reachable in >= 1 transition?
    let reaches_match = |from: StateID| -> bool {
        let mut seen = std::collections::HashSet::new();
        let mut stack: Vec<StateID> = Vec::new();
        for ns in succ(from) {
            if seen.insert(ns) {
                stack.push(ns);
            }
        }
        while let Some(s) = stack.pop() {
            if dfa.is_match_state(s) {
                return true;
            }
            if dfa.is_dead_state(s) {
                continue;
            }
            for ns in succ(s) {
                if seen.insert(ns) {
                    stack.push(ns);
                }
            }
        }
        false
    };

    let mut seen = std::collections::HashSet::new();
    let mut stack = vec![start];
    seen.insert(start);
    while let Some(s) = stack.pop() {
        if dfa.is_match_state(s) && reaches_match(s) {
            return false; // a match extends another match → not prefix-free
        }
        if dfa.is_dead_state(s) {
            continue;
        }
        for ns in succ(s) {
            if seen.insert(ns) {
                stack.push(ns);
            }
        }
    }
    true
}
