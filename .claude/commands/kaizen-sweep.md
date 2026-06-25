---
description: Drain the entire kaizen backlog in one orchestrated sweep — stage every kit/governance fix as a child PR onto a kaizen integration branch and land it as one architect-merged omnibus PR; the sweep proposes, never ratifies
---

> _Internal maintainer automation — not an invitation for external agents to claim issues or open PRs, and not a public bug-bounty program. See [`/CONTRIBUTING.md`](/CONTRIBUTING.md)._

This is the codified **process-improvement sweep**. Where `/next-task` and
`/start-sprint` move the *product* backlog, `/kaizen-sweep` improves the *kit* —
the commands, governance docs (`PRINCIPLES.md`, `LABELS.md`, ADRs), harness, and
review discipline. It runs **on demand**, off the feature cadence.

**One `/kaizen-sweep` drains the entire open `kaizen` backlog in a single
orchestrated session.** It mirrors `/start-sprint`'s omnibus mechanics (ADR-0018,
ADR-0022): each coherent kit change is a **child PR** staged onto a **kaizen
integration branch**, and the whole batch lands as **one omnibus PR** that only the
**architect** merges. The sweep **proposes; it does not ratify** — see PRINCIPLES.md
§9 (governance/policy changes ride their own PR; only the architect edits
`PRINCIPLES.md`).

> **Reuse, don't fork.** This command is the kaizen *specialization* of the sprint
> orchestration. For the shared machinery — the worker dispatch packet, the
> verdict-only review sub-agent contract, the staging queue, the parking protocol,
> resumability (GitHub is the ledger — reconstruct-at-finalize + a committed append-only
> residue file, **not** a churned PR body; ADR-0023), rollback-first, and the live
> Retrospective — **follow `.claude/commands/start-sprint.md` verbatim**, substituting
> "kaizen" for "sprint" throughout. The sections below state only the kaizen-specific
> **deltas**; everything not contradicted here is inherited from `/start-sprint`.

## 0. Invariants (binding — read first)

The sprint §0 invariants all hold (workers/review sub-agents never merge; only the
orchestrator stages into the integration branch; only the architect merges the
omnibus; nothing reaches `master` except that one merge). Plus the kaizen-specific:

- **One concern per child PR.** A `kaizen` issue that bundles several fixes is split
  into **one child PR per coherent change**, so the architect can accept/reject each
  independently *inside* the omnibus and revert it independently afterward (§9).
- **No product behavior change.** If a `kaizen` item turns out to need a code/oracle
  change to lark-rs itself, it is *not* kaizen — **drop the `kaizen` label, re-triage
  it into the normal backlog**, exclude it from the sweep, and leave it for
  `/next-task`.
