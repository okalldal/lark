//! Bounded-lookaround **lowering** — turning a classified terminal regex into
//! lookaround-free sub-patterns the combined DFA can host
//! (`docs/LEXER_DFA_PLAN.md`, "How the lowering works").
//!
//! The classifier ([`super::classify`]) decides *whether* a terminal's assertions
//! are a supported shape; this module performs the actual transform once it is.
//! The boundary shapes lower to a **guarded accept** on a stripped base pattern:
//!
//!   * **Trailing boundary** (`X(?!S)` / `X(?=S)`, M1) → the base `X` becomes an
//!     ordinary sub-pattern and the assertion is stripped into a *trailing* guard
//!     ("this accept is valid only if the next chars do/don't match `S`"). The driver
//!     consults it at the accept position — the lookahead char, which belongs to the
//!     *next* token, is never consumed.
//!   * **Leading boundary** (`(?!S)X` / `(?=S)X`, M2) → the base `X` becomes an
//!     ordinary sub-pattern and the assertion is stripped into a *leading* guard,
//!     which the driver checks once at the match **start** (`pos`): a fixed
//!     precondition on the bytes the match begins with. (This covers a leading guard
//!     at a top-level alternation-branch start; a guard *after a variable-width
//!     prefix* — `python.STRING`'s `(?!"")` after the `([ubf]?r?|r[ubf])"` prefix —
//!     is **not** a fixed-position guard and is left to the nested-splice lowering.)
//!
//! A terminal's top-level alternation is split **per branch** ([`LoweredBranch`]):
//! one branch may carry a guard while a sibling does not (the bundled `lark.OP` =
//! `[+*]|[?](?![a-z])` is exactly this — `[+*]` is unguarded, `[?]` is guarded).
//! Splitting into per-branch sub-patterns is what lets the driver attach a guard to
//! the *accepting path*, not to the whole terminal — applying `OP`'s `(?![a-z])` to
//! the `[+*]` branch would wrongly reject `+a`. A branch may carry **both** a leading
//! and a trailing guard (`(?!a)X(?!b)`).
//!
//!   * **Bounded lookbehind** (`(?<!S)` / `(?<=S)`, M3) → a *backward* guard checked at
//!     a **fixed char-offset** from the match start (`docs/LEXER_DFA_PLAN.md`, "carry
//!     the window forward in the state" — here the window is read directly from the
//!     haystack at the fixed offset, which the driver knows because the offset never
//!     changes with the match length). The lookbehind assertion is stripped into a
//!     [`LookbehindGuard`] carrying its char-offset and the body's width window; the
//!     driver checks the ≤`W` chars ending at that offset against `S`. The offset is
//!     fixed only when every base element *before* the lookbehind is fixed-width — a
//!     lookbehind after a variable-width prefix (`python.LONG_STRING`'s
//!     `.*?(?<!\\)`) has no fixed offset and is **declined** here (routed to
//!     `fancy-regex`), the reject-when-unsure direction; the variable-offset
//!     window-carry is a later milestone.

use super::{Look, Node};
use crate::error::GrammarError;

/// One lowered top-level alternation branch of a terminal: a lookaround-free base
/// regex plus optional leading / trailing guards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoweredBranch {
    /// The branch's base regex with its boundary assertion(s) stripped — a plain
    /// regular language the NFA builder can compile directly.
    pub regex: String,
    /// A guard lifted off the *front* of the branch, checked at the match start.
    pub leading: Option<GuardSpec>,
    /// A guard lifted off the *end* of the branch, checked at the match end.
    pub trailing: Option<GuardSpec>,
    /// Bounded-lookbehind guards lifted out of the branch, each at a fixed char-offset
    /// from the match start. Empty for a branch with no lookbehind. M3.
    pub lookbehind: Vec<LookbehindGuard>,
}

/// A boundary guard: the maximal-munch driver records an accept of this branch only
/// when the guard holds at the relevant position (start for leading, end for
/// trailing).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuardSpec {
    /// `true` for `(?!S)` (the chars must **not** match `S`), `false` for `(?=S)`
    /// (the chars **must** match `S`).
    pub neg: bool,
    /// The assertion body `S` as a lookaround-free regex, matched anchored at the
    /// guard position.
    pub set: String,
}

/// A bounded-lookbehind guard lifted out of a branch. The driver checks the ≤`width`
/// chars ending at byte offset `pos + (offset_chars chars)` against the body `set`:
/// for `(?<!S)` (`neg`) the window must **not** match `S`; for `(?<=S)` it must. The
/// offset is in *characters* from the match start and is fixed (the lowering declines
/// any lookbehind whose preceding prefix is variable-width), so the driver evaluates
/// it once as a precondition, independent of how long the base matches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LookbehindGuard {
    /// `true` for `(?<!S)` (the window must **not** match `S`), `false` for `(?<=S)`.
    pub neg: bool,
    /// The assertion body `S` as a lookaround-free regex.
    pub set: String,
    /// Char offset from the match start to the lookbehind point.
    pub offset_chars: usize,
    /// Maximum width (in chars) of the body `S` — the size of the history window the
    /// driver inspects (it tries window lengths `1..=width`).
    pub width: usize,
}

/// Lower a **boundary** terminal pattern (every assertion a leading- or
/// trailing-boundary lookahead) into its per-branch sub-patterns. The caller (the
/// lowering entry point) has already classified the pattern as fully supported with
/// every assertion a [`LeadingBoundary`] or [`TrailingBoundary`], so here every
/// assertion encountered is a boundary lookahead at a branch's start or end.
///
/// [`LeadingBoundary`]: super::classify::ShapeClass::LeadingBoundary
/// [`TrailingBoundary`]: super::classify::ShapeClass::TrailingBoundary
pub fn lower_boundary(pattern: &str) -> Result<Vec<LoweredBranch>, GrammarError> {
    lower_boundary_dotall(pattern, false)
}

