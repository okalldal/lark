//! Shape classifier + lowering entry point — the safety boundary the L2
//! bounded-lookaround lowering is gated by (`docs/LEXER_DFA_PLAN.md`, "What we
//! support" + "How the lowering works").
//!
//! **This session implements NO real lowering.** [`lower_terminal`] is a stub that
//! *rejects every lookaround terminal* — the verification harness is built first,
//! against a reject-everything lowering, so the net exists before the risky code
//! (the plan's "harness-first" process). What is real here is the **classifier**:
//! it decides, for each terminal pattern, whether the assertion(s) fall into a
//! **supported shape** (which a future session will lower) or an **unsupported**
//! one (which must be rejected at build time, forever).
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
//! Recognizing a leading-boundary lookahead that sits *after a fixed-width prefix
//! inside a sub-group* — `python.STRING`'s `(?!"")` right after the opening quote —
//! is a deliberate **first-shape refinement**, not done here: this skeleton only
//! recognizes assertions at the terminal's *top-level* boundary, and conservatively
//! classifies a deeper lookahead as internal (reject). That keeps the dangerous
//! direction safe; the reject/pending split is re-derived the moment the lowering
//! for a shape lands.

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

/// The outcome of lowering one terminal. In this session the only non-error variant
/// is [`Lowered::Plain`] — a terminal with no lookaround, which needs no lowering.
/// Real lowered fragments are a future session's work (`docs/LEXER_DFA_PLAN.md`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lowered {
    /// No lookaround assertion — the terminal is already a plain regular language.
    Plain,
}

/// **THE LOWERING ENTRY POINT — stubbed to reject every lookaround terminal.**
///
/// A plain terminal returns [`Lowered::Plain`]. Any terminal with a lookaround
/// assertion returns `Err`, whether its shape is *supported* (rejected as "pending
/// — not yet lowered", so the differential harness records it and flips to a gated
/// comparison once the shape lands) or *unsupported* (rejected for good). Either way
/// the message names the terminal and the offending assertion.
///
/// Keeping both behind one entry point is the point: the build path calls this and
/// never sees a half-lowered terminal during the harness-first phase.
pub fn lower_terminal(name: &str, pattern: &str) -> Result<Lowered, GrammarError> {
    lower_terminal_with(&DefaultClassifier, name, pattern)
}

/// [`lower_terminal`] over an explicit classifier — the mutation meta-test drives a
/// mutant classifier through this same entry point.
pub fn lower_terminal_with(
    classifier: &dyn Classifier,
    name: &str,
    pattern: &str,
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
    // Otherwise every assertion is a supported shape, but the lowering for it is not
    // implemented yet — reject as pending so the harness tracks it.
    let (info, shape) = classification
        .first_supported()
        .expect("non-plain, non-rejected classification has a supported assertion");
    Err(GrammarError::Other {
        msg: format!(
            "terminal `{name}`: lookaround assertion `{}` is a supported \
             {} shape, but the bounded-lookaround lowering is not yet \
             implemented (pending first shape — docs/LEXER_DFA_PLAN.md L2).",
            info.source,
            shape.describe(),
        ),
    })
}

/// True iff `e` is the *pending-shape* rejection [`lower_terminal`] emits for a
/// supported-but-not-yet-lowered assertion (as opposed to a permanent rejection).
/// The differential harness uses this to bucket a grammar as "pending" vs
/// "genuinely unsupported".
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

