---
description: Drive the entire open backlog onto a sprint integration branch in one orchestrated session, landing it as a single architect-approved omnibus PR — workers never merge, nothing reaches master except the omnibus
---

This is the codified **whole-backlog sprint**. Where `/next-task` takes *one* issue,
`/start-sprint` runs the loop over *all* schedulable open issues in a single session,
**staging** every result onto a temporary integration branch and presenting the whole
batch to you as **one omnibus PR** to `master`.

It **orchestrates; it does not implement, and it does not land to `master`.** The
session that runs this stays thin: it schedules workers, owns review and the staging
queue, and maintains the omnibus PR. All code is written by **worker sub-agents** whose
context is discarded — so a 12-issue sprint costs the orchestrator a few lines per
issue, not a window full of diffs.

## 0. The hard invariant (binding — read first)

These are non-negotiable. If any of them cannot hold, **stop and tell the architect**
rather than proceeding:

- **Worker sub-agents NEVER merge a PR.** They open child PRs and stop.
- **Review sub-agents NEVER merge a PR.** Review during a sprint is *verdict-only*.
- **`/start-sprint` never invokes `/review-pr` in a mode that can merge.** It uses the
  verdict-only path in §5; it does not run `/review-pr`'s normal acting/merge flow.
- **Only the sprint orchestrator may merge child PRs — and only into the sprint
  integration branch**, never into `master`.
- **Only the architect may merge the final omnibus PR into `master`.**
- **No sprint path ever merges directly to `master`.** `master` is mutated by exactly
  one event in the whole sprint: the architect merging the omnibus PR.

> **Why this shape.** ADR-0016 (Accepted) lets `/review-pr` merge `auto`-tier PRs
> *outside* a sprint. Inside a sprint that authority is deliberately withdrawn: a
> whole-backlog batch is exactly the case where automation must not touch `master`, so
> the sprint trades per-PR auto-merge for one human approval point on the integrated
> result. See ADR-0018.

## 1. Preflight (refuse to start unless all hold)

- **ADR-0016 Accepted** — otherwise staged work could never land; stop and report.
- **ADR-0017 Accepted** — the divergence-routing rule the workers rely on.
- **Green `master` CI** — never sprint on a broken base.
- **Triaged backlog** — run `/triage` first if issues are unlabeled; the labels are the
  state machine this loop reads.
- **Capture the current `master` SHA** as the immutable **sprint base**.

## 2. Create the integration branch + draft omnibus PR (early)

1. Create a fresh branch from the captured `master` base:
   `sprint/YYYYMMDD-HHMM` (or `sprint/<short-base-sha>`).
2. Open the **omnibus PR immediately, as a draft**, so you get continuous visibility
   into the integrated diff and GitHub Actions runs on the real final target shape
   throughout the sprint:
   - **base:** `master`
   - **head:** the sprint integration branch
   - **title:** `sprint: omnibus <date/short-sha>`
3. The omnibus PR is the **only** PR that will ever target `master`, and the **only**
   PR that carries `Closes #N` lines (added in §7). Child PRs do not.

## 3. Build the plan (thin, from GitHub state)

In parallel: `mcp__github__list_issues` (OPEN) + `mcp__github__list_pull_requests`
(OPEN). From the labels (`lark-rs/docs/LABELS.md`), classify each open issue:

- `needs-decision` → already terminal; collect for the memo, **never pick**.
- `status:in-progress` / already has a linked PR → claimed; skip.
- `status:blocked` → defer until its named blocker reaches a terminal state.
- everything else → **work to schedule**.

Group the schedulable issues into **waves by blast-radius overlap** so parallel workers
don't collide: use each issue's *Files* section + topic labels as the key — same-module
issues serialize (e.g. an `earley.rs` cluster, or the loader/EBNF path), disjoint-file
issues run concurrently. Cap concurrency at **~3** workers to bound CI cost and thrash.

## 4. Dispatch a wave (parallel worker sub-agents — child PRs only)

For each issue in the wave, launch one `Task` (general-purpose) sub-agent with
`isolation: "worktree"`. Send the independent ones **in a single message** so they run
concurrently. The worker brief:

> Execute issue **#N** in your own worktree. Claim it first (`status:in-progress`).
> Follow the repo's oracle-first discipline (`lark-rs/CLAUDE.md`): a failing test
> before the fix, banks green after. Run `/code-review` and the fast gate
> (`lark-rs/scripts/check-fast.sh`) on your diff. Then open a **child PR whose base is
> the sprint integration branch `<sprint-branch>`** (NOT `master`); its body uses
> **`Refs #N`** (or `Part of #<omnibus>`) — **never `Closes #N`**, because the child
> targets a non-default branch and closing keywords would not fire correctly anyway.
> **Do NOT run `/review-pr` in any acting/merge mode. Do NOT merge anything.** If you
> hit a fork only the architect can settle — a genuine `needs-decision` (taste, product
> direction, a real trade-off with no oracle) — **STOP, do not guess**, and return
> `NEEDS_DECISION:` plus a crisp, self-contained writeup (context + options +
> recommendation). Otherwise return **only**: child PR number, issue number, the test
> evidence (what now passes that failed before), and a one-line summary.

Record each compact result in the plan table. The worker's file reads, diffs, and test
output never enter this session.

## 5. Review — verdict-only, in a throwaway sub-agent (never merges)