/// [`lower_boundary`] aware of whether the terminal's flags include `DOTALL` (`s`).
/// `dotall` only affects the **string-idiom** body normalization (whether `.` — and so
/// the normalized content class — admits a newline); the boundary/lookbehind shapes are
/// flag-agnostic at this layer (the engine wraps each branch in the terminal's flags).
///
/// The first move is the **string-literal opening-guard idiom splice** (`python.STRING`):
/// a leading `(?!"")` after a variable-width prefix + the opening quote is not a
/// fixed-offset guard, so it is not lowerable by the generic [`lower_branch`] boundary
/// path. [`recognize_string_idiom`] matches that exact shape and lowers it by normalizing
/// the lazy escaped body to its proven greedy character-class equivalent (the Type-A
/// rewrite `tests/test_lookaround.rs::matchlen` justifies) and reducing the `(?!"")`
/// splice to an empty/non-empty arm split with a trailing `(?!")` guard on the empty arm
/// (the only place the assertion's window over-reaches the matched token). Anything that
/// is not exactly that idiom falls through to the generic boundary lowering.
pub fn lower_boundary_dotall(
    pattern: &str,
    dotall: bool,
) -> Result<Vec<LoweredBranch>, GrammarError> {
    let node = super::parse(pattern)?;
    if let Some(idiom) = recognize_string_idiom(&node) {
        return idiom.lower(pattern, dotall);
    }
    let mut branches = Vec::new();
    match &node {
        Node::Alt(arms) => {
            for arm in arms {
                branches.push(lower_branch(pattern, arm, dotall)?);
            }
        }
        other => branches.push(lower_branch(pattern, other, dotall)?),
    }
    Ok(branches)
}

/// Backwards-compatible alias used by the trailing-only call sites and the harness.
pub fn lower_trailing(pattern: &str) -> Result<Vec<LoweredBranch>, GrammarError> {
    lower_boundary(pattern)
}

/// Lower one top-level alternation branch: peel a leading lookahead off the front and
/// a trailing lookahead off the end into forward guards, peel every interior
/// bounded-lookbehind into a fixed-offset backward guard; whatever remains is the base
/// regex.
fn lower_branch(pattern: &str, branch: &Node, dotall: bool) -> Result<LoweredBranch, GrammarError> {
    // Normalize the branch to a slice of concat parts so we can peel both ends.
    let parts: Vec<Node> = match branch {
        Node::Concat(parts) => parts.clone(),
        other => vec![other.clone()],
    };
    let mut lo = 0usize;
    let mut hi = parts.len();

    let mut leading = None;
    if let Some(Node::Assertion {
        neg,
        look: Look::Ahead,
        body,
        quant,
    }) = parts.first()
    {
        debug_assert!(quant.is_empty(), "quantified assertion reached lowering");
        leading = Some(GuardSpec {
            neg: *neg,
            set: body.to_source(),
        });
        lo += 1;
    }

    let mut trailing = None;
    if hi > lo {
        if let Some(Node::Assertion {
            neg,
            look: Look::Ahead,
            body,
            quant,
        }) = parts.get(hi - 1)
        {
            debug_assert!(quant.is_empty(), "quantified assertion reached lowering");
            trailing = Some(GuardSpec {
                neg: *neg,
                set: body.to_source(),
            });
            hi -= 1;
        }
    }

    // Walk the remaining middle parts, peeling each bounded lookbehind into a
    // fixed-offset backward guard. `offset` is the char count consumed from the match
    // start so far; it becomes `None` the moment a variable-width base element is seen,
    // and a lookbehind reached with `offset == None` has no fixed position → decline.
    let mut lookbehind: Vec<LookbehindGuard> = Vec::new();
    let mut base_parts: Vec<Node> = Vec::new();
    let mut offset: Option<usize> = Some(0);
    for p in &parts[lo..hi] {
        match p {
            Node::Assertion {
                neg,
                look: Look::Behind,
                body,
                quant,
            } => {
                if !quant.is_empty() {
                    return Err(decline(pattern, "the lookbehind carries a quantifier"));
                }
                let off = offset.ok_or_else(|| {
                    decline(
                        pattern,
                        "a bounded lookbehind sits after a variable-width prefix, so its \
                         offset from the match start is not fixed",
                    )
                })?;
                let w = max_width_chars(body).ok_or_else(|| {
                    decline(pattern, "the lookbehind body has no fixed maximum width")
                })?;
                if w == 0 {
                    return Err(decline(pattern, "the lookbehind body is zero-width"));
                }
                lookbehind.push(LookbehindGuard {
                    neg: *neg,
                    set: body.to_source(),
                    offset_chars: off,
                    width: w,
                });
            }
            // An interior *forward* lookahead is the priority-entangled case the
            // classifier rejects; it should never reach here, but decline defensively.
            Node::Assertion {
                look: Look::Ahead, ..
            } => return Err(decline(pattern, "an interior forward lookahead")),
            other => {
                offset = match offset {
                    Some(cur) => fixed_width_chars(other).map(|w| cur + w),
                    None => None,
                };
                base_parts.push(other.clone());
            }
        }
    }

    let base = Node::Concat(base_parts);
    let regex = base.to_source();
    if regex.is_empty() {
        // A branch that is *only* boundary assertions has an empty (nullable) base —
        // a zero-width terminal branch, which the lexer forbids.
        return Err(zero_width(pattern));
    }
    // If the base *still* carries a lookaround assertion, it was nested inside a group
    // (or behind a flag wrapper `(?s:…)` the loader bakes in — e.g. `python.LONG_STRING`
    // arrives as `(?s:…(?<!\\)…)`), so we could not peel it to a fixed offset. Decline
    // (route to fancy) rather than hand a lookaround-bearing base to the DFA builder,
    // which cannot parse it — the reject-when-unsure direction.
    if base.has_assertion() {
        return Err(decline(
            pattern,
            "a lookaround assertion is nested inside a group (or behind a flag \
             wrapper), so it cannot be peeled to a fixed offset",
        ));
    }
    // A guarded branch rides the driver's "longest accept where the guard holds"
    // accumulator, which only coincides with Python's backtracking result when the
    // base is **greedy-monotone** (its leftmost-first match is always its longest).
    // A base with an order-sensitive alternation (`ab|abc`) or a lazy/possessive
    // quantifier (`.*?`, `a*+`) can prefer a *shorter* match, so the accumulator would
    // mis-lower it. Decline here (reject-when-unsure) so the caller routes the whole
    // terminal to `fancy-regex` instead — correct, never mis-lowered. (A lookbehind
    // guard is a uniform precondition independent of the match length, but the base it
    // rides must still be greedy-monotone for the accumulator to pick Python's length.)
    let guarded = leading.is_some() || trailing.is_some() || !lookbehind.is_empty();
    if guarded && !is_guard_realizable(&regex, dotall) {
        return Err(GrammarError::Other {
            msg: format!(
                "terminal pattern `{pattern}`: a guarded branch's base is not \
                 guard-realizable (it has an order-sensitive alternation or a lazy/\
                 possessive quantifier and is not prefix-free), so its match-length \
                 under the guard is not reproducible by the longest-accept accumulator."
            ),
        });
    }
    Ok(LoweredBranch {
        regex,
        leading,
        trailing,
        lookbehind,
    })
}

