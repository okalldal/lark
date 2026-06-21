---
description: Proactively survey the repo and propose the next epics/issues for architect approval
---

The **proactive planning** layer — the modern form of the original phase plans.
Where `/next-task` *executes* the backlog, `/roadmap` *grows* it: survey the
state of the project, propose what should come next, and bring the proposal to the
architect. It **proposes; it does not execute, and it does not file unapproved
issues.** Direction is the architect's (`PRINCIPLES.md` §1).

## 1. Survey the ground truth (in parallel)

- **Open issues & PRs** (`mcp__github__list_issues` / `list_pull_requests`,
  state OPEN) — what's already in flight or filed; don't propose duplicates.
- **`lark-rs/docs/STATUS.md`** — phase completion, open follow-ups, wild-bank
  findings, the "deferred" notes.
- **XFAIL counts** (`lark-rs/tests/fixtures/oracles/**/*xfail*.json`) and
  `#[ignore]` gaps — known divergences that could anchor an epic.
- **`PRINCIPLES.md` §2/§3 and the ADR log** — so proposals respect the
  invariants and don't re-open settled decisions.

## 2. Synthesize, don't enumerate

Cluster what you found into a small number (≈2–4) of **candidate epics** — themes,
not tickets. For each, state in a few lines:

- **Theme & why now** — what capability or gap it closes, and what makes it the
  right next bet (unblocks others? burns down a bank? completes a phase?).
- **Grounding** — the falsifiable done-when this epic would be measured by
  (oracle, bank, scaling gate). If you can't name one, say so — that's a sign the
  epic is research-shaped and needs an architect framing first.
- **Blast radius & risk** (`PRINCIPLES.md` §3 default), and any dependency.
- **Draft child issues** — titles + one-line done-when each, in the §7 issue
  contract shape. *Drafts only*, not yet filed.

## 3. Bring it to the architect — do not file yet

Present the candidate epics and ask the architect to choose direction with
`AskUserQuestion` (recommended option first; enough context to decide without
scrolling). This is the "principal engineer proposes, architect signs off" gate.

Only **after** approval: **record the approval durably** — chat is not durable
state (`PRINCIPLES.md` §0). Capture the architect's go/no-go and the chosen
direction in GitHub — a tracking issue (or milestone) for the epic, or a roadmap ADR
for a phase-level direction call — referenced from each child. Then file the
approved issues (`mcp__github__issue_write`), applying labels per
`lark-rs/docs/LABELS.md` (`kind:`, `prio:`, topic, and `good-autonomous` vs
`needs-decision`), grouped under the relevant `phase-N` label / tracking issue.
Report back the filed issue numbers.

Anything genuinely undecidable (a real product/taste fork) is filed as
`needs-decision` rather than guessed — that is the correct outcome, not a failure.

### Decision memo skeleton

Any `needs-decision` issue created by `/roadmap` must include this skeleton so
that `/architect-brief` can synthesize reliably instead of reverse-engineering
prose:

```md
## Decision needed
## Background
## Recommended path
## Alternatives considered
## Consequences of recommended path
## Consequences of alternatives / deferral
## Unblocks
## Done-when
```
