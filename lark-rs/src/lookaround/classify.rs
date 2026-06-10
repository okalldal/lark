//! Shape classifier + lowering entry point — the safety boundary the L2
//! bounded-lookaround lowering is gated by (`docs/LEXER_DFA_PLAN.md`, "What we
//! support" + "How the lowering works").
//!
//! [`lower_terminal`] is the entry point the build path calls. It classifies a
//! terminal and, for every supported shape whose lowering has landed — M1
//! trailing-boundary, M2 leading-boundary, M3 fixed-offset bounded-lookbehind, the
//! M4 `python.STRING` opening-guard splice, and the Stage-B `lark.REGEXP` /
//! `python.LONG_STRING` delimited-token idioms — returns the lowered per-branch
//! sub-patterns
//! ([`super::lower`]). A per-instance lowering that cannot ride the engine (a
//! variable-offset lookbehind, a non-realizable guarded base) is **declined** (routed to
//! `fancy-regex`), and an out-of-shape assertion is reported as a permanent rejection
//! `GrammarError`. No supported shape is *pending* any longer — all four lowerings are
//! live. The classifier is the safety boundary: it decides, for each terminal pattern,
//! whether the assertion(s) fall into a **supported shape** or an **unsupported** one —
//! and it must never false-accept.
//!
//! **Caveat — "rejected" here is the classifier verdict, not yet the runtime outcome.**
//! This entry point *reports* an unsupported assertion as a `GrammarError`. The
//! decline-vs-reject split PR #131 flagged is now **typed**: [`route_terminal_dotall`]
//! returns a [`LoweringRoute`] that distinguishes [`LoweringRoute::Declined`] (a
//! transitional route to `fancy-regex`) from [`LoweringRoute::Unsupported`] (the final L4
//! reject path). The `DfaScanner` build path matches that route directly — but as a
//! **compatibility fallback** it still routes `Unsupported` to `fancy-regex` today (so an
//! out-of-shape *user* assertion lexes rather than failing the build). L4 must flip only
//! that one route to a build error (`docs/LEXER_DFA_PLAN.md`, "Runtime routing taxonomy").
//! The classifier's *own* contract below is unaffected: it must still reject-when-unsure
//! and never false-accept. The [`lower_terminal`] / [`lower_terminal_dotall`] API is
//! retained as a thin `Result`-flattening of the route for existing callers.
//!
//! ## The classifier's contract
//!
//! The dangerous direction is **false-accept** — classifying an out-of-shape
//! assertion as supported, which would later be mis-lowered. So the rule is
//! *reject when unsure*. The three supported shapes (`docs/LEXER_DFA_PLAN.md`):
//!
//!   * **Leading boundary** — a fixed-position lookahead `(?=S)` / `(?!S)` at the
//!     start of the match. Lowered by splicing peek-branch states.
//!   * **Trailing boundary** — a lookahead `X(?=S)` / `X(?!S)` at the end of the
//!     match. Lowered as a *guarded accept* (the maximal-munch driver records the
//!     accept only when the next byte is allowed).
//!   * **Bounded lookbehind** — `(?<=…)` / `(?<!…)` of *bounded* width, anywhere.
//!     Lowered by carrying the needed history window in the DFA state.
//!
//! Everything else is **rejected** with a clear, actionable [`GrammarError`] naming
//! the terminal, the assertion, and the reason: unbounded-width lookahead
//! (`(?![ ]*X)`), an *internal* (mid-pattern / priority-entangled) lookahead, a
//! backreference, a nested assertion, or a variable-width lookbehind.
//!
//! The recognized idioms are **narrow gates, not general internal-lookahead support.**
//! `python.STRING`'s `(?!"")` sits *after a variable-width prefix + the opening quote*,
//! and `lark.REGEXP`'s `(?!\/)` sits *between the opening slash and the lazy body* —
//! positions the top-level walk sees as `Internal`. Each lowers only because its exact
//! recognizer ([`super::lower::recognize_string_idiom`] /
//! [`super::lower::recognize_regexp_idiom`]) matches that *precise* terminal shape and
//! re-tags its interior lookaheads as `Leading`; outside a recognized idiom a deeper
//! lookahead stays `Internal` (reject). The third idiom, `python.LONG_STRING`
//! ([`super::lower::recognize_long_string_idiom`]), needs **no** re-tag at all: its only
//! assertions are bounded lookbehinds, which classify as supported at any position — the
//! recognizer's job there is purely to let the *lowering* absorb them. The recognizer —
//! never a position heuristic — is the single gate, so the dangerous direction
//! (false-accept) stays closed. **Every bundled lookaround terminal now lowers**; the
//! decline-to-fancy route remains only for per-instance cases (a variable-offset
//! lookbehind outside the idiom, a non-realizable guarded base); see
//! [`LEXER_DFA_STATUS.md`](../../docs/LEXER_DFA_STATUS.md).

use super::{Look, Node};
use crate::error::GrammarError;

/// Where, structurally, an assertion sits in the terminal's *top-level* shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Position {
    /// First element of a top-level concatenation / alternation branch.
    Leading,
    /// Last element of a top-level concatenation / alternation branch.
    Trailing,
    /// Anywhere else — mid-concat, or nested inside a group/repetition. For a
    /// lookahead this is the priority-entangled case the plan rejects.
    Internal,
}

/// One of the three supported lowering shapes (`docs/LEXER_DFA_PLAN.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShapeClass {
    LeadingBoundary,
    TrailingBoundary,
    BoundedLookbehind,
}

impl ShapeClass {
    pub fn describe(self) -> &'static str {
        match self {
            ShapeClass::LeadingBoundary => "leading-boundary",
            ShapeClass::TrailingBoundary => "trailing-boundary",
            ShapeClass::BoundedLookbehind => "bounded-lookbehind",
        }
    }
}

/// The two-category scope taxonomy every refused lookaround pattern falls into
/// (`docs/LOOKAROUND_SCOPE.md`). The category is part of the build-error contract:
/// the scope scoreboard (`tests/test_lookaround_scope.rs`) asserts it end-to-end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// A by-design non-goal: never lowered, permanently rejected at grammar build.
    /// End-to-end tests assert these rejections as the contract.
    OutOfScope,
    /// In-principle lowerable, rejected conservatively today — never silently
    /// mis-lowered. The scoreboard entries for this category are **promotion
    /// tripwires**: if such a pattern starts lowering, the test fails loudly, and
    /// promotion requires the Stage-B audit ladder (`docs/LEXER_DFA_PLAN.md`).
    NotYetImplemented,
}

