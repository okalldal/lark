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

mod fence;
mod idioms;
mod realizability;
mod width;

// Re-export every submodule item at the `lower::` path so the module's external
// surface (and the in-file references below) stay byte-identical after the #478
// submodule split: callers keep using `crate::lookaround::lower::X` / `super::lower::X`
// for every `X` regardless of which submodule now hosts it.
pub use fence::{recognize_fence_idiom, FenceSpec};
pub use idioms::{
    recognize_long_string_idiom, recognize_regexp_idiom, recognize_short_string_idiom,
    recognize_string_idiom, LongStringIdiom, RegexpIdiom, ShortStringIdiom, StringIdiom,
};
pub(crate) use width::width_range;
// Internal helpers consumed by the boundary lowering below and across submodules
// (`is_guard_realizable`, `max_width_chars`, `fixed_width_chars`, …). The idiom and
// fence recognizers are already re-exported above, so only the two analysis modules
// need a private glob here.
use realizability::*;
use width::*;

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

#[cfg(test)]
mod tests {
    use super::*;

    /// #454: `atom_width_range` sizes a multi-char backslash escape at **1** code point,
    /// matching Python `sre_parse.getwidth()` — it does not re-count the escape's trailing
    /// chars as separate literal atoms. Grounded against `lark.utils.get_regexp_width`
    /// (each `(min, max)` below is what Python reports for the same source).
    #[test]
    fn atom_width_range_escapes_size_one_codepoint() {
        let w = atom_width_range;
        // Hex / unicode escapes: one code point each.
        assert_eq!(w("\\x41"), (1, Some(1)));
        assert_eq!(w("\\uABCD"), (1, Some(1)));
        assert_eq!(w("\\U0001F600"), (1, Some(1)));
        // Octal escapes (leading-0 and 3-digit `\1`–`\7` forms).
        assert_eq!(w("\\012"), (1, Some(1)));
        assert_eq!(w("\\101"), (1, Some(1)));
        // Named character escape.
        assert_eq!(w("\\N{BULLET}"), (1, Some(1)));
        assert_eq!(w("\\N{LATIN SMALL LETTER A}"), (1, Some(1)));
        // Single-char escapes are unaffected (1 for a class/literal, 0 for an anchor).
        assert_eq!(w("\\d"), (1, Some(1)));
        assert_eq!(w("\\."), (1, Some(1)));
        assert_eq!(w("\\b"), (0, Some(0)));
        // A following quantifier binds to the *whole* escape, not its last digit.
        assert_eq!(w("\\x41?"), (0, Some(1)));
        assert_eq!(w("\\x41+"), (1, None));
        assert_eq!(w("\\x41{2,3}"), (2, Some(3)));
        // Concatenations sum correctly.
        assert_eq!(w("A\\x41"), (2, Some(2)));
        assert_eq!(w("\\x41\\012\\u0041"), (3, Some(3)));
        // A `\xHH` *inside a character class* is consumed by the class walk, not the
        // escape arm — the class is still one code point.
        assert_eq!(w("[\\x41-\\x5a]"), (1, Some(1)));
    }

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
