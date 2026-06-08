//! Bounded-lookaround **lowering** — turning a classified terminal regex into
//! lookaround-free sub-patterns the combined DFA can host
//! (`docs/LEXER_DFA_PLAN.md`, "How the lowering works").
//!
//! The classifier ([`super::classify`]) decides *whether* a terminal's assertions
//! are a supported shape; this module performs the actual transform once it is.
//! Each supported shape lowers a different way:
//!
//!   * **Trailing boundary** (`X(?!S)` / `X(?=S)`, M1) → a **guarded accept**. The
//!     base `X` becomes an ordinary sub-pattern and the assertion is stripped into a
//!     side-table [`GuardSpec`] ("this branch's accept is valid only if the next
//!     chars do/▒don't match `S`"). The maximal-munch driver consults the guard
//!     when it records the accept — so the lookahead char, which belongs to the
//!     *next* token, is never consumed.
//!
//! A terminal's top-level alternation is split **per branch** ([`LoweredBranch`]):
//! one branch may carry a trailing guard while a sibling does not (the bundled
//! `lark.OP` = `[+*]|[?](?![a-z])` is exactly this — `[+*]` is unguarded, `[?]` is
//! guarded). Splitting into per-branch sub-patterns is what lets the driver attach
//! the guard to the *accepting path*, not to the whole terminal — applying `OP`'s
//! `(?![a-z])` to the `[+*]` branch would wrongly reject `+a`.
//!
//! Leading-boundary and bounded-lookbehind lowering land in later milestones; until
//! then a terminal that needs them is reported *pending* by the entry point.

use super::{Look, Node};
use crate::error::GrammarError;

/// One lowered top-level alternation branch of a terminal: a lookaround-free base
/// regex plus an optional trailing guard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoweredBranch {
    /// The branch's base regex with its trailing assertion (if any) stripped — a
    /// plain regular language the NFA builder can compile directly.
    pub regex: String,
    /// The trailing guard lifted out of the branch, or `None` for an unguarded
    /// branch.
    pub guard: Option<GuardSpec>,
}

/// A trailing-boundary guard: the maximal-munch driver records an accept of this
/// branch only when the guard holds at the accept position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuardSpec {
    /// `true` for `(?!S)` (the next chars must **not** match `S`), `false` for
    /// `(?=S)` (the next chars **must** match `S`).
    pub neg: bool,
    /// The assertion body `S` as a lookaround-free regex, matched anchored at the
    /// accept position against the text that follows.
    pub set: String,
}

/// Lower a **pure trailing-boundary** terminal pattern into its per-branch
/// sub-patterns. The caller (the lowering entry point) has already classified the
/// pattern as fully supported with every assertion a [`TrailingBoundary`], so here
/// every assertion encountered is the trailing lookahead at a branch's end.
///
/// [`TrailingBoundary`]: super::classify::ShapeClass::TrailingBoundary
pub fn lower_trailing(pattern: &str) -> Result<Vec<LoweredBranch>, GrammarError> {
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

/// Lower one top-level alternation branch. If it is a concatenation ending in a
/// trailing lookahead, strip the assertion into a [`GuardSpec`]; otherwise it is an
/// unguarded plain branch.
fn lower_branch(pattern: &str, branch: &Node) -> Result<LoweredBranch, GrammarError> {
    match branch {
        Node::Concat(parts) => {
            if let Some(Node::Assertion {
                neg,
                look: Look::Ahead,
                body,
                quant,
            }) = parts.last()
            {
                // The classifier rejects a quantified assertion, so `quant` is empty
                // here — assert it so a mis-route is loud rather than silent.
                debug_assert!(quant.is_empty(), "quantified assertion reached lowering");
                let _ = quant;
                let base = Node::Concat(parts[..parts.len() - 1].to_vec());
                let regex = base.to_source();
                if regex.is_empty() {
                    return Err(zero_width(pattern));
                }
                Ok(LoweredBranch {
                    regex,
                    guard: Some(GuardSpec {
                        neg: *neg,
                        set: body.to_source(),
                    }),
                })
            } else {
                Ok(LoweredBranch {
                    regex: branch.to_source(),
                    guard: None,
                })
            }
        }
        // A bare trailing assertion as a whole branch is a zero-width terminal branch
        // — its base is empty, which the lexer forbids. Reject rather than emit a
        // nullable sub-pattern.
        Node::Assertion {
            look: Look::Ahead, ..
        } => Err(zero_width(pattern)),
        other => Ok(LoweredBranch {
            regex: other.to_source(),
            guard: None,
        }),
    }
}

fn zero_width(pattern: &str) -> GrammarError {
    GrammarError::Other {
        msg: format!(
            "terminal pattern `{pattern}` lowers to a zero-width branch (a bare \
             trailing assertion); the lexer forbids zero-width terminals."
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn op_splits_into_guarded_and_unguarded_branches() {
        let b = lower_trailing("[+*]|[?](?![a-z])").unwrap();
        assert_eq!(b.len(), 2);
        assert_eq!(b[0].regex, "[+*]");
        assert!(b[0].guard.is_none());
        assert_eq!(b[1].regex, "[?]");
        assert_eq!(
            b[1].guard,
            Some(GuardSpec {
                neg: true,
                set: "[a-z]".to_string()
            })
        );
    }

    #[test]
    fn dec_number_trailing_guard_is_stripped() {
        let b = lower_trailing(r"[1-9](_?[0-9])*|0(_?0)*(?![1-9])").unwrap();
        assert_eq!(b.len(), 2);
        assert!(b[0].guard.is_none());
        assert_eq!(b[1].regex, "0(_?0)*");
        assert_eq!(b[1].guard.as_ref().unwrap().set, "[1-9]");
        assert!(b[1].guard.as_ref().unwrap().neg);
    }

    #[test]
    fn single_branch_trailing() {
        let b = lower_trailing("[0-9]+(?![0-9])").unwrap();
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].regex, "[0-9]+");
        assert_eq!(b[0].guard.as_ref().unwrap().set, "[0-9]");
    }

    #[test]
    fn positive_trailing_guard() {
        let b = lower_trailing("[a-z]+(?=:)").unwrap();
        assert_eq!(
            b[0].guard,
            Some(GuardSpec {
                neg: false,
                set: ":".into()
            })
        );
    }
}