/// Why an assertion is out of the supported set (rejected at build time).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    /// A lookahead whose body has no finite maximum width — `(?![ ]*X)`.
    Unbounded,
    /// A lookahead that is neither the first nor the last element of the match —
    /// the priority-entangled case where greedy/lazy match-length is not
    /// reproducible by a per-state guard.
    Internal,
    /// The assertion body uses a backreference (`\1`).
    Backref,
    /// The assertion body itself contains another assertion.
    Nested,
    /// A lookbehind whose body has no finite maximum width — `(?<!a*)`.
    VariableWidthBehind,
    /// The assertion itself carries a quantifier — `(?=a)?`, `(?!b){0}`. Degenerate
    /// and priority-entangled; rejected rather than guessed (reject-when-unsure).
    QuantifiedAssertion,
}

impl Rejection {
    /// Which scope category this rejection belongs to (`docs/LOOKAROUND_SCOPE.md`).
    /// Deliberately a non-wildcard match: adding a `Rejection` variant forces a
    /// conscious category decision here, and the scoreboard's exhaustiveness
    /// meta-test forces a matching scoreboard entry.
    pub fn scope(self) -> Scope {
        match self {
            // Non-regular (backrefs) or degenerate/priority-entangled constructs,
            // and the two shapes ruled out by design: general internal lookahead
            // (the audited delimited-token idioms are the sanctioned growth path)
            // and variable-width lookbehind bodies (Python `re` rejects those too,
            // so rejection is oracle parity).
            Rejection::Backref
            | Rejection::Nested
            | Rejection::QuantifiedAssertion
            | Rejection::VariableWidthBehind
            | Rejection::Internal => Scope::OutOfScope,
            // Unbounded trailing-context lookahead is a regular-language construct
            // (classic lex trailing context) — implementable, just not implemented.
            Rejection::Unbounded => Scope::NotYetImplemented,
        }
    }

    /// A human-readable reason + a fix suggestion, for the build error.
    pub fn explain(self) -> &'static str {
        match self {
            Rejection::Unbounded => {
                "unbounded-width lookahead — its body can match arbitrarily many \
                 characters, so it is not a fixed-window boundary assertion. Bound \
                 the body's width (e.g. drop a `*`/`+` quantifier) or restructure \
                 the terminal."
            }
            Rejection::Internal => {
                "internal (mid-pattern) lookahead — it is neither at the start nor \
                 the end of the match, so its match-length under greedy/lazy \
                 priority cannot be reproduced by a per-state guard. Move the \
                 assertion to a token boundary, or split the terminal."
            }
            Rejection::Backref => {
                "the assertion body uses a backreference, which is not a regular \
                 language and cannot be lowered into a DFA. Rewrite without the \
                 backreference."
            }
            Rejection::Nested => {
                "the assertion body contains another assertion; nested assertions \
                 are not supported. Flatten the assertion."
            }
            Rejection::VariableWidthBehind => {
                "variable-width lookbehind — its body has no fixed maximum width, so \
                 the history window it needs is unbounded. Use a fixed-width \
                 lookbehind."
            }
            Rejection::QuantifiedAssertion => {
                "the assertion carries a quantifier (e.g. `(?=…)?`), which is \
                 degenerate and priority-entangled. Remove the quantifier."
            }
        }
    }
}

/// Why the *lowering* refused a particular instance of a supported shape (or the
/// frontend could not analyze the pattern at all) — the typed successor of the
/// free-form decline string. Historically these routed to `fancy-regex` at runtime;
/// since L4 they are **build errors**, categorized by [`DeclineReason::scope`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeclineReason {
    /// `(?<=…){n}` — the lookbehind itself carries a quantifier (degenerate).
    QuantifiedLookbehind,
    /// A fixed-width lookbehind sitting after a variable-width prefix, outside a
    /// recognized idiom — its offset from the match start is not fixed. The
    /// headline NotYetImplemented case (Python `re` accepts these).
    VariableOffsetLookbehind,
    /// The lookbehind body has no fixed maximum width (defensive twin of
    /// [`Rejection::VariableWidthBehind`] at the lowering layer).
    UnboundedLookbehindBody,
    /// The lookbehind body can match empty — degenerate (always/never satisfiable
    /// at width 0).
    ZeroWidthLookbehindBody,
    /// An interior forward lookahead reached the lowering (defensive twin of
    /// [`Rejection::Internal`]; the classifier rejects these first).
    InteriorLookahead,
    /// The branch is nothing but boundary assertions — a zero-width terminal
    /// branch, which the lexer forbids.
    ZeroWidthBranch,
    /// An assertion is nested inside a group (or a flag wrapper the strip did not
    /// unwrap), so it cannot be peeled to a fixed offset.
    NestedInGroup,
    /// A guarded branch's base is not greedy-monotone (order-sensitive alternation,
    /// lazy/possessive quantifier), so the longest-accept accumulator cannot
    /// reproduce its match length.
    NonRealizableGuardedBase,
    /// The string idiom's empty arm has a non-length-deterministic prefix, so its
    /// trailing guard is not realizable.
    EmptyArmNotRealizable,
    /// The lookaround frontend could not parse the pattern at all.
    FrontendParse,
    /// A whole-pattern `(?x:…)` VERBOSE wrapper: the frontend's width/offset
    /// analysis is not verbose-aware, so the wrapper is deliberately not stripped
    /// and the pattern is not analyzed (see
    /// `lexer::strip_whole_pattern_flag_wrapper`).
    VerboseWrapper,
    /// The pattern has no lookaround at all but still fails to compile on the
    /// `regex` crate — backtracking-only syntax such as a top-level backreference,
    /// an atomic group, or a possessive quantifier.
    BacktrackingOnlySyntax,
}

impl DeclineReason {
    /// Which scope category this decline belongs to (`docs/LOOKAROUND_SCOPE.md`).
    /// Non-wildcard match — a new variant forces a conscious category decision,
    /// and the scoreboard's exhaustiveness meta-test forces a matching entry.
    pub fn scope(self) -> Scope {
        match self {
            // Degenerate constructs and non-regular syntax: by-design non-goals.
            DeclineReason::QuantifiedLookbehind
            | DeclineReason::UnboundedLookbehindBody
            | DeclineReason::ZeroWidthLookbehindBody
            | DeclineReason::InteriorLookahead
            | DeclineReason::ZeroWidthBranch
            | DeclineReason::BacktrackingOnlySyntax => Scope::OutOfScope,
            // Regular-language instances the machinery could be extended to host —
            // rejected conservatively, promotion path documented.
            DeclineReason::VariableOffsetLookbehind
            | DeclineReason::NestedInGroup
            | DeclineReason::NonRealizableGuardedBase
            | DeclineReason::EmptyArmNotRealizable
            | DeclineReason::FrontendParse
            | DeclineReason::VerboseWrapper => Scope::NotYetImplemented,
        }
    }