- **Everything is `escalate`-tier.** Kit/governance changes are escalate by
  definition; the verdict-only review classifies but **nothing auto-merges** (matches
  a sprint's deliberately-withdrawn merge authority).
- **A `PRINCIPLES.md` change is the architect's to write.** The sweep may *draft* a
  proposal child PR and flag it explicitly for the architect, but never authors the
  constitution edit itself.

## 1. Preflight (refuse to start unless all hold)

- **ADR-0018 Accepted** and **ADR-0022 Accepted** — the omnibus orchestration and its
  kaizen application; refuse to run if either is not Accepted (matches ADR-0016's
  staged-activation style).
- **Green `master` CI** — never sweep on a broken base.
- **Capture the current `master` SHA** as the immutable **sweep base**.

(There is no `good-autonomous` triage gate as in a sprint — `kaizen` issues are kit
work, not product work. The schedulability bar is §3's: each picked concern must name
a concrete kit change and where it lives.)

## 2. Create the integration branch + draft omnibus PR (early)

Sprint §2 mechanics, kaizen-named:

```bash
git checkout -b kaizen/YYYYMMDD-HHMM <master-sha>
git commit --allow-empty -m "kaizen: seed omnibus ledger"
git push -u origin kaizen/YYYYMMDD-HHMM
```

Open the **omnibus PR immediately, as a draft** — base `master`, head the kaizen
branch, title `kaizen: omnibus <date/short-sha>`, body seeded as a **stable pointer +
short summary** (sprint base SHA + a pointer to `lark-rs/docs/sprints/<kaizen-id>.md`,
plus rough *Parked* / *Triage-repair* / *Follow-ups* lines) — **not** a per-stage live
ledger (ADR-0023; sprint §2/§6/§7). The omnibus is the **only** PR that targets `master`
and the **only** one carrying `Closes #N` lines.

## 3. Survey + plan the whole kaizen backlog

In parallel: `mcp__github__list_issues` with `labels: ["kaizen"]`, state OPEN (the
queue) + `mcp__github__list_pull_requests` OPEN (skip any kaizen issue that already has
an open PR — don't collide).

Read each open `kaizen` issue's body + any architect comment. A `kaizen` issue must
name the **concrete kit change and where it lives** (`.claude/commands/*`,
`lark-rs/docs/*`, `.githooks/*`, a workflow). Then classify:

- **Schedulable** — names a concrete kit change + location, not `needs-decision`, no
  open PR. **Decompose it into one *concern* per coherent change** (a 3-item issue →
  3 concerns → 3 child PRs). The concern, not the issue, is the unit of work.
- **Needs triage repair** — a `kaizen` issue with no concrete change/location → do
  **not** dispatch; record it in the omnibus *Triage-repair needed* section for the
  close-out.
- **`needs-decision`** — surface in the parked inbox; never pick (a sweep forbids
  mid-run `AskUserQuestion`).
- **Not kaizen** — needs a product code/oracle change → drop the `kaizen` label,
  re-triage, exclude (Invariant §0).

Group concerns into **waves by file/blast-radius overlap** so parallel workers don't
collide (same-file concerns serialize; e.g. two edits to `start-sprint.md`). Cap
concurrency at **~3**.

Because the sweep drains the *whole* queue, **re-evaluate the queue against GitHub each
cycle** (sprint §7) and schedule successive waves until no schedulable concern remains
non-terminal — do not stop at a small batch.

## 4. Dispatch a wave (parallel worker sub-agents — child PRs only)

Sprint §4 dispatch, with a kaizen worker brief. The startup context packet adds an
`issue concern:` field naming the specific change. Each worker takes **one concern** and:

- reads the issue + the repo rules it needs (`CLAUDE.md`, `lark-rs/docs/PRINCIPLES.md`
  §9 governance-PR rule, `lark-rs/docs/LABELS.md`);
- makes the **minimal** edit the concern describes — command text, a `LABELS.md` row, a
  `decisions/TEMPLATE.md` convention, a hook tweak, a workflow guard;
- if the concern changes a **load-bearing governance rule or a §3 default**, adds or
  supersedes an **ADR** in the same child PR, authored **`Status: Proposed`** (never
  self-ratify — the architect flips it to `Accepted` on omnibus merge) + a README index
  row;
- runs the fast gate **only if code/hooks/CI are touched**
  (`lark-rs/scripts/check-fast.sh`); pure-doc/command concerns skip it;
- opens a **child PR based on `<kaizen-branch>`** (NOT `master`) carrying **`Refs #N` +
  `Part of #M`**, a one-line rationale, and the **before/after** of the instruction or
  rule, with **no closing keyword**;
- if the concern would touch `PRINCIPLES.md`, **drafts the proposal but flags it for the
  architect** (does not author the constitution edit) — return `NEEDS_DECISION:` if
  unsure;
- ends with a `RETRO:` block.

## 5. Review — verdict-only (sprint §5), kaizen DoD

Per child PR, run the verdict-only review sub-agent (never merges, never mutates GitHub
state). The kaizen Definition of Done differs from the product DoD: there is **no
failed-first oracle** for a doc/command change — verify instead (PRINCIPLES §6 DoD-1,
docs/governance arm) that **the policy/command path was walked through and any
contradictions resolved**, the **before/after is accurate**, the child carries `Refs #N`
+ `Part of #M` with **no closing keyword**, any ADR is `Status: Proposed`, and the change
is **kit-only (no product behavior change)**. **Tier is always `escalate`.** Route a
`needs-decision` via the sprint §5 parking protocol.

## 6. Staging queue (sprint §6)

Stage eligible child PRs into the kaizen branch one at a time; **do not rewrite the PR
body per stage** — the staged fact is reconstructable from the merge commit + child PR
body, so the *Staged* table is rebuilt at finalize (sprint §6/§7, ADR-0023). In the same
step, only persist the **irreducible residue** (orchestrator/review `RETRO:`, synced SHAs)
by appending + committing `lark-rs/docs/sprints/<kaizen-id>.md`. Rebase the remaining child
PRs; keep `master` *merged into* (never rebase) the kaizen branch. CI waits and CI-fix
dispatch as sprint §6.

**`Closes #N` for multi-concern issues:** the omnibus carries `Closes #N` only once
**every** concern of issue #N is staged (reconstructed at finalize). Until then #N's
concerns are partially staged — the reconstructed table references the concern, not yet
the close.

## 7–9. Finalize, terminal states, close-out (sprint §7–§9)

Finalize/verify the omnibus (sprint §7), apply the terminal-state predicate (sprint §8,
kaizen-flavored: *in the green omnibus* / *parked needs-decision* / *triage-repair
needed* / *already-PR'd-or-closed*), and after the **architect merges the omnibus** run
the close-out (sprint §9): verify the included issues closed, report the **remaining
kaizen queue depth**, the parked inbox, triage-repair items, follow-ups, and the
aggregated **Retrospective**.

**Inherited (sprint §9), restated because it bites hardest here: filing every
retro-flagged kaizen item is a required, checkable close-out deliverable.** A sweep
*generates* process/kit retro notes by the handful, so its close-out is the most likely
place to silently drop them (the exact §7 violation #284 committed — dropped them,
recovered late as #309–#314). For every worker/review/orchestrator `RETRO:` note tagged
"kaizen" / "KIT BUG" / "file as follow-up," confirm a `kaizen`-labelled tracking issue
exists and link it; the close-out is **not done** until each is filed or explicitly
marked already-tracked/duplicate. Report follow-ups in the sprint §9 **two enumerated
arms — *product follow-ups filed* AND *kaizen follow-ups filed* — separately**, so a
zero-kaizen arm is conspicuous and must be justified, not silent.

## Guardrails (binding)

- **Off-cadence by design** — never run inside `/start-sprint` or as part of a feature
  pick; it is a deliberate, separate ritual.
- **Never bake kit fixes into a product PR or a sprint omnibus** (§9) — they ride the
  **kaizen** omnibus so they can be reverted independently and reviewed as governance.
- **Resumability, rollback-first, no mid-run `AskUserQuestion`** — inherited from the
  sprint guardrails (GitHub is the ledger — reconstruct-at-finalize + the committed residue
  file, ADR-0023; a bad stage is reverted out of the integration branch before it can
  escape, since nothing reaches `master` until the omnibus merge).
- A `kaizen` change that would alter `PRINCIPLES.md` is the architect's to write; the
  sweep may *draft* a proposal child PR but flags it explicitly for the architect.
