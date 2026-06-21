# ADR-0028: RecoveryAction enum over direct &mut InteractiveParser for on_error

- **Status:** Proposed (pending architect ratification)
- **Date:** 2026-06-21

## Context

The `on_error` recovery handler (#223, follow-up to #168) returned a `bool`:
`true` = delete the offending token, `false` = stop. This cannot express
"insert a corrective token, then retry the current lookahead" — Python Lark's
model where the handler receives `e.interactive_parser` and can `feed_token` /
inspect `accepts()` / `resume`.

Two designs were on the table:

- **(a) RecoveryAction enum** — the handler returns `Delete | Resume | Stop`
  and receives a short-lived `RecoveryContext` (not the public
  `InteractiveParser`) exposing `accepts()`, `feed_token()`, `feed()`.
- **(b) Direct `&mut InteractiveParser`** — richer, but the lifetime of the
  owned stacks in a re-entrant `FnMut` is borrow-checker-hostile, and it
  couples recovery to the standalone cursor's ownership and lexer model — it
  does not fit the generic `TokenSource` recovery path.

## Decision

Choose **(a)**: replace the `bool` result with `RecoveryAction::{Delete, Resume,
Stop}`. The handler receives a `RecoveryContext` backed by the same
`ParserStack` seam used by batch, recovery, and interactive parsing, but it is
not the public `InteractiveParser`.

A no-progress guard on `Resume` prevents infinite loops: if the parser state is
unchanged after the handler returns `Resume`, the loop treats it as `Stop`.

## Consequences

- **Explicit semantics.** `Delete`/`Resume`/`Stop` are self-documenting; the
  old bool conflated deletion with continuation.
- **All existing recovery banks stay meaningful.** `Delete` is a drop-in for the
  old `true`; `Stop` for `false`. Oracle parity is preserved.
- **Insertion/resume cases are expressible.** A handler can `ctx.feed("SEMI", ";")`
  to insert a missing token and return `Resume` to retry the current lookahead.
- **No borrow-checker gymnastics.** The `RecoveryContext` borrows the stack only
  for the duration of the handler call; no self-referential ownership.
- **Ruled out:** handing `&mut InteractiveParser` directly. If a future use case
  demands the full interactive cursor inside `on_error`, this ADR should be
  revisited.
- **Enforced by:** `tests/test_recovery.rs` — `test_recovery_context_feed_inserts_token_then_resume`,
  `test_resume_no_progress_guard_stops`, and the full oracle bank.