    /// One actionable sentence per reason, for the build error and the scope doc.
    pub fn explain(self) -> &'static str {
        match self {
            DeclineReason::QuantifiedLookbehind => {
                "the lookbehind carries a quantifier, which is degenerate. Remove \
                 the quantifier."
            }
            DeclineReason::VariableOffsetLookbehind => {
                "a fixed-width lookbehind sits after a variable-width prefix, so its \
                 offset from the match start is not fixed. Restructure the terminal \
                 so the lookbehind's position is fixed, or split the terminal."
            }
            DeclineReason::UnboundedLookbehindBody => {
                "the lookbehind body has no fixed maximum width. Use a fixed-width \
                 lookbehind (Python `re` requires this too)."
            }
            DeclineReason::ZeroWidthLookbehindBody => {
                "the lookbehind body can match the empty string, which is \
                 degenerate. Make the body's width at least 1."
            }
            DeclineReason::InteriorLookahead => {
                "an interior (mid-pattern) lookahead is priority-entangled. Move \
                 the assertion to a token boundary, or split the terminal."
            }
            DeclineReason::ZeroWidthBranch => {
                "the branch consists only of boundary assertions, so it matches the \
                 empty string; the lexer forbids zero-width terminals."
            }
            DeclineReason::NestedInGroup => {
                "the assertion is nested inside a group, so it cannot be analyzed \
                 at a fixed offset. Lift the assertion to the top level of the \
                 pattern."
            }
            DeclineReason::NonRealizableGuardedBase => {
                "the guarded base has an order-sensitive alternation or a lazy/\
                 possessive quantifier, so its match length under a guard is not \
                 reproducible by the longest-accept scan. Make the base \
                 greedy-monotone, or split the terminal."
            }
            DeclineReason::EmptyArmNotRealizable => {
                "the empty-string arm's prefix is not length-deterministic, so its \
                 trailing guard cannot be realized."
            }
            DeclineReason::FrontendParse => {
                "the lookaround analyzer could not parse the pattern. Simplify the \
                 pattern, or report the construct so the analyzer can learn it."
            }
            DeclineReason::VerboseWrapper => {
                "the pattern is wrapped in a VERBOSE `(?x:…)` flag group, which the \
                 lookaround analyzer does not understand (whitespace/comments would \
                 be miscounted as literal width). Rewrite the terminal without the \
                 `x` flag."
            }
            DeclineReason::BacktrackingOnlySyntax => {
                "the pattern uses backtracking-only syntax (e.g. a backreference, \
                 an atomic group, or a possessive quantifier), which is not a \
                 regular language and cannot run on a DFA. Rewrite the pattern \
                 without it. (Note: this is a deliberate parity break with Python \
                 Lark, which runs on a backtracking engine.)"
            }
        }
    }
}

/// The verdict for a single assertion: a supported shape (a future session lowers
/// it) or a rejection reason (rejected at build time, always).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Supported(ShapeClass),
    Rejected(Rejection),
}

/// One assertion in a terminal, with everything the classifier and the build-error
/// message need. Deterministic and engine-agnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssertionInfo {
    /// The exact assertion source, e.g. `"(?!\"\")"`, for the build message.
    pub source: String,
    pub neg: bool,
    pub look: Look,
    /// Maximum match width of the body in characters, or `None` if unbounded.
    pub width: Option<usize>,
    pub position: Position,
    pub has_backref: bool,
    pub has_nested: bool,
    /// Whether the assertion node itself carries a trailing quantifier (`(?=a)?`).
    pub has_quant: bool,
}

impl AssertionInfo {
    /// Classify this single assertion into a [`Verdict`]. The order of checks is the
    /// reject-when-unsure priority: non-regular bodies first, then width, then
    /// position.
    pub fn verdict(&self) -> Verdict {
        if self.has_backref {
            return Verdict::Rejected(Rejection::Backref);
        }
        if self.has_nested {
            return Verdict::Rejected(Rejection::Nested);
        }
        if self.has_quant {
            return Verdict::Rejected(Rejection::QuantifiedAssertion);
        }
        match (self.look, self.width) {
            // A bounded lookbehind is lowerable wherever it sits (carried in state).
            (Look::Behind, Some(_)) => Verdict::Supported(ShapeClass::BoundedLookbehind),
            (Look::Behind, None) => Verdict::Rejected(Rejection::VariableWidthBehind),
            (Look::Ahead, None) => Verdict::Rejected(Rejection::Unbounded),
            (Look::Ahead, Some(_)) => match self.position {
                Position::Leading => Verdict::Supported(ShapeClass::LeadingBoundary),
                Position::Trailing => Verdict::Supported(ShapeClass::TrailingBoundary),
                Position::Internal => Verdict::Rejected(Rejection::Internal),
            },
        }
    }
}

/// The full classification of one terminal pattern: every assertion it contains,
/// each with its verdict. A terminal with no assertions is *plain* (an empty list).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Classification {
    pub assertions: Vec<AssertionInfo>,
}

impl Classification {
    /// No lookaround at all — a plain regular language, nothing to lower.
    pub fn is_plain(&self) -> bool {
        self.assertions.is_empty()
    }

    /// At least one assertion, and *every* assertion is a supported shape.
    pub fn is_fully_supported(&self) -> bool {
        !self.assertions.is_empty()
            && self
                .assertions
                .iter()
                .all(|a| matches!(a.verdict(), Verdict::Supported(_)))
    }

    /// The first assertion the classifier rejects, if any (with its reason).
    pub fn first_rejection(&self) -> Option<(&AssertionInfo, Rejection)> {
        self.assertions.iter().find_map(|a| match a.verdict() {
            Verdict::Rejected(r) => Some((a, r)),
            _ => None,
        })
    }

    /// The first supported assertion, if any (with its shape).
    pub fn first_supported(&self) -> Option<(&AssertionInfo, ShapeClass)> {
        self.assertions.iter().find_map(|a| match a.verdict() {
            Verdict::Supported(s) => Some((a, s)),
            _ => None,
        })
    }
}

