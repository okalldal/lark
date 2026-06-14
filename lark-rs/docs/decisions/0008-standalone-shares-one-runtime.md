# ADR-0008: Standalone parsers share one compiled runtime + the same scanner plan

- **Status:** Accepted
- **Date:** backfilled 2026-06-13 (decision made for the standalone generator, #42)

## Context

`lark-rs generate-parser` emits a self-contained Rust LALR parser that depends
only on `regex` + std (issue #42). The hazard with any code generator is *drift*:
the generated parser re-implements the lexer and the parse/tree-shaping driver,
and over time those copies diverge from the real in-process engine — so a
generated parser silently parses differently from `Lark::parse`.

## Decision

Share the two drift-prone pieces by construction rather than copying them:

- **The driver** (basic lexer + LALR loop + tree shaping) lives in
  `src/standalone/runtime.rs` as a *real compiled, type-checked, unit-tested*
  module that is `include_str!`'d into each generated parser — not a hand-copied
  text blob.
- **The lexer recipe** is the *same* `lexer::scanner_plan` the in-process
  `Scanner::build` uses; the generator bakes that plan, it doesn't re-derive one.

The generator (`src/standalone/mod.rs`) runs the normal pipeline once and bakes
the `ParseTable`, per-rule shaping flags, and the scanner plan into a `static`.

## Consequences

- A generated parser is byte-identical to lark-rs because both drift vectors are
  the *same source*. Pinned two ways: committed `tests/standalone/*.rs` fixtures
  run against the live oracle (+ a freshness gate), and a full compliance-bank
  replay through the shared runtime (`standalone_compliance_bank`, #86 —
  508/512, the 4 XFAILs are basic-lexer-incompatible grammars Python's own basic
  lexer can't reproduce either).
- The value proposition is *dependency footprint* and Python-`standalone`
  parity, **not** throughput (still table-interpreted) or `no_std` (runtime regex
  compile).
- Limits: LALR + basic lexer only; no postlex (clean error); lookaround grammars
  aren't standalone-able yet (the baked runtime is pure-`regex`). The L5
  serialized-DFA bake is the path that lifts the lookaround limit.
