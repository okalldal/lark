---
description: Survey the backlog, pick the highest-value task, execute it end-to-end
---

This is the codified flow for "what is the most high-value work in this repo?
Take it on as a task." Survey the backlog, pick **one** task, announce it, then
carry it through implementation and `/finish-task` in this session.

## 1. Gather candidates (in parallel)

- **Open PRs** (`mcp__github__list_pull_requests`, state OPEN) — in-flight work
  is off the table; parallel web sessions must not collide. Also skim the last
  few merged PRs so you don't redo something that just landed.
- **Open issues** (`mcp__github__list_issues`, state OPEN) — the primary
  backlog. Most carry a done-when section and a stated priority.
- **`lark-rs/docs/STATUS.md`** — open follow-ups and wild-bank findings.
- **XFAIL burndown state** — entry counts of
  `lark-rs/tests/fixtures/oracles/**/*xfail*.json`; every entry is a known
  oracle divergence, and shrinking these lists is always on-mission.
- **Pinned known gaps** — `grep -rn '#\[ignore' lark-rs/tests/` (e.g.
  `test_known_gaps.rs`); each is a reproducible, pre-verified target.

## 2. Pick by this rubric, in order

1. **Red CI or a regression** anywhere → that is the task, full stop.
2. **Correctness/parity gaps with an existing oracle or pinned failing test**
   (xfail entries, `#[ignore]` gaps, issues with a concrete done-when). These
   are verifiable end-to-end — the repo's testing philosophy in one line.
3. Prefer **small blast radius** over big-bang rework when value is comparable.
   A large-blast-radius pick (e.g. loader-wide EBNF changes) is allowed, but
   say so explicitly and lean on the compliance banks as the net.
4. **Skip as autonomous picks**: issues marked "priority: low", research-scoped
   items with no validation story, and decision-checkpoint issues ("decision
   needed", "assess & challenge") — those need the user; surface them via
   `AskUserQuestion` only if nothing implementable remains.

## 3. Announce, then execute

State the pick in one short paragraph — what, why it beats the alternatives,
and the done-when — then start immediately; do not wait for approval.

Follow the repo discipline: oracle/failing test first (Testing Philosophy in
`lark-rs/CLAUDE.md`), implement, `/xfail-burndown` if a bank shrank, then
`/finish-task` (review → fast gate → PR → CI subscription). If the task came
from an issue, put "Closes #N" in the PR description.