/// Classify a terminal regex `pattern` into its per-assertion verdicts. Errors only
/// on a pattern the front-end cannot parse (which the regex engines also reject).
///
/// This is the swappable seam the mutation meta-test attacks: see [`Classifier`].
pub fn classify(pattern: &str) -> Result<Classification, GrammarError> {
    let node = super::parse(pattern)?;
    let mut assertions = Vec::new();
    walk_top_level(&node, &mut assertions);
    // Audited-idiom refinement (the recognizers are the single gate, never a position
    // heuristic that could false-accept):
    //   * **python.STRING** ([`super::lower::recognize_string_idiom`]): a leading
    //     `(?!"")` after a variable-width prefix + the opening quote is seen by the
    //     top-level walk as `Internal` (it is nested inside the arms group, after a
    //     bounded prefix) but is a *supported* leading boundary the splice lowers.
    //   * **lark.REGEXP** ([`super::lower::recognize_regexp_idiom`]): the `(?!\/)`
    //     between the opening slash and the lazy body is likewise `Internal` to the
    //     walk, but inside the exact recognized idiom it reduces to a non-empty-body
    //     condition the delimited-token lowering absorbs.
    // So **only when the whole terminal is a recognized, lowerable idiom**, re-tag its
    // interior `(?=)/(?!)` lookaheads as `Leading`. Outside a recognized idiom the
    // verdict is unchanged, so a genuinely-internal lookahead (`a(?=b)c`, the verilog
    // `(?!/)` inside a `*`) stays `Internal` (rejected).
    //
    // The third idiom — **python.LONG_STRING** (`recognize_long_string_idiom`) — is
    // deliberately absent here: its only assertions are bounded lookbehinds, which the
    // walk already classifies `Supported(BoundedLookbehind)` position-independently, so
    // there is nothing to re-tag; its recognizer gates only the *lowering*.
    if super::lower::recognize_string_idiom(&node).is_some()
        || super::lower::recognize_regexp_idiom(&node).is_some()
    {
        for a in &mut assertions {
            if a.look == Look::Ahead && a.position == Position::Internal {
                a.position = Position::Leading;
            }
        }
    }
    Ok(Classification { assertions })
}

/// A classifier — the unit the mutation meta-test swaps to prove the harness
/// catches a deliberately-wrong implementation. [`DefaultClassifier`] is the real
/// one; mutants live in the test tree.
pub trait Classifier {
    fn classify(&self, pattern: &str) -> Result<Classification, GrammarError>;
}

/// The real classifier (delegates to [`classify`]).
#[derive(Debug, Default, Clone, Copy)]
pub struct DefaultClassifier;

impl Classifier for DefaultClassifier {
    fn classify(&self, pattern: &str) -> Result<Classification, GrammarError> {
        classify(pattern)
    }
}

/// The outcome of lowering one terminal (`docs/LEXER_DFA_PLAN.md`, "How the lowering
/// works"). A plain terminal needs no lowering; a supported terminal lowers into
/// lookaround-free sub-patterns the combined DFA hosts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lowered {
    /// No lookaround assertion — the terminal is already a plain regular language.
    Plain,
    /// A lowered terminal: its top-level alternation split into per-branch
    /// sub-patterns, each a lookaround-free base regex plus optional leading/trailing
    /// boundary guards (M1 trailing, M2 leading). Sibling branches may differ in
    /// whether they carry a guard.
    Branches(Vec<super::lower::LoweredBranch>),
}

/// A **typed routing decision** for one terminal — the explicit decline-vs-reject split
/// PR #131 documented as design debt (`docs/LEXER_DFA_PLAN.md`, "Runtime routing
/// taxonomy"). The historical [`Lowered`] / `Result<Lowered, GrammarError>` API collapses
/// the last three variants into a single `Err`, conflating a *transitional decline to
/// `fancy-regex`* with a *permanent out-of-shape rejection*. This enum keeps them distinct
/// in the type, so the build path ([`crate::lexer`]'s `DfaScanner`) can route each
/// explicitly instead of catching any `Err` and falling back. [`lower_terminal`] still
/// flattens this back to `Result` for existing callers and tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoweringRoute {
    /// No lookaround assertion. A pattern that already compiled on the `regex` crate is
    /// handled by the plain DFA path *before* it reaches routing; a pattern that reaches
    /// the route path and classifies `Plain` only does so because it is fancy-only for some
    /// *other* reason (e.g. a top-level backreference outside any lookaround), and the
    /// `DfaScanner` keeps the compatibility fallback for it.
    Plain,
    /// A supported shape that lowered into lookaround-free per-branch sub-patterns — the
    /// DFA hosts it directly.
    Lowered(Vec<super::lower::LoweredBranch>),
    /// A terminal the lowering **declined**: every assertion is a supported *shape*,
    /// but this particular instance cannot ride the lowered engine (a variable-offset
    /// lookbehind outside a recognized idiom, a non-greedy-monotone guarded base), or
    /// the frontend could not analyze the pattern. Since L4 this is a **categorized
    /// build error** ([`DeclineReason::scope`] — most declines are
    /// [`Scope::NotYetImplemented`]); historically it routed to `fancy-regex`.
    /// **No bundled terminal is here**: `python.STRING` (M4 splice), `lark.REGEXP`,
    /// and `python.LONG_STRING` (the Stage-B delimited-token idioms) all route
    /// [`Self::Lowered`].
    Declined {
        /// The typed reason — the scoreboard key.
        reason: DeclineReason,
        /// The scope-phrased build message naming the terminal and the cause.
        message: String,
    },
    /// The classifier rejects this assertion as **out-of-shape** ([`Classification::first_rejection`]).
    /// Since L4 this is a **categorized build error** ([`Rejection::scope`] — most
    /// rejections are [`Scope::OutOfScope`] by design).
    Unsupported {
        /// The offending assertion's exact source, e.g. `"(?=b)"`.
        assertion: String,
        /// Why the classifier rejected it.
        rejection: Rejection,
        /// The full build-error message naming the terminal and the assertion.
        message: String,
    },
    /// Neither the lookaround frontend nor any regex engine can host the pattern — a
    /// genuine build error. **Reserved:** the current router never constructs this (a
    /// frontend-only parse failure prefers [`Self::Declined`] to preserve
    /// `fancy-regex` compatibility), per the PR scope.
    Invalid {
        /// The build-error message.
        message: String,
    },
}

/// **THE ROUTING ENTRY POINT** (non-dotall). Classifies `pattern` and returns the typed
/// [`LoweringRoute`] the build path matches on. The dotall-aware [`route_terminal_dotall`]
/// is the real worker; this wrapper passes `dotall = false`.
pub fn route_terminal(name: &str, pattern: &str) -> LoweringRoute {
    route_terminal_dotall(name, pattern, false)
}

