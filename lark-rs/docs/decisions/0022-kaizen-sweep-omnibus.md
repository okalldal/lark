# ADR-0022: `/kaizen-sweep` drains the whole kaizen backlog via the omnibus pattern

- **Status:** Accepted
- **Date:** 2026-06-21

## Context

The original `/kaizen-sweep` model — pick 1–3 issues, one PR per concern, stop at
PR opened — has two structural problems:

- **It does not actually drain the backlog.** Most `kaizen` issues bundle several
  fixes, and the small-batch cap plus the single-working-branch constraint means one
  sweep clears one concern and leaves the queue ~unchanged. Process debt accretes
  between runs.
- **A scatter of tiny governance PRs is high-overhead for the architect.** Each
  kit/governance change is `escalate`-tier and needs an architect merge; draining the
  queue that way is N separate approval points.

`/start-sprint` (ADR-0018) already solved the analogous problem for the *product*
backlog: worker sub-agents open child PRs against a shared integration branch, the
orchestrator stages them, and the whole batch lands as **one omnibus PR** the architect
merges once — with per-child granularity preserved *inside* the omnibus.

## Decision

Rework `/kaizen-sweep` to **mirror ADR-0018's omnibus orchestration, scoped to the
`kaizen` backlog**. One sweep drains the **entire** open kaizen queue: each coherent
kit change is a child PR staged onto a `kaizen/<date>` integration branch, and the
batch lands as a single architect-merged omnibus PR. The kaizen command reuses the
sprint's shared machinery by reference (dispatch packet, verdict-only review, staging
queue, parking protocol, resumability, rollback-first, live Retrospective) and states
only the kaizen-specific deltas.

The kaizen invariants are **preserved**: one concern per child PR (independently
accept/reject/revert inside the omnibus); no product behavior change (a code/oracle
need → drop the label and re-triage); everything `escalate`-tier; `PRINCIPLES.md` edits
remain the architect's to author; staged ADRs stay `Proposed` until the architect
ratifies on merge. The change is *how the batch is shaped and landed* — bundled behind
one approval point instead of N — not *who ratifies*.

## Consequences

- The whole kaizen backlog is drained per sweep instead of a 1–3 batch, so process debt
  stops accreting between runs.
- The architect reviews one integrated omnibus (with independently-revertable child PRs
  inside) instead of a scatter of tiny PRs — fewer approval points, same accept/reject
  granularity.
- Inherits the sprint's resumability (omnibus body as the live ledger) and
  rollback-first net for free, and stays consistent with the established orchestration
  so there is one pattern to learn, not two.
- Cost: a sweep is now a heavier orchestration than a quick 1–3 batch; for a single
  trivial fix the setup (integration branch + draft omnibus) may exceed the benefit.
  Acceptable because kaizen is off-cadence and explicitly a *batch* ritual.
- **Supersedes** the "small batch, one PR each, stop at PR opened" model of the original
  `/kaizen-sweep`. Tripwire to revisit: if sweeps routinely carry only one concern, the
  omnibus overhead isn't paying for itself — reconsider a lightweight single-PR path.
