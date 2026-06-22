# ADR-0028: RecoveryAction enum over direct &mut InteractiveParser for on_error

- **Status:** Proposed (pending architect ratification)
- **Date:** 2026-06-21 (updated 2026-06-22)

## Context

The `on_error` recovery handler (#223, follow-up to #168) returned a `bool`:
`true` = delete the offending token, `false` = stop. This cannot express
"insert a corrective token, then resume" — Python Lark's model where the
handler receives `e.interactive_parser` and can `feed_token` / inspect
`accepts()` / `resume_parse`.

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

### Resume semantics match Python Lark's `resume_parse()`

`Resume` **drops the errored token** and parses the *next* token in the state
the handler's feeds produced. At `$END`, the sentinel is retried (there is no
next token). This matches Python Lark's recovery loop exactly — verified
against Python Lark 1.3.1 with differential probes (`+ 2`, `1 + + 2`, `1 +`).

A no-progress guard prevents infinite loops: if the handler returns `Resume`
without feeding any tokens, the loop treats it as `Stop`.

## Consequences

- **Explicit semantics.** `Delete`/`Resume`/`Stop` are self-documenting; the
  old bool conflated deletion with continuation.
- **All existing recovery banks stay meaningful.** `Delete` is a drop-in for the
  old `true`; `Stop` for `false`. Oracle parity is preserved.
- **Insertion at $END is expressible.** A handler can `ctx.feed("NUMBER", "0")`
  to insert a missing token at EOF and return `Resume` to retry `$END` — the
  canonical "insert at end of input" recovery, oracle-verified.
- **Insertion at non-$END drops the errored token.** This matches Python: after
  the handler feeds corrective tokens, `resume_parse()` continues with the next
  lexer token, not the errored one. Handlers that need the errored token's value
  can read it from the `ParseError` before feeding.
- **No borrow-checker gymnastics.** The `RecoveryContext` borrows the stack only
  for the duration of the handler call; no self-referential ownership.
- **Failed feeds are transactional.** If `feed_token` returns `Err`, the stack
  is rolled back, so candidate-insertion patterns (try feed, fall back to
  `Delete`) are safe.
- **Ruled out:** handing `&mut InteractiveParser` directly. If a future use case
  demands the full interactive cursor inside `on_error`, this ADR should be
  revisited.
- **Enforced by:** `tests/test_recovery.rs` —
  `test_resume_drops_errored_token_at_non_eof`,
  `test_resume_at_eof_inserts_missing_token`,
  `test_resume_no_progress_guard_stops`,
  `test_feed_rollback_is_transactional`, and the full oracle bank.
