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
//! Bounded-lookbehind lowering lands in a later milestone; until then a terminal that
//! needs it is reported *pending* by the entry point.

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
/// a trailing lookahead off the end into guards; whatever remains is the base regex.
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

    let base = Node::Concat(parts[lo..hi].to_vec());
    let regex = base.to_source();
    if regex.is_empty() {
        // A branch that is *only* boundary assertions has an empty (nullable) base —
        // a zero-width terminal branch, which the lexer forbids.
        return Err(zero_width(pattern));
    }
    // A guarded branch rides the driver's "longest accept where the guard holds"
    // accumulator, which only coincides with Python's backtracking result when the
    // base is **greedy-monotone** (its leftmost-first match is always its longest).
    // A base with an order-sensitive alternation (`ab|abc`) or a lazy/possessive
    // quantifier (`.*?`, `a*+`) can prefer a *shorter* match, so the accumulator would
    // mis-lower it. Decline here (reject-when-unsure) so the caller routes the whole
    // terminal to `fancy-regex` instead — correct, never mis-lowered.
    if (leading.is_some() || trailing.is_some()) && !is_greedy_monotone(&base) {
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
    })
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
}
