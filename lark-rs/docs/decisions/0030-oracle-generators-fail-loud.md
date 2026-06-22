# ADR-0030: Oracle generators fail loud; the oracle suite is honest by construction

- **Status:** Proposed (pending architect ratification)
- **Date:** 2026-06-22

## Context

Python Lark is our oracle (ADR-0001) and known gaps are reviewable XFAIL ledgers
that only shrink (ADR-0009). But the *oracle layer itself* had several places where
a disagreement could ship green and silent:

- **Generators warned, then exited 0.** Every `generate_*` in
  `tools/generate_oracles.py` (and the transformer generator) printed
  `WARNING: … expected to parse/reject …` when a committed case's author-declared
  expectation (`should_pass` / `should_parse` / `valid`) disagreed with what Python
  Lark actually did — then wrote the contradicting fixture and exited 0. The CI
  freshness diff was the only net, and it only catches *drift*, not a contradiction
  that was committed from the start.
- **Replay tests silently skipped contradictions.** Several oracle replays carried
  `(true, false, _) => {}` / `(false, true, _) => {}` arms (and bare `_ => {}`
  catch-alls) that swallowed exactly the cases where the fixture's expectation and
  Python disagreed — and, worse, swallowed the *more-permissive* case where lark-rs
  accepts input Python rejects (unfalsifiable per ADR-0017).
- **Self-referential oracle fields.** `final_accepts` was hardcoded to `[]` on a
  successful parse instead of being read from Python — an assertion comparing
  against a literal the generator wrote, not against the oracle.
- **A stub-era self-gate could disable a suite.** The Earley tests `return`ed early
  if `earley_unimplemented()` — dead now that Earley is complete, but a regression
  of the build probe would have silently skipped every Earley oracle.

## Decision

The oracle layer is **honest by construction** — a disagreement with Python fails
loudly or is recorded with a written reason; it is never swallowed:

1. **Generators exit non-zero** on any contradiction (Python disagrees with a case's
   declared expectation) that is not in the reasoned allow-list
   `tools/oracle_contradictions.json`, and on any *stale* allow-list entry — the
   same ratchet as ADR-0009. The detected set is frozen to
   `tests/fixtures/oracles/_meta/contradictions.json`. Keys are language-neutral
   (`"suite: kind <json-encoded-input>"`) so a Rust test can match them.
2. **Replays hold lark-rs to Python's recorded behavior** (`ok` + `tree`), never to
   the author annotation, with no silent skips (`common::replay_oracle_cases`). A
   more-permissive divergence (lark-rs accepts what Python rejects) fails unless
   it appears in an explicit, documented `more_permissive` allow-list (ADR-0017).
3. **Oracle fields are Python-derived**, never hardcoded or self-referential.
4. **Stub-era self-gates are assertions, not skips:** `assert!(!earley_unimplemented())`
   turns a backend regression into a loud failure.
5. **A meta-test enforces it going forward** (`tests/test_oracle_honesty.rs`): the
   allow-list must exactly match the generator's frozen detected set (every entry
   reasoned, none stale), and the silent-skip arm may not reappear in any replay.

## Consequences

- Green now means "lark-rs agrees with Python everywhere it is checked," and
  `tools/oracle_contradictions.json` is an honest, reviewable ledger of every place
  a test grammar's authored expectation diverges from the oracle, each with a reason
  — exactly parallel to the XFAIL ledgers (ADR-0009).
- The enforcement is layered: the generator gates at regeneration time (CI freshness
  step), and the meta-test gates at `cargo test` time without needing Python, so a
  hand-edit that desyncs the two files fails the build either way.
- Cost: a new genuine contradiction now *blocks* regeneration until it is either
  fixed or allow-listed with a reason — the intended forcing function, not a chore
  to route around. The tolerated set is small (today: a CSV comment-line shape and
  five Python-number literals the test grammars don't accept; lark-rs matches Python
  on all of them — author-aspirational labels, not engine divergences).
- Tripwire to revisit: if the allow-list grows to absorb real engine divergences
  rather than documented test-grammar quirks, that is a signal a divergence is being
  hidden — promote it to a tracked bug, not an allow-list line.
- This is test/tooling discipline: no public API or parser semantics change.