/// [`route_terminal`] aware of whether the terminal's flags include `DOTALL` (`s`) — the
/// engine passes the real flag so the string-idiom body normalization admits a newline
/// exactly when the terminal's `.` would. `dotall` is inert for every shape other than the
/// string idiom.
pub fn route_terminal_dotall(name: &str, pattern: &str, dotall: bool) -> LoweringRoute {
    route_terminal_with(&DefaultClassifier, name, pattern, dotall)
}

/// [`route_terminal_dotall`] over an explicit classifier — the mutation meta-test drives a
/// mutant classifier through this same routing entry point.
pub fn route_terminal_with(
    classifier: &dyn Classifier,
    name: &str,
    pattern: &str,
    dotall: bool,
) -> LoweringRoute {
    let classification = match classifier.classify(pattern) {
        Ok(c) => c,
        // The lookaround frontend could not parse the pattern — a frontend
        // limitation, not proof the pattern is out of scope, so it declines as
        // NotYetImplemented (the conservative direction is still a clean reject,
        // never a mis-lowering).
        Err(e) => {
            return LoweringRoute::Declined {
                reason: DeclineReason::FrontendParse,
                message: scope_message(
                    name,
                    pattern,
                    LookaroundIssue::Declined(DeclineReason::FrontendParse),
                    &format!("{} ({e})", DeclineReason::FrontendParse.explain()),
                ),
            };
        }
    };
    if classification.is_plain() {
        return LoweringRoute::Plain;
    }
    // An out-of-shape assertion is the firmer verdict — report it before attempting to
    // lower the (supported) remainder.
    if let Some((info, rejection)) = classification.first_rejection() {
        return LoweringRoute::Unsupported {
            assertion: info.source.clone(),
            rejection,
            message: unsupported_message(name, &info.source, rejection),
        };
    }
    // Every assertion is a supported shape. Run the lowering; it may still **decline**
    // a particular instance (a non-greedy-monotone guarded base, a lookbehind after a
    // variable-width prefix outside a recognized idiom) with a typed reason — a clean
    // categorized refusal, never a mis-lowering.
    match super::lower::lower_boundary_dotall(pattern, dotall) {
        Ok(branches) => LoweringRoute::Lowered(branches),
        Err(d) => LoweringRoute::Declined {
            reason: d.reason,
            message: scope_message(
                name,
                pattern,
                LookaroundIssue::Declined(d.reason),
                &d.detail,
            ),
        },
    }
}

/// Why a lookaround terminal was refused, typed for the scope scoreboard — either the
/// classifier rejected an out-of-shape assertion or the lowering declined the instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LookaroundIssue {
    Rejected(Rejection),
    Declined(DeclineReason),
}

impl LookaroundIssue {
    pub fn scope(self) -> Scope {
        match self {
            LookaroundIssue::Rejected(r) => r.scope(),
            LookaroundIssue::Declined(d) => d.scope(),
        }
    }
}

/// **THE build-error message builder** — the single place the scope-phrased refusal
/// text is produced, shared by the route arms, the flattened [`lower_terminal`] error,
/// and the engine build path, so the wording (and the scoreboard's `msg_contains`
/// pins) can never drift between sites. `subject` is the assertion source for a
/// rejection, the whole pattern for a decline; `detail` is the site-specific cause.
pub fn scope_message(name: &str, subject: &str, issue: LookaroundIssue, detail: &str) -> String {
    match issue.scope() {
        Scope::OutOfScope => format!(
            "terminal `{name}`: lookaround in `{subject}` is not supported (by design): \
             {detail} This is a permanent non-goal; see docs/LOOKAROUND_SCOPE.md."
        ),
        Scope::NotYetImplemented => format!(
            "terminal `{name}`: lookaround in `{subject}` is not yet implemented: {detail} \
             It is rejected conservatively rather than risk a mis-lex; see \
             docs/LOOKAROUND_SCOPE.md for the promotion path."
        ),
    }
}

/// The build-error message for an out-of-shape (rejected) assertion — names the terminal
/// and shows the assertion source. Shared by the route's [`LoweringRoute::Unsupported`]
/// arm and the flattened [`lower_terminal`] error so the two never drift.
fn unsupported_message(name: &str, source: &str, rejection: Rejection) -> String {
    scope_message(
        name,
        source,
        LookaroundIssue::Rejected(rejection),
        rejection.explain(),
    )
}

/// **THE LOWERING ENTRY POINT.**
///
/// A plain terminal returns [`Lowered::Plain`]. A terminal whose every assertion is a
/// supported shape (leading/trailing boundary, bounded lookbehind) is lowered to
/// [`Lowered::Branches`] — unless the lowering *declines* a particular instance (a
/// non-greedy-monotone guarded base, or a lookbehind after a variable-width prefix),
/// in which case it returns `Err` and the caller routes that terminal to
/// `fancy-regex`. A terminal with an out-of-shape assertion is rejected permanently
/// (`Err`). Either error names the terminal and the offending assertion.
///
/// This is a thin compatibility flattening of [`route_terminal`]: the
/// `Declined` / `Unsupported` / `Invalid` routes all collapse to `Err`, which is
/// why a caller that only checks `Ok(Branches)` cannot tell a decline from a reject. The
/// build path uses [`route_terminal_dotall`] directly to keep them apart.
pub fn lower_terminal(name: &str, pattern: &str) -> Result<Lowered, GrammarError> {
    lower_terminal_dotall(name, pattern, false)
}

/// [`lower_terminal`] aware of whether the terminal's flags include `DOTALL` (`s`) — the
/// engine passes the real flag so the string-idiom body normalization admits a newline
/// exactly when the terminal's `.` would (`docs/LEXER_DFA_PLAN.md`). `dotall` is inert
/// for every shape other than the string idiom.
pub fn lower_terminal_dotall(
    name: &str,
    pattern: &str,
    dotall: bool,
) -> Result<Lowered, GrammarError> {
    lower_terminal_with(&DefaultClassifier, name, pattern, dotall)
}

