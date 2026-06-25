---
description: Survey the backlog, pick the highest-value task, execute it end-to-end
---

> _Internal maintainer automation — not an invitation for external agents to claim issues or open PRs, and not a public bug-bounty program. See [`/CONTRIBUTING.md`](/CONTRIBUTING.md)._

This is the codified flow for "what is the most high-value work in this repo?
Take it on as a task." Survey the backlog, pick **one** task, announce it, then
carry it through implementation and `/finish-task` in this session.

## 1. Gather candidates (in parallel)

- **Open PRs** (`mcp__github__list_pull_requests`, state OPEN) — in-flight work
  is off the table; parallel web sessions must not collide. Also skim the last
  few merged PRs so you don't redo something that just landed.
- **Open issues** (`mcp__github__list_issues`, state OPEN) — the primary
  backlog. Most carry a done-when section and a stated priority. If the backlog
  has been triaged (`lark-rs/docs/LABELS.md`), prefer the labels: pull from
  `good-autonomous` + `prio:now` first, and treat `needs-decision` as off-limits
  for autonomous picks (it's the architect's inbox). Untriaged backlog → fall
  back to reading the bodies as below, and consider running `/triage` first.
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
4. **Skip as autonomous picks**: `prio:later` / "priority: low" items,
   research-scoped items with no validation story, and `needs-decision` /
   decision-checkpoint issues ("decision needed", "assess & challenge") — those
   need the architect (`PRINCIPLES.md` §4–5); surface them via `AskUserQuestion`
   only if nothing implementable remains. Don't invent groundable work to avoid
   asking. If the best next work is blocked only by architect decisions, run
   `/architect-brief` and report the blocking decision queue instead of asking
   ad hoc questions.

## 3. Announce, then execute

State the pick in one short paragraph — what, why it beats the alternatives, and
the done-when. Then **claim it before coding**: comment on the issue with your
branch/session intent, self-assign if possible, and set `status:in-progress`. If it
is already `status:in-progress` or carries another active claim, pick a different
task — never double-work an issue (parallel web sessions must not collide). Then
start immediately; do not wait for approval.

Follow the repo discipline: oracle/failing test first (Testing Philosophy in
`lark-rs/CLAUDE.md`), implement, `/xfail-burndown` if a bank shrank, then
`/finish-task` (review → fast gate → PR → CI subscription). If the task came
from an issue, put "Closes #N" in the PR description.
