---
description: Drain a small batch of process/kit debt (kaizen issues) as architect-ratified proposal PRs, off the feature cadence
---

This is the codified **process-improvement** flow. Where `/next-task` and
`/start-sprint` move the *product* backlog, `/kaizen-sweep` improves the *kit* —
the commands, governance docs (`PRINCIPLES.md`, `LABELS.md`, ADRs), harness, and
review discipline. It runs **on demand**, never on feature cadence, because
process debt otherwise competes with features under the same rubric and most of it
is `escalate`-tier (it edits the constitution or the commands).

It **proposes; it does not ratify.** Every change rides its own PR for the
architect to merge — see PRINCIPLES.md §9 (governance/policy changes ride their own
PR; only the architect edits `PRINCIPLES.md`).

## 0. Invariants (binding)

- **`/kaizen-sweep` never auto-merges.** Governance-doc and command changes are
  `escalate`-tier by definition; the architect merges them.
- **One concern per PR.** A `kaizen` issue that bundles several fixes is split into
  one PR per coherent change, so the architect can accept/reject independently.
- **No product behavior change.** If a `kaizen` item turns out to need a code/oracle
  change to lark-rs itself, it is *not* kaizen — drop the `kaizen` label, re-triage
  it into the normal backlog, and leave it for `/next-task`.

## 1. Survey (in parallel)

- **Open `kaizen` issues** — `mcp__github__list_issues` with `labels: ["kaizen"]`,
  state OPEN. This is the queue.
- **Open PRs** — skip any `kaizen` issue that already has an open PR (don't collide).
- Read each issue's body + any architect comment. A `kaizen` issue should name the
  concrete kit change and where it lives (`.claude/commands/*`, `lark-rs/docs/*`,
  `.githooks/*`, a workflow). If it doesn't, it needs triage repair first — say so.

## 2. Pick a small batch (rubric, in order)

1. **A kit bug that is actively misleading or dead** (e.g. an instruction that can't
   be executed, a hook that misfires) → highest value; fix first.
2. **A recurring-failure-mode fix** — something a retrospective flagged more than
   once across workers/sprints (these prevent future waste).
3. **A clarity/contract tightening** (a brief that had to be re-explained, a missing
   convention) — cheap, compounding.
4. Prefer **small, independent** changes; cap the batch (≈1–3 issues) so the architect
   reviews a focused set, not a grab-bag.

Skip items that need an architect *decision* first (`needs-decision`) — surface them,
don't guess.

## 3. Execute each as its own proposal PR

For each picked issue, on a branch off the current default branch:

- Make the **minimal** edit the issue describes — command text, `LABELS.md` row,
  a `decisions/TEMPLATE.md` convention, a hook tweak, a workflow guard.
- If it changes a **load-bearing governance rule or a §3 default**, add or supersede
  an **ADR** in `lark-rs/docs/decisions/` in the same PR (the doc-maintenance rule),
  authored as **`Status: Proposed`** — never self-ratify; the architect flips it to
  `Accepted` on merge.
- Run the fast gate only if code/hooks/CI are touched (`lark-rs/scripts/check-fast.sh`);
  pure-doc/command PRs don't need it.
- Open the PR with `Closes #N`, a one-line rationale, and the before/after of the
  instruction or rule. Tag it as a kit/governance change.

Do **not** run `/finish-task` (it classifies merge tiers and can merge `auto` PRs);
kaizen PRs are always architect-ratified. Stop at "PR opened."

## 4. Report

Post a short summary: which `kaizen` issues were drained (with PR links), which were
left and why (needs-decision, too big, already has a PR), and the remaining `kaizen`
queue depth. The architect merges the PRs to land the improvements.

## Guardrails

- **Never bake kit fixes into a product PR or a sprint omnibus** (§9) — they ride
  their own PRs so they can be reverted independently and reviewed as governance.
- **Off-cadence by design:** do not run this inside `/start-sprint` or as part of a
  feature pick; it is a deliberate, separate ritual.
- A `kaizen` change that would alter `PRINCIPLES.md` is the architect's to write;
  `/kaizen-sweep` may *draft* a proposal but flags it explicitly for the architect.
