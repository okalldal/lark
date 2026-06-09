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
    let node = super::parse(pattern)?;
    let mut branches = Vec::new();
    match &node {
        Node::Alt(arms) => {
            for arm in arms {
                branches.push(lower_branch(pattern, arm)?);
            }
        }
        other => branches.push(lower_branch(pattern, other)?),
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
fn lower_branch(pattern: &str, branch: &Node) -> Result<LoweredBranch, GrammarError> {
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
    if guarded && !is_greedy_monotone(&base) {
        return Err(GrammarError::Other {
            msg: format!(
                "terminal pattern `{pattern}`: a guarded branch's base is not \
                 greedy-monotone (it has an order-sensitive alternation or a lazy/\
                 possessive quantifier), so its match-length under the guard is not \
                 reproducible by the longest-accept accumulator."
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
/// zero consumed width.
fn width_range(node: &Node) -> (usize, Option<usize>) {
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
