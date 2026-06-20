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
- Recovery over the **contextual** lexer (issue #166): the contextual driver lexes
  contextually during recovery and falls back to a lazily-built *root* (full
  terminal set) scanner only where the per-state scanner refuses — Python Lark's
  `ContextualLexer.lex` except-branch. A root match there is an
  out-of-context-but-valid token the loop deletes; a root miss is an un-lexable
  character it skips. So a grammar whose contextual lexer is load-bearing
  (overlapping terminals disambiguated only by parser state) recovers to the same
  tree a clean contextual parse builds, instead of mis-tokenizing under a stored
  basic lexer. (The basic-lexer driver still recovers with the global lexer, which
  is the correct stream for it.)
- An oracle suite gating the recovered trees + deletion counts against Python
  (both grammatically-misplaced tokens and stray un-lexable `@`/`#` characters).
- Recovery over the **LALR + postlex (Indenter)** path (issue #94) — every LALR
  configuration (basic/contextual lexer × with/without an Indenter) recovers. It
  mirrors Python's `lexer → PostLexConnector(postlex) → parser` wiring: the
  streaming indenter sits upstream of the parser's token deletion and **resets on
  every resume** (`indent_stack=[0]`, `paren_level=0`) exactly as Python's
  `Indenter.process` does per `resume_parse` — for token deletions *and* char
  skips, so a multi-deletion or a char-skip-inside-a-block re-raises at `$END`
  where Python does. One streaming-indenter machine (`PostlexContextual<S>`) drives
  the clean parse, contextual recovery (`ContextualRecovering`), and basic recovery
  (`BasicRecovering`) alike; a `DedentError` surfaces as a hard error. Oracle:
  `indenter_recovery/cases.json`, replayed by `test_indenter_recovery.rs`. ADR-0020.

**Out of scope (won't-do per the #94 architect verdict):**

- Recovery on the **Earley/CYK** backends — Python exposes no `on_error` there, so
  there is no oracle; building it would be unfalsifiable.
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
      ├─ basic-lexer driver (LalrBasic):
      │   → BasicLexer::lex_recovering  →  Vec<Token>    (recovery_lexer field)
      │       → at an un-lexable position: record an UnexpectedCharacter, ask
      │         on_error, skip ONE char, resume  (Python's line_ctr.feed(text[p:p+1]))
      │   → LalrParser::parse_recovering over a PreLexed source
      └─ contextual-lexer driver (LalrContextual, issue #166):
          → LalrParser::parse_contextual_recovering over a ContextualRecovering
            source: lex per parser state; where the per-state scanner refuses,
            consult the lazily-built ROOT (full-terminal) scanner —
            ContextualLexer::next_root_token. A root match → out-of-context-but-
            valid token (the no-action arm deletes it); a root miss → un-lexable
            char surfaced as SourceError::Lex, which run_recovering's Lex arm
            records and resolves with source.skip_char()
      → run_recovering: the shared LALR loop, but on a no-action token: record the
          error, ask on_error, delete the token, resume; on a SourceError::Lex:
          record an UnexpectedCharacter, ask on_error, skip ONE char, resume
          → Some(tree) on ACCEPT; None when ACCEPT is unreachable (premature
            $END, or on_error stopping) — no fabricated partial (issue #167)
  → RecoveredTree { tree: Option<..>, errors }   (char-skips + token-deletions, one list)
```

- **`src/lexer/mod.rs`** — `BasicLexer::lex_recovering` is `lex` with one extra arm
  on the `None` (no-match) position: record an `UnexpectedCharacter`, consult
  `on_error`, and skip exactly one character (advancing toward end-of-input).
  `lex` and `lex_recovering` share the token-construction (`make_token`) and the
  newline-aware `LexCursor`, so the two paths cannot drift on position bookkeeping.
  `ContextualLexer` carries a lazily-built `root` scanner over the full terminal
  set (Python's `root_lexer`); `next_root_token` matches a position against it for
  the contextual recovery fallback. The per-state and root token builds share
  `build_token` so they cannot drift on position bookkeeping (issue #166).
- **`src/parsers/lalr.rs`** — `run_recovering` is `run` with two extra arms: on a
  no-action token push the error, consult `on_error`, and `source.advance()` to
  delete the token (staying in the same state); on a `SourceError::Lex` (only the
  lazy contextual recovery source surfaces one mid-stream) record an
  `UnexpectedCharacter`, consult `on_error`, and `source.skip_char()`. Termination
  is guaranteed: every iteration shifts, reduces, deletes/skips (advancing toward
  `$END`), or stops.
- **`src/parsers/token_source.rs`** — `ContextualRecovering` is the recovery-aware
  contextual source (issue #166): contextual scan with root-lexer fallback, and a
  `skip_char` hook (a no-op `unreachable!` default on the trait, overridden here)
  for the run-loop's char-level skip. `LexFailure` now carries a byte `pos` so the
  char skip builds a position-carrying error.
- **`src/parsers/mod.rs`** — the basic-lexer driver keeps a basic `recovery_lexer`
  and recovers via `lalr_recover`; the contextual driver recovers via
  `parse_contextual_recovering` over its own contextual lexer (no stored basic
  lexer). `parse_recovering` rejects unsupported configurations (postlex,
  Earley/CYK) with a clear `GrammarError::Other`.
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
- `tests/test_recovery.rs::test_recovery_contextual_oracle` (#166) — tree +
  deletion-count parity vs Python under `lexer='contextual'` for the
  `recovery_contextual/cases.json` bank, whose grammar's `AWORD`/`BWORD` terminals
  share a pattern but are valid only in disjoint states (the contextual lexer is
  load-bearing — a basic-lexer recovery would mis-tokenize and fail entirely). It
  exercises both root-fallback branches: an out-of-context-but-valid token the
  root scanner yields (deleted) and a digit un-lexable even by the root set
  (one-char skip). Plus two pins: a clean contextual input recovers nothing, and
  the stray-`}` case deletes exactly one out-of-context token.