fn make_info(neg: bool, look: Look, body: &Node, position: Position) -> AssertionInfo {
    AssertionInfo {
        source: assertion_source(neg, look, body),
        neg,
        look,
        width: max_width(body),
        position,
        has_backref: has_backref(body),
        has_nested: body.has_assertion(),
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
                    neg, look, body, ..
                } = p
                {
                    let position = boundary_position(*look, idx == 0, idx + 1 == n);
                    out.push(make_info(*neg, *look, body, position));
                } else {
                    walk_internal(p, out);
                }
            }
        }
        Node::Assertion {
            neg, look, body, ..
        } => {
            // A bare assertion is both ends; treat a bare lookahead as trailing
            // (the guarded-accept framing) and a bare lookbehind position-free.
            out.push(make_info(
                *neg,
                *look,
                body,
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
            neg, look, body, ..
        } => {
            out.push(make_info(*neg, *look, body, Position::Internal));
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
/// / `{m,}` quantifier). Used only for the bounded-vs-unbounded verdict and a small
/// window size, so character counts are conservatively *over*-approximated — never
/// under — and a nested assertion contributes its zero consumed width.
fn max_width(node: &Node) -> Option<usize> {
    match node {
        Node::Atom(s) => atom_max_width(s),
        Node::Concat(parts) => {
            let mut total = 0usize;
            for p in parts {
                total = total.saturating_add(max_width(p)?);
            }
            Some(total)
        }
        Node::Alt(branches) => {
            let mut m = 0usize;
            for b in branches {
                m = m.max(max_width(b)?);
            }
            Some(m)
        }
        Node::Group { body, quant, .. } => apply_quant_width(max_width(body)?, quant),
        Node::Assertion { .. } => Some(0),
    }
}

/// Apply a group/element quantifier to a known body width, or `None` if the
/// quantifier makes it unbounded.
fn apply_quant_width(width: usize, quant: &str) -> Option<usize> {
    let q: Vec<char> = quant.chars().collect();
    match q.first().copied() {
        None => Some(width),
        Some('*') | Some('+') => None,
        Some('?') => Some(width),
        Some('{') => match parse_brace(&q, 0) {
            Some((None, _)) => None, // {m,}
            Some((Some(n), _)) => Some(width.saturating_mul(n)),
            None => Some(width), // a literal `{` that wasn't a quantifier
        },
        _ => Some(width),
    }
}

/// Conservative max width of a flat, assertion-free atom run.
fn atom_max_width(atom: &str) -> Option<usize> {
    let chars: Vec<char> = atom.chars().collect();
    let mut i = 0usize;
    let mut total = 0usize;
    while i < chars.len() {
        let c = chars[i];
        let elem_w = match c {
            '\\' => {
                i += 1;
                let n = chars.get(i).copied();
                i += 1;
                match n {
                    // Zero-width escapes: word/text boundaries and anchors.
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
                    i += 1; // a literal `]` as the first class member
                }
                while i < chars.len() && chars[i] != ']' {
                    if chars[i] == '\\' {
                        i += 1;
                    }
                    i += 1;
                }
                if i < chars.len() {
                    i += 1; // consume the closing `]`
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
        match chars.get(i).copied() {
            Some('*') | Some('+') => return None,
            Some('?') => {
                i += 1;
                if matches!(chars.get(i), Some('?') | Some('+')) {
                    i += 1;
                }
                total = total.saturating_add(elem_w);
            }
            Some('{') => {
                if let Some((maxrep, consumed)) = parse_brace(&chars, i) {
                    i += consumed;
                    if matches!(chars.get(i), Some('?') | Some('+')) {
                        i += 1;
                    }
                    match maxrep {
                        None => return None,
                        Some(n) => total = total.saturating_add(elem_w.saturating_mul(n)),
                    }
                } else {
                    // A literal `{` — counts as the element it already is.
                    total = total.saturating_add(elem_w);
                }
            }
            _ => total = total.saturating_add(elem_w),
        }
    }
    Some(total)
}

/// Parse a `{m}` / `{m,}` / `{m,n}` brace quantifier at `chars[start] == '{'`.
/// Returns `(max_repetitions, chars_consumed)` where `max_repetitions` is `None`
/// for the unbounded `{m,}`. Returns `None` if it is not a well-formed quantifier
/// (a literal `{`).
fn parse_brace(chars: &[char], start: usize) -> Option<(Option<usize>, usize)> {
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
        return None; // `{,n}` / `{}` are not quantifiers here
    }
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
            None // `{m,}` — unbounded
        } else {
            Some(hi.parse::<usize>().unwrap_or(usize::MAX))
        }
    } else {
        Some(lo.parse::<usize>().unwrap_or(usize::MAX)) // `{m}`
    };
    if chars.get(i) == Some(&'}') {
        Some((max, i + 1 - start))
    } else {
        None
    }
}

/// Whether `node` contains a backreference (`\1` … `\9`) in any atom.
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
            if let Some(&n) = chars.get(i + 1) {
                if ('1'..='9').contains(&n) {
                    return true;
                }
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
        assert_eq!(
            verdicts(r#"(a)(?=\1)b"#),
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
    fn lower_terminal_rejects_every_lookaround_terminal() {
        // Plain terminals lower trivially.
        assert!(matches!(
            lower_terminal("NAME", r#"[^\W\d]\w*"#),
            Ok(Lowered::Plain)
        ));

        // Every lookaround terminal is rejected — supported shapes as "pending",
        // unsupported as a permanent rejection. Either way it is an Err.
        for (name, pat) in [
            ("LEAD", "(?!--)[a-z]+"),
            ("TRAIL", "[0-9]+(?![0-9])"),
            ("BEHIND", "(?<!_)/"),
            ("UNB", "(?![ ]*X)Y"),
            ("INT", "a(?=b)c"),
        ] {
            let e = lower_terminal(name, pat).unwrap_err();
            let msg = format!("{e}");
            assert!(msg.contains(name), "message must name the terminal: {msg}");
            // and must show the assertion source.
            assert!(msg.contains("(?"), "message must show the assertion: {msg}");
        }
    }

    #[test]
    fn pending_vs_permanent_rejection_is_distinguishable() {
        let pending = lower_terminal("T", "[0-9]+(?![0-9])").unwrap_err();
        assert!(is_pending_shape_error(&pending), "{pending}");

        let permanent = lower_terminal("T", "a(?=b)c").unwrap_err();
        assert!(!is_pending_shape_error(&permanent), "{permanent}");
    }

    /// Documents the conservative skeleton boundary: `python.STRING`'s `(?!"")` is a
    /// *leading boundary of the string body sub-group*, but this skeleton only sees
    /// the terminal's top level, where the guard is internal — so it currently
    /// classifies as unsupported. Extending leading-boundary detection past a
    /// fixed-width prefix is a documented first-shape refinement.
    #[test]
    fn string_nested_leading_guard_is_currently_internal() {
        let string = r#"([ubf]?r?|r[ubf])("(?!"").*?(?<!\\)(\\\\)*?"|'(?!'').*?(?<!\\)(\\\\)*?')"#;
        let c = classify(string).unwrap();
        // The `(?!"")` / `(?!'')` lookaheads are seen as internal (reject); the
        // `(?<!\\)` lookbehinds are bounded and supported.
        assert!(
            c.first_rejection()
                .is_some_and(|(_, r)| r == Rejection::Internal),
            "STRING's interior lookahead should currently reject as Internal"
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
