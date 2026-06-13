# ADR-0005: Lower bounded lookaround into the DFA; no backtracking runtime engine

- **Status:** Accepted
- **Date:** backfilled 2026-06-13 (active direction set 2026-06-08; see `docs/LEXER_DFA_PLAN.md`)

## Context

Some bundled Lark grammars (`python.lark`, `lark.lark`) use regex **lookaround** —
e.g. `python.STRING`'s `(?!"")…(?<!\\)(\\\\)*?` guards, `DEC_NUMBER`'s `(?![1-9])`,
`lark.REGEXP`. Rust's `regex`/`regex-automata` crates support **no** lookaround or
backreferences by design (it's what guarantees their linear-time matching).

The obvious escape hatch is a backtracking engine (`fancy-regex`, or PR #110's
Pike-VM). But that means a second, slower matching path with different complexity
characteristics living on the hot lexer loop — and a runtime dependency carrying
backtracking semantics we'd then have to reason about for every grammar.

## Decision

Do **not** ship a backtracking runtime engine. Instead, **lower** the bounded
lookaround shapes we support into the `regex-automata` DFA at build time, so
every terminal lexes single-pass. Anything we can't lower takes a *categorized
build error* (`GrammarError::LookaroundScope`) rather than silently falling back.

- The supported shapes and their proofs live in `src/lookaround/` (`lower.rs`,
  `classify.rs`); the refusal goes through one seam,
  `lexer::route_fancy_only_terminal`.
- The scope taxonomy (`docs/LOOKAROUND_SCOPE.md`) splits refusals into
  `OutOfScope` (by-design non-goals) and `NotYetImplemented` (conservative
  refusals that double as promotion tripwires).
- Every bundled lookaround terminal lowers, so the `python`/`lark` grammars stay
  **verbatim** (one standing exception: `common.lark`'s `ESCAPED_STRING` keeps a
  hand-written lookaround-free adaptation — it's the hottest terminal).

## Consequences

- One linear-time lexer path, no backtracking pathology, grammars unedited.
- **`fancy-regex` is not a runtime dependency.** It survives only as a
  dev-dependency oracle behind the default-off, TEST-ONLY `fancy-oracle` feature,
  used to gate the DFA byte-identical over the full differential
  (`tests/test_scanner_differential.rs`, 0 divergences).
- Cost: lowering each new shape is real work and must be *proven* equivalent
  (per-idiom generative-equivalence + mutation tests). The `NotYetImplemented`
  refusals are deliberate tripwires marking the growth path.
- Full history (per-idiom proofs, the flag-wrapper strip, the superseded
  lookaround-*elimination* plan) lives in `docs/STATUS.md` +
  `docs/LEXER_DFA_STATUS.md`.