/// A "decline to fancy" error: the assertion is a supported *shape* but this instance
/// cannot ride the lowered engine (e.g. a variable-offset lookbehind), so the caller
/// routes the whole terminal to `fancy-regex`. Distinct from a permanent rejection.
fn decline(pattern: &str, why: &str) -> GrammarError {
    GrammarError::Other {
        msg: format!(
            "terminal pattern `{pattern}`: {why}, so it cannot be lowered into the \
             combined DFA and is routed to fancy-regex."
        ),
    }
}

fn zero_width(pattern: &str) -> GrammarError {
    GrammarError::Other {
        msg: format!(
            "terminal pattern `{pattern}` lowers to a zero-width branch (a bare \
             boundary assertion); the lexer forbids zero-width terminals."
        ),
    }
}

/// Whether a guarded branch's base is **greedy-monotone**: its leftmost-first match
/// always equals its longest match, so the driver's "longest accept where the guard
/// holds" coincides with Python's backtracking result. True for a base with no
/// alternation and no lazy/possessive quantifier. Conservative — the caller treats a
/// `false` as "route to fancy."
fn is_greedy_monotone(base: &Node) -> bool {
    !node_has_alt(base) && !node_has_lazy(base)
}

fn node_has_alt(n: &Node) -> bool {
    match n {
        Node::Alt(_) => true,
        Node::Concat(parts) => parts.iter().any(node_has_alt),
        Node::Group { body, .. } => node_has_alt(body),
        Node::Atom(_) | Node::Assertion { .. } => false,
    }
}

fn node_has_lazy(n: &Node) -> bool {
    match n {
        Node::Atom(s) => atom_has_lazy(s),
        Node::Concat(parts) | Node::Alt(parts) => parts.iter().any(node_has_lazy),
        Node::Group { body, quant, .. } => quant.ends_with('?') || node_has_lazy(body),
        Node::Assertion { .. } => false,
    }
}

