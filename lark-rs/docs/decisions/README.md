# Architecture Decision Records (ADRs)

This log is the **feedback loop** that lets the architect stay in control without
reviewing every decision (`PRINCIPLES.md` §8). When an agent makes a call that the
constitution grounds only as a *default* (§3) or a judgement (§4 middle row), it
records the decision here. The architect reads these in arrears, in batch, and
promotes any correction into `PRINCIPLES.md` — sharpening the grounding for every
future agent.

## When to write one

Write an ADR when you:

- **deviate from a §3 default** (this is mandatory — it's the price of the
  deviation), or
- make an **architecture / public-API / cross-cutting** choice that a future
  reader would otherwise have to reverse-engineer from the diff, or
- **resolve a `needs-decision`** the architect signed off on (record what was
  decided and why, so it doesn't get re-litigated).

Do **not** write one for routine, fully-grounded work (a bug fix with an oracle,
an xfail burndown) — the test *is* the record there.

## How

1. Copy `0000-template.md` to `NNNN-short-kebab-title.md` (next free number).
2. Fill it in — keep it short; a screenful is plenty.
3. Commit it *in the same PR* as the change it explains, and link it from the PR
   body. `/finish-task` prompts for this.
4. Status starts `Proposed`; the architect flips it to `Accepted` (or
   `Superseded by NNNN`) when reviewing the log.

## Index

| # | Title | Status |
|---|-------|--------|
| [0001](0001-python-lark-as-oracle.md) | Python Lark as the oracle (oracle-first testing) | Accepted (retroactive) |
| [0002](0002-deterministic-work-counters-over-wall-clock.md) | Deterministic work-counters over wall-clock for perf gates | Accepted (retroactive) |
| [0003](0003-tiered-merge-autonomy.md) | Tiered merge autonomy by blast radius | Proposed |