/// [`lower_terminal`] over an explicit classifier — the mutation meta-test drives a
/// mutant classifier through this same entry point.
pub fn lower_terminal_with(
    classifier: &dyn Classifier,
    name: &str,
    pattern: &str,
    dotall: bool,
) -> Result<Lowered, GrammarError> {
    // Flatten the typed route back to the historical `Result`. A *decline* and an
    // out-of-shape *reject* both collapse to `Err` here (now the typed
    // `GrammarError::LookaroundScope`, carrying the category) — the conflation
    // `DfaScanner` avoids by matching the route directly.
    match route_terminal_with(classifier, name, pattern, dotall) {
        LoweringRoute::Plain => Ok(Lowered::Plain),
        LoweringRoute::Lowered(branches) => Ok(Lowered::Branches(branches)),
        LoweringRoute::Declined { reason, message } => Err(GrammarError::LookaroundScope {
            terminal: name.to_string(),
            subject: pattern.to_string(),
            scope: reason.scope(),
            issue: LookaroundIssue::Declined(reason),
            msg: message,
        }),
        LoweringRoute::Unsupported {
            assertion,
            rejection,
            message,
        } => Err(GrammarError::LookaroundScope {
            terminal: name.to_string(),
            subject: assertion,
            scope: rejection.scope(),
            issue: LookaroundIssue::Rejected(rejection),
            msg: message,
        }),
        LoweringRoute::Invalid { message } => Err(GrammarError::Other { msg: message }),
    }
}

/// True iff `e` is a *pending-shape* rejection — a supported assertion shape whose
/// lowering has not yet landed (as opposed to a permanent out-of-shape rejection or a
/// per-instance decline-to-fancy). All three supported shapes now lower (M1/M2/M3), so
/// no shape is pending and this is always `false`; it is retained so the harness's
/// pending-vs-permanent bucketing keeps a stable name. A per-instance *decline* (a
/// variable-offset lookbehind, a non-greedy-monotone base) is **not** pending — it is
/// a routed-to-fancy outcome the differential records as a skip.
pub fn is_pending_shape_error(e: &GrammarError) -> bool {
    matches!(e, GrammarError::Other { msg } if msg.contains("pending first shape"))
}

// ─── The classification walk ──────────────────────────────────────────────────

/// Build the assertion source text from its parts, for the build message.
fn assertion_source(neg: bool, look: Look, body: &Node) -> String {
    let open = match (look, neg) {
        (Look::Ahead, false) => "(?=",
        (Look::Ahead, true) => "(?!",
        (Look::Behind, false) => "(?<=",
        (Look::Behind, true) => "(?<!",
    };
    format!("{open}{})", body.to_source())
}

fn make_info(neg: bool, look: Look, body: &Node, quant: &str, position: Position) -> AssertionInfo {
    AssertionInfo {
        source: assertion_source(neg, look, body),
        neg,
        look,
        width: max_width(body),
        position,
        has_backref: has_backref(body),
        has_nested: body.has_assertion(),
        has_quant: !quant.is_empty(),
    }
}

/// Walk the terminal's top-level shape, tagging each assertion's [`Position`]. Only
/// top-level `Concat` / `Alt` structure carries leading/trailing information;
/// anything found below a `Group` is [`Position::Internal`].
fn walk_top_level(node: &Node, out: &mut Vec<AssertionInfo>) {
    match node {
        Node::Alt(branches) => {
            for b in branches {
                walk_branch(b, out);
            }
        }
        other => walk_branch(other, out),
    }
}

/// One top-level alternation branch (a `Concat`, a bare element, or a bare
/// assertion). Leading = first element, Trailing = last, else Internal.
fn walk_branch(node: &Node, out: &mut Vec<AssertionInfo>) {
    match node {
        Node::Concat(parts) => {
            let n = parts.len();
            for (idx, p) in parts.iter().enumerate() {
                if let Node::Assertion {
                    neg,
                    look,
                    body,
                    quant,
                } = p
                {
                    let position = boundary_position(*look, idx == 0, idx + 1 == n);
                    out.push(make_info(*neg, *look, body, quant, position));
                } else {
                    walk_internal(p, out);
                }
            }
        }
        Node::Assertion {
            neg,
            look,
            body,
            quant,
        } => {
            // A bare assertion is both ends; treat a bare lookahead as trailing
            // (the guarded-accept framing) and a bare lookbehind position-free.
            out.push(make_info(
                *neg,
                *look,
                body,
                quant,
                boundary_position(*look, true, true),
            ));
        }
        other => walk_internal(other, out),
    }
}

/// Pick a boundary [`Position`] from the start/end flags. A lookahead that is both
/// ends (a bare assertion) is treated as trailing.
fn boundary_position(look: Look, at_start: bool, at_end: bool) -> Position {
    match (look, at_start, at_end) {
        (_, true, true) => Position::Trailing,
        (_, true, false) => Position::Leading,
        (_, false, true) => Position::Trailing,
        (_, false, false) => Position::Internal,
    }
}

/// Everything reachable here is below the top level (inside a group, or mid-concat
/// next to non-assertion siblings) — every assertion found is [`Position::Internal`].
fn walk_internal(node: &Node, out: &mut Vec<AssertionInfo>) {
    match node {
        Node::Atom(_) => {}
        Node::Assertion {
            neg,
            look,
            body,
            quant,
        } => {
            out.push(make_info(*neg, *look, body, quant, Position::Internal));
        }
        Node::Concat(parts) | Node::Alt(parts) => {
            for p in parts {
                walk_internal(p, out);
            }
        }
        Node::Group { body, .. } => walk_internal(body, out),
    }
}

// ─── Width / backref analysis ──────────────────────────────────────────────────

/// Maximum match width of `node` in characters, or `None` if unbounded (a `*` / `+`
/// / `{m,}` quantifier). Used for the bounded-vs-unbounded verdict and the stored
/// assertion width. Delegates to [`super::lower::width_range`] — the single width
/// routine the module shares — so the classifier's width, the Route-1 proof bound, and
/// the runtime lookbehind window can never drift apart.
fn max_width(node: &Node) -> Option<usize> {
    super::lower::width_range(node).1
}

/// Whether `node` contains a backreference in any atom. Covers the numeric form
/// (`\1` … `\9`) and the named/indexed forms (`\k<name>`, `\k'name'`, `\g{1}`) — a
/// backref is not a regular language, so the conservative gate must catch all of
/// them, not just `\1`. (A *named* backref `(?P=name)` never reaches here: the
/// front-end errors on it; this covers the escape-spelled variants.)
fn has_backref(node: &Node) -> bool {
    match node {
        Node::Atom(s) => atom_has_backref(s),
        Node::Concat(parts) | Node::Alt(parts) => parts.iter().any(has_backref),
        Node::Group { body, .. } => has_backref(body),
        Node::Assertion { body, .. } => has_backref(body),
    }
}

