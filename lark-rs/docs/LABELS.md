# Label schema — the backlog state machine

GitHub labels are the durable state the autonomy commands read and write
(`PRINCIPLES.md` §0: state lives in git + GitHub). `/triage` keeps them accurate;
`/roadmap` applies them to new issues; `/next-task` reads them to pick work;
`/review-pr` reads the merge tier.

This schema **extends** the labels already in use — it does not replace them.

## Already in use (keep as-is) — topic / area

`lark-rs`, `earley`, `performance`, `testing`, `fuzzer`, `distribution`,
`phase-3`, `phase-4` … These tag *what the work is about*. Keep them. New topic
labels are fine ad hoc (e.g. `lexer`, `cyk`, `loader`). Phase labels (`phase-N`)
double as the epic grouping.

## Added by this kit — process metadata (colon-namespaced, so they read distinctly)

| Namespace | Values | Meaning |
|-----------|--------|---------|
| `kind:` | `bug` `feat` `refactor` `perf` `docs` `infra` | Class of work — drives the merge tier (`PRINCIPLES.md` §6). |
| `prio:` | `now` `next` `later` | Triage priority. `now` = next pick; `later` ≈ the old inline "Priority: low". |
| `status:` | `triaged` `in-progress` `needs-review` `blocked` | Where the item is in the lifecycle. Absent = untriaged. |
| `needs-decision` | (flag) | **The architect's inbox.** A fork only the architect can resolve (e.g. #159, #101, #95). `/next-task` never auto-picks these. A `needs-decision` issue should be written as a decision memo: background, decision needed, recommended path, alternatives, consequences, and unblocks — so `/architect-brief` can synthesize reliably. |
| `good-autonomous` | (flag) | Fully groundable, safe for an unattended `/next-task` pick — a done-when with an oracle and no open fork. **Fix-site verification status (ADR-0031):** before applying it `/triage` checks the issue's stated fix site against a fast repro. This is a *verification status*, not a blocking gate — a verified site is load-bearing, and an unverified site still keeps the flag but carries a "fix site unverified" note so the worker treats the named file as exploratory, not load-bearing. |
| `kaizen` | (flag) | **Process/kit debt**, not product work: a fix to the commands, governance docs, harness, or review discipline (usually surfaced by a sprint retrospective). Drained on a separate low cadence by `/kaizen-sweep`, never folded into feature cadence. Product-affecting retro items (e.g. an oracle/bank gap) stay in the normal backlog *without* this flag. |
| `frozen` | (flag) | **Parked pending a precondition** (e.g. a user-needs signal — ADR-0035). The work is *deferred, not rejected*: `/next-task`, `/xfail-burndown`, and `/bug-hackathon` **skip** `frozen` issues, and new findings in a frozen area are catalogued (XFAIL) rather than turned into fix-work. Un-freezing requires the documented trigger on the issue's tracking issue. Distinct from `status:blocked` (waiting on another *task*); `frozen` waits on a *product/architecture decision*. |

## Lifecycle

```
(filed) ──/triage──▶ status:triaged + kind: + prio: ──┐
                                                       │
        needs-decision ──▶ architect ──▶ removes flag ─┤
                                                       ▼
   /next-task picks good-autonomous ──▶ status:in-progress
                                                       │
                      /finish-task PR open ──▶ status:needs-review
                                                       │
            CI green + DoD met ──▶ merge (auto | escalate) ──▶ issue Closed
```

## Notes

- **Merge tier is derived, not stored:** `/review-pr` computes `auto` vs
  `escalate` from `kind:` + blast radius per `PRINCIPLES.md` §6, rather than a
  label that can go stale.
- **Creating the labels:** they don't exist on the repo yet. The first `/triage`
  run creates any missing label (idempotently, via the GitHub MCP) before
  applying it — no manual setup step.
- **`kaizen` is drained off-cadence.** `/next-task` and `/start-sprint` do **not**
  schedule `kaizen` issues — process/kit debt competes with feature work under the
  same rubric otherwise, and most of it is `escalate`-tier (touches commands or
  governance docs). The dedicated **`/kaizen-sweep`** surveys open `kaizen` issues,
  drains a small batch as proposal PRs, and leaves ratification to the architect.
