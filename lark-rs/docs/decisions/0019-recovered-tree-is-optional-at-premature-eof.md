# ADR-0019: `RecoveredTree.tree` is `Option`, `None` at premature `$END`

- **Status:** Accepted (2026-06-19; ratified by the architect on merge of omnibus #172)
- **Date:** 2026-06-19

## Context

Error recovery (#43) returns a `RecoveredTree { tree, errors }`. On a premature
end of input (`$END` with no parser action), deletion can't fix the error — there
is no token after `$END`. Python Lark's recovery loop re-raises there (its
infinite-loop guard), so those cases are `recovered: false` / `tree: null` in the
oracle.

lark-rs originally diverged: it called `synthesize_partial`, wrapping whatever
fragments remained on the value stack under the start-symbol name and returning
`Ok(tree)`. Two problems:

1. **It diverges from Python**, which re-raises — those cases are only loosely
   asserted in the oracle (error fired, shape ignored).
2. **The synthesized tree is not a real derivation.** Its shape was unspecified,
   and a caller could not tell it apart from a clean parse — `tree` was always a
   `ParseTree`, so "did recovery actually complete?" was unanswerable from the
   type. Misleading for the editor/LSP use case the feature exists for.

Two alternative shapes: `RecoveredTree { tree: Option<…>, errors }` with `None` at
premature EOF, or a partial carrying an explicit incomplete marker.

## Decision

`RecoveredTree.tree` is `Option<ParseTree>`. It is `Some(tree)` **only** when
recovery reached a normal ACCEPT (a real derivation the surviving tokens produce);
it is `None` when recovery could not reach ACCEPT — premature `$END`, or `on_error`
returning `false` before a valid parse. `synthesize_partial` (and its `start_name`
helper) are deleted; no synthetic tree is fabricated. `Option` over an explicit
marker: idiomatic Rust, zero extra surface.

## Consequences

- **Honest result.** `tree: Some(..)` always means a real parse; a non-empty
  `errors` with `tree: None` is the distinguishable partial. Callers can branch on
  `Option` instead of guessing whether a tree is genuine.
- **Oracle parity at `$END`.** Pins Python's `recovered: false` / `tree: null`
  behavior exactly; `tests/test_recovery.rs` asserts `tree.is_none()` against the
  oracle's `tree: null` (was: shape ignored). Tripwire: a regression that
  fabricates a tree fails the tightened test.
- **Public-API shape change.** `RecoveredTree.tree: ParseTree → Option<ParseTree>`
  is a breaking change to a public type (`escalate`-tier). No other binding consumes
  the field (no PyO3/WASM/C surface for recovery yet), so the blast radius is the
  Rust API and its tests.
- **Cost.** Callers that only ever want "a tree, any tree" must now handle `None`.
  Acceptable: the feature's audience (editor tooling) needs the distinction, and a
  fabricated non-derivation served no one.
