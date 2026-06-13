# ADR-0002: Compute true LALR(1) lookaheads, not SLR FOLLOW

- **Status:** Accepted
- **Date:** backfilled 2026-06-13 (decision made during Phase 1, LALR construction)

## Context

A bottom-up parser needs lookahead information to decide when to reduce. There
are two common ways to get it:

- **SLR**: use the grammar-wide FOLLOW set. Cheap to compute, but imprecise — it
  over-approximates, so it reports shift/reduce and reduce/reduce conflicts that
  a more precise analysis would not.
- **LALR(1)**: compute per-item lookaheads via spontaneous generation +
  propagation. More work, but precise.

Because lark-rs must match Python Lark's *conflict outcomes* exactly — Lark
accepts certain grammars and rejects others, and `strict=True` turns conflicts
into hard errors — an over-reporting analysis would diverge from the oracle.

## Decision

Implement true LALR(1) lookaheads (`LookaheadComputer` in `src/parsers/lalr.rs`),
and compute **no FOLLOW set** at all. The grammar analysis (`grammar/analysis.rs`)
produces only NULLABLE and FIRST.

## Consequences

- Conflict detection (`GrammarError::Conflict`) and `strict=True` behavior match
  Lark, instead of spuriously rejecting valid grammars.
- The classic motivating case — the dangling-else grammar that is LALR but not
  SLR — parses correctly (pinned by `tests/test_lalr_core.rs`).
- Slightly more construction cost than SLR, paid once at build time. Acceptable:
  parsing throughput is unaffected (the tables are dense and id-indexed).
