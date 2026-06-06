# Phase 3 — Error Recovery: Scope & Implementation

**Status:** ✅ done (issue #43). Panic-mode single-token-deletion recovery on the
LALR backend, oracle-gated against Python Lark's `on_error` driver.

This document records what error recovery means for lark-rs, the design choices
that fall out of the LR model, and exactly how the implementation stays
oracle-faithful — the same discipline the Phase-2 plan set
([`PHASE_2_PLAN.md`](archive/PHASE_2_PLAN.md)).

---

## 1. Scope

On a parse failure, lark-rs previously returned an `UnexpectedToken`/`UnexpectedEof`
and stopped. Error recovery lets the parser continue past an error and return a
best-effort tree plus the list of errors — the substrate editor tooling and LSPs
need. The done-when (issue #43): *a basic panic-mode strategy that produces a
partial tree rather than aborting, the recovered tree clearly marked with error
nodes, validated against Python Lark's `on_error` callback where applicable.*

**In scope (shipped):**

- Single-token-deletion panic-mode recovery in the LALR parse loop.
- A `RecoveredTree { tree, errors }` result type (the "partial-tree error type").
- `Lark::parse_with_recovery` (the built-in strategy) and `Lark::parse_on_error`
  (a custom handler) — the `on_error` extensibility Python Lark exposes.
- An oracle suite gating the recovered trees + deletion counts against Python.

**Out of scope (follow-ups):**

- Character-level recovery (skipping an un-lexable character, Python's
  `UnexpectedCharacters` branch). lark-rs's recovery lexes with the basic/global
  lexer, so out-of-context-but-valid tokens are deletable, but a genuinely
  un-lexable character is still a hard error.
- Recovery on the Earley/CYK backends and on the LALR+postlex (indenter) path.
- Inline error nodes spliced into the tree (see §3).

---

## 2. Why single-token-deletion, and why it's oracle-checkable

Python Lark has **no** built-in tree-with-error-nodes recovery. What it has is the
`on_error` callback on `parse()`: on each `UnexpectedToken`, it calls the handler
and, if the handler returns truthy, calls `interactive_parser.resume_parse()`.
By the time the error was raised, the offending token had already been pulled off
the lexer — so `resume_parse()` continues with the *next* token, in the *same*
parser state. The net effect of `on_error=lambda e: True` is therefore precisely
**"delete the offending token and carry on."**

That is a strategy lark-rs can reproduce token-for-token, because both engines
share the same LALR tables (already proven at 512/512 compliance). Deleting a
token and re-running the identical state machine over the survivors yields the
identical tree. So the oracle is concrete: run Python with `on_error=lambda e:
True`, capture the recovered tree and the number of handler invocations (= tokens
deleted), and assert lark-rs matches both. See
`tools/generate_oracles.py::generate_recovery` and `tests/test_recovery.rs`.

The one divergence: a `$END` error (premature end of input) can't be fixed by
deletion — there is no token after `$END`. Python's loop has an infinite-loop
guard that re-raises there. lark-rs instead returns a best-effort partial tree
(the issue's "produce a partial tree rather than abort"), so the oracle marks
those cases `recovered: false` and the Rust test checks only that recovery fired,
not the partial's shape.

---

## 3. Why error nodes are surfaced *alongside* the tree, not inline

An LR value stack stays in lockstep with the state stack: a REDUCE pops exactly
`len(rule.expansion)` values. There is no slot for a synthetic "error" value
unless the grammar has a yacc-style `error` production to give it a symbol and a
state — and Lark's grammar syntax has no way to write one. So *inline* error
nodes are not expressible in a pure LR parser without changing the grammar model.

This is also why Python Lark's own recovery drops the bad tokens from the tree and
leaves the caller to collect the errors. lark-rs mirrors that exactly:
`RecoveredTree.errors` is the authoritative, position-carrying record of what went
wrong (the "error nodes"), sitting next to the partial `tree`. Together they are
the "partial tree clearly marked with error nodes" the issue asks for.

---

## 4. Implementation

```
Lark::parse_with_recovery / parse_on_error          (src/lib.rs)
  → ParsingFrontend::parse_recovering               (src/parsers/mod.rs)
      → basic/global lexer  →  Vec<Token>            (recovery_lexer field)
      → LalrParser::parse_recovering                 (src/parsers/lalr.rs)
          → run_recovering: the shared LALR loop, but on a no-action token:
              record the error, ask on_error, delete the token, resume
          → synthesize_partial when ACCEPT is unreachable
  → RecoveredTree { tree, errors }                   (src/error.rs)
```

- **`src/parsers/lalr.rs`** — `run_recovering` is `run` with one extra arm on the
  `None` (error) action: push the error, consult `on_error`, and `source.advance()`
  to delete the token (staying in the same state). Termination is guaranteed:
  every iteration shifts, reduces, deletes (advancing toward `$END`), or stops.
- **`src/parsers/mod.rs`** — the frontend keeps a basic `recovery_lexer` (LALR,
  no-postlex only) so recovery lexes the global terminal set; `parse_recovering`
  rejects unsupported configurations with a clear `GrammarError::Other`.
- **`src/error.rs`** — `RecoveredTree`.
- **`src/lib.rs`** — `parse_with_recovery` / `parse_on_error`
  (+ `parse_on_error_with_start`).

---

## 5. Test surface

- `tests/test_recovery.rs::test_recovery_oracle` — tree + deletion-count parity
  vs Python for the `recovery/cases.json` bank (stray/leading/mid-stream tokens,
  multi-deletion, clean control, premature-EOF).
- Behaviour tests: clean input records no errors; `on_error` returning `false`
  stops at the first error; trailing-`+` never aborts; recovery on Earley reports
  the unsupported-configuration error.
