# ADR-0013: EBNF nullable helpers — distribute non-final, share only the recurse core

- **Status:** Accepted
- **Date:** backfilled 2026-06-13 from PRs #98, #99, #100, #138

## Context

Expanding EBNF (`a*`, `(a b)?`, `x?`) into anonymous helper rules has a subtle
correctness boundary that interacts with *both* the LALR automaton and the
contextual lexer. Two opposing pressures: we want to **share** structurally
identical helpers (smaller tables, Python's `rules_cache` parity), but sharing
the wrong ones breaks parsing. The rule that resolves this lived only in PR
discussions.

## Decision

Mirror Python Lark's `SimplifyRule_Visitor`/`rules_cache` split:

- **A non-final nullable helper is distributed into the parent's alternatives,
  not kept as a shared node.** Why (#100): *"a non-final nullable helper … the
  closure never expands `Y`. The LALR automaton therefore mispredicted."* Under
  `maybe_placeholders` the distribution also threads `nones_before` so non-final
  optionals don't hide LALR branches (#138, fixing #106).
- **Only the `*`/`+` recurse core is shared; `Opt`/`Maybe`/`GroupOptional` are
  NOT shared.** The gotcha (#98): *"Sharing a nullable helper across two parents
  unions their follow-sets, and the contextual lexer derives each state's scanner
  from those follows, so an over-merge silently widens a state's terminal set and
  breaks state-narrowing (it made csv.lark's `header` start trying `row`'s
  terminals)."*

## Consequences

- Over-sharing is a *silent* correctness bug, not a crash: it widens a contextual
  lexer state's terminal set, defeating the state-narrowing that is Lark's whole
  point. Any future helper-dedup change must respect the nullable boundary.
- The constraint couples three subsystems — EBNF expansion, the LALR closure, and
  the contextual lexer's per-state scanner derivation — so it can't be reasoned
  about in the loader alone. Pinned by `tests/test_*` follow-union parity tests
  added in #99.
