# 0002. Deterministic work-counters over wall-clock for perf gates

- **Status:** Accepted (retroactive — recorded to seed the log and to model a perf-class ADR)
- **Date:** 2026-06-13 (decision predates this record)
- **Deciders:** architect
- **Grounds:** new policy — became PRINCIPLES.md §2 invariant 5

## Context

Several engine paths have algorithmic-complexity risks (Earley super-linearity,
CYK cubic envelope, lexer linear scan, dense-DFA build cost). The natural way to
catch a regression is to time it — but wall-clock is noisy, machine-dependent, and
flaky in CI, so a timing gate either tolerates so much slack it misses real
regressions or flakes constantly. It also tempts fixes that target the cause we
*guessed* rather than the one that's real.

## Decision

Gate suspected super-linearities on **deterministic work counters** (`src/perf.rs`,
compiled only under `--features perf-counters`, zero overhead otherwise), asserting
a flat *per-unit* work envelope (per byte / per n³ / per position / per terminal).
A suspected pathology must become a committed, deterministic scaling test *before*
the fix, and the fix targets the cause the profiler names.

## Why / alternatives rejected

- *Wall-clock benchmarks as gates* — rejected: noise forces useless slack;
  machine-dependent; flaky.
- *No perf gate, review-by-eye* — rejected: complexity regressions are exactly the
  bugs that pass every functional test and only bite at scale.

Counters make "is it the right complexity class?" a falsifiable, reproducible
assertion — the §2.7 meta-invariant applied to performance.

## Consequences

- Easier: deterministic CI scaling gates; bisectable regressions; a demonstrate-
  first discipline that has already corrected wrong root-cause guesses (#56).
- Harder: a new pathology requires authoring a counter + scaling test first — more
  upfront work than eyeballing a flamegraph.
- Tripwire: the scaling gates themselves (`tests/test_*_scaling.rs`); `BENCH.md`
  records the trend.
