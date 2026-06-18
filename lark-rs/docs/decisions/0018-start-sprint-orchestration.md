# ADR-0018: `/start-sprint` — whole-backlog autonomy via an integration branch + one architect-approved omnibus PR

- **Status:** Proposed
- **Date:** 2026-06-18

## Context

The autonomy kit drives the backlog **one issue at a time** (`/next-task` →
`/finish-task`). The architect asked for a single session that drives the *entire* open
backlog forward in one pass, under four constraints: don't dilute the session's context,
don't interrupt before the target is met, exploit parallelism, and keep merge gruntwork
off the architect's desk.

A naive "loop `/next-task` in one session" fails all four: each issue's diffs accumulate
in one context (dilution); a mid-issue fork triggers a synchronous `AskUserQuestion`
(interruption); issues run serially (no parallelism); and every PR still queues for the
architect (merge hell).

An *earlier* draft of this ADR fixed three of those but mis-handled the fourth: it had
worker/review sub-agents run `/review-pr` and let the orchestrator **auto-merge
`auto`-tier child PRs straight to `master`**. With ADR-0016 Accepted, `/review-pr` *can*
merge — so that path would let automation mutate `master` in a whole-backlog batch with
no single human approval point, which is exactly the blast radius that should not be
automated. The model below removes that.

## Decision

`/start-sprint` is a **thin orchestrator** over worker sub-agents that **never touches
`master` directly**. Concretely:

- **Workers never merge.** A worker executes one issue in an isolated worktree and opens
  a **child PR whose base is a temporary sprint integration branch** (created from the
  current `master`), not `master`. Child PR bodies use `Refs #N` / `Part of #<omnibus>`,
  never `Closes #N`.
- **Review is verdict-only during a sprint.** Review runs in a throwaway sub-agent that
  returns a tier (`auto` | `escalate` | `needs-decision`) + DoD status + rationale, and
  **must not merge, must not ask the architect synchronously, and must not mutate the PR
  beyond labels/comments.** `/start-sprint` does not invoke `/review-pr` in any
  acting/merge mode.
- **The orchestrator stages, it does not land.** It serially merges eligible child PRs
  (`auto` and `escalate` alike) **into the sprint integration branch** one at a time,
  rebasing the remaining child PRs after each so conflicts surface inside the session;
  the sprint branch is kept based on the current `master`.
- **One omnibus PR is the only thing that lands.** A single PR (`base: master`, `head:
  sprint branch`), opened **early as a draft** for continuous visibility and real-target
  CI, carries all the `Closes #N` lines and is merged **only by the architect**. No
  sprint path merges directly to `master`.

Workers keep the orchestrator context thin; a terminal-state predicate + park-don't-ask
+ `Monitor` (no `sleep`) keep it from interrupting; blast-radius waves give parallelism;
and the integration-branch staging queue plus the single omnibus PR keep merge gruntwork
off the architect's desk **without** letting automation write to `master`.

## Consequences

- **Avoids merge hell without letting automation mutate `master`.** Conflict resolution
  happens serially inside the sprint branch; the architect sees one integrated diff.
- **Preserves a single final human approval point** — merging the omnibus PR is the one
  privileged act, consistent with ADR-0016's rule that `escalate`/governance work is the
  architect's to land.
- **Keeps CI and review per child PR, plus one final CI on the integrated result** (the
  omnibus PR), so nothing lands without both per-change and whole-batch gates passing.
- **No reliance on conversation memory.** Durable state is the integration branch, the
  child PRs, the labels, and the omnibus PR body (PRINCIPLES.md §0); a rolled-over
  session rebuilds the plan from GitHub and resumes. The omnibus body is a **live
  ledger**, not an end-of-sprint summary: a *Staged* row (PR, issue, tier, evidence) is
  appended the instant a child PR is merged into the sprint branch — necessarily, since
  a staged child is no longer an *open* PR and the body is then its only record — so a
  roll-over mid-staging loses nothing.
- **Process improves itself.** Every worker, every review sub-agent, and the orchestrator
  emit a `RETRO:` block of process quirks (wrong/stale instructions, misfiring steps,
  missing know-how, context-draining tooling). These are harvested into the omnibus body's
  *Retrospective* section as they arrive (same durability rule as the ledger) and
  presented to the architect at close-out, with persistent fixes filed as their own
  governance follow-ups (§9) — so the kit sharpens sprint over sprint instead of
  re-hitting the same friction.
- It *adds scheduling*, not new authority: it inherits per-issue DoD (§6), ADR-0016
  tiers (nothing `auto`-merges to `master`; governance/`needs-decision` never auto), §9
  rollback-first, and escalate-don't-guess (§4–5).
- **Tripwire — integration drift.** If the sprint branch repeatedly diverges from
  `master` or needs frequent conflict resolution, the wave grouping is too coarse or
  concurrency too high: reduce concurrency or tighten blast-radius grouping.
- **Tripwire — omnibus too big to review.** If an `escalate` child PR (or the omnibus as
  a whole) is too large for a meaningful final review, split the sprint or require
  per-`escalate` architect approval *before* staging that child, rather than batching it.
- Depends on **ADR-0016 Accepted** (so the eventual omnibus *can* be merged) and
  **ADR-0017 Accepted** (the divergence-routing rule workers apply). If ADR-0016 is ever
  reverted, `/start-sprint` refuses to start (its §1 preflight) rather than staging work
  that can never land.
- This is a command/policy artifact, so per §9 it ships on its own PR and is
  escalate-tier.