fn atom_has_backref(atom: &str) -> bool {
    let chars: Vec<char> = atom.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '\\' {
            match chars.get(i + 1) {
                // Numeric backref `\1` … `\9`.
                Some(n) if ('1'..='9').contains(n) => return true,
                // Named / indexed backref `\k<name>`, `\k'name'`, `\g{1}`, `\g<1>`.
                Some('k') | Some('g') => return true,
                _ => {}
            }
            i += 2; // skip the escape pair
        } else {
            i += 1;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn verdicts(pattern: &str) -> Vec<Verdict> {
        classify(pattern)
            .unwrap_or_else(|e| panic!("classify {pattern:?} failed: {e:?}"))
            .assertions
            .iter()
            .map(AssertionInfo::verdict)
            .collect()
    }

    #[test]
    fn plain_pattern_is_plain() {
        let c = classify("[a-z]+[0-9]*").unwrap();
        assert!(c.is_plain());
        assert!(!c.is_fully_supported());
    }

    #[test]
    fn leading_boundary_lookahead_supported() {
        // assertion is the first element of a top-level concat.
        assert_eq!(
            verdicts("(?!--)[a-z]+"),
            vec![Verdict::Supported(ShapeClass::LeadingBoundary)]
        );
        assert_eq!(
            verdicts("(?=[A-Z])[a-z]+"),
            vec![Verdict::Supported(ShapeClass::LeadingBoundary)]
        );
        // reserved-word exclusion with a bounded alternation body.
        assert_eq!(
            verdicts("(?!if|else|while)[a-z]+"),
            vec![Verdict::Supported(ShapeClass::LeadingBoundary)]
        );
    }

    #[test]
    fn trailing_boundary_lookahead_supported() {
        assert_eq!(
            verdicts("[0-9]+(?![0-9])"),
            vec![Verdict::Supported(ShapeClass::TrailingBoundary)]
        );
        assert_eq!(
            verdicts("=(?!=|>)"),
            vec![Verdict::Supported(ShapeClass::TrailingBoundary)]
        );
        assert_eq!(
            verdicts(":(?!:)"),
            vec![Verdict::Supported(ShapeClass::TrailingBoundary)]
        );
        // the bundled lark OP shape.
        assert_eq!(
            verdicts("[+*]|[?](?![a-z])"),
            vec![Verdict::Supported(ShapeClass::TrailingBoundary)]
        );
        // the bundled DEC_NUMBER trailing guard, after content.
        assert_eq!(
            verdicts("0(?![1-9])"),
            vec![Verdict::Supported(ShapeClass::TrailingBoundary)]
        );
    }

    #[test]
    fn bounded_lookbehind_supported_anywhere() {
        assert_eq!(
            verdicts("(?<!_)/"),
            vec![Verdict::Supported(ShapeClass::BoundedLookbehind)]
        );
        // fixed-width lookbehind of width 4 (pep508 `(?<====)` idiom shape).
        assert_eq!(
            verdicts("(?<====)x"),
            vec![Verdict::Supported(ShapeClass::BoundedLookbehind)]
        );
        // mid-pattern lookbehind (LONG_STRING's `(?<!\\)` style) is still supported.
        assert_eq!(
            verdicts(r#"a(?<!\\)b"#),
            vec![Verdict::Supported(ShapeClass::BoundedLookbehind)]
        );
    }

    #[test]
    fn unbounded_lookahead_rejected() {
        assert_eq!(
            verdicts("(?![ ]*X)Y"),
            vec![Verdict::Rejected(Rejection::Unbounded)]
        );
        assert_eq!(
            verdicts("(?=a*b)c"),
            vec![Verdict::Rejected(Rejection::Unbounded)]
        );
        assert_eq!(
            verdicts("[a-z]+(?=ab+)"),
            vec![Verdict::Rejected(Rejection::Unbounded)]
        );
    }

    #[test]
    fn internal_lookahead_rejected() {
        // mid-concat: an element on each side.
        assert_eq!(
            verdicts("a(?=b)c"),
            vec![Verdict::Rejected(Rejection::Internal)]
        );
        // nested inside a quantified group (verilog MULTILINE_COMMENT shape).
        assert_eq!(
            verdicts(r#"\*(\*(?!/)|[^*])*\*/"#),
            vec![Verdict::Rejected(Rejection::Internal)]
        );
    }

    #[test]
    fn variable_width_lookbehind_rejected() {
        assert_eq!(
            verdicts("(?<!a*)b"),
            vec![Verdict::Rejected(Rejection::VariableWidthBehind)]
        );
        assert_eq!(
            verdicts("(?<!ab+)c"),
            vec![Verdict::Rejected(Rejection::VariableWidthBehind)]
        );
    }

    #[test]
    fn backref_rejected() {
        // Numeric backref.
        assert_eq!(
            verdicts(r#"(a)(?=\1)b"#),
            vec![Verdict::Rejected(Rejection::Backref)]
        );
        // Named / indexed escape-spelled backrefs must reject too (conservative gate).
        assert_eq!(
            verdicts(r#"(?<name>a)(?=\k<name>)b"#),
            vec![Verdict::Rejected(Rejection::Backref)]
        );
        assert_eq!(
            verdicts(r#"(a)(?=\g{1})b"#),
            vec![Verdict::Rejected(Rejection::Backref)]
        );
    }

    #[test]
    fn nested_assertion_rejected() {
        assert_eq!(
            verdicts("(?=(?!a)b)c"),
            vec![Verdict::Rejected(Rejection::Nested)]
        );
    }

    #[test]
    fn quantified_assertion_rejected() {
        // A quantifier on the assertion itself is degenerate — reject when unsure.
        assert_eq!(
            verdicts("(?=a)?[a-z]+"),
            vec![Verdict::Rejected(Rejection::QuantifiedAssertion)]
        );
        assert_eq!(
            verdicts("[0-9]+(?![0-9]){2}"),
            vec![Verdict::Rejected(Rejection::QuantifiedAssertion)]
        );
    }

    /// A bodiless inline-flag group `(?i)` is a plain terminal (no lookaround) — the
    /// front-end fix means `classify` no longer errors on it.
    #[test]
    fn bodiless_inline_flag_group_is_plain() {
        assert!(classify("(?i)[a-z]+").unwrap().is_plain());
        assert!(classify("(?ms)a(?-s)b").unwrap().is_plain());
    }

    #[test]
    fn lower_terminal_handles_each_shape() {
        // Plain terminals lower trivially.
        assert!(matches!(
            lower_terminal("NAME", r#"[^\W\d]\w*"#),
            Ok(Lowered::Plain)
        ));

        // M1/M2/M3: trailing-, leading-boundary and bounded-lookbehind terminals all
        // lower into branches now.
        assert!(matches!(
            lower_terminal("TRAIL", "[0-9]+(?![0-9])"),
            Ok(Lowered::Branches(_))
        ));
        assert!(matches!(
            lower_terminal("LEAD", "(?!--)[a-z]+"),
            Ok(Lowered::Branches(_))
        ));
        assert!(matches!(
            lower_terminal("BEHIND", "(?<!_)/"),
            Ok(Lowered::Branches(_))
        ));

        // Out-of-shape assertions are permanently rejected — an Err that names the
        // terminal and shows the assertion source.
        for (name, pat) in [("UNB", "(?![ ]*X)Y"), ("INT", "a(?=b)c")] {
            let e = lower_terminal(name, pat).unwrap_err();
            let msg = format!("{e}");
            assert!(msg.contains(name), "message must name the terminal: {msg}");
            assert!(msg.contains("(?"), "message must show the assertion: {msg}");
        }
    }

    #[test]
    fn supported_shapes_lower_and_out_of_shape_is_permanently_rejected() {
        // All three supported shapes now lower (M1/M2/M3) — none are pending.
        for pat in ["[0-9]+(?![0-9])", "(?!--)[a-z]+", "(?<!_)/"] {
            assert!(
                matches!(lower_terminal("T", pat), Ok(Lowered::Branches(_))),
                "supported shape {pat:?} must lower"
            );
        }

        // An out-of-shape assertion is a permanent rejection (never pending).
        let permanent = lower_terminal("T", "a(?=b)c").unwrap_err();
        assert!(!is_pending_shape_error(&permanent), "{permanent}");
    }

    /// `python.STRING`'s nested opening guard is now a **supported** leading boundary.
    /// `(?!"")` / `(?!'')` sit after the variable-width prefix + the opening quote — an
    /// internal/variable-position leading boundary the
    /// [`super::lower::recognize_string_idiom`] splice lowers (the marquee L2 piece). So
    /// the classifier, recognizing the whole terminal as the lowerable string idiom,
    /// re-tags those `(?!…)` lookaheads as [`ShapeClass::LeadingBoundary`]; the `(?<!\\)`
    /// lookbehinds stay [`ShapeClass::BoundedLookbehind`] (absorbed by the body
    /// normalization, not lowered as a fixed-offset guard). Every assertion is supported,
    /// so STRING fully lowers — gated end-to-end by the `""""`/`"" ""` adversarial test,
    /// the Route-1 proof, and the python.lark differential.
    #[test]
    fn string_nested_leading_guard_is_supported_via_idiom_splice() {
        let string = r#"([ubf]?r?|r[ubf])("(?!"").*?(?<!\\)(\\\\)*?"|'(?!'').*?(?<!\\)(\\\\)*?')"#;
        let c = classify(string).unwrap();
        assert!(
            c.first_rejection().is_none(),
            "STRING must have no rejected assertion now: {:?}",
            c.first_rejection()
        );
        assert!(
            c.is_fully_supported(),
            "every STRING assertion must be a supported shape"
        );
        // Exactly the four assertions, with the expected shapes in source order:
        // (?!""), (?<!\\), (?!''), (?<!\\).
        let shapes: Vec<Verdict> = c.assertions.iter().map(AssertionInfo::verdict).collect();
        assert_eq!(
            shapes,
            vec![
                Verdict::Supported(ShapeClass::LeadingBoundary),
                Verdict::Supported(ShapeClass::BoundedLookbehind),
                Verdict::Supported(ShapeClass::LeadingBoundary),
                Verdict::Supported(ShapeClass::BoundedLookbehind),
            ]
        );

        // A genuinely-internal lookahead OUTSIDE a recognized idiom is still rejected —
        // the recognizer is the single gate, not a position heuristic.
        assert!(
            classify("a(?=b)c")
                .unwrap()
                .first_rejection()
                .is_some_and(|(_, r)| r == Rejection::Internal),
            "a bare internal lookahead must still reject as Internal"
        );
    }

    /// `lark.REGEXP`'s `(?!\/)` is now a **supported** leading boundary, but *only*
    /// inside the exact recognized regex-literal idiom
    /// ([`super::lower::recognize_regexp_idiom`]) — the same recognizer-is-the-gate
    /// discipline as the STRING splice. The identical `(?!\/)` in any other position
    /// (the verilog block-comment shape, a bare mid-concat use) stays `Internal`
    /// (rejected).
    #[test]
    fn regexp_idiom_guard_is_supported_via_exact_recognizer() {
        let regexp = r"\/(?!\/)(\\\/|\\\\|[^\/])*?\/[imslux]*";
        let c = classify(regexp).unwrap();
        assert!(
            c.first_rejection().is_none(),
            "REGEXP must have no rejected assertion now: {:?}",
            c.first_rejection()
        );
        assert_eq!(
            c.assertions
                .iter()
                .map(AssertionInfo::verdict)
                .collect::<Vec<_>>(),
            vec![Verdict::Supported(ShapeClass::LeadingBoundary)],
            "exactly the one (?!\\/) guard, re-tagged Leading inside the idiom"
        );

        // The same `(?!\/)` OUTSIDE the recognized idiom is still rejected as Internal.
        for p in [
            r"\/\*(\*(?!\/)|[^*])*\*\/",              // verilog MULTILINE_COMMENT
            r"\/(?!\/)(\\\/|\\\\|[^\/])*\/[imslux]*", // greedy near-miss of the idiom
        ] {
            assert!(
                classify(p)
                    .unwrap()
                    .first_rejection()
                    .is_some_and(|(_, r)| r == Rejection::Internal),
                "{p:?} must still reject as Internal"
            );
        }
    }

    /// The robustness requirement: the classifier never panics on arbitrary input —
    /// it returns a verdict or a clean parse error.
    #[test]
    fn classifier_never_panics_on_adversarial_patterns() {
        let probes = [
            "",
            "(",
            ")",
            "(?",
            "(?!",
            "(?<",
            "(?<!",
            "[",
            "[]",
            "[^]",
            r#"\"#,
            "(?=)",
            "a{",
            "a{,3}",
            "a{2,}",
            "(?P<n>x)",
            "(?:(?:(?:a)))",
            "((((((((((",
            "(?!(?!(?!a)))",
            r#"\1\2\3"#,
            "(?<=abcdefghij)",
        ];
        for p in probes {
            // Must not panic; Ok or Err are both fine.
            let _ = classify(p);
        }
    }
}
