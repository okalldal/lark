//! Shape classifier + lowering entry point — the safety boundary the L2
//! bounded-lookaround lowering is gated by (`docs/LEXER_DFA_PLAN.md`, "What we
//! support" + "How the lowering works").
//!
//! [`lower_terminal`] is the entry point the build path calls. It classifies a
//! terminal and, for every supported shape whose lowering has landed — M1
//! trailing-boundary, M2 leading-boundary, M3 fixed-offset bounded-lookbehind, and the
//! M4 `python.STRING` opening-guard splice — returns the lowered per-branch sub-patterns
//! ([`super::lower`]). A per-instance lowering that cannot ride the engine (a
//! variable-offset lookbehind, a non-realizable guarded base) is **declined** (routed to
//! `fancy-regex`), and an out-of-shape assertion is rejected *permanently*. No supported
//! shape is *pending* any longer — all four lowerings are live. The classifier is the
//! safety boundary: it decides, for each terminal pattern, whether the assertion(s)
//! fall into a **supported shape** or an **unsupported** one (rejected at build time,
//! forever) — and it must never false-accept.
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
//! `python.STRING`'s `(?!"")` sits *after a variable-width prefix + the opening quote* —
//! a position the top-level walk sees as `Internal`. It lowers only because
//! [`super::lower::recognize_string_idiom`] matches that *exact* terminal shape and
//! re-tags its interior lookaheads as `Leading`; outside a recognized idiom a deeper
//! lookahead stays `Internal` (reject). The recognizer — never a position heuristic — is
//! the single gate, so the dangerous direction (false-accept) stays closed. Two bundled
//! lookaround terminals are *not* lowered yet and **decline to `fancy-regex`**:
//! `python.LONG_STRING` (multi-char `"""` close) and `lark.REGEXP` (internal `(?!\/)`);
//! see [`LEXER_DFA_STATUS.md`](../../docs/LEXER_DFA_STATUS.md).

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
    // String-literal opening-guard idiom refinement (python.STRING): a leading
    // `(?!"")` after a variable-width prefix + the opening quote is seen by the
    // top-level walk as `Internal` (it is nested inside the arms group, after a
    // bounded prefix). It is, however, a *supported* leading boundary the
    // [`super::lower::recognize_string_idiom`] splice lowers. So **only when the whole
    // terminal is a recognized, lowerable idiom**, re-tag its interior `(?=)/(?!)`
    // lookaheads as `Leading`. Outside a recognized idiom the verdict is unchanged, so
    // a genuinely-internal lookahead (`a(?=b)c`, the verilog `(?!/)` inside a `*`) stays
    // `Internal` (rejected) — the recognizer is the single gate, never a position
    // heuristic that could false-accept.
    if super::lower::recognize_string_idiom(&node).is_some() {
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

/// **THE LOWERING ENTRY POINT.**
///
/// A plain terminal returns [`Lowered::Plain`]. A terminal whose every assertion is a
/// supported shape (leading/trailing boundary, bounded lookbehind) is lowered to
/// [`Lowered::Branches`] — unless the lowering *declines* a particular instance (a
/// non-greedy-monotone guarded base, or a lookbehind after a variable-width prefix),
/// in which case it returns `Err` and the caller routes that terminal to
/// `fancy-regex`. A terminal with an out-of-shape assertion is rejected permanently
/// (`Err`). Either error names the terminal and the offending assertion.
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
    let classification = classifier.classify(pattern)?;
    if classification.is_plain() {
        return Ok(Lowered::Plain);
    }
    // An unsupported assertion is the firmer error — report it first.
    if let Some((info, reason)) = classification.first_rejection() {
        return Err(GrammarError::Other {
            msg: format!(
                "terminal `{name}`: lookaround assertion `{}` is unsupported \
                 ({}); it cannot be lowered into the combined DFA \
                 (docs/LEXER_DFA_PLAN.md, \"What we support\").",
                info.source,
                reason.explain(),
            ),
        });
    }
    // Every assertion is a supported shape. All three boundary/lookbehind lowerings
    // are implemented (M1 trailing, M2 leading, M3 bounded-lookbehind), so the lowering
    // runs. It may still **decline** a particular instance (a non-greedy-monotone
    // guarded base, or a lookbehind after a variable-width prefix) by returning `Err`;
    // the caller then routes that terminal to `fancy-regex` — correct, never
    // mis-lowered.
    let branches = super::lower::lower_boundary_dotall(pattern, dotall)?;
    Ok(Lowered::Branches(branches))
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