The orchestrator owns review. For each child PR, run a **sprint-only, verdict-only**
review in a fresh review sub-agent (the diff stays out of this session's context). This
is *not* `/review-pr`'s normal flow: the review sub-agent

- **must not call `merge_pull_request`**,
- **must not ask the architect synchronously** (no `AskUserQuestion`),
- **must not mutate the PR** except optionally labels/comments,

and returns exactly: **DoD status**, **tier** (`auto` | `escalate` | `needs-decision`),
a **short rationale**, and **missing items** if any. (If you prefer a flag, this is the
behavior of a `/review-pr --verdict-only` invocation; the contract above is what
"verdict-only" means.) Route the verdict:

- **`auto`** — eligible to stage into the integration branch (§6).
- **`escalate`** — *also* eligible to stage into the integration branch, but final
  approval is **deferred to the architect through the omnibus PR**; it is never merged
  to `master` mid-sprint. (Governance/policy child PRs are always `escalate`.)
- **`needs-decision`** — **not staged.** Park it (file/label the `needs-decision`
  issue) and include it in the close-out memo.

## 6. Staging queue — serially merge child PRs into the integration branch

This is staging onto the sprint branch, **not** landing to `master`:

- The orchestrator merges eligible child PRs (`auto` or `escalate`) into the sprint
  integration branch **one at a time**.
- After each child PR is staged:
  - **rebase/update the remaining open child PRs** onto the new sprint-branch tip
    (`mcp__github__update_pull_request_branch`) so any conflict surfaces **now**;
  - if a rebase conflicts, **dispatch a worker** to resolve it in-worktree and re-push,
    then continue the queue;
  - keep going until the wave's eligible PRs are all staged.
- **The sprint branch must stay based on the current `master`.** If `master` moves
  during the sprint, refresh the sprint branch against current `master`, resolve any
  conflicts **inside the sprint** (dispatch a worker), and rerun the relevant checks —
  the omnibus diff must always be "what lands on top of today's `master`".

Wait on CI without polling-by-sleep: after a wave, wait on in-flight child PRs and the
omnibus with the **`Monitor`** tool's until-loop over `mcp__github__pull_request_read`
(`get_status` / `get_check_runs`) — **never** Bash `sleep`. A child PR red on CI →
dispatch a worker to fix (≤2 rounds); still red and out of scope → convert its issue to
`needs-decision` (or `status:blocked` with the blocker named), park it, and move on so
one stuck PR doesn't stall the sprint.

## 7. The omnibus PR — the one and only landing PR

Re-evaluate the §3 plan against GitHub each cycle; schedule the next wave (newly
unblocked issues, each rebased on the new sprint tip) until no schedulable issue is
non-terminal. **Then** prepare the omnibus PR for the architect. Before marking it
**ready for review** (out of draft), confirm:

- current `master` is an **ancestor** of the sprint integration branch;
- the **omnibus PR CI is green**;
- **all staged child PRs are merged** into the sprint branch;
- **no child PR remains in a non-terminal state** (each is staged, or parked as
  `needs-decision`, or `blocked` with a named blocker).

Then update the **omnibus PR body** so it owns the whole sprint's record:

- the list of **included child PRs**, each with its tier (`auto` | `escalate`);
- the list of **included issues as `Closes #N`** (these live on the omnibus *only*);
- **review + CI evidence** per child;
- any **`needs-decision` items excluded** from the sprint, with their memos;
- any **follow-up issues** filed during the sprint.

**The architect gives final approval by merging the omnibus PR into `master`.** The
session does not merge it.

## 8. Terminal states (the loop's goal predicate)

An issue is **terminal** when it is exactly one of:

1. **Included in the green omnibus PR**, awaiting the architect's approval/merge.
2. **Excluded and parked as `needs-decision`** (in the close-out memo).
3. **`blocked` with a named blocker** that is itself terminal.
4. **Already closed/merged** before this sprint pass.

Do **not** describe any issue as "merged to `master`" until the architect has merged the
omnibus PR. Until then the honest status is "staged, awaiting omnibus approval".

## 9. Close-out after the architect merges the omnibus

The sprint is finished only once the omnibus PR is merged by the architect. After that:

- verify the **included issues are closed** (the omnibus `Closes #N` lines should fire
  on merge to the default branch); for any that did not auto-close, **close/comment
  manually** referencing the omnibus PR;
- verify each child PR is either **merged into the sprint branch** or **explicitly
  superseded** by the omnibus (comment + close);
- **clean up the sprint integration branch** if appropriate;
- post the single batched close-out: what landed, the parked `needs-decision` inbox
  (each with a recommendation, `/triage`-shaped), and any follow-ups filed.

## Guardrails (binding)

- **No `AskUserQuestion` mid-sprint** — a blocking prompt defeats "run until the target
  is met". Forks are parked as `needs-decision` issues (a terminal state) and surfaced
  together in the close-out.
- **Resumable.** All durable state lives in GitHub branches, child PRs, labels, and the
  omnibus PR body (PRINCIPLES.md §0). If this session is summarized or restarted
  mid-sprint, the next invocation rebuilds the plan from the integration branch + open
  child PRs and continues — no progress lives in conversation memory.
- **Rollback-first (§9).** If a staged change reddens the omnibus CI, revert it out of
  the integration branch immediately (and open an incident issue) — *then* diagnose.
  Because nothing reaches `master` until the omnibus merge, a bad stage never escapes
  the sprint branch.
- One review + one CI run per child task; never run the full CI locally (the
  `pull_request` run is the gate).
- The sprint only *parks* `needs-decision` issues — it never resolves their substance.
