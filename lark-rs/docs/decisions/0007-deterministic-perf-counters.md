# ADR-0007: Gate performance on deterministic work counters, not wall-clock

- **Status:** Accepted
- **Date:** backfilled 2026-06-13 (decision made when the scaling gates were introduced; see `BENCH.md`)

## Context

Several parts of lark-rs have complexity contracts that must not silently
regress: Earley must stay near-linear on unambiguous input (#56/#58), CYK must
stay within its cubic envelope (#87), the lexer must scan linearly (#104), the
dense-DFA build must stay linear per terminal. The natural way to test this is to
time it — but wall-clock timing is flaky in CI (noisy neighbors, thermal, etc.),
so a timing-based gate either flaps or has to be so loose it catches nothing.

## Decision

Gate suspected super-linearities on **deterministic work counters**, never
wall-clock. `src/perf.rs` exposes counters (Earley per-byte work, `cyk_table_steps`,
lexer per-position scans, dense-DFA build per terminal) compiled in **only** under
`--features perf-counters` (zero overhead otherwise). The scaling tests
(`test_earley_scaling.rs`, `test_cyk_scaling.rs`, `test_lexer_scaling.rs`,
`test_lexer_dfa_build_scaling.rs`) assert a flat-per-unit work envelope.

A corollary discipline: a performance fix targets the cause the *profiler* names,
not the one we guessed — and the pathology must be a committed, deterministic,
reproducible benchmark before the fix lands.

## Consequences

- The scaling gates are deterministic: they pass/fail on algorithmic behavior,
  not on the CI machine's mood. A complexity regression is caught precisely.
- Counters are free in normal builds (feature-gated out).
- Wall-clock numbers still exist, but as a *trend* in `cargo bench` / `BENCH.md`,
  not as a pass/fail gate.
- `examples/profile_parse.rs scaling` prints the same counters as a demo table.
