# Architecture Decision Records (ADRs)

This folder is the **why** of lark-rs: the load-bearing decisions, each as a
short, dated, append-only record. A decision is a historical fact — once made it
doesn't go stale, so unlike implementation prose these records need almost no
maintenance. When you (the steerer) ask "why did we do it this way," this is the
first place to look.

The records here were **backfilled on 2026-06-13** from the design rationale
already scattered through [`CLAUDE.md`](../../CLAUDE.md) and
[`docs/STATUS.md`](../STATUS.md). New decisions get a new file from then on.

## Index

| # | Decision | Status |
|---|---|---|
| [0001](0001-python-lark-is-the-oracle.md) | Python Lark is the oracle (oracle-first testing) | Accepted |
| [0002](0002-true-lalr1-not-slr.md) | Compute true LALR(1) lookaheads, not SLR FOLLOW | Accepted |
| [0003](0003-intern-symbols-to-ids-with-flags.md) | Lower to integer `SymbolId`s; semantics as flags, not name-prefixes | Accepted |
| [0004](0004-python-re-regex-dialect.md) | Terminal regexes follow the Python `re` dialect | Accepted |
| [0005](0005-lower-lookaround-into-the-dfa.md) | Lower bounded lookaround into the DFA; no backtracking runtime engine | Accepted |
| [0006](0006-dfa-default-lexer-backend.md) | DFA (`regex-automata`) is the default lexer backend | Accepted |
| [0007](0007-deterministic-perf-counters.md) | Gate performance on deterministic work counters, not wall-clock | Accepted |
| [0008](0008-standalone-shares-one-runtime.md) | Standalone parsers share one compiled runtime + the same scanner plan | Accepted |
| [0009](0009-xfail-burndown-discipline.md) | Known gaps are XFAIL allow-lists that only shrink | Accepted |

## How to add one

1. Copy [`TEMPLATE.md`](TEMPLATE.md) to `NNNN-short-title.md` (next number).
2. Fill in Context / Decision / Consequences. Keep it to a screen.
3. Add a row to the index above.
4. Set Status: `Proposed` → `Accepted`. To reverse a past decision, add a **new**
   ADR that supersedes it (set the old one's status to `Superseded by NNNN`) —
   never delete or rewrite history.

## The maintenance rule (keep docs from rotting)

A PR that **changes a load-bearing decision** must, in the same PR, either add a
new ADR or mark an existing one superseded. The durable docs
([`ARCHITECTURE.md`](../../ARCHITECTURE.md), [`GLOSSARY.md`](../../GLOSSARY.md),
and these ADRs) are short and reference module paths so drift is easy to spot.
Everything fast-changing is documented by the tests, not by prose.
