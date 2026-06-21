# ADR-0012: Differential fuzzer — turn the static oracle into an active one

- **Status:** Accepted
- **Date:** backfilled 2026-06-13 from PRs #12, #13

## Context

[ADR-0001](0001-python-lark-is-the-oracle.md) pins behavior against Python Lark
using *curated* grammars/inputs, and [ADR-0009](0009-xfail-burndown-discipline.md)
replays *captured* corpora. Both are static: they only check cases someone
already thought to write down. A whole class of divergences — inputs nobody
imagined — slips through.

## Decision

Add a **differential fuzzer** that generates inputs and compares lark-rs against
Python Lark live, as an *active* oracle complementing the static ones — but keep
it off the PR critical path. Two tiers:

- **Discovery** (manual / nightly): generates fresh inputs, hunts for new
  divergences. Never on the PR critical path.
- **Regression** (every PR): replays a committed corpus (`fuzz/inputs.json`).
  A divergence found in discovery is minimized and frozen into that corpus,
  after which it behaves like any other oracle case.

## Consequences

- New divergence classes get discovered without a human pre-imagining them, then
  become permanent regression cases — the fuzzer *feeds* the static banks rather
  than replacing them.
- CI stays deterministic and fast: only the frozen corpus runs per-PR; generation
  is nightly/manual. No flaky fuzz run gates a merge.
- Same discipline as the XFAIL banks: a discovered failure is committed (as an
  input, or allow-listed) before it's fixed.
