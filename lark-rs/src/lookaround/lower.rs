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
//!     lookbehind after a variable-width prefix (`.*?(?<!\\)` outside a recognized
//!     idiom) has no fixed offset and is **declined** here (routed to `fancy-regex`),
//!     the reject-when-unsure direction. `python.LONG_STRING` — historically the case
//!     this declined — now lowers via the audited **delimited-token** long-string idiom
//!     ([`recognize_long_string_idiom`] below, a sibling of the `python.STRING` splice),
//!     which *absorbs* the lookbehind by escape-pair body normalization rather than
//!     window-carrying it over arbitrary prefixes (`docs/LEXER_DFA_PLAN.md`, Stage B).
//!     The generic decline remains for every non-idiom variable-offset instance.

use super::classify::DeclineReason;
use super::{Look, Node};

/// A typed **decline**: every assertion in the pattern is a supported shape, but this
/// particular instance cannot ride the lowered engine (or the frontend could not
/// analyze it). The router turns this into [`LoweringRoute::Declined`] and, since L4,
/// the build path turns that into a categorized `GrammarError::LookaroundScope`
/// ([`DeclineReason::scope`]) — historically it routed to `fancy-regex`.
///
/// [`LoweringRoute::Declined`]: super::classify::LoweringRoute::Declined
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LowerDecline {
    pub reason: DeclineReason,
    /// The site-specific cause, naming the pattern — feeds the build message's
    /// `detail` slot (`classify::scope_message`).
    pub detail: String,
}

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
pub fn lower_boundary(pattern: &str) -> Result<Vec<LoweredBranch>, LowerDecline> {
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
) -> Result<Vec<LoweredBranch>, LowerDecline> {
    // The router has already classified this pattern with the same parser, so a parse
    // failure here is unreachable in the engine path; map it defensively to the same
    // typed decline the router would have produced.
    let node = super::parse(pattern).map_err(|e| LowerDecline {
        reason: DeclineReason::FrontendParse,
        detail: format!("the lookaround analyzer could not parse `{pattern}` ({e})"),
    })?;
    // The same vacuous-`(?:…)`-wrapper normalization the classifier applies, so the
    // two layers see one shape (a loader-wrapped alternation arm's trailing guard is
    // a *boundary* guard to both, never a group-nested internal assertion).
    let node = super::classify::unwrap_vacuous_groups(node);
    if let Some(idiom) = recognize_string_idiom(&node) {
        return idiom.lower(pattern, dotall);
    }
    // The second audited delimited-token idiom (Stage B): the bundled `lark.REGEXP`
    // regex-literal shape. Exact-match recognizer; `dotall` is inert (the idiom contains
    // no `.` — its body is the explicit class `[^\/]`, which admits a newline under any
    // flags, exactly as the original does).
    if let Some(idiom) = recognize_regexp_idiom(&node) {
        return Ok(idiom.lower());
    }
    // The third audited delimited-token idiom (Stage B): the bundled `python.LONG_STRING`
    // long-string shape. `dotall` is threaded exactly as the string idiom's — the lowered
    // body class admits a newline iff the terminal's `.` would. The branches are
    // unguarded, so no realizability gate is involved.
    if let Some(idiom) = recognize_long_string_idiom(&node) {
        return Ok(idiom.lower(dotall));
    }
    // The fourth audited delimited-token idiom: the **short-string** shape
    // `<q>.+?(?<!\\)(\\\\)*?<q>` (single-char delimiter, *non-empty* lazy body) — the
    // wild-bank dotmotif `FLEXIBLE_KEY`. Same escape-pair normalization family as
    // STRING/LONG_STRING; see the section comment below for the first-item twist.
    if let Some(idiom) = recognize_short_string_idiom(&node) {
        return Ok(idiom.lower(dotall));
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
pub fn lower_trailing(pattern: &str) -> Result<Vec<LoweredBranch>, LowerDecline> {
    lower_boundary(pattern)
}

/// Lower one top-level alternation branch: peel a leading lookahead off the front and
/// a trailing lookahead off the end into forward guards, peel every interior
/// bounded-lookbehind into a fixed-offset backward guard; whatever remains is the base
/// regex.
fn lower_branch(pattern: &str, branch: &Node, dotall: bool) -> Result<LoweredBranch, LowerDecline> {
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
                    return Err(decline(
                        pattern,
                        DeclineReason::QuantifiedLookbehind,
                        "the lookbehind carries a quantifier",
                    ));
                }
                let off = offset.ok_or_else(|| {
                    decline(
                        pattern,
                        DeclineReason::VariableOffsetLookbehind,
                        "a bounded lookbehind sits after a variable-width prefix, so its \
                         offset from the match start is not fixed",
                    )
                })?;
                let w = max_width_chars(body).ok_or_else(|| {
                    decline(
                        pattern,
                        DeclineReason::UnboundedLookbehindBody,
                        "the lookbehind body has no fixed maximum width",
                    )
                })?;
                if w == 0 {
                    return Err(decline(
                        pattern,
                        DeclineReason::ZeroWidthLookbehindBody,
                        "the lookbehind body is zero-width",
                    ));
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
            } => {
                return Err(decline(
                    pattern,
                    DeclineReason::InteriorLookahead,
                    "an interior forward lookahead",
                ))
            }
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
    // (or behind a flag wrapper — a user-written `(?s:…(?<!x)…)` reaches here with the
    // assertion buried in the group), so we could not peel it to a fixed offset. Decline
    // (route to fancy) rather than hand a lookaround-bearing base to the DFA builder,
    // which cannot parse it — the reject-when-unsure direction.
    if base.has_assertion() {
        return Err(decline(
            pattern,
            DeclineReason::NestedInGroup,
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
        return Err(decline(
            pattern,
            DeclineReason::NonRealizableGuardedBase,
            "a guarded branch's base is not guard-realizable (it has an \
             order-sensitive alternation or a lazy/possessive quantifier and is not \
             prefix-free), so its match-length under the guard is not reproducible by \
             the longest-accept accumulator",
        ));
    }
    Ok(LoweredBranch {
        regex,
        leading,
        trailing,
        lookbehind,
    })
}

/// A typed decline: the assertion is a supported *shape* but this instance cannot
/// ride the lowered engine (e.g. a variable-offset lookbehind). Distinct from a
/// permanent classifier rejection; categorized by [`DeclineReason::scope`].
fn decline(pattern: &str, reason: DeclineReason, why: &str) -> LowerDecline {
    LowerDecline {
        reason,
        detail: format!("in pattern `{pattern}`, {why}."),
    }
}

fn zero_width(pattern: &str) -> LowerDecline {
    decline(
        pattern,
        DeclineReason::ZeroWidthBranch,
        "the pattern lowers to a zero-width branch (a bare boundary assertion) and \
         the lexer forbids zero-width terminals",
    )
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
/// Conservative: a base meeting neither (nor the exact [`is_leftmost_longest`] decision)
/// is declined — since L4 a categorized NotYetImplemented build error. `dotall`
/// is the terminal's `s` flag — it changes what `.` matches and so the base's language,
/// so the prefix-free check must evaluate the base under the same flag the engine wraps.
fn is_guard_realizable(base: &str, dotall: bool) -> bool {
    // The greedy-monotone test works on the parsed tree (it predates this routine), so
    // re-parse the base; on a parse failure fall back to "not realizable" (decline).
    match super::parse(base) {
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
fn is_leftmost_longest(base: &str, dotall: bool) -> bool {
    let s = if dotall {
        format!("(?s:{base})")
    } else {
        base.to_string()
    };
    leftmost_longest_one(&s) && leftmost_longest_one(&format!("(?i:{s})"))
}

fn leftmost_longest_one(base: &str) -> bool {
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

// ─── The string-literal opening-guard idiom (python.STRING family) ──────────────
//
// `python.STRING` is `([ubf]?r?|r[ubf])("(?!"").*?(?<!\\)(\\\\)*?"|'(?!'').*?(?<!\\)(\\\\)*?')`.
// Its `(?!"")` sits **after a variable-width prefix + the opening quote** — an
// internal/variable-position leading boundary the generic boundary path cannot lower
// (it is not at a fixed offset). `docs/LEXER_DFA_PLAN.md` calls for an NFA-state splice:
// "peek-branch states where the forbidden continuation ("" after the opening quote)
// leads to a DEAD (non-accepting) state." We realize that splice by case analysis,
// composing with the variable-width prefix, the lazy body, and the `(?<!\\)` lookbehind:
//
//   * **Lazy body + escape lookbehind → greedy character class.** The arm body
//     `.*?(?<!\\)(\\\\)*?<q>` is normalized *internally* (no grammar edit) to its proven
//     greedy equivalent `(?:[^<q>\\<nl>]|\\.)*<q>` — the Type-A rewrite
//     `tests/test_lookaround.rs::matchlen` (`string_lookaround_free_rewrite_is_not_equivalent`)
//     pins as match-length-identical to fancy **except** for the `(?!"")` divergence.
//     `<nl>` (the `\n` exclusion) is present iff the terminal is *not* DOTALL — under
//     DOTALL the body may span newlines, exactly as `LONG_STRING`'s `(?is)` body does.
//   * **The `(?!"")` splice.** Given that normalized body can never *begin* with the
//     delimiter (`[^<q>…]` excludes it, `\\.` starts with a backslash), the forbidden
//     continuation `<q><q>` right after the opening quote can only arise when the body is
//     **empty** — i.e. the token is the empty string `<q><q>` and the assertion's second
//     character lies *past* the matched token. So the splice reduces, exactly, to:
//       - a **non-empty** arm `<prefix><q>(?:[^<q>\\<nl>]|\\.)+<q>` — unguarded (the
//         `(?!"")` is vacuous, the body's first char is never the delimiter); and
//       - an **empty** arm `<prefix><q><q>` carrying a trailing guard `(?!<q>)` — the
//         empty string is valid only when the next input char is not another delimiter
//         (`""""` is a lex error; `"" ""` is two empty strings).
//     The two arms are mutually exclusive at any position (the char after the opening
//     quote is the delimiter in exactly one of them), so their relative priority never
//     bites. The empty arm's base `<prefix><q><q>` is *prefix-free* (the fixed `<q><q>`
//     pins the variable prefix's length), so the guarded longest-accept accumulator
//     reproduces fancy's match (see [`is_prefix_free`]).
//
// The recognizer matches **only** this exact shape; anything else returns `None` and the
// caller falls back to the generic boundary lowering (which rejects/declines it) — the
// reject-when-unsure direction. Newly-accepted instances are gated by the Route-1 proof
// (`tests/test_lowering_proof.rs`, the real nested STRING representative), the generative
// equivalence layer, and the python.lark differential.

/// A recognized string-literal opening-guard idiom: an optional bounded-width,
/// assertion-free prefix followed by an alternation of quote-delimited arms, each
/// `<q>(?!<q><q>).*?(?<!\\)(\\\\)*?<q>` for a single-character delimiter `<q>`.
pub struct StringIdiom {
    /// The prefix regex source (e.g. `([ubf]?r?|r[ubf])`), or empty when there is none.
    prefix: String,
    /// The delimiter source of each arm (e.g. `"` then `'`), in source order.
    delims: Vec<String>,
}

impl StringIdiom {
    /// Lower the idiom into its per-arm branches (two per arm: a non-empty plain branch
    /// and an empty trailing-guarded branch). `dotall` controls whether the body class
    /// admits a newline (excluded iff not DOTALL). Declines (the conservative direction)
    /// if an empty arm's base is not guard-realizable.
    fn lower(&self, pattern: &str, dotall: bool) -> Result<Vec<LoweredBranch>, LowerDecline> {
        let nl = if dotall { "" } else { r"\n" };
        let mut branches = Vec::new();
        for d in &self.delims {
            // The delimiter is a fixed literal (the recognizer's `literal_delimiter_source`
            // guarantees a bare non-metacharacter or an escaped punctuation literal), so it
            // is safe both bare (the open/close `<q>`) and inside the negated class
            // `[^<q>\\<nl>]`.
            // Non-empty arm: unguarded greedy escaped body. The `(?!<q><q>)` is vacuous
            // here (the body never begins with the delimiter).
            let non_empty = format!("{p}{d}(?:[^{d}\\\\{nl}]|\\\\.)+{d}", p = self.prefix);
            branches.push(LoweredBranch {
                regex: non_empty,
                leading: None,
                trailing: None,
                lookbehind: Vec::new(),
            });
            // Empty arm: `<prefix><q><q>` with a trailing `(?!<q>)` guard — the spliced
            // residual of `(?!"")` once the in-token part is shown vacuous.
            let empty = format!("{p}{d}{d}", p = self.prefix);
            if !is_guard_realizable(&empty, dotall) {
                return Err(decline(
                    pattern,
                    DeclineReason::EmptyArmNotRealizable,
                    "the empty-string arm's base is not guard-realizable (prefix not \
                     length-deterministic), so the trailing-guard accumulator cannot \
                     reproduce the original match",
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
        Ok(branches)
    }
}

// ─── Why these three recognizers are kept separate (do NOT unify) ────────────
//
// `recognize_string_idiom`, `recognize_long_string_idiom`, and
// `recognize_short_string_idiom` (below) are near-duplicates, and it is tempting
// to fold them into one parameterized `recognize_delimited_idiom`. We
// deliberately do not. Each recognizer pins the *exact* bundled shape of its
// idiom, and that per-idiom matcher IS the soundness proof that its lowering
// reproduces the original match — the same "a variant must re-prove, not ride
// along" invariant the `regexp` recognizer documents further down. A shared
// abstraction trades that independent auditability for DRY: it makes it easy for
// a later edit to widen one idiom's accept set through the common helper, and the
// differential oracle does **not** catch that — a faithful unification and an
// accidental widening both stay green until a real grammar hits the gap. The ~3×
// duplication is the intended cost of keeping every idiom's soundness
// independently checkable. Architect decision (#478, 2026-06-30): keep separate;
// do not DRY this. (The orthogonal, no-fork half of #478 — splitting this file
// into submodules — stays available as good-autonomous work.)

/// Recognize the [`StringIdiom`] in a parsed terminal `node`, or `None`. Structural and
/// exact: the only newly-supported shape is `python.STRING`'s `(?!"")`-after-prefix
/// opening guard, so the matcher pins the precise arm shape and declines everything else.
pub fn recognize_string_idiom(node: &Node) -> Option<StringIdiom> {
    let (prefix, arms_node) = split_prefix_and_arms(node)?;
    let arm_nodes: Vec<&Node> = match arms_node {
        Node::Alt(branches) => branches.iter().collect(),
        other => vec![other],
    };
    let mut delims = Vec::new();
    for arm in arm_nodes {
        delims.push(match_string_arm(arm)?);
    }
    if delims.is_empty() {
        return None;
    }
    Some(StringIdiom { prefix, delims })
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

/// Peel a single capturing/non-capturing group wrapper to reach the arms `Alt` (or a
/// bare single arm). Returns the inner node iff it is an `Alt` or a `Concat` (one arm).
///
/// **Only `(` and `(?:` opens are peeled — never a flag-scoped `(?i:`/`(?s:` wrapper.**
/// Peeling a flag wrapper would silently discard its flags: the lowering would emit a
/// branch whose body class reflects the *caller's* `dotall` while the original pattern
/// ran under the wrapper's — the exact dotall mis-lowering the
/// `g_regex_flags_dotall_long_string` seam fixture pins. The engine strips a
/// whole-pattern flag wrapper back into the flag bitset *before* routing
/// (`strip_whole_pattern_flag_wrapper` in `crate::lexer`), so a wrapper reaching here
/// is out-of-idiom and must decline (reject-when-unsure).
fn unwrap_arms(node: &Node) -> Option<&Node> {
    match node {
        Node::Group { open, body, quant } if quant.is_empty() && (open == "(" || open == "(?:") => {
            match body.as_ref() {
                inner @ (Node::Alt(_) | Node::Concat(_)) => Some(inner),
                _ => None,
            }
        }
        inner @ (Node::Alt(_) | Node::Concat(_)) => Some(inner),
        _ => None,
    }
}

/// Match one arm `<q>(?!<q><q>).*?(?<!\\)(\\\\)*?<q>`, returning the delimiter source
/// `<q>`, or `None` if the arm is not exactly that shape.
fn match_string_arm(arm: &Node) -> Option<String> {
    let parts = match arm {
        Node::Concat(parts) => parts.as_slice(),
        _ => return None,
    };
    if parts.len() != 6 {
        return None;
    }
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

    // parts[3]: (?<!\\)
    match &parts[3] {
        Node::Assertion {
            neg: true,
            look: Look::Behind,
            body,
            quant,
        } if quant.is_empty() && body.to_source() == r"\\" => {}
        _ => return None,
    }

    // parts[4]: (\\\\)*? — the even-backslash run
    match &parts[4] {
        Node::Group { open, body, quant }
            if open == "("
                && quant == "*?"
                && matches!(body.as_ref(), Node::Atom(s) if s == r"\\\\") => {}
        _ => return None,
    }

    // parts[5]: the closing delimiter, identical to the opening one.
    if literal_delimiter_source(&parts[5])? != delim {
        return None;
    }
    Some(delim)
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

// ─── The regex-literal idiom (the bundled lark.REGEXP, Stage B) ──────────────────
//
// `lark.REGEXP` is `\/(?!\/)(\\\/|\\\\|[^\/])*?\/[imslux]*` — a `/ body / flags`
// delimited token whose `(?!\/)` sits *between* the opening slash and the lazy body, an
// internal position the top-level classifier rejects. It is the second audited
// **delimited-token idiom** (`docs/LEXER_DFA_PLAN.md`, Stage B), after the M4 STRING
// splice. The lowering rests on one exact observation:
//
//   **The guard reduces to "the body is non-empty."** At the guard position (right
//   after the opening `\/`), the forbidden continuation is a `/`. Every body
//   alternative starts with a char that is *not* `/` (`\\\/` and `\\\\` start with a
//   backslash; `[^\/]` excludes the slash), and the close `\/` starts with exactly `/`.
//   So at that position the engine can close (next char is `/`) **xor** consume a body
//   item — never both. `(?!\/)` therefore fails exactly when the body would match zero
//   items and the close would fire immediately (the empty `//`), and holds in every
//   other case where the token can proceed. Dropping the guard and bumping the lazy
//   repetition's minimum — `(…)*?` → `(…)+?` — is an *exact* rewrite, not an
//   approximation. (The same close-vs-item first-char disjointness holds at **every**
//   iteration boundary, which is also why `tests/test_lookaround.rs::matchlen`'s E2a
//   harness found this terminal Type-A regex-rewritable.)
//
// The single lowered branch is **unguarded** and joins the leftmost-first plain
// engine, which reproduces the lazy `+?` / ordered-alternation match end exactly
// (including the backtracking "dangling escaped slash" close — `/a\/b` matches
// `/a\/` — and the greedy `[imslux]*` flags suffix), so no guard machinery and no
// realizability question is involved. Gated by the route pins
// (`tests/test_lowering_routes.rs`), the hand canaries (`tests/test_regexp_splice.rs`),
// the generative equivalence + `*?`-mutant (`tests/test_lowering_equivalence.rs`), the
// state-pruned Route-1 proof (`tests/test_lowering_proof.rs`), and the scanner
// differential population.
//
// The recognizer matches **only** the exact bundled shape — anything else returns
// `None` and falls through to the generic path (which rejects/declines it), the
// reject-when-unsure direction. It is deliberately *not* parameterized over the
// delimiter, the body alternatives, their order, the quantifier, or the flags suffix:
// each of those is load-bearing in the reduction above (the first-char disjointness,
// the close shape, the laziness), so a variant must re-prove, not ride along.

/// A recognized regex-literal idiom — exactly the bundled `lark.REGEXP` shape
/// `\/(?!\/)(\\\/|\\\\|[^\/])*?\/[imslux]*`. Carries no parameters: the recognizer
/// pins every part of the shape, so the lowering is a fixed, audited rewrite.
pub struct RegexpIdiom;

/// Recognize the [`RegexpIdiom`] in a parsed terminal `node`, or `None`. Structural and
/// exact — see the section comment above for why no variant is admitted.
pub fn recognize_regexp_idiom(node: &Node) -> Option<RegexpIdiom> {
    let parts = match node {
        Node::Concat(parts) if parts.len() == 4 => parts,
        _ => return None,
    };
    // parts[0]: the opening delimiter, exactly the escaped slash `\/`.
    if !matches!(&parts[0], Node::Atom(s) if s == r"\/") {
        return None;
    }
    // parts[1]: the empty-body guard, exactly `(?!\/)`, unquantified.
    match &parts[1] {
        Node::Assertion {
            neg: true,
            look: Look::Ahead,
            body,
            quant,
        } if quant.is_empty() && matches!(body.as_ref(), Node::Atom(s) if s == r"\/") => {}
        _ => return None,
    }
    // parts[2]: the lazy escaped body, exactly `(\\\/|\\\\|[^\/])*?` — the capturing
    // group, the three alternatives in source order, and the lazy star are all pinned.
    match &parts[2] {
        Node::Group { open, body, quant } if open == "(" && quant == "*?" => {
            let arms = match body.as_ref() {
                Node::Alt(arms) if arms.len() == 3 => arms,
                _ => return None,
            };
            for (arm, want) in arms.iter().zip([r"\\\/", r"\\\\", r"[^\/]"]) {
                if !matches!(arm, Node::Atom(s) if s == want) {
                    return None;
                }
            }
        }
        _ => return None,
    }
    // parts[3]: the close + flags tail, exactly `\/[imslux]*`.
    if !matches!(&parts[3], Node::Atom(s) if s == r"\/[imslux]*") {
        return None;
    }
    Some(RegexpIdiom)
}

impl RegexpIdiom {
    /// Lower the idiom: drop the `(?!\/)` and bump the lazy body to non-empty
    /// (`*?` → `+?`) — the exact rewrite the section comment proves. One unguarded,
    /// lookaround-free branch; its lazy/priority match end is the plain leftmost-first
    /// engine's native semantics.
    fn lower(&self) -> Vec<LoweredBranch> {
        vec![LoweredBranch {
            regex: r"\/(\\\/|\\\\|[^\/])+?\/[imslux]*".to_string(),
            leading: None,
            trailing: None,
            lookbehind: Vec::new(),
        }]
    }
}

// ─── The long-string idiom (the bundled python.LONG_STRING, Stage B) ─────────────
//
// `python.LONG_STRING` is `([ubf]?r?|r[ubf])(""".*?(?<!\\)(\\\\)*?"""|'''.*?(?<!\\)(\\\\)*?''')`
// with `/is` flags — a `<prefix> <qqq> body <qqq>` delimited token whose `(?<!\\)`
// lookbehind sits after the variable-width `.*?`, the no-fixed-offset position the
// generic M3 path declines. It is the third audited **delimited-token idiom**
// (`docs/LEXER_DFA_PLAN.md`, Stage B), after the M4 STRING splice and the REGEXP
// regex-literal idiom. The lowering is the escape-pair body normalization:
//
//   **The `(?<!\\)(\\\\)*?` is absorbed by forced escape pairing.** Rewrite the lazy
//   escaped body to lazy escape-pair items:
//
//       .*?(?<!\\)(\\\\)*?<qqq>   →   (?:[^\\<nl>]|\\.)*?<qqq>
//
//   (`<nl>` = `\n` iff the terminal is not DOTALL, exactly the string idiom's
//   threading.) A backslash can only be consumed as the start of a `\\.` pair (the
//   class excludes it), so item segmentation is forced and an item *boundary* exists
//   exactly at the positions where the maximal preceding backslash run has even
//   length — which is precisely the `(?<!\\)(\\\\)*?` close condition. The lazy `*?`
//   is **kept**: both sides close at the *first* even-parity `<qqq>`. This is the
//   committed Type-A finding `tests/test_lookaround.rs::long_string_match_length_equivalence`
//   pins (`LONG_ORIG ≡ LONG_NEW` over an exhaustive corpus with quotes, backslashes,
//   newlines, and the `r` prefix). Unlike the STRING splice, the delimiter quote is
//   *not* excluded from the body class — a lone `"` (or `""`) inside the body does not
//   close; laziness picks the first full `<qqq>`, so no multi-char delimiter automaton
//   is needed.
//
// The per-arm branches are **unguarded** (prefix duplicated per branch, arms in source
// order) and join the leftmost-first plain engine, whose native lazy/priority semantics
// reproduce the match end — the REGEXP precedent, so no guard machinery and no
// realizability question. The per-arm split is itself verified: leftmost-first across
// the two prefix-duplicated branches ≡ the original single pattern under `(?is)`
// (0 divergences over 2,015,539 inputs, lengths 0–8 over `" ' \ a \n r`), and the
// non-DOTALL `[^\\\n]` variant ≡ the unflagged original (0 divergences over 349,525
// inputs). Gated by the route pins (`tests/test_lowering_routes.rs`), the hand canaries
// (`tests/test_long_string_splice.rs`), the generative equivalence + parity/two-quote/
// greedy mutants (`tests/test_lowering_equivalence.rs`), the state-pruned Route-1 proof
// (`tests/test_lowering_proof.rs`), and the scanner-differential population.
//
// The recognizer matches **only** the exact bundled arm shape — delimiters `"""` or
// `'''` only, open == close, the lazy `.*?`, the `(?<!\\)` lookbehind, and the lazy
// `(\\\\)*?` escape group are all pinned; the optional prefix rides the same
// [`split_prefix_and_arms`] gate the string idiom uses (bounded, assertion-free).
// Anything else returns `None` and falls through to the generic path (which declines
// the variable-offset lookbehind), the reject-when-unsure direction.

/// A recognized long-string idiom: an optional bounded-width, assertion-free prefix
/// followed by 1..n arms, each exactly `<qqq>.*?(?<!\\)(\\\\)*?<qqq>` for a
/// triple-quote delimiter `<qqq>` ∈ {`"""`, `'''`}.
pub struct LongStringIdiom {
    /// The prefix regex source (e.g. `([ubf]?r?|r[ubf])`), or empty when there is none.
    prefix: String,
    /// The triple-quote delimiter of each arm (`"""` / `'''`), in source order.
    delims: Vec<String>,
}

/// Recognize the [`LongStringIdiom`] in a parsed terminal `node`, or `None`. Structural
/// and exact — see the section comment above for why no variant is admitted.
pub fn recognize_long_string_idiom(node: &Node) -> Option<LongStringIdiom> {
    let (prefix, arms_node) = split_prefix_and_arms(node)?;
    let arm_nodes: Vec<&Node> = match arms_node {
        Node::Alt(branches) => branches.iter().collect(),
        other => vec![other],
    };
    let mut delims = Vec::new();
    for arm in arm_nodes {
        delims.push(match_long_string_arm(arm)?);
    }
    if delims.is_empty() {
        return None;
    }
    Some(LongStringIdiom { prefix, delims })
}

/// Match one arm `<qqq>.*?(?<!\\)(\\\\)*?<qqq>`, returning the triple-quote delimiter
/// `<qqq>`, or `None` if the arm is not exactly that shape. The opening delimiter and
/// the lazy `.*?` arrive merged in a single atom (no structural boundary between them).
fn match_long_string_arm(arm: &Node) -> Option<String> {
    let parts = match arm {
        Node::Concat(parts) => parts.as_slice(),
        _ => return None,
    };
    if parts.len() != 4 {
        return None;
    }

    // parts[0]: `<qqq>.*?` — the opening triple quote + the lazy any-body, one atom.
    // Only the two bundled delimiters are admitted.
    let delim = match &parts[0] {
        Node::Atom(s) if s == "\"\"\".*?" => "\"\"\"".to_string(),
        Node::Atom(s) if s == "'''.*?" => "'''".to_string(),
        _ => return None,
    };

    // parts[1]: `(?<!\\)`, unquantified.
    match &parts[1] {
        Node::Assertion {
            neg: true,
            look: Look::Behind,
            body,
            quant,
        } if quant.is_empty() && matches!(body.as_ref(), Node::Atom(s) if s == r"\\") => {}
        _ => return None,
    }

    // parts[2]: `(\\\\)*?` — the lazy even-backslash run.
    match &parts[2] {
        Node::Group { open, body, quant }
            if open == "("
                && quant == "*?"
                && matches!(body.as_ref(), Node::Atom(s) if s == r"\\\\") => {}
        _ => return None,
    }

    // parts[3]: the closing delimiter, identical to the opening one.
    if !matches!(&parts[3], Node::Atom(s) if *s == delim) {
        return None;
    }
    Some(delim)
}

impl LongStringIdiom {
    /// Lower the idiom: normalize each arm's lazy escaped body to lazy escape-pair
    /// items (absorbing the `(?<!\\)(\\\\)*?` — the exact rewrite the section comment
    /// proves), keeping the lazy close. One unguarded branch per arm, prefix duplicated;
    /// `dotall` controls whether the body class admits a newline (excluded iff not
    /// DOTALL, so the class tracks what the original `.` matches under the terminal's
    /// flags; the `\\.` pair's second char tracks it natively via the engine's flag
    /// wrap).
    fn lower(&self, dotall: bool) -> Vec<LoweredBranch> {
        let nl = if dotall { "" } else { r"\n" };
        self.delims
            .iter()
            .map(|d| LoweredBranch {
                regex: format!("{p}{d}(?:[^\\\\{nl}]|\\\\.)*?{d}", p = self.prefix),
                leading: None,
                trailing: None,
                lookbehind: Vec::new(),
            })
            .collect()
    }
}

// ─── The short-string idiom (the wild-bank dotmotif FLEXIBLE_KEY, idiom #4) ──────
//
// dotmotif's `FLEXIBLE_KEY` is `".+?(?<!\\)(\\\\)*?"|'.+?(?<!\\)(\\\\)*?'` — a
// quote-delimited token with a **non-empty** lazy escaped body, whose `(?<!\\)`
// lookbehind sits after the variable-width `.+?` (the no-fixed-offset position the
// generic M3 path declines). It is the fourth audited **delimited-token idiom**, the
// guardless single-delimiter sibling of the M4 STRING splice (the same `<q> body <q>`
// family; LONG_STRING is the triple-quote sibling). The lowering is the same
// escape-pair body normalization, with one twist the missing `(?!<q><q>)` guard forces:
//
//   **A close needs more body than its own escape run.** The body decomposes as
//   `X·P`: `X` = the `.+?` chars (**≥ 1**, anything), `P` = the `(\\\\)*?` even
//   backslash run, with `(?<!\\)` forcing `P` to cover the *entire* maximal trailing
//   backslash run. So a `<q>` at body length `ℓ` with a maximal trailing backslash
//   run of length `r` closes iff `r` is even **and `ℓ > r`** — and the lazy close
//   fires at the *first* such `<q>`. A `<q>` where that fails is **consumed as a
//   body char**: at `ℓ = r = 0` (`"""` is one 3-char token, the empty `""` is no
//   token) and, the subtle case, after a **pure-pair body** (`ℓ = r > 0`: `"\\"` is
//   no token — `X` would be empty — and `"\\""` is one 5-char token whose third
//   quote is body). The exact lookaround-free equivalent tracks "body so far is pure
//   backslash pairs" structurally:
//
//       <q>.+?(?<!\\)(\\\\)*?<q>
//         →   <q> (?:\\\\)* (?:[^\\<nl>]|\\[^\\<nl>]) (?:[^<q>\\<nl>]|\\.)* <q>
//
//   — a greedy pure-pair run (the `ℓ = r` zone, where a `<q>` is consumed, never a
//   close), then one **mandatory transition item** (any non-backslash char,
//   *including a bare `<q>`*, or an escape pair whose second char is not a
//   backslash — exactly the moves that make `ℓ > r` and keep it so), then
//   LONG_STRING's escape-pair items with the delimiter excluded so the greedy `*`
//   closes at the first free-standing `<q>` (the M4 close-exclusion argument; at
//   every item boundary past the transition the trailing run is even and `ℓ > r`,
//   so the first free `<q>` is exactly the original's lazy close). The pure-pair
//   run and the transition pair are first-two-char disjoint, so the decomposition
//   is deterministic and the lowered branch is unguarded — its leftmost-first match
//   end is the plain engine's native semantics. `<nl>` (the `\n` exclusion, in the
//   classes and the pair tails) is present iff the terminal is not DOTALL, exactly
//   the STRING/LONG_STRING threading.
//
// Gated by the recognizer-exactness + behavior unit tests below, the generative
// equivalence sweep vs the `fancy-regex` dev-oracle
// (`tests/test_lowering_equivalence.rs`), and end-to-end by the wild bank's dotmotif
// replay (23 real queries vs the Python-Lark oracle). The recognizer matches **only**
// this exact shape — in particular the non-empty `.+?`: the *empty-capable* `.*?`
// variant without a `(?!<q><q>)` guard closes at width 0 on `""` where this rewrite
// would consume a char, so it must keep declining (reject-when-unsure) until someone
// proves its own rewrite.

/// A recognized short-string idiom: an optional bounded-width, assertion-free prefix
/// followed by 1..n arms, each exactly `<q>.+?(?<!\\)(\\\\)*?<q>` for a
/// single-character literal delimiter `<q>`.
pub struct ShortStringIdiom {
    /// The prefix regex source, or empty when there is none.
    prefix: String,
    /// The delimiter source of each arm (e.g. `"` then `'`), in source order.
    delims: Vec<String>,
}

/// Recognize the [`ShortStringIdiom`] in a parsed terminal `node`, or `None`.
/// Structural and exact — see the section comment above for why no variant is
/// admitted.
pub fn recognize_short_string_idiom(node: &Node) -> Option<ShortStringIdiom> {
    let (prefix, arms_node) = split_prefix_and_arms(node)?;
    let arm_nodes: Vec<&Node> = match arms_node {
        Node::Alt(branches) => branches.iter().collect(),
        other => vec![other],
    };
    let mut delims = Vec::new();
    for arm in arm_nodes {
        delims.push(match_short_string_arm(arm)?);
    }
    if delims.is_empty() {
        return None;
    }
    Some(ShortStringIdiom { prefix, delims })
}

/// Match one arm `<q>.+?(?<!\\)(\\\\)*?<q>`, returning the delimiter source `<q>`, or
/// `None` if the arm is not exactly that shape. The opening delimiter and the lazy
/// non-empty body arrive merged in a single atom (no structural boundary between
/// them), like the long-string arm's.
fn match_short_string_arm(arm: &Node) -> Option<String> {
    let parts = match arm {
        Node::Concat(parts) => parts.as_slice(),
        _ => return None,
    };
    if parts.len() != 4 {
        return None;
    }

    // parts[0]: `<q>.+?` — the opening delimiter + the lazy *non-empty* any-body, one
    // atom. The delimiter must be a single-character literal (the same contract as the
    // STRING idiom's `literal_delimiter_source`, for the same reason: it is emitted
    // both bare and inside a negated class below).
    let delim = match &parts[0] {
        Node::Atom(s) => {
            let head = s.strip_suffix(".+?")?;
            let head_node = Node::Atom(head.to_string());
            literal_delimiter_source(&head_node)?
        }
        _ => return None,
    };

    // parts[1]: `(?<!\\)`, unquantified.
    match &parts[1] {
        Node::Assertion {
            neg: true,
            look: Look::Behind,
            body,
            quant,
        } if quant.is_empty() && matches!(body.as_ref(), Node::Atom(s) if s == r"\\") => {}
        _ => return None,
    }

    // parts[2]: `(\\\\)*?` — the lazy even-backslash run.
    match &parts[2] {
        Node::Group { open, body, quant }
            if open == "("
                && quant == "*?"
                && matches!(body.as_ref(), Node::Atom(s) if s == r"\\\\") => {}
        _ => return None,
    }

    // parts[3]: the closing delimiter, identical to the opening one.
    if !matches!(&parts[3], Node::Atom(s) if *s == delim) {
        return None;
    }
    Some(delim)
}

impl ShortStringIdiom {
    /// Lower the idiom: one unguarded branch per arm — the exact rewrite the section
    /// comment proves (pure-pair run, mandatory transition item, close-excluded
    /// items). `dotall` controls whether the body classes admit a newline.
    fn lower(&self, dotall: bool) -> Vec<LoweredBranch> {
        let nl = if dotall { "" } else { r"\n" };
        self.delims
            .iter()
            .map(|d| LoweredBranch {
                regex: format!(
                    "{p}{d}(?:\\\\\\\\)*(?:[^\\\\{nl}]|\\\\[^\\\\{nl}])(?:[^{d}\\\\{nl}]|\\\\.)*{d}",
                    p = self.prefix
                ),
                leading: None,
                trailing: None,
                lookbehind: Vec::new(),
            })
            .collect()
    }
}

// ─── Fence idiom (idiom #5): named-backref tag-echo delimited tokens ───────────
//
// The fence idiom matches languages that use the same run-time tag to open and
// close a delimited span — heredoc / heredoc-indent (HCL2 Terraform) and CMake
// bracket arguments (gersemi). The Python `re` module handles these via named
// capturing groups and backreferences:
//
//   <<(?P<heredoc>[a-zA-Z][a-zA-Z0-9._-]+)\n(?:.|\n)*?(?P=heredoc)
//   \[(?P<equal_signs>(=*))\[([\s\S]+?)\](?P=equal_signs)\]
//
// These patterns are **non-regular** (the `regex` crate and `regex-automata`
// both reject them). However, they are **linear-time** recognisable:
//
//   (1) match the open literal + run the tag DFA anchored at `pos` → capture
//       the tag bytes (e.g. "MARKER" or "====");
//   (2) check the separator literal;
//   (3) build `close_seq = close_pre ++ tag_bytes ++ close_post` and scan the
//       rest for its first occurrence at least `body_min` chars in — a single
//       forward pass.
//
// No backtracking, no quadratic *matching*; one failed attempt still scans the
// remaining input once (the same worst case Python `re` pays for the identical
// lazy-body pattern, so oracle parity holds).
//
// **Reject-when-unsure (the recognizer's contract).** Step (3) reproduces
// Python's lazy `body` semantics only when the body matches *any* character, so
// the recognizer demands a body group whose unit is universal (`[\s\S]`,
// `.|\n`) under a **lazy** quantifier (`*?` → `body_min` 0, `+?` → 1); a greedy
// quantifier means Python takes the *last* close occurrence, a constrained body
// means content must be validated — both are rejected so the matcher never
// silently diverges from the oracle. One residual assumption is documented on
// [`FenceSpec`]: no backtracking between the (greedy) tag and the separator.
//
// [`recognize_fence_idiom`] detects the exact shape without calling the
// lookaround AST parser (which fails on named backreferences). The compiled
// [`FenceSpec`] is consumed in `lexer/fence.rs` to build a `FenceMatcher`.

/// The components of a recognised fence pattern; consumed by `lexer/fence.rs`
/// to build a `FenceMatcher`.
///
/// Assumption baked into the two-phase matcher: the tag DFA matches greedily
/// and the separator is then checked with **no backtracking** into the tag.
/// Python `re` would shrink the tag if that made the separator fit. The three
/// audited wild-bank patterns are immune (the tag's character class cannot
/// match the separator's first byte), and a pattern that does backtrack there
/// simply fails to lex where Python matches — it cannot mis-lex a longer or
/// shorter token. Verifying disjointness automatically needs tag-DFA
/// introspection; revisit if a wild grammar ever trips this.
pub struct FenceSpec {
    /// Literal bytes before the named capture group (e.g. `b"<<"` or `b"["`).
    pub open: Vec<u8>,
    /// The tag regex (content of the named capture group, e.g.
    /// `"[a-zA-Z][a-zA-Z0-9._-]+"` or `"(=*)"`).
    pub tag_re: String,
    /// Literal bytes between the tag and the body (e.g. `b"\n"` or `b"["`).
    pub sep: Vec<u8>,
    /// Minimum number of body characters (0 for a `*?` body, 1 for `+?`).
    pub body_min: usize,
    /// Literal bytes between the body and the backreference (e.g. `b""` or `b"]"`).
    pub close_pre: Vec<u8>,
    /// Literal bytes after the backreference (e.g. `b""` or `b"]"`).
    pub close_post: Vec<u8>,
}

/// Try to recognise the fence idiom in the raw regex pattern `raw`.
///
/// The recognised shape is:
///   `OPEN (?P<NAME>TAG_RE) SEP BODY CLOSE_PRE (?P=NAME) CLOSE_POST`
///
/// where OPEN, SEP, CLOSE_PRE, CLOSE_POST are all pure regex literals
/// (no unescaped metacharacters), BODY is one balanced group whose unit is a
/// universal single character (`[\s\S]`, `[\S\s]`, `.|\n`, `\n|.`) under a lazy
/// `*?`/`+?` quantifier (inside or outside the group), and `(?P=NAME)` is the
/// standard named backreference, appearing exactly once.
///
/// Returns `None` if the pattern does not match this exact shape. Never panics.
pub fn recognize_fence_idiom(raw: &str) -> Option<FenceSpec> {
    // Quick pre-check: must contain a named backreference.
    if !raw.contains("(?P=") {
        return None;
    }

    // Find the first `(?P<` at top level (skipping `\X` and character classes).
    let named_open = scan_for(raw.as_bytes(), b"(?P<")?;

    // Everything before `(?P<` must be a pure literal.
    let open = unescape_regex_literal(&raw[..named_open])?;

    // Extract NAME: alphanumeric/underscore chars between `<` and `>`.
    let name_start = named_open + 4; // skip `(?P<`
    let rest_after_open = raw.get(name_start..)?;
    let gt_offset = rest_after_open.find('>')?;
    let name = &rest_after_open[..gt_offset];
    if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
        return None;
    }

    // The `(` of `(?P<NAME>...)` is at `named_open`; find its matching `)`.
    let group_close = find_group_close(raw.as_bytes(), named_open)?;
    let after_name_gt = name_start + gt_offset + 1; // position after `>`
    let tag_re = &raw[after_name_gt..group_close];
    if tag_re.is_empty() {
        return None;
    }

    // After the named group: rest = SEP BODY CLOSE_PRE (?P=NAME) CLOSE_POST
    let after_group = group_close + 1;
    let rest = raw.get(after_group..)?;

    // Find `(?P=NAME)` in the rest — must appear exactly once.
    let backref = format!("(?P={})", name);
    let backref_pos = rest.find(backref.as_str())?;
    if rest[backref_pos + backref.len()..].contains(backref.as_str()) {
        return None; // more than one backref: too complex
    }

    let mid = &rest[..backref_pos];
    let close_post_str = &rest[backref_pos + backref.len()..];

    // CLOSE_POST must be a pure literal.
    let close_post = unescape_regex_literal(close_post_str)?;

    // Parse MID → (sep_str, body_str, close_pre_str).
    let (sep_str, body_str, close_pre_str) = split_mid(mid)?;
    let body_min = universal_lazy_body_min(body_str)?;
    let sep = unescape_regex_literal(sep_str)?;
    let close_pre = unescape_regex_literal(close_pre_str)?;

    Some(FenceSpec {
        open,
        tag_re: tag_re.to_string(),
        sep,
        body_min,
        close_pre,
        close_post,
    })
}

/// Find the first occurrence of the byte-string `pat` in `s`, scanning past
/// `\X` escape sequences and `[...]` character classes (so a `pat` inside an
/// escape or class is not reported). Does not track group depth.
fn scan_for(s: &[u8], pat: &[u8]) -> Option<usize> {
    let n = s.len();
    let pn = pat.len();
    let mut i = 0;
    while i + pn <= n {
        if s[i] == b'\\' {
            i += 2;
            continue;
        }
        if s[i] == b'[' {
            i = skip_char_class(s, i);
            continue;
        }
        if s[i..].starts_with(pat) {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Skip a `[...]` character class starting at `i` (which must point at `[`).
/// Handles a leading `^` and a literal `]` as the first class char. Returns the
/// index just past the closing `]` (or `s.len()` if unterminated).
fn skip_char_class(s: &[u8], i: usize) -> usize {
    let n = s.len();
    let mut i = i + 1;
    if i < n && s[i] == b'^' {
        i += 1;
    }
    // `[]` or `[^]` — a literal `]` as first class char.
    if i < n && s[i] == b']' {
        i += 1;
    }
    while i < n && s[i] != b']' {
        if s[i] == b'\\' {
            i += 1;
        }
        i += 1;
    }
    if i < n {
        i += 1; // skip `]`
    }
    i
}

/// Find the matching `)` for the `(` at position `pos` in `s`, respecting
/// `\X` escapes, `[...]` character classes, and nested groups. Returns the
/// byte index of the matching `)`, or `None` if unbalanced.
fn find_group_close(s: &[u8], pos: usize) -> Option<usize> {
    debug_assert_eq!(s.get(pos), Some(&b'('));
    let n = s.len();
    let mut i = pos + 1;
    let mut depth = 1usize;
    while i < n && depth > 0 {
        match s[i] {
            b'\\' => {
                i += 2;
            }
            b'[' => {
                i = skip_char_class(s, i);
            }
            b'(' => {
                depth += 1;
                i += 1;
            }
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
                i += 1;
            }
            _ => {
                i += 1;
            }
        }
    }
    None
}

/// Split the MID section `SEP BODY CLOSE_PRE` of a fence pattern into
/// `(sep_str, body_str, close_pre_str)`. SEP is the literal prefix before the
/// first unescaped `(`; BODY is one balanced group plus any trailing
/// quantifier; CLOSE_PRE is the literal suffix after the body.
///
/// Returns `None` if the MID section is not this exact shape (a missing body
/// group is rejected: without one, Python requires the close sequence to start
/// *immediately*, which the forward close-scan does not reproduce).
fn split_mid(mid: &str) -> Option<(&str, &str, &str)> {
    let s = mid.as_bytes();
    let n = s.len();
    let mut i = 0;

    // SEP: literal chars until the first unescaped `(`.
    while i < n {
        match s[i] {
            b'\\' => {
                if i + 1 >= n {
                    return None;
                }
                i += 2;
            }
            b'(' => break,
            // Unescaped metacharacters other than `(` mean the SEP is not a
            // plain literal → reject.
            b'[' | b'*' | b'+' | b'?' | b'^' | b'$' | b'|' | b')' | b'{' | b'.' => return None,
            _ => i += 1,
        }
    }
    let sep_end = i;
    if i >= n {
        return None; // no body group at all
    }

    // BODY: one balanced `(...)` group plus any trailing quantifier chars
    // (validated by `universal_lazy_body_min`, not here).
    let close_pos = find_group_close(s, i)?;
    let mut j = close_pos + 1;
    while j < n && matches!(s[j], b'*' | b'+' | b'?') {
        j += 1;
    }
    let body = &mid[sep_end..j];
    let close_pre_start = j;

    // CLOSE_PRE: must be a pure literal (no more groups or metacharacters).
    let mut k = j;
    while k < n {
        match s[k] {
            b'\\' => {
                if k + 1 >= n {
                    return None;
                }
                k += 2;
            }
            b'(' | b'[' | b'*' | b'+' | b'?' | b'^' | b'$' | b'|' | b')' | b'{' | b'.' => {
                return None;
            }
            _ => k += 1,
        }
    }

    Some((&mid[..sep_end], body, &mid[close_pre_start..]))
}

/// Validate that `body` is a balanced group whose repetition unit is a
/// universal single character under a **lazy** quantifier, and return the
/// quantifier's minimum (`*?` → 0, `+?` → 1). The quantifier may sit inside
/// the group (gersemi `([\s\S]+?)`) or outside it (hcl2 `(?:.|\n)*?`).
///
/// Anything else — greedy quantifiers (Python would take the *last* close
/// occurrence), bounded `{m,n}` repeats, or a content-constrained unit like
/// `[0-9]` (the close-scan never validates body content) — returns `None`,
/// so the caller rejects the pattern rather than risk a silent divergence
/// from Python's semantics.
fn universal_lazy_body_min(body: &str) -> Option<usize> {
    // Strip one balanced group layer: `(X)q` or `(?:X)q` → (`X`, outer `q`).
    let s = body.as_bytes();
    if s.first() != Some(&b'(') {
        return None;
    }
    let close = find_group_close(s, 0)?;
    let outer_quant = &body[close + 1..];
    let mut inner = &body[1..close];
    inner = inner.strip_prefix("?:").unwrap_or(inner);

    let (unit, quant) = if outer_quant.is_empty() {
        // Quantifier inside the group: `([\s\S]+?)`.
        match inner {
            i if i.ends_with("*?") || i.ends_with("+?") => (&i[..i.len() - 2], &i[i.len() - 2..]),
            _ => return None,
        }
    } else {
        // Quantifier outside the group: `(?:.|\n)*?`.
        (inner, outer_quant)
    };

    let min = match quant {
        "*?" => 0,
        "+?" => 1,
        _ => return None, // greedy / bounded / double-quantified → reject
    };
    let universal = matches!(unit, r"[\s\S]" | r"[\S\s]" | ".|\\n" | "\\n|.");
    universal.then_some(min)
}

/// Convert a pure regex literal string (no unescaped metacharacters) to the
/// actual bytes it matches. Returns `None` if `s` contains any unescaped
/// regex metacharacter (`.`, `*`, `+`, `?`, `^`, `$`, `|`, `[`, `]`, `(`,
/// `)`, `{`, `}`).
fn unescape_regex_literal(s: &str) -> Option<Vec<u8>> {
    let chars: Vec<char> = s.chars().collect();
    let mut out: Vec<u8> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '.' | '*' | '+' | '?' | '^' | '$' | '|' | '[' | ']' | '(' | ')' | '{' | '}' => {
                return None; // unescaped metacharacter
            }
            '\\' => {
                i += 1;
                if i >= chars.len() {
                    return None;
                }
                let b: u8 = match chars[i] {
                    'n' => b'\n',
                    't' => b'\t',
                    'r' => b'\r',
                    'a' => 0x07,
                    'f' => 0x0c,
                    'v' => 0x0b,
                    '\\' => b'\\',
                    '0' => b'\0',
                    'x' => {
                        // `\xHH` hex escape
                        if i + 2 >= chars.len() {
                            return None;
                        }
                        let h: String = chars[i + 1..=i + 2].iter().collect();
                        let v = u8::from_str_radix(&h, 16).ok()?;
                        i += 2;
                        v
                    }
                    // An escaped ASCII punctuation char is itself (`\[` → `[`).
                    // `\d`/`\w`-style class escapes are NOT literals → reject.
                    c if c.is_ascii_punctuation() || c == ' ' => c as u8,
                    _ => return None, // unrecognised escape in a literal context
                };
                out.push(b);
            }
            c => {
                if c.is_ascii() {
                    out.push(c as u8);
                } else {
                    let mut buf = [0u8; 4];
                    let encoded = c.encode_utf8(&mut buf);
                    out.extend_from_slice(encoded.as_bytes());
                }
            }
        }
        i += 1;
    }
    Some(out)
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
        // A guarded branch whose base prefers a shorter match (order-sensitive
        // alternation, lazy quantifier) is declined with the typed
        // NonRealizableGuardedBase reason (reject-when-unsure) rather than
        // mis-lowered via the longest-accept accumulator.
        for pat in [
            "(ab|abc)(?!z)",
            "ab??(?!c)",
            "(?!z)(ab|abc)",
            r"a.*?(?=c)",
            // The non-nullable all-greedy optional chain, end-to-end through the
            // lowering: the semantic gate's product walk (not the nullability
            // guard) is what declines its guarded base.
            "x(?:a)?(?:ab)?(?!z)",
        ] {
            assert_eq!(
                lower_boundary(pat).unwrap_err().reason,
                DeclineReason::NonRealizableGuardedBase,
                "{pat}"
            );
        }
        // But a greedy-monotone guarded base (and an *unguarded* order-sensitive base)
        // lower fine.
        assert!(lower_boundary("[0-9]+(?![0-9])").is_ok());
        assert!(lower_boundary("ab|abc").is_ok()); // unguarded: order-sensitivity is the engine's job
    }

    /// The **semantic** realizability gate (`is_leftmost_longest`): exact
    /// leftmost-first-equals-longest on the product of the LeftmostFirst and All
    /// DFAs. Must accept the loader-shaped `python.DEC_NUMBER` guarded-arm base
    /// (which both syntactic fast paths miss) and must keep rejecting every
    /// shorter-match-preferring base — including the all-greedy optional-chain
    /// counterexample, where preferring the first optional blocks a longer match.
    #[test]
    fn semantic_gate_decides_leftmost_equals_longest_exactly() {
        // The real loader-baked DEC_NUMBER arm base: optional `(?:_)?` group inside a
        // star — syntactically suspect, semantically descending-by-length.
        assert!(is_leftmost_longest(r"0(?:(?:(?:_)?0))*", false));
        // Longer-first alternation: leftmost-first IS longest.
        assert!(is_leftmost_longest("abc|ab", false));
        // Shorter-first alternation: leftmost-first picks "ab" where longest is "abc".
        assert!(!is_leftmost_longest("ab|abc", false));
        // Lazy quantifier: prefers the shorter match outright.
        assert!(!is_leftmost_longest("ab??", false));
        // ALL-GREEDY counterexample: on "ab", preference order tries "a"+"ab"
        // (fails), then "a" (len 1) — but skipping the first optional gives "ab"
        // (len 2). Greedy-only syntax is NOT sufficient; the semantic gate must
        // catch it.
        assert!(!is_leftmost_longest("(?:a)?(?:ab)?", false));
        // The same shape made NON-nullable by a required head: the conservative
        // nullability guard cannot shadow the product walk here, so this pins the
        // walk itself catching the optional-chain preference inversion ("xab":
        // leftmost-first takes "xa", skipping the first optional gives "xab").
        assert!(!is_leftmost_longest("x(?:a)?(?:ab)?", false));
        // A deeper all-greedy chain where TWO present optionals must be skipped to
        // unlock the longest match ("xabc": leftmost-first "xab" vs longest
        // "xabc") — only the exhaustive product reaches that preference branch.
        assert!(!is_leftmost_longest("x(?:a)?(?:b)?(?:abc)?", false));
        // The positive twin: an optional chain whose arms start on disjoint
        // letters in both case wraps, so skipping an earlier optional can never
        // lengthen the match — all-greedy preference IS descending by length and
        // the gate must keep admitting it (no over-rejection of every chain).
        assert!(is_leftmost_longest("x(?:ab)?(?:b)?", false));
        // Case-folding direction: leftmost-longest holds case-sensitively (the `Ab`
        // arm can never overlap a consumed `a`) but breaks under `(?i)` — the gate
        // requires BOTH wraps to pass, so this must decline.
        assert!(!is_leftmost_longest("(?:a)?(?:Ab)?", false));
        // Nullable base: declined conservatively.
        assert!(!is_leftmost_longest("a*", false));
        // End-to-end: the full loader-baked DEC_NUMBER lowers to two branches with
        // the trailing guard on the zero arm.
        let b = lower_boundary(r"(?:0(?:(?:(?:_)?0))*(?![1-9]))|(?:[1-9](?:(?:(?:_)?[0-9]))*)")
            .expect("the loader-baked DEC_NUMBER must lower");
        assert_eq!(b.len(), 2);
        assert_eq!(b[0].trailing.as_ref().unwrap().set, "[1-9]");
        assert!(b[0].trailing.as_ref().unwrap().neg);
        assert!(b[1].trailing.is_none());
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
    fn regexp_idiom_lowers_to_one_unguarded_branch() {
        let b = lower_boundary(r"\/(?!\/)(\\\/|\\\\|[^\/])*?\/[imslux]*").unwrap();
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].regex, r"\/(\\\/|\\\\|[^\/])+?\/[imslux]*");
        assert!(
            b[0].leading.is_none() && b[0].trailing.is_none() && b[0].lookbehind.is_empty(),
            "the regexp idiom lowers to a single unguarded branch"
        );
    }

    #[test]
    fn regexp_idiom_recognizer_is_exact() {
        // The bundled shape is recognized…
        let node = super::super::parse(r"\/(?!\/)(\\\/|\\\\|[^\/])*?\/[imslux]*").unwrap();
        assert!(recognize_regexp_idiom(&node).is_some());
        // …and every near-miss is not (each deviates in one pinned part).
        for p in [
            r"\#(?!\#)(\\\#|\\\\|[^\#])*?\#[imslux]*", // wrong delimiter
            r"\/(\\\/|\\\\|[^\/])*?\/[imslux]*",       // missing the guard
            r"\/(?!x)(\\\/|\\\\|[^\/])*?\/[imslux]*",  // guard body is not the close
            r"\/(?!\/)((?=a)\\\/|\\\\|[^\/])*?\/[imslux]*", // nested assertion in body
            r"\/(?!\/)(.*?|\\\\|[^\/])*?\/[imslux]*",  // an unrelated lazy `.*?` arm
            r"\/(?!\/)(\\\/|\\\\|[^\/])*\/[imslux]*",  // greedy body, not the lazy `*?`
            r"\/(?!\/)(\\\/|\\\\|[^\/])*?[imslux]*",   // missing the close slash
            r"\/(?!\/)(\\\/|\\\\|[^\/])*?\/[a-z]*",    // different flags suffix
            r"\/(?!\/)(\\\\|\\\/|[^\/])*?\/[imslux]*", // body alternatives reordered
            r"\/(?!\/)(\\\/|\\\\|\\n|[^\/])*?\/[imslux]*", // extra body alternative
        ] {
            let node = super::super::parse(p).unwrap_or_else(|e| panic!("parse {p:?}: {e:?}"));
            assert!(
                recognize_regexp_idiom(&node).is_none(),
                "recognizer wrongly accepted near-miss {p:?}"
            );
        }
    }

    #[test]
    fn long_string_idiom_lowers_to_two_unguarded_branches() {
        const LONG: &str =
            r#"([ubf]?r?|r[ubf])(""".*?(?<!\\)(\\\\)*?"""|'''.*?(?<!\\)(\\\\)*?''')"#;
        // DOTALL (the bundled `/is` case): the body class admits a newline.
        let b = lower_boundary_dotall(LONG, true).unwrap();
        assert_eq!(b.len(), 2);
        assert_eq!(b[0].regex, r#"([ubf]?r?|r[ubf])"""(?:[^\\]|\\.)*?""""#);
        assert_eq!(b[1].regex, r#"([ubf]?r?|r[ubf])'''(?:[^\\]|\\.)*?'''"#);
        for br in &b {
            assert!(
                br.leading.is_none() && br.trailing.is_none() && br.lookbehind.is_empty(),
                "the (?<!\\\\)(\\\\\\\\)*? is absorbed by the body normalization, not \
                 carried as a guard"
            );
        }
        // Non-DOTALL: the body class must exclude the newline the original `.` excludes.
        let b = lower_boundary_dotall(LONG, false).unwrap();
        assert_eq!(b[0].regex, r#"([ubf]?r?|r[ubf])"""(?:[^\\\n]|\\.)*?""""#);
        assert_eq!(b[1].regex, r#"([ubf]?r?|r[ubf])'''(?:[^\\\n]|\\.)*?'''"#);
        // The prefix-less single-arm form (the `newline_dotall_body` fixture's shape)
        // lowers too, to one branch.
        let b = lower_boundary_dotall(r#"""".*?(?<!\\)(\\\\)*?""""#, true).unwrap();
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].regex, r#""""(?:[^\\]|\\.)*?""""#);
    }

    #[test]
    fn long_string_idiom_recognizer_is_exact() {
        // The bundled shape is recognized…
        for p in [
            r#"([ubf]?r?|r[ubf])(""".*?(?<!\\)(\\\\)*?"""|'''.*?(?<!\\)(\\\\)*?''')"#,
            r#"""".*?(?<!\\)(\\\\)*?""""#,
            r#"('''.*?(?<!\\)(\\\\)*?''')"#,
        ] {
            let node = super::super::parse(p).unwrap_or_else(|e| panic!("parse {p:?}: {e:?}"));
            assert!(
                recognize_long_string_idiom(&node).is_some(),
                "recognizer must accept the bundled long-string shape: {p:?}"
            );
        }
        // …and every near-miss is not (each deviates in one pinned part).
        for p in [
            r#"(r?)("".*?(?<!\\)(\\\\)*?"")"#, // two-quote delimiter
            r#"""".*?(?<!\\)(\\\\)*?'''"#,     // mismatched open/close
            r#"""".*?(\\\\)*?""""#,            // missing the lookbehind
            r#"""".*?(?<!x)(\\\\)*?""""#,      // wrong lookbehind body
            r#"""".*?(?<=\\)(\\\\)*?""""#,     // positive lookbehind
            r#"""".*(?<!\\)(\\\\)*?""""#,      // greedy `.*` body
            r#"""".*?(?<!\\)(\\\\)*""""#,      // greedy escape group
            r#"""".*?(?<!\\)(\\)*?""""#,       // wrong escape-group body
            r"\/\/\/.*?(?<!\\)(\\\\)*?\/\/\/", // tripled non-quote delimiter
        ] {
            let node = super::super::parse(p).unwrap_or_else(|e| panic!("parse {p:?}: {e:?}"));
            assert!(
                recognize_long_string_idiom(&node).is_none(),
                "recognizer wrongly accepted near-miss {p:?}"
            );
        }
    }

    #[test]
    fn short_string_idiom_lowers_to_unguarded_branches() {
        const SHORT: &str = r#"(?:".+?(?<!\\)(\\\\)*?")|(?:'.+?(?<!\\)(\\\\)*?')"#;
        // Non-DOTALL (the wild dotmotif case): the body classes exclude the newline.
        let b = lower_boundary_dotall(SHORT, false).unwrap();
        assert_eq!(b.len(), 2);
        assert_eq!(
            b[0].regex,
            r#""(?:\\\\)*(?:[^\\\n]|\\[^\\\n])(?:[^"\\\n]|\\.)*""#
        );
        assert_eq!(
            b[1].regex,
            r#"'(?:\\\\)*(?:[^\\\n]|\\[^\\\n])(?:[^'\\\n]|\\.)*'"#
        );
        for br in &b {
            assert!(
                br.leading.is_none() && br.trailing.is_none() && br.lookbehind.is_empty(),
                "the (?<!\\\\)(\\\\\\\\)*? is absorbed by the body normalization, not \
                 carried as a guard"
            );
        }
        // DOTALL: the classes admit a newline.
        let b = lower_boundary_dotall(r#"".+?(?<!\\)(\\\\)*?""#, true).unwrap();
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].regex, r#""(?:\\\\)*(?:[^\\]|\\[^\\])(?:[^"\\]|\\.)*""#);
    }

    #[test]
    fn short_string_idiom_recognizer_is_exact() {
        // The wild dotmotif shape is recognized (both arms, single arm, either quote)…
        for p in [
            r#"(?:".+?(?<!\\)(\\\\)*?")|(?:'.+?(?<!\\)(\\\\)*?')"#,
            r#"".+?(?<!\\)(\\\\)*?""#,
            r#"'.+?(?<!\\)(\\\\)*?'"#,
        ] {
            let node = super::super::classify::unwrap_vacuous_groups(
                super::super::parse(p).unwrap_or_else(|e| panic!("parse {p:?}: {e:?}")),
            );
            assert!(
                recognize_short_string_idiom(&node).is_some(),
                "recognizer must accept the wild short-string shape: {p:?}"
            );
        }
        // …and every near-miss is not (each deviates in one pinned part). The headline
        // near-miss is the empty-capable `.*?` body: without an opening guard it closes
        // at width 0 on `""` where this rewrite would consume a char.
        for p in [
            r#"".*?(?<!\\)(\\\\)*?""#,  // empty-capable `.*?` body
            r#"".+?(\\\\)*?""#,         // missing the lookbehind
            r#"".+?(?<!x)(\\\\)*?""#,   // wrong lookbehind body
            r#"".+?(?<=\\)(\\\\)*?""#,  // positive lookbehind
            r#"".+(?<!\\)(\\\\)*?""#,   // greedy `.+` body
            r#"".+?(?<!\\)(\\\\)*""#,   // greedy escape group
            r#"".+?(?<!\\)(\\)*?""#,    // wrong escape-group body
            r#"".+?(?<!\\)(\\\\)*?'"#,  // mismatched open/close
            r#"ab.+?(?<!\\)(\\\\)*?b"#, // multi-char opener
        ] {
            let node = super::super::parse(p).unwrap_or_else(|e| panic!("parse {p:?}: {e:?}"));
            assert!(
                recognize_short_string_idiom(&node).is_none(),
                "recognizer wrongly accepted near-miss {p:?}"
            );
        }
    }

    /// The short-string behavioral boundaries the rewrite's section comment proves,
    /// pinned end-to-end against the `regex` crate on the lowered branch: the
    /// quote-leading body (`"""` is one token), the first-close laziness, the
    /// non-empty reject, and escape parity.
    #[test]
    fn short_string_lowered_branch_behavior() {
        let b = lower_boundary_dotall(r#"".+?(?<!\\)(\\\\)*?""#, false).unwrap();
        let re = regex::Regex::new(&format!("^(?:{})", b[0].regex)).unwrap();
        let len_at = |s: &str| re.find(s).map(|m| m.end());
        assert_eq!(
            len_at(r#"""""#),
            Some(3),
            "quote-leading body: one 3-char token"
        );
        assert_eq!(
            len_at(r#"""x""#),
            Some(4),
            "quote-leading body with content"
        );
        assert_eq!(len_at(r#""""""#), Some(3), "still the first close");
        assert_eq!(len_at(r#""""#), None, "the empty body is rejected (`.+?`)");
        assert_eq!(len_at(r#""a"b""#), Some(3), "lazy first close");
        assert_eq!(len_at(r#""\"""#), Some(4), "escaped quote does not close");
        assert_eq!(len_at(r#""\""#), None, "dangling escaped close");
        assert_eq!(
            len_at("\"a\nb\""),
            None,
            "non-DOTALL body excludes the newline"
        );
        // The pure-pair-body boundary the generative net caught in review: a close
        // needs ℓ > r, so `"\\"` has no token (X would be empty) and `"\\""`'s third
        // quote is *body*, closed by the fourth.
        assert_eq!(len_at(r#""\\""#), None, "pure-pair body cannot close");
        assert_eq!(
            len_at(r#""\\"""#),
            Some(5),
            "quote after pure pairs is body"
        );
        assert_eq!(
            len_at(r#""\\a""#),
            Some(5),
            "pairs then content closes normally"
        );
        assert_eq!(
            len_at(r#""\\\\""#),
            None,
            "longer pure-pair body cannot close"
        );
    }

    #[test]
    fn declines_lookbehind_after_variable_prefix() {
        // A lookbehind after a variable-width prefix (`\w+`, `.*?`) has no fixed offset
        // — declined (routed to fancy), the reject-when-unsure direction. The audited
        // long-string idiom now lowers the *complete* bundled shape, but only the exact
        // shape: this truncated near-miss (no closing `"""`) is not the idiom and must
        // still decline — never a generic variable-offset window-carry.
        assert!(lower_boundary(r"\w+(?<!_)x").is_err());
        assert!(lower_boundary(r#"""".*?(?<!\\)(\\\\)*?"#).is_err());
    }

    #[test]
    fn fence_idiom_recognizer_accepts_wild_bank_patterns() {
        // HCL2 heredoc: open=<<, tag=[a-zA-Z][a-zA-Z0-9._-]+, sep=\n, body=(?:.|\n)*?
        let spec = recognize_fence_idiom(
            r"<<(?P<heredoc>[a-zA-Z][a-zA-Z0-9._-]+)\n(?:.|\n)*?(?P=heredoc)",
        )
        .expect("hcl2 heredoc must be recognized");
        assert_eq!(spec.open, b"<<");
        assert_eq!(spec.tag_re, "[a-zA-Z][a-zA-Z0-9._-]+");
        assert_eq!(spec.sep, b"\n");
        assert_eq!(spec.body_min, 0); // `*?` body — an empty heredoc is valid
        assert_eq!(spec.close_pre, b"");
        assert_eq!(spec.close_post, b"");

        // HCL2 heredoc-trim: open=<<-
        let spec = recognize_fence_idiom(
            r"<<-(?P<heredoc_trim>[a-zA-Z][a-zA-Z0-9._-]+)\n(?:.|\n)*?(?P=heredoc_trim)",
        )
        .expect("hcl2 heredoc_trim must be recognized");
        assert_eq!(spec.open, b"<<-");

        // gersemi CMake bracket argument: open=[, tag=(=*), sep=[, close_pre=],
        // close_post=]. The `+?` body demands at least one body char (`body_min`
        // 1): Python rejects `[[]]`, and so must the matcher.
        let spec =
            recognize_fence_idiom(r"\[(?P<equal_signs>(=*))\[([\s\S]+?)\](?P=equal_signs)\]")
                .expect("gersemi bracket arg must be recognized");
        assert_eq!(spec.open, b"[");
        assert_eq!(spec.tag_re, "(=*)");
        assert_eq!(spec.sep, b"[");
        assert_eq!(spec.body_min, 1);
        assert_eq!(spec.close_pre, b"]");
        assert_eq!(spec.close_post, b"]");
    }

    #[test]
    fn fence_idiom_recognizer_rejects_non_fence_patterns() {
        // Regular boundary patterns — not fences.
        assert!(recognize_fence_idiom(r"[a-z]+(?![a-z])").is_none());
        // A named group without a backref.
        assert!(recognize_fence_idiom(r"(?P<name>[a-z]+)foo").is_none());
        // A pattern where the open section is not a literal.
        assert!(recognize_fence_idiom(r"[a-z](?P<name>[a-z]+)(?P=name)").is_none());
        // Two backrefs: too complex.
        assert!(recognize_fence_idiom(r"<(?P<t>[a-z]+)>(?:.|\n)*?(?P=t)(?P=t)").is_none());
    }

    #[test]
    fn fence_idiom_recognizer_demands_a_universal_lazy_body() {
        // No body group at all: Python requires the close IMMEDIATELY after the
        // separator; the forward close-scan can't reproduce that → reject.
        assert!(recognize_fence_idiom(r"<<(?P<t>[A-Z]+)\n(?P=t)").is_none());
        // GREEDY body: Python takes the LAST close occurrence, the scan takes the
        // first → reject rather than silently diverge.
        assert!(recognize_fence_idiom(r"\[(?P<e>(=*))\[([\s\S]+)\](?P=e)\]").is_none());
        assert!(recognize_fence_idiom(r"<<(?P<t>[A-Z]+)\n(?:.|\n)*(?P=t)").is_none());
        // CONTENT-CONSTRAINED body: the close-scan never validates body content →
        // reject (`[0-9]+?` would silently admit non-digit bodies).
        assert!(recognize_fence_idiom(r"<<(?P<t>[A-Z]+)\n([0-9]+?)(?P=t)").is_none());
        // Bounded repeat: not the lazy-to-first-close shape.
        assert!(recognize_fence_idiom(r"<<(?P<t>[A-Z]+)\n(?:.|\n){1,9}?(?P=t)").is_none());
    }
}
