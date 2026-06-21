# ADR-0027: Semantic output backends

- **Status:** Accepted (2026-06-21; architect ratified)
- **Date:** 2026-06-21
- **Depends on:** ADR-0026 (behaviour is scoped to the oracle's)

## Context

lark-rs returns a generic `ParseTree`. Application-specific values require building
that tree first and walking it afterwards — two passes and a heap node per
syntax-tree node. ADR-0011 identified `Tree`/`Token` allocation as the dominant
remaining cost (parsing is allocation-bound, not algorithm-bound), and ADR-0015
deliberately centralized tree shaping in `TreeBuilder` so the representation could
later change. Earley is now a second consumer of that shaper, so the deferred
seam decision is due.

Python Lark gives an oracle for the LALR **embedded transformer** (it runs
reductions into user values during the parse; rejected for Earley by design). It
gives **no** oracle for Rust-specific output representations — span trees, arenas,
event streams, tapes. ADR-0026 governs exactly that asymmetry: behaviour with no
Python counterpart is not an autonomous default without a validation story.

## Decision

Adopt **semantic output backends** as the next capability area, and refactor
`TreeBuilder` into the default tree-producing implementation of an internal
`OutputBuilder` seam. Tree-compatible output stays the default backend, but the
engine no longer treats generic tree construction as its identity; every parser
algorithm eventually reduces through the seam. (Trait sketch and public-API options
live in the design RFC, `docs/notes/semantic-output-builder-design.md` — not here.)

Validate the area in three tiers, per ADR-0026's falsifiability ladder:

1. **LALR transformer parity is oracle-backed** against Python Lark, validated by
   *both* final-value equality *and* callback-trace equality (result equality alone
   misses callback order and skipped/extra callbacks). LALR + basic and contextual
   lexers only; Earley/CYK embedded transform is out of scope (Python rejects it).
2. **Internal fast-output backends ship only behind a relative oracle:** materialize
   the backend output and compare it byte-for-byte with the oracle-backed
   tree/transformer output, plus deterministic counters proving the fast path did
   not silently build the generic tree/token representation.
3. **Public API shape, binding (PyO3/WASM/C) exposure, and the committed
   output-mode taxonomy remain architect-ratified** API/product decisions — no
   Python oracle exists for Rust ergonomics (ADR-0026, PRINCIPLES.md §6).

## Consequences

- The first implementation work is the value + callback-trace **oracle harness**,
  followed by a **no-behaviour-change** refactor from `TreeBuilder` to the internal
  seam (existing tree/compliance/wild banks stay green). Only then a test-only
  semantic backend, parity hardening, and counters. Detailed ordering lives in the
  child issues, not this ADR.
- Span/event/tape/arena outputs do **not** become public commitments merely because
  the internal seam exists; a backend that cannot be projected back to an
  oracle-backed output is not autonomous and needs separate architect approval.
- **No throughput claim ships unless it is a gated counter result** (ADR-0007). In
  particular: no "simdjson-class for arbitrary grammars" claim, and SIMD lexing is
  not part of this epic — it attacks the lexer half, not the allocation half, and
  carries its own portability cost. It rides its own ADR if/when funded.
- First application of ADR-0026 to a *performance* representation rather than an
  error-recovery shape; pairs with ADR-0011 (why), ADR-0015 (the seam), ADR-0025
  (surface is free to reshape).
