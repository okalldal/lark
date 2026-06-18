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
- **ADR-0018 Accepted** — this command *is* the ADR-0018 policy; refuse to run if its own
  ADR is not Accepted (matches ADR-0016's staged-activation style).
- **Green `master` CI** — never sprint on a broken base.
- **Triaged backlog** — the labels are the state machine this loop reads. `/triage` is
  **dry-run by default** and only `/triage apply` mutates labels (after architect
  approval), and a sprint forbids mid-run `AskUserQuestion` — so if backlog labels are
  missing/stale, **stop before creating the sprint branch** and report that the architect
  must run `/triage apply` first. Do **not** start a sprint from an untriaged state.
- **Capture the current `master` SHA** as the immutable **sprint base**.

## 2. Create the integration branch + draft omnibus PR (early)

1. Create a fresh branch from the captured `master` base and **seed it with one empty
   commit** — a branch identical to `master` has no commits ahead of base, and GitHub
   will refuse to open a PR with nothing between base and head, so the seed is what makes
   the *early* draft omnibus possible:
   ```bash
   git checkout -b sprint/YYYYMMDD-HHMM <master-sha>   # or sprint/<short-base-sha>
   git commit --allow-empty -m "sprint: seed omnibus ledger"
   git push -u origin sprint/YYYYMMDD-HHMM
   ```
2. Open the **omnibus PR immediately, as a draft**, so you get continuous visibility
   into the integrated diff and GitHub Actions runs on the real final target shape
   throughout the sprint:
   - **base:** `master`
   - **head:** the sprint integration branch
   - **title:** `sprint: omnibus <date/short-sha>`
   - **body:** seed it as the **live sprint ledger** (see §6) — the sprint base SHA,
     and sections for *Staged*, *In-flight child PRs*, *Parked needs-decision*,
     *Follow-ups filed*, and *Retrospective* (see the Retrospective section). This body
     is the sprint's durable state from now on, not a thing assembled at the end. The
     *Staged*, *Parked*, *Follow-ups*, and *Retrospective* sections are **authoritative**
     (resume relies on them); the *In-flight child PRs* section is a **convenience
     mirror** of the open child PRs — keep it roughly current, but resume reconstructs
     in-flight state from the live open-PR list, not from this section.
3. The omnibus PR is the **only** PR that will ever target `master`, and the **only**
   PR that carries `Closes #N` lines (filled in as children are staged, §6). Child PRs
   do not.

## 3. Build the plan (thin, from GitHub state)

In parallel: `mcp__github__list_issues` (OPEN) + `mcp__github__list_pull_requests`
(OPEN). From the labels (`lark-rs/docs/LABELS.md`), classify each open issue. An issue is
**schedulable** only if **all** hold (the sprint forbids mid-run architect questions, so
the bar is higher than a single `/next-task` pick):

- labelled **`good-autonomous`** — fully groundable, safe for unattended work (a done-when
  with an oracle and no open fork);
- **not** `needs-decision` (architect's inbox — collect for the memo, never pick);
- **not** `status:in-progress` / `status:needs-review`, and has **no** open linked PR
  (already claimed → skip);
- **not** `status:blocked` (defer until its named blocker is terminal);
- has a **parseable Done-when and Files/blast-radius** in the issue body.

Triaged but **not `good-autonomous`** → report as *not sprint-schedulable* (it needs an
architect call or more grounding); do not dispatch it. An otherwise-schedulable issue
**missing Done-when or Files** → stop before dispatch and report that triage repair is
needed — never hand a worker an issue without a falsifiable done-when.

Group the schedulable issues into **waves by blast-radius overlap** so parallel workers
don't collide: use each issue's *Files* section + topic labels as the key — same-module
issues serialize (e.g. an `earley.rs` cluster, or the loader/EBNF path), disjoint-file
issues run concurrently. Cap concurrency at **~3** workers to bound CI cost and thrash.

## 4. Dispatch a wave (parallel worker sub-agents — child PRs only)

For each issue in the wave, launch one `Task` (general-purpose) sub-agent with
`isolation: "worktree"`. Send the independent ones **in a single message** so they run
concurrently. A fresh `Task` inherits **none** of this session's working memory — only
the prompt, the checkout, and tool access — so the brief must carry a real context
packet, not a bare issue number.

**Worker startup context packet (required — fill every field from the §3 plan):**

```
repo:               okalldal/lark
issue:              #N
issue title:        <title>
issue labels:       <labels>
issue Done-when:    <the issue's done-when, verbatim>
issue Files:        <expected files / blast radius>
issue Notes:        <Notes / Decision-needed summary, if any>
linked PRs:         <any already found by the orchestrator, else "none">
sprint branch:      <sprint-branch>
sprint tip SHA:     <sprint-tip-sha>     # current tip, not master
omnibus PR:         #M
```

The worker brief:

> **Before editing**, read issue **#N**'s body, comments, labels, and any linked PRs, and
> restate its **Done-when** and **Files / blast radius** to yourself. Also read the repo
> rules a normal session would have in context (you start with none): `CLAUDE.md`
> (repo-level active-work + binding autonomy core), `lark-rs/CLAUDE.md` (testing, oracle,
> generated-file, architecture notes), `lark-rs/docs/PRINCIPLES.md` §2/§4/§6/§7
> (invariants, decision routing, DoD, the issue contract), and `lark-rs/docs/LABELS.md`
> (the label state machine). If the issue is no longer schedulable — already
> `status:in-progress` or otherwise claimed, `blocked`, or it contains an unresolved
> decision fork — **stop and return `NEEDS_DECISION:` or `BLOCKED:`; do not code.**
>
> **Claim it before coding** (the `/next-task` protocol, not just the label): comment on
> the issue with your branch/session intent, self-assign if possible, and set
> `status:in-progress`. If it is already claimed, stop and return `BLOCKED:` — never
> double-work an issue (parallel workers must not collide).
>
> **Branch from the sprint tip, not `master`.** Create your working branch from
> `<sprint-branch>` at `<sprint-tip-sha>`. Before opening or updating the child PR, fetch
> and rebase onto the **current** `<sprint-branch>` tip. If that rebase conflicts, **stop
> and report the conflict** — do **not** retarget the PR to `master`.
>
> Follow the repo's oracle-first discipline (`lark-rs/CLAUDE.md`): a failing test before
> the fix, banks green after. Run `/code-review` **if available; otherwise launch a fresh
> review sub-agent over your branch diff** and address its findings before opening the
> child PR. Run the fast gate (`lark-rs/scripts/check-fast.sh`).
>
> **Do NOT run `/finish-task`.** This brief *replaces* it for sprint work. `/finish-task`
> (and `lark-rs/CLAUDE.md`'s "finishing a task" pointer) targets ordinary single-issue
> work: it requires `Closes #N`, classifies a merge tier, and invokes `/review-pr` —
> which, now that ADR-0016 is Accepted, can **merge** an `auto` PR. All of that is
> forbidden here. You stop at "child PR opened against the sprint branch."
>
> Open a **child PR whose base is `<sprint-branch>`** (NOT `master`). Its body **must
> include both** links — **`Refs #N`** for the originating issue **and** **`Part of #M`**
> for the omnibus PR (`Part of #M` is *not* a substitute for `Refs #N`; the reviewer
> follows `Refs #N` to the issue) — plus: a one-line Done-when summary; the **failed-first
> / oracle / repro evidence** (what now passes that failed before); the **`/code-review`
> summary and how findings were addressed**; the local gate run
> (`lark-rs/scripts/check-fast.sh`); and any follow-ups filed. Its body
> **must not contain `Closes #N`, `Fixes #N`, or `Resolves #N`** — only the omnibus owns
> closing keywords (and on a non-default base they would not fire anyway). Putting this
> evidence *in the PR body* is what lets the independent verdict-only reviewer (§5) judge
> the child without inferring or failing it for missing evidence.
>
> **Do NOT run `/review-pr` in any acting/merge mode. Do NOT merge anything.** If you hit
> a fork only the architect can settle — a genuine `needs-decision` (taste, product
> direction, a real trade-off with no oracle) — **STOP, do not guess**, and return
> `NEEDS_DECISION:` plus a crisp, self-contained writeup (context + options +
> recommendation). Otherwise return **only**: child PR number, issue number, the test
> evidence (what now passes that failed before), and a one-line summary.
>
> **End every return with a `RETRO:` block** (see the Retrospective section): terse
> bullets on any process quirk you hit — a wrong/stale instruction, a confusing step, a
> missing piece of know-how, a tool that misbehaved, anything that wasted effort or
> context a future run should be warned about. Write `RETRO: none` if there was nothing.

Record each compact result in the plan table. The worker's file reads, diffs, and test
output never enter this session.

## 5. Review — verdict-only, in a throwaway sub-agent (never merges)

The orchestrator owns review. For each child PR, run a **sprint-only, verdict-only**
review in a fresh review sub-agent (the diff stays out of this session's context), handing
it a **review startup context packet** (it inherits no memory, and this carries the issue
number even if the PR body is malformed — a malformed link then becomes a DoD failure, not
a context failure):

```
repo:           okalldal/lark
child PR:       #P
issue:          #N
issue title:    <title>
issue labels:   <labels>
sprint branch:  <sprint-branch>
omnibus PR:     #M
expected base:  <sprint-branch>
closure rule:   child carries `Refs #N` + `Part of #M`; omnibus owns `Closes #N`
```

This is *not* `/review-pr`'s normal flow: the review sub-agent

- **must not call `merge_pull_request`**,
- **must not ask the architect synchronously** (no `AskUserQuestion`),
- **must not mutate GitHub state at all** — no labels, no comments, no PR edits. It
  **returns the verdict only**; the **orchestrator** owns every durable write (comments,
  labels, parking, ledger), so all state changes happen in one place and resume has one
  source of truth.

**The review sub-agent must read** (it inherits no context): the child PR diff + CI
status + body; the referenced issue's body, comments, and labels; `lark-rs/docs/PRINCIPLES.md`
§6 (the DoD + merge-tier rules — `auto` is gated bugfix/xfail/perf/refactor/trivial-docs;
`escalate` is new API, new grammar semantics, architecture, governance/policy docs, and
anything `needs-decision`); `lark-rs/docs/LABELS.md`; and
`.claude/commands/start-sprint.md` §0, §5, §6.

**Do not invoke the normal `/review-pr` command** unless the environment explicitly
provides a *verified* verdict-only mode that cannot merge. The repo's `/review-pr` is
`/review-pr <number>` and its normal action path **merges** `auto` PRs now that ADR-0016
is Accepted — exactly what a sprint forbids. So perform the DoD checklist below manually
in this throwaway sub-agent and **return the verdict only**.

**Sprint-child DoD override.** Apply the §6 Definition of Done **except** replace the
normal "PR body says `Closes #N`" item with the sprint-child closure contract:

- the child PR **targets `<sprint-branch>`**, not `master`;
- the body carries **both** `Refs #N` (originating issue) **and** `Part of #M` (omnibus) —
  a missing `Refs #N` is a **DoD failure**, not just a context gap;
- the body contains **no closing keyword** (`Closes #N` / `Fixes #N` / `Resolves #N`);
- the eventual `Closes #N` is owned by the **omnibus ledger**, not the child PR.

A faithful reviewer must **not** fail a child PR for "missing `Closes #N`" — that is
*required* here. The review returns exactly: **DoD status** (against the override above),
**tier** (`auto` | `escalate` | `needs-decision`), a **short rationale**, **missing
items** if any, and a closing **`RETRO:` block** (process quirks worth surfacing, or
`RETRO: none`). Route the verdict:

- **`auto`** — eligible to stage into the integration branch (§6).
- **`escalate`** — *also* eligible to stage into the integration branch, but final
  approval is **deferred to the architect through the omnibus PR**; it is never merged
  to `master` mid-sprint. (Governance/policy child PRs are always `escalate`.)
- **`needs-decision`** — **not staged.** Park it via the **parking protocol** below.

**Parking protocol (do all of it in the same step the issue is parked).** Because workers
claim issues with `status:in-progress`, a parked issue left with a stale claim would
confuse resume and future `/next-task` runs (the label schema *is* the backlog state
machine). So whenever an issue becomes `needs-decision` or `blocked` — here, or via the
§6 CI path — update GitHub atomically: **remove the stale `status:in-progress` /
`status:needs-review`**, **add `needs-decision`** (or **`status:blocked` with the named
blocker**), **post the self-contained memo** (context + options + recommendation) on the
issue, and **append the parked row + its `RETRO:` bullets to the omnibus ledger**. Only
then move on.

## 6. Staging queue — serially merge child PRs into the integration branch

This is staging onto the sprint branch, **not** landing to `master`:

- The orchestrator merges eligible child PRs (`auto` or `escalate`) into the sprint
  integration branch **one at a time**.
- **Update the omnibus ledger in the same step you stage a child PR** — this is the
  load-bearing move for resumability (§ guardrails). The instant a child PR is merged
  into the sprint branch it stops being an *open* PR, so the omnibus body is the **only**
  durable record that it happened. After each stage, append to the omnibus body a
  *Staged* row carrying: the child PR number, the issue(s) it covers as a `Closes #N`
  line, its tier (`auto` | `escalate`), and its review + CI evidence. Do this **before**
  moving to the next child PR, so a roll-over between stages never loses a staged child.
  In the same step, **append the child's (and its review's) `RETRO:` bullets to the
  omnibus *Retrospective* section** (see the Retrospective section) — and likewise when
  you harvest a parked or failed result — so no retro note depends on conversation memory.
- After each child PR is staged:
  - **rebase/update the remaining open child PRs** onto the new sprint-branch tip
    (`mcp__github__update_pull_request_branch`) so any conflict surfaces **now**;
  - if a rebase conflicts, **dispatch a worker** to resolve it in-worktree and re-push,
    then continue the queue;
  - keep going until the wave's eligible PRs are all staged.
- **The sprint branch must stay based on the current `master` — but never by rewriting
  it.** Once any child PR exists, **do not rebase or force-push the sprint integration
  branch** (child PRs target it; rewriting it would break their bases). If `master` moves
  during the sprint, **merge `origin/master` *into* the sprint branch** (a real merge
  commit), resolve any conflicts **inside the sprint** (dispatch a worker), **record the
  synced `master` SHA in the omnibus ledger**, and rerun the relevant checks. Child PR
  *branches* are rebased onto the sprint branch (§ above); the sprint branch itself only
  ever moves forward. The omnibus diff must always be "what lands on top of today's
  `master`".

Wait on CI without polling-by-sleep: after a wave, wait on in-flight child PRs and the
omnibus with the **`Monitor`** tool's until-loop over `mcp__github__pull_request_read`
(`get_status` / `get_check_runs`) — **never** Bash `sleep`. A child PR red on CI →
dispatch a CI-fix worker (≤2 rounds); still red and out of scope → **park it via the §5
parking protocol** (`needs-decision`, or `status:blocked` with the blocker named) and
move on so one stuck PR doesn't stall the sprint.

The conflict-fix and CI-fix dispatches are **not** first-pass workers — they update an
*existing* child PR and must not broaden scope or open/merge PRs. Brief them explicitly:

> **Conflict-resolution worker.** Context: child PR `#P` (issue `#N`), sprint branch
> `<branch>`, conflict caused by staging child PR `#Q`, conflict files `<files / GitHub
> conflict summary>`. **Before editing, read child PR `#P`'s body + diff, issue `#N`'s
> body/comments/labels, and the current `<branch>` state, and restate `#P`'s intended
> scope and the exact conflict to yourself.** Task: in your own worktree on `#P`'s branch,
> **resolve only the conflict**, preserving `#P`'s original intent; run the narrowest
> relevant tests + the fast gate if the change warrants it; **push to the existing `#P`
> branch**. **If resolving it would change `#N`'s scope, stop and return `NEEDS_DECISION:`
> or `BLOCKED:`.** Do **not** open a new PR, retarget to `master`, or merge anything.
> Return the result + a `RETRO:` block.

> **CI-fix worker.** Context: child PR `#P` (issue `#N`), sprint branch `<sprint-branch>`,
> sprint tip SHA `<current-sprint-tip-sha>`, failing check(s) `<names>`,
> log excerpt `<failure summary>`. **Before editing, read child PR `#P`'s body + diff,
> issue `#N`'s body/comments/labels, and the current sprint-branch state, and restate
> `#P`'s intended scope and the exact CI failure to yourself.** Task: in your own worktree
> on `#P`'s branch, **fix only the CI failure**, preserving `#P`'s issue scope; rerun the
> relevant local gate; **push to the existing `#P` branch**. **If the fix would change
> `#N`'s scope, stop and return `NEEDS_DECISION:` or `BLOCKED:`.** After **two** failed
> rounds, return `BLOCKED:` or `NEEDS_DECISION:` with evidence. Do **not** open a new PR,
> retarget to `master`, or
> merge anything. Return the result + a `RETRO:` block.

## 7. The omnibus PR — the one and only landing PR

Re-evaluate the §3 plan against GitHub each cycle; schedule the next wave (newly
unblocked issues, each rebased on the new sprint tip) until no schedulable issue is
non-terminal. **Then** prepare the omnibus PR for the architect.

By this point the omnibus body is **already** the complete record — every staged child
appended its ledger row in §6, and parked `needs-decision` items were recorded as they
arose. So this step **finalizes and verifies**, it does not assemble. Before marking the
omnibus **ready for review** (out of draft), confirm:

- current `master` is an **ancestor** of the sprint integration branch;
- the **omnibus PR CI is green**;
- **all staged child PRs are merged** into the sprint branch;
- **no child PR remains in a non-terminal state** (each is staged, or parked as
  `needs-decision`, or `blocked` with a named blocker);
- the ledger is **complete and consistent** with GitHub: every staged child has a
  *Staged* row with a `Closes #N` line and tier; reconcile any gaps (e.g. a child staged
  just before a roll-over) by reading the sprint branch's merge history against the
  ledger, then add the missing rows.

The finalized omnibus body therefore owns the whole sprint's record:

- the **included child PRs**, each with its tier (`auto` | `escalate`);
- the **included issues as `Closes #N`** (these live on the omnibus *only*);
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
  (each with a recommendation, `/triage`-shaped), any follow-ups filed, and the
  **aggregated Retrospective** (deduped + grouped, per the Retrospective section) so the
  architect sees every process quirk the sprint surfaced in one place.

## Retrospective — a live, aggregated process ledger (everyone contributes)

The sprint keeps a running **retrospective** so process friction is captured the moment
it's felt and surfaced to the architect at the end — the point is to fix the *kit*
(instructions, steps, tooling, missing know-how) over time, not just to ship issues.

- **Everyone contributes.** Each worker and each review sub-agent ends its return with a
  `RETRO:` block (§4/§5). The **orchestrator** adds its own bullets too — anything in
  *this* command or the wider kit that proved wrong, stale, ambiguous, or context-draining
  while running the sprint (e.g. a brief that had to be re-explained, a tool that needed
  an undocumented argument, a step that was actually a no-op).
- **What's worth a note:** incorrect or stale instructions; steps that misfire or are
  redundant; know-how a future run needs up front to avoid rediscovering it; anything
  that burned context or tokens. Keep each bullet terse and *actionable* ("X said Y, but
  Z — suggest updating §N / ADR-NNNN"). Skip praise and routine status.
- **Durability = the omnibus body, harvested immediately.** Because sub-agent context is
  discarded and a staged child stops being an open PR, the **only** safe home for a retro
  note is the **`Retrospective` section of the omnibus PR body**. When the orchestrator
  harvests *any* sub-agent result (staged, parked, or failed), it appends that result's
  `RETRO:` bullets to that section **in the same step** — exactly like the §6 ledger
  rows, and for the same resumability reason. A roll-over therefore loses no retro notes:
  resume reads them straight back from the omnibus body.
- **Presented at close-out (§9).** The aggregated retrospective is part of the final
  report: deduped and grouped (instructions / steps / tooling / know-how), each item with
  a concrete suggested fix. Persistent fixes that change the constitution or a command
  ride their **own** governance PR (§9 / PRINCIPLES.md §9) — file them as follow-up
  issues rather than smuggling them into the omnibus.

## Guardrails (binding)

- **No `AskUserQuestion` mid-sprint** — a blocking prompt defeats "run until the target
  is met". Forks are parked as `needs-decision` issues (a terminal state) and surfaced
  together in the close-out.
- **Resumable — the omnibus body is the live ledger.** All durable state lives in GitHub
  branches, child PRs, labels, and the **continuously-updated omnibus PR body**
  (PRINCIPLES.md §0). This works *only because* §6 appends a *Staged* row the moment each
  child PR is merged into the sprint branch: once staged, a child is no longer an open PR,
  so the ledger is the sole record of which PRs were staged, which issues they covered,
  their tier, and their review/CI evidence. On a summarize/restart, the next invocation
  **first reads the omnibus ledger + open child PRs + labels**, reconstructs the
  issue→state table (staged ↔ ledger rows, in-flight ↔ open child PRs, parked ↔
  `needs-decision`), reconciles it against the sprint branch's merge history, and only
  *then* schedules more work — no progress lives in conversation memory.
- **Rollback-first (§9).** If a staged change reddens the omnibus CI, revert it out of
  the integration branch immediately (and open an incident issue) — *then* diagnose.
  Because nothing reaches `master` until the omnibus merge, a bad stage never escapes
  the sprint branch.
- One pre-PR `/code-review` (worker, bug-hunting), one verdict-only sprint review
  (orchestrator-owned, DoD/tier/governance), and one normal `pull_request` CI run per
  child task; never run the full CI locally (the `pull_request` run is the gate).
- The sprint only *parks* `needs-decision` issues — it never resolves their substance.
