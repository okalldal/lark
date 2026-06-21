# ADR-0011: Parsing is allocation-bound; the tree representation is the deferred path to 10–100×

- **Status:** Accepted
- **Date:** backfilled 2026-06-13 from PRs #22, #23, #24, #75

## Context

The project goal is "10–100× faster than Python Lark." A natural assumption is
that the headroom is algorithmic. A profiling spike (#23) measured otherwise;
this finding underpins the caveat repeated across the standalone / PyO3 /
benchmark docs ("value is X, **not** throughput").

## Decision

Treat LALR parsing as **allocation-bound, not algorithm-bound**, and defer the
tree-representation rework that would unlock the rest of the speedup.

Measured (#23, one parse of a 92 KB input): *"~301K allocations / 105 MB churn
(~3 allocs/byte); ~40% of instructions are memcpy+malloc/free, ~10% SipHash …
~55% lexing (regex-dominated), ~32% reduce/tree-building."* The #24 perf sprint
took the cheap wins (per-token lexer allocations, swap SipHash for a faster
hasher). The remaining gap is structural (#75): *"The remaining headroom to
'10–100×' is the deliberately-deferred tree-representation work (parsing is
allocation-bound, not algorithm-bound)."*

## Consequences

- Explains why several features are honestly scoped as "parity / footprint, not
  throughput" — e.g. the standalone parser is still table-interpreted
  ([ADR-0008](0008-standalone-shares-one-runtime.md)). Those caveats trace to
  *this* measurement, not to incomplete work.
- The next big perf lever is the `Tree`/`Token` representation (arena/owned-string
  churn), not a faster parse algorithm. A perf effort that targets the algorithm
  is aiming at the wrong 32%.
- Consistent with [ADR-0007](0007-deterministic-perf-counters.md): the claim is
  grounded in a profiler measurement, not a guess.