/// A lazy/possessive quantifier in a flat atom run: a `*` / `+` / `?` / `}` followed
/// by `?` (lazy) or `+` (possessive). Over-approximates (the safe direction).
fn atom_has_lazy(atom: &str) -> bool {
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
/// Conservative: a base meeting neither is declined (routed to `fancy-regex`). `dotall`
/// is the terminal's `s` flag — it changes what `.` matches and so the base's language,
/// so the prefix-free check must evaluate the base under the same flag the engine wraps.
fn is_guard_realizable(base: &str, dotall: bool) -> bool {
    // The greedy-monotone test works on the parsed tree (it predates this routine), so
    // re-parse the base; on a parse failure fall back to "not realizable" (decline).
    match super::parse(base) {
        Ok(node) if is_greedy_monotone(&node) => true,
        _ => is_prefix_free(base, dotall),
    }
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
fn is_prefix_free(base: &str, dotall: bool) -> bool {
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

/// Fixed char width of `node` — `Some(w)` iff its min and max match widths are equal,
/// `None` otherwise (variable or unbounded). Used to compute a lookbehind's fixed
/// offset from the match start.
fn fixed_width_chars(node: &Node) -> Option<usize> {
    let (lo, hi) = width_range(node);
    match hi {
        Some(h) if h == lo => Some(lo),
        _ => None,
    }
}

/// Maximum char width of `node`, or `None` if unbounded — the lookbehind window size.
fn max_width_chars(node: &Node) -> Option<usize> {
    width_range(node).1
}

/// The `(min, max)` match width of `node` in characters; `max` is `None` when
/// unbounded (a `*` / `+` / `{m,}` quantifier). A nested assertion contributes its
/// zero consumed width. This is the single width routine the whole `lookaround` module
/// shares: the classifier's bounded-vs-unbounded verdict and stored assertion width
/// ([`super::classify::max_width`]) both delegate here, so the proof bound and the
/// runtime lookbehind window can never drift apart.
pub(crate) fn width_range(node: &Node) -> (usize, Option<usize>) {
    match node {
        Node::Atom(s) => atom_width_range(s),
        Node::Concat(parts) => {
            let mut lo = 0usize;
            let mut hi = Some(0usize);
            for p in parts {
                let (plo, phi) = width_range(p);
                lo = lo.saturating_add(plo);
                hi = match (hi, phi) {
                    (Some(a), Some(b)) => Some(a.saturating_add(b)),
                    _ => None,
                };
            }
            (lo, hi)
        }
        Node::Alt(branches) => {
            let mut lo = usize::MAX;
            let mut hi = Some(0usize);
            for b in branches {
                let (blo, bhi) = width_range(b);
                lo = lo.min(blo);
                hi = match (hi, bhi) {
                    (Some(a), Some(b)) => Some(a.max(b)),
                    _ => None,
                };
            }
            (if lo == usize::MAX { 0 } else { lo }, hi)
        }
        Node::Group { body, quant, .. } => {
            let (blo, bhi) = width_range(body);
            apply_quant_range(blo, bhi, quant)
        }
        Node::Assertion { .. } => (0, Some(0)),
    }
}

/// Apply a group/element quantifier to a known `(min, max)` body width.
fn apply_quant_range(lo: usize, hi: Option<usize>, quant: &str) -> (usize, Option<usize>) {
    let q: Vec<char> = quant.chars().collect();
    match q.first().copied() {
        None => (lo, hi),
        Some('*') => (0, None),
        Some('+') => (lo, None),
        Some('?') => (0, hi),
        Some('{') => match parse_brace(&q, 0) {
            // `{m,}` — unbounded above, at least m·lo.
            Some((m, None, _)) => (lo.saturating_mul(m), None),
            Some((m, Some(n), _)) => (lo.saturating_mul(m), hi.map(|h| h.saturating_mul(n))),
            None => (lo, hi), // a literal `{` that wasn't a quantifier
        },
        _ => (lo, hi),
    }
}

/// `(min, max)` char width of a flat, assertion-free atom run; `max` is `None` if any
/// element is unbounded.
fn atom_width_range(atom: &str) -> (usize, Option<usize>) {
    let chars: Vec<char> = atom.chars().collect();
    let mut i = 0usize;
    let mut lo = 0usize;
    let mut hi = Some(0usize);
    while i < chars.len() {
        let c = chars[i];
        let elem_w = match c {
            '\\' => {
                i += 1;
                let n = chars.get(i).copied();
                i += 1;
                match n {
                    Some('b') | Some('B') | Some('A') | Some('z') | Some('Z') | Some('G') => 0,
                    _ => 1,
                }
            }
            '[' => {
                i += 1;
                if chars.get(i) == Some(&'^') {
                    i += 1;
                }
                if chars.get(i) == Some(&']') {
                    i += 1;
                }
                while i < chars.len() && chars[i] != ']' {
                    if chars[i] == '\\' {
                        i += 1;
                    }
                    i += 1;
                }
                if i < chars.len() {
                    i += 1;
                }
                1
            }
            '^' | '$' => {
                i += 1;
                0
            }
            _ => {
                i += 1;
                1
            }
        };

        // A quantifier binding to this element.
        let (elo, ehi): (usize, Option<usize>) = match chars.get(i).copied() {
            Some('*') => {
                i += 1;
                consume_lazy_marker(&chars, &mut i);
                (0, None)
            }
            Some('+') => {
                i += 1;
                consume_lazy_marker(&chars, &mut i);
                (elem_w, None)
            }
            Some('?') => {
                i += 1;
                consume_lazy_marker(&chars, &mut i);
                (0, Some(elem_w))
            }
            Some('{') => {
                if let Some((m, maxrep, consumed)) = parse_brace(&chars, i) {
                    i += consumed;
                    consume_lazy_marker(&chars, &mut i);
                    (
                        elem_w.saturating_mul(m),
                        maxrep.map(|n| elem_w.saturating_mul(n)),
                    )
                } else {
                    (elem_w, Some(elem_w))
                }
            }
            _ => (elem_w, Some(elem_w)),
        };
        lo = lo.saturating_add(elo);
        hi = match (hi, ehi) {
            (Some(a), Some(b)) => Some(a.saturating_add(b)),
            _ => None,
        };
    }
    (lo, hi)
}

/// Skip a lazy (`?`) / possessive (`+`) marker after a quantifier.
fn consume_lazy_marker(chars: &[char], i: &mut usize) {
    if matches!(chars.get(*i), Some('?') | Some('+')) {
        *i += 1;
    }
}

/// Parse a `{m}` / `{m,}` / `{m,n}` brace quantifier at `chars[start] == '{'`.
/// Returns `(min, max, chars_consumed)` where `max` is `None` for the unbounded
/// `{m,}`. Returns `None` if it is not a well-formed quantifier (a literal `{`).
fn parse_brace(chars: &[char], start: usize) -> Option<(usize, Option<usize>, usize)> {
    debug_assert_eq!(chars.get(start), Some(&'{'));
    let mut i = start + 1;
    let mut lo = String::new();
    while let Some(&c) = chars.get(i) {
        if c.is_ascii_digit() {
            lo.push(c);
            i += 1;
        } else {
            break;
        }
    }
    if lo.is_empty() {
        return None;
    }
    let min = lo.parse::<usize>().unwrap_or(usize::MAX);
    let max = if chars.get(i) == Some(&',') {
        i += 1;
        let mut hi = String::new();
        while let Some(&c) = chars.get(i) {
            if c.is_ascii_digit() {
                hi.push(c);
                i += 1;
            } else {
                break;
            }
        }
        if hi.is_empty() {
            None // `{m,}`
        } else {
            Some(hi.parse::<usize>().unwrap_or(usize::MAX))
        }
    } else {
        Some(min) // `{m}`
    };
    if chars.get(i) == Some(&'}') {
        Some((min, max, i + 1 - start))
    } else {
        None
    }
}

// ─── The string-literal escaped-body idiom (python.STRING / python.LONG_STRING) ─────
//
// Two bundled terminals share an escaped, quote-delimited body the generic boundary path
// cannot lower (their assertions sit after a variable-width prefix + the opening quote,
// so neither is a fixed-offset boundary). Both are recognized here and lowered by
// normalizing the lazy escaped body to its proven lookaround-free equivalent (no grammar
// edit). The two arm shapes differ only in the **close delimiter width** and whether an
// **opening guard** is present:
//
// **Short arm (`python.STRING`):** `<q>(?!<q><q>).*?(?<!\\)(\\\\)*?<q>` — a *single*-char
//   delimiter `<q>` with a leading `(?!<q><q>)` guard.
//   * **Lazy body + escape lookbehind → greedy character class.** `.*?(?<!\\)(\\\\)*?<q>`
//     normalizes to its proven greedy equivalent `(?:[^<q>\\<nl>]|\\.)*<q>` — the Type-A
//     rewrite `tests/test_lookaround.rs::matchlen`
//     (`string_lookaround_free_rewrite_is_not_equivalent`) pins as match-length-identical
//     to fancy **except** for the `(?!"")` divergence. The greedy class is sound *because*
//     a single-char delimiter is excluded from the content class (`[^<q>…]`), so the body
//     stops at the first unescaped `<q>`. `<nl>` (the `\n` exclusion) is present iff the
//     terminal is *not* DOTALL.
//   * **The `(?!<q><q>)` splice.** That normalized body can never *begin* with the
//     delimiter, so the forbidden `<q><q>` right after the opening quote can only arise
//     when the body is **empty** — the empty string `<q><q>`, the assertion's second char
//     lying *past* the token. The splice reduces, exactly, to a **non-empty** arm
//     `<prefix><q>(?:[^<q>\\<nl>]|\\.)+<q>` (unguarded — the `(?!<q><q>)` is vacuous) plus
//     an **empty** arm `<prefix><q><q>` carrying a trailing `(?!<q>)` guard (`""""` is a
//     lex error; `"" ""` is two empty strings). The empty arm's base is *prefix-free* so
//     the guarded longest-accept accumulator reproduces fancy (see [`is_prefix_free`]).
//
// **Long arm (`python.LONG_STRING`):** `<q3>.*?(?<!\\)(\\\\)*?<q3>` — a *multi*-character
//   delimiter `<q3>` (e.g. `"""`) and **no** opening guard.
//   * Because the close is multi-character, a single `<q>` is legal *inside* the body
//     (only the full `<q3>` run closes it), so the short arm's "exclude the delimiter from
//     the content class + greedy" trick does **not** apply. The body keeps its **lazy**
//     form: `<q3>(?:[^\\<nl>]|\\.)*?<q3>` — any non-backslash char (the delimiter char
//     included) or an escaped pair, lazily up to the first unescaped `<q3>`. This is the
//     proven E2a rewrite (`tests/test_lookaround.rs::long_string_match_length_equivalence`
//     pins `"""(?:[^\\]|\\.)*?"""` as match-length-identical to the fancy original under
//     DOTALL). The `(?<!\\)(\\\\)*?` close lookbehind is *absorbed* by the `\\.` escape
//     pairing, exactly as in the short arm.
//   * There is **no opening guard**, so there is no arm split: the long arm lowers to a
//     **single unguarded** branch. Being unguarded it rides the plain leftmost-first
//     engine, which reproduces Python `re`'s lazy-quantifier semantics natively — so the
//     lazy `*?` (which the guarded longest-accept accumulator could not host) is correct
//     here precisely because no guard is involved.
//
// The recognizer matches **only** these exact shapes; anything else returns `None` and the
// caller falls back to the generic boundary lowering (which rejects/declines it) — the
// reject-when-unsure direction. Newly-accepted instances are gated by the Route-1 proof
// (`tests/test_lowering_proof.rs`, the real nested STRING / LONG_STRING representatives),
// the generative equivalence layer, and the python.lark differential.

/// Which escaped-body arm shape was recognized: a short single-char-delimiter arm with an
/// opening guard ([`python.STRING`]), or a long multi-char-delimiter arm with no guard
/// ([`python.LONG_STRING`]).
#[derive(Debug, Clone, PartialEq, Eq)]
enum ArmKind {
    /// `<q>(?!<q><q>).*?(?<!\\)(\\\\)*?<q>` — single-char delimiter, greedy normalized
    /// body, opening-guard splice into a non-empty + a guarded empty arm.
    Short,
    /// `<q3>.*?(?<!\\)(\\\\)*?<q3>` — multi-char delimiter, lazy normalized body, no
    /// opening guard, a single unguarded branch.
    Long,
}

/// One recognized arm: its (fixed-literal) delimiter source and its shape.
#[derive(Debug, Clone, PartialEq, Eq)]
struct StringArm {
    /// The delimiter source — for a short arm a single literal (e.g. `"`); for a long arm
    /// a run of the *same* literal repeated (e.g. `"""`). Safe to emit both bare and (for
    /// a short arm) inside a negated class.
    delim: String,
    kind: ArmKind,
}

/// A recognized string-literal escaped-body idiom: an optional bounded-width,
/// assertion-free prefix followed by an alternation of quote-delimited arms, each either a
/// short ([`python.STRING`]) or long ([`python.LONG_STRING`]) escaped-body arm.
pub struct StringIdiom {
    /// The prefix regex source (e.g. `([ubf]?r?|r[ubf])`), or empty when there is none.
    prefix: String,
    /// The recognized arms, in source order.
    arms: Vec<StringArm>,
}

impl StringIdiom {
    /// Lower the idiom into its per-arm branches. `dotall` controls whether the body class
    /// admits a newline (excluded iff not DOTALL). A **short** arm lowers to two branches
    /// (a non-empty plain branch + an empty trailing-guarded branch); a **long** arm lowers
    /// to one unguarded branch. Declines (the conservative direction) if a short arm's
    /// empty base is not guard-realizable.
    fn lower(&self, pattern: &str, dotall: bool) -> Result<Vec<LoweredBranch>, GrammarError> {
        let nl = if dotall { "" } else { r"\n" };
        let mut branches = Vec::new();
        for arm in &self.arms {
            let d = &arm.delim;
            match arm.kind {
                ArmKind::Short => {
                    // The delimiter is a fixed single literal (the recognizer's
                    // `literal_delimiter_source` guarantees a bare non-metacharacter or an
                    // escaped punctuation literal), so it is safe both bare (the open/close
                    // `<q>`) and inside the negated class `[^<q>\\<nl>]`.
                    // Non-empty arm: unguarded greedy escaped body. The `(?!<q><q>)` is
                    // vacuous here (the body never begins with the delimiter).
                    let non_empty = format!("{p}{d}(?:[^{d}\\\\{nl}]|\\\\.)+{d}", p = self.prefix);
                    branches.push(LoweredBranch {
                        regex: non_empty,
                        leading: None,
                        trailing: None,
                        lookbehind: Vec::new(),
                    });
                    // Empty arm: `<prefix><q><q>` with a trailing `(?!<q>)` guard — the
                    // spliced residual of `(?!"")` once the in-token part is shown vacuous.
                    let empty = format!("{p}{d}{d}", p = self.prefix);
                    if !is_guard_realizable(&empty, dotall) {
                        return Err(decline(
                            pattern,
                            "the empty-string arm's base is not guard-realizable (prefix \
                             not length-deterministic), so the trailing-guard accumulator \
                             cannot reproduce fancy's match",
                        ));
                    }
                    branches.push(LoweredBranch {
                        regex: empty,
                        leading: None,
                        trailing: Some(GuardSpec {
                            neg: true,
                            set: d.clone(),
                        }),
                        lookbehind: Vec::new(),
                    });
                }
                ArmKind::Long => {
                    // A multi-char delimiter admits a single `<q>` char inside the body, so
                    // the content class must NOT exclude it — only the backslash (and a
                    // newline when not DOTALL). The body stays **lazy** so it stops at the
                    // first unescaped `<q3>`: `<q3>(?:[^\\<nl>]|\\.)*?<q3>`. Unguarded, so it
                    // rides the plain leftmost-first engine (Python `re` lazy semantics).
                    // `d` is the full `<q3>` run, emitted verbatim at open and close.
                    let body = format!("{p}{d}(?:[^\\\\{nl}]|\\\\.)*?{d}", p = self.prefix);
                    branches.push(LoweredBranch {
                        regex: body,
                        leading: None,
                        trailing: None,
                        lookbehind: Vec::new(),
                    });
                }
            }
        }
        Ok(branches)
    }
}

/// Recognize the [`StringIdiom`] in a parsed terminal `node`, or `None`. Structural and
/// exact: the supported shapes are `python.STRING`'s opening-guard short arm and
/// `python.LONG_STRING`'s guard-less long arm, so the matcher pins each precise arm shape
/// and declines everything else.
pub fn recognize_string_idiom(node: &Node) -> Option<StringIdiom> {
    let (prefix, arms_node) = split_prefix_and_arms(node)?;
    let arm_nodes: Vec<&Node> = match arms_node {
        Node::Alt(branches) => branches.iter().collect(),
        other => vec![other],
    };
    let mut arms = Vec::new();
    for arm in arm_nodes {
        arms.push(match_string_arm(arm)?);
    }
    if arms.is_empty() {
        return None;
    }
    Some(StringIdiom { prefix, arms })
}

/// Split `node` into `(prefix_source, arms_node)`: an optional leading bounded-width,
/// assertion-free prefix and the alternation-of-arms that follows it. The arms may sit
/// directly at top level, or (as in `python.STRING`) inside a single trailing group.
fn split_prefix_and_arms(node: &Node) -> Option<(String, &Node)> {
    match node {
        // `PREFIX (arm|arm|…)` — the bundled shape: a concat of [prefix-group, arms-group].
        Node::Concat(parts) if parts.len() == 2 => {
            let prefix = &parts[0];
            if prefix.has_assertion() || width_range(prefix).1.is_none() {
                return None; // prefix must be assertion-free and bounded-width
            }
            let arms = unwrap_arms(&parts[1])?;
            Some((prefix.to_source(), arms))
        }
        // No prefix: the arms alternation (optionally wrapped in one group) at top level.
        other => unwrap_arms(other).map(|arms| (String::new(), arms)),
    }
}

/// Peel a single **plain** capturing / non-capturing group wrapper to reach the arms `Alt`
/// (or a bare single arm). Returns the inner node iff it is an `Alt` or a `Concat` (one
/// arm).
///
/// A **flag-scoped** wrapper `(?flags:…)` is deliberately *not* peeled: when the grammar
/// loader folds a terminal's `/is` flags into the pattern, the terminal arrives here as
/// `(?is:…)` with its real `dotall` no longer visible to the lowering (it reads `dotall=0`
/// off the flag-stripped `PatternRe`). Lowering the body with the wrong `dotall` would
/// silently change the content class's newline handling — a false-accept. So a flag-wrapped
/// idiom is declined (`unwrap_arms` returns `None`, the caller routes it to `fancy-regex`),
/// exactly as the `python.STRING` splice declines its real `(?i:…)`-folded form — the
/// reject-when-unsure direction. The bare patterns (synthetic grammars + the harness +
/// `g_regex_flags`, where flags stay separate from the pattern) still lower.
fn unwrap_arms(node: &Node) -> Option<&Node> {
    match node {
        Node::Group { open, body, quant } if quant.is_empty() && is_plain_group_open(open) => {
            match body.as_ref() {
                inner @ (Node::Alt(_) | Node::Concat(_)) => Some(inner),
                _ => None,
            }
        }
        inner @ (Node::Alt(_) | Node::Concat(_)) => Some(inner),
        _ => None,
    }
}

/// Whether a group opener is a **plain** capturing / non-capturing group (`(` or `(?:`) —
/// i.e. *not* a flag-scoped `(?flags:…)` or a named group — so peeling it does not hide any
/// flag (e.g. DOTALL) that would change the lowered body's language.
fn is_plain_group_open(open: &str) -> bool {
    open == "(" || open == "(?:"
}

/// Match one escaped-body arm — either the short single-char-delimiter opening-guard form
/// (`python.STRING`) or the long multi-char-delimiter guard-less form
/// (`python.LONG_STRING`) — returning its [`StringArm`], or `None` if it is neither shape.
fn match_string_arm(arm: &Node) -> Option<StringArm> {
    let parts = match arm {
        Node::Concat(parts) => parts.as_slice(),
        _ => return None,
    };
    match parts.len() {
        6 => match_short_arm(parts),
        4 => match_long_arm(parts),
        _ => None,
    }
}

/// Match the short arm `<q>(?!<q><q>).*?(?<!\\)(\\\\)*?<q>` — a single-char delimiter `<q>`
/// with the `(?!<q><q>)` opening guard.
fn match_short_arm(parts: &[Node]) -> Option<StringArm> {
    debug_assert_eq!(parts.len(), 6);
    let delim = literal_delimiter_source(&parts[0])?;

    // parts[1]: (?!<delim><delim>)
    match &parts[1] {
        Node::Assertion {
            neg: true,
            look: Look::Ahead,
            body,
            quant,
        } if quant.is_empty() && body.to_source() == format!("{delim}{delim}") => {}
        _ => return None,
    }

    // parts[2]: the lazy any-body `.*?`
    if !matches!(&parts[2], Node::Atom(s) if s == ".*?") {
        return None;
    }
    match_close_lookbehind(&parts[3], &parts[4])?;
    // parts[5]: the closing delimiter, identical to the opening one.
    if literal_delimiter_source(&parts[5])? != delim {
        return None;
    }
    Some(StringArm {
        delim,
        kind: ArmKind::Short,
    })
}

/// Match the long arm `<q3>.*?(?<!\\)(\\\\)*?<q3>` — a multi-character delimiter `<q3>`
/// (a run of the same literal, e.g. `"""`) and **no** opening guard. Because there is no
/// guard between the opening delimiter and the body, the parser fuses them into one atom
/// `<q3>.*?`, so the arm is 4 parts: `[Atom("<q3>.*?"), (?<!\\), (\\\\)*?, Atom("<q3>")]`.
fn match_long_arm(parts: &[Node]) -> Option<StringArm> {
    debug_assert_eq!(parts.len(), 4);
    // parts[0]: the opening delimiter run fused with the lazy body `.*?`.
    let head = match &parts[0] {
        Node::Atom(s) => s.as_str(),
        _ => return None,
    };
    let delim = head.strip_suffix(".*?")?;
    let delim = multi_char_delimiter_source(delim)?;

    match_close_lookbehind(&parts[1], &parts[2])?;
    // parts[3]: the closing delimiter run, identical to the opening one.
    match &parts[3] {
        Node::Atom(s) if *s == delim => {}
        _ => return None,
    }
    Some(StringArm {
        delim,
        kind: ArmKind::Long,
    })
}

/// Match the shared escaped-close lookbehind `(?<!\\)(\\\\)*?` at two consecutive arm
/// parts (the `(?<!\\)` assertion then the `(\\\\)*?` even-backslash run), or `None`.
fn match_close_lookbehind(lb: &Node, run: &Node) -> Option<()> {
    // (?<!\\)
    match lb {
        Node::Assertion {
            neg: true,
            look: Look::Behind,
            body,
            quant,
        } if quant.is_empty() && body.to_source() == r"\\" => {}
        _ => return None,
    }
    // (\\\\)*? — the even-backslash run
    match run {
        Node::Group { open, body, quant }
            if open == "("
                && quant == "*?"
                && matches!(body.as_ref(), Node::Atom(s) if s == r"\\\\") => {}
        _ => return None,
    }
    Some(())
}

/// The source of a **multi-character literal-run** close delimiter — a run of *two or more*
/// of the *same* fixed single literal (e.g. `"""`, `'''`). Returns the verbatim run source
/// iff every character is the same plain literal; `None` otherwise (a mixed run, a
/// metacharacter, an escape). A single literal returns `None` here — that is the short
/// arm's case, matched by [`literal_delimiter_source`] instead. The long-arm lowering emits
/// the run **only bare** (open and close), never inside a character class, so unlike the
/// short delimiter it need not be class-safe; it must, however, be a fixed string of plain
/// literals so the lowered open/close match exactly the same bytes the original did.
fn multi_char_delimiter_source(run: &str) -> Option<String> {
    let chars: Vec<char> = run.chars().collect();
    if chars.len() < 2 {
        return None;
    }
    let first = chars[0];
    // Every char must be the same plain literal (not a metacharacter, not class-special,
    // not a backslash-escape). `is_plain_literal` rejects every regex metacharacter, so a
    // run that passes is a fixed literal string emitted verbatim with no further escaping.
    if chars.iter().all(|&c| c == first && is_plain_literal(c)) {
        Some(run.to_string())
    } else {
        None
    }
}

/// The source of a single-character **literal** delimiter — the only delimiters the
/// idiom lowering can faithfully reproduce, because the delimiter is emitted in the
/// lowered base both *bare* (the open/close `<q>`) and *inside a negated class*
/// (`[^<q>\\…]`), and must denote exactly one fixed character in both positions:
///
///   * a **bare ordinary literal** — any char that is not a regex metacharacter or a
///     character-class-special char (so `"`, `'`, `/`, `:`, … are fine; `.`, `^`, `$`,
///     `*`, `+`, `?`, `(`, `)`, `[`, `]`, `{`, `}`, `|`, `\`, `-` are not); or
///   * an **escaped literal** `\X` where `X` is ASCII *punctuation* (`\.`, `\"`, `\/`,
///     `\$`, … — a literal-escape of a metacharacter or other punctuation, emitted
///     escaped in both positions so it stays literal).
///
/// Returns `None` for everything else — crucially `.` (any char), the anchors
/// (`^ $ \b \B \A \z \Z \G`), and the class escapes (`\d \w \s …`): these are *not*
/// fixed single literals, so an arm built on them would mis-lower. Declining them routes
/// the terminal to `fancy-regex` (reject-when-unsure) and closes the false-accept.
fn literal_delimiter_source(node: &Node) -> Option<String> {
    let s = match node {
        Node::Atom(s) => s.as_str(),
        _ => return None,
    };
    let chars: Vec<char> = s.chars().collect();
    match chars.as_slice() {
        // A bare ordinary literal: not a regex metacharacter, not class-special.
        [c] if is_plain_literal(*c) => Some(c.to_string()),
        // An escaped punctuation literal (`\.`, `\"`, `\/`, …); excludes `\d \w \b \n …`
        // (letters/digits — classes, assertions, encoded literals).
        ['\\', c] if c.is_ascii_punctuation() => Some(format!("\\{c}")),
        _ => None,
    }
}

/// Whether `c` is an ordinary literal usable *bare* as a delimiter — neither a regex
/// metacharacter (special standalone) nor a character-class-special char (`-` `]` `^`
/// `\`). Anything excluded here can still be a delimiter in its **escaped** form.
fn is_plain_literal(c: char) -> bool {
    !matches!(
        c,
        '.' | '^' | '$' | '*' | '+' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | '\\' | '-'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn op_splits_into_guarded_and_unguarded_branches() {
        let b = lower_boundary("[+*]|[?](?![a-z])").unwrap();
        assert_eq!(b.len(), 2);
        assert_eq!(b[0].regex, "[+*]");
        assert!(b[0].leading.is_none() && b[0].trailing.is_none());
        assert_eq!(b[1].regex, "[?]");
        assert_eq!(
            b[1].trailing,
            Some(GuardSpec {
                neg: true,
                set: "[a-z]".to_string()
            })
        );
        assert!(b[1].leading.is_none());
    }

    #[test]
    fn prefix_free_realizability() {
        // Prefix-free (unique match length per start) → realizable even with alternation.
        assert!(is_prefix_free(r#"([ubf]?r?|r[ubf])"""#, false)); // STRING empty arm
        assert!(is_prefix_free(r#"([ubf]?r?|r[ubf])''"#, false));
        assert!(is_prefix_free("\"\"", false));
        assert!(is_prefix_free("[0-9]", false));
        // Not prefix-free: a string is a proper prefix of another.
        assert!(!is_prefix_free("[0-9]+", false));
        assert!(!is_prefix_free("ab|abc", false));
        // Nullable bases are NOT prefix-free — `""` is a prefix of every non-empty match.
        // The empty match's match-state is the EOI state (no outgoing edges), so the
        // reachability scan alone would miss it; the explicit nullability guard catches it.
        assert!(!is_prefix_free("(|a)", false));
        assert!(!is_prefix_free("a?", false));
        assert!(!is_prefix_free("[0-9]*", false));
        // Guard-realizability subsumes greedy-monotone OR prefix-free.
        assert!(is_guard_realizable("[0-9]+", false)); // greedy-monotone (not prefix-free)
        assert!(is_guard_realizable("[0-9]*", false)); // greedy-monotone (nullable * is fine)
        assert!(is_guard_realizable(r#"([ubf]?r?|r[ubf])"""#, false)); // prefix-free
                                                                       // Nullable AND alternation → not greedy-monotone, not prefix-free → declines.
        assert!(!is_guard_realizable("(|a)", false));
    }

    #[test]
    fn declines_non_greedy_monotone_guarded_base() {
        // A guarded branch whose base has an order-sensitive alternation or a lazy/
        // possessive quantifier is declined (reject-when-unsure) so the caller routes
        // it to fancy-regex rather than mis-lowering it via the longest-accept
        // accumulator.
        assert!(lower_boundary("(ab|abc)(?!z)").is_err());
        assert!(lower_boundary("ab??(?!c)").is_err());
        assert!(lower_boundary("(?!z)(ab|abc)").is_err());
        assert!(lower_boundary(r"a.*?(?=c)").is_err());
        // But a greedy-monotone guarded base (and an *unguarded* order-sensitive base)
        // lower fine.
        assert!(lower_boundary("[0-9]+(?![0-9])").is_ok());
        assert!(lower_boundary("ab|abc").is_ok()); // unguarded: order-sensitivity is the engine's job
    }

    #[test]
    fn dec_number_trailing_guard_is_stripped() {
        let b = lower_boundary(r"[1-9](_?[0-9])*|0(_?0)*(?![1-9])").unwrap();
        assert_eq!(b.len(), 2);
        assert!(b[0].trailing.is_none());
        assert_eq!(b[1].regex, "0(_?0)*");
        assert_eq!(b[1].trailing.as_ref().unwrap().set, "[1-9]");
        assert!(b[1].trailing.as_ref().unwrap().neg);
    }

    #[test]
    fn leading_guard_is_stripped() {
        let b = lower_boundary("(?!--)[a-z]+").unwrap();
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].regex, "[a-z]+");
        assert_eq!(
            b[0].leading,
            Some(GuardSpec {
                neg: true,
                set: "--".to_string()
            })
        );
        assert!(b[0].trailing.is_none());
    }

    #[test]
    fn positive_leading_guard() {
        let b = lower_boundary(r#"(?=")[^a-z]+"#).unwrap();
        assert_eq!(b[0].regex, "[^a-z]+");
        assert_eq!(
            b[0].leading,
            Some(GuardSpec {
                neg: false,
                set: "\"".into()
            })
        );
    }

    #[test]
    fn both_leading_and_trailing() {
        let b = lower_boundary("(?!a)[a-z]+(?!b)").unwrap();
        assert_eq!(b[0].regex, "[a-z]+");
        assert_eq!(b[0].leading.as_ref().unwrap().set, "a");
        assert_eq!(b[0].trailing.as_ref().unwrap().set, "b");
    }

    #[test]
    fn positive_trailing_guard() {
        let b = lower_boundary("[a-z]+(?=:)").unwrap();
        assert_eq!(
            b[0].trailing,
            Some(GuardSpec {
                neg: false,
                set: ":".into()
            })
        );
    }

    #[test]
    fn leading_lookbehind_at_offset_zero() {
        // `(?<!\\)"` — the canonical "quote not preceded by a backslash" close.
        let b = lower_boundary(r#"(?<!\\)""#).unwrap();
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].regex, "\"");
        assert!(b[0].leading.is_none() && b[0].trailing.is_none());
        assert_eq!(
            b[0].lookbehind,
            vec![LookbehindGuard {
                neg: true,
                set: "\\\\".to_string(),
                offset_chars: 0,
                width: 1,
            }]
        );
    }

    #[test]
    fn interior_lookbehind_at_fixed_offset() {
        // `\w(?<!_)x` — the guard bites within an offset-0 match: the `\w` may be the
        // trigger `_`, at char-offset 1 from the match start.
        let b = lower_boundary(r"\w(?<!_)x").unwrap();
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].regex, r"\wx");
        assert_eq!(b[0].lookbehind.len(), 1);
        let lb = &b[0].lookbehind[0];
        assert!(lb.neg && lb.set == "_" && lb.offset_chars == 1 && lb.width == 1);
    }

    #[test]
    fn positive_fixed_width_lookbehind() {
        // `(?<=ab)c` — width-2 positive lookbehind at offset 0.
        let b = lower_boundary("(?<=ab)c").unwrap();
        let lb = &b[0].lookbehind[0];
        assert!(!lb.neg && lb.set == "ab" && lb.offset_chars == 0 && lb.width == 2);
    }

    #[test]
    fn declines_lookbehind_after_variable_prefix() {
        // A lookbehind after a variable-width prefix (`\w+`, `.*?`) has no fixed offset
        // — declined (routed to fancy), the reject-when-unsure direction. This is the
        // python.LONG_STRING `.*?(?<!\\)` case the variable-offset milestone covers.
        assert!(lower_boundary(r"\w+(?<!_)x").is_err());
        assert!(lower_boundary(r#"""".*?(?<!\\)(\\\\)*?"#).is_err());
    }
}
