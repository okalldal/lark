# ADR-0006: DFA (`regex-automata`) is the default lexer backend

- **Status:** Accepted
- **Date:** backfilled 2026-06-13 (default flipped during the Lexer-DFA plan, L4)

## Context

The combined scanner (try every candidate terminal at the current position, take
the leftmost-longest match) can be built on two engines:

- `LexerBackend::Regex` — one combined alternation on the `regex` crate.
- `LexerBackend::Dfa` — a staged `regex-automata` DFA build.

The DFA backend is also the one that can host lowered lookaround
([ADR-0005](0005-lower-lookaround-into-the-dfa.md)), which the `regex` backend
cannot, so the `python`/`lark` grammars only work under the DFA.

## Decision

Make `LexerBackend::Dfa` the default (`LexerBackend::default()`). Keep
`LexerBackend::Regex` selectable, and keep both engines gated against each other
in CI.

## Consequences

- The swap is correctness-identical: the L0 differential oracle reports 0
  divergences over the compliance bank + JSON corpus + the `python`/`lark` files,
  so the DFA accepts exactly what the `regex` backend did, plus the lowered
  lookaround grammars.
- It is faster on the all-plain common path (`benches/lex_backends`, `BENCH.md`).
- Keeping the `Regex` backend alive is intentional: it is the differential
  cross-check (and, under `fancy-oracle`, hosts the historical fancy-regex
  reference probes). Removing it would remove the thing that proves the DFA
  correct.
- `lexer_backend` has **no** Python Lark equivalent — it selects between
  byte-for-byte equivalent implementations, not a behavior. Both refuse the same
  patterns with the same categorized errors.
