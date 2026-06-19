# Phase 3 — Error Recovery: Scope & Implementation

**Status:** ✅ done (issues #43 + #93). Panic-mode single-token-deletion recovery
plus character-level recovery (skipping un-lexable characters) on the LALR backend,
oracle-gated against Python Lark's `on_error` driver.

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
- Character-level recovery — skipping an un-lexable character (issue #93,
  shipped). At a position no terminal matches, the recovery lexer records an
  `UnexpectedCharacter` and skips **one character at a time**, then resumes,
  mirroring Python's `UnexpectedCharacters` branch of `on_error`
  (`s.line_ctr.feed(text[p:p+1])`). The skip fires `on_error` once per skipped
  character and is recorded in `RecoveredTree.errors` alongside the token-level
  deletions, so both deletion kinds are counted.
- A `RecoveredTree { tree: Option<…>, errors }` result type (the "partial-tree
  error type"); `tree` is `None` when recovery can't reach a valid parse (#167).
- `Lark::parse_with_recovery` (the built-in strategy) and `Lark::parse_on_error`
  (a custom handler) — the `on_error` extensibility Python Lark exposes.
- An oracle suite gating the recovered trees + deletion counts against Python
  (both grammatically-misplaced tokens and stray un-lexable `@`/`#` characters).

**Out of scope (follow-ups):**

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
guard that re-raises there. lark-rs returns `Ok` rather than aborting, but with
`RecoveredTree.tree == None` — **no** fabricated derivation (issue #167, ADR-0019;
it once synthesized a partial from the value stack, which was not a real
derivation and a caller could not tell apart from a clean parse). The oracle marks
those cases `recovered: false` / `tree: null`, and the Rust test pins `tree.is_none()`
against it. `tree: Some(..)` therefore always means a real parse the surviving
tokens produced; the non-empty `errors` list with `tree: None` is the
distinguishable partial.

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
      → BasicLexer::lex_recovering  →  Vec<Token>    (recovery_lexer field)
          → at an un-lexable position: record an UnexpectedCharacter, ask
            on_error, skip ONE char, resume  (Python's line_ctr.feed(text[p:p+1]))
      → LalrParser::parse_recovering                 (src/parsers/lalr.rs)
          → run_recovering: the shared LALR loop, but on a no-action token:
              record the error, ask on_error, delete the token, resume
          → Some(tree) on ACCEPT; None when ACCEPT is unreachable (premature
            $END, or on_error stopping) — no fabricated partial (issue #167)
  → RecoveredTree { tree: Option<..>, errors }   (char-skips + token-deletions, one list)
```

- **`src/lexer/mod.rs`** — `BasicLexer::lex_recovering` is `lex` with one extra arm
  on the `None` (no-match) position: record an `UnexpectedCharacter`, consult
  `on_error`, and skip exactly one character (advancing toward end-of-input).
  `lex` and `lex_recovering` share the token-construction (`make_token`) and the
  newline-aware `LexCursor`, so the two paths cannot drift on position bookkeeping.
- **`src/parsers/lalr.rs`** — `run_recovering` is `run` with one extra arm on the
  `None` (error) action: push the error, consult `on_error`, and `source.advance()`
  to delete the token (staying in the same state). Termination is guaranteed:
  every iteration shifts, reduces, deletes (advancing toward `$END`), or stops.
- **`src/parsers/mod.rs`** — the frontend keeps a basic `recovery_lexer` (LALR,
  no-postlex only) so recovery lexes the global terminal set; `lalr_recover` lexes
  via `lex_recovering` (char-skips into `errors`) then drives the token loop over
  the survivors; `parse_recovering` rejects unsupported configurations with a clear
  `GrammarError::Other`.
- **`src/error.rs`** — `RecoveredTree`.
- **`src/lib.rs`** — `parse_with_recovery` / `parse_on_error`
  (+ `parse_on_error_with_start`).

---

## 5. Test surface

- `tests/test_recovery.rs::test_recovery_oracle` — tree + deletion-count parity
  vs Python for the `recovery/cases.json` bank (stray/leading/mid-stream tokens,
  multi-deletion, clean control, premature-EOF, **and the un-lexable-character
  cases**: stray `@`/`#`, consecutive bad chars, a char-skip that uncovers a
  token-level deletion — both counted).
- Behaviour tests: clean input records no errors; `on_error` returning `false`
  stops at the first error; trailing-`+` never aborts; recovery on Earley reports
  the unsupported-configuration error. Character-level pins (#93): an un-lexable
  char is recorded as `UnexpectedCharacter`; char + token deletions both count;
  `on_error` returning `false` at an un-lexable position stops lexing.
