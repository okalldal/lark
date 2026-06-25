---
description: Summarize the architect's open decision inbox as decision memos
---

> _Internal maintainer automation — not an invitation for external agents to claim issues or open PRs, and not a public bug-bounty program. See [`/CONTRIBUTING.md`](/CONTRIBUTING.md)._

Produce a read-only architect decision brief. Do not mutate GitHub, do not apply
labels, do not file issues, do not start implementation, and do not ask the
architect to decide via `AskUserQuestion` — the brief itself is the deliverable.

The goal is to answer: "What is in the architect's court, why, what decision is
needed, what is recommended, what alternatives were considered, and what happens
next?"

## 1. Gather the architect-owned queue

In parallel, gather:

- **Open issues labeled `needs-decision`** — the canonical architect inbox
  (`LABELS.md`). Use `mcp__github__list_issues` with the `needs-decision` label.
- **Open issues whose title/body contains decision-shaped language** even if the
  label is missing. Use `mcp__github__search_issues` for phrases:
  `Decision needed`, `needs-decision`, `architect`, `escalate-tier`,
  `must not be guessed`, `no Python oracle`, `blocked on the decision`,
  `AskUserQuestion`, `unresolved fork`.
  Deduplicate against the labeled set — the point is to catch label drift, not
  to double-count. **Filter out false positives:** an issue that says `architect
  approved` / `architect ratified` is already decided; an `escalate-tier` mention
  describes a merge tier, not an open fork; a blocked implementation child
  (`status:blocked`) is not itself the decision. Only flag issues with an
  explicit unresolved fork (a `## Decision needed` section, named alternatives
  with no recorded verdict, or a concrete "must not be guessed" statement about
  an open choice).
- **Open PRs and their changed paths** (`mcp__github__list_pull_requests` +
  `mcp__github__pull_request_read` for changed files). Treat as architect-owned
  if `/review-pr` would classify them as `escalate`: public API, grammar
  semantics, architecture, command behavior, governance/policy docs, ADRs,
  `LABELS.md`, `CLAUDE.md`, or anything tied to `needs-decision`.
- **Blocked issues that name a decision issue as blocker** — scan issue bodies
  for "blocked by #N" / "depends on #N" / "Blocked on #N" where #N is a
  `needs-decision` issue. Show #N as the action item and list the blocked issue
  as a consequence/unblocked child, not as a separate decision.
- **Parent epics and linked ADR/RFC/status docs** for each decision issue. Read
  only the minimum needed to explain context — the linked epic's title and
  done-when, the referenced ADR's status and decision summary, or the relevant
  section of a design doc.

## 2. Classify

Group findings into:

1. **Blocking active work** — a decision blocks a `prio:now`/`prio:next` child,
   open PR, or active epic path.
2. **Architect merge/review queue** — open escalate-tier PRs.
3. **Deferred architect decisions** — real forks, but `prio:later` or no active
   blocker.
4. **Triage repair** — decision-shaped issues missing `needs-decision`, stale
   labels, or dependents that should be `status:blocked`.

Do not include ordinary `good-autonomous` work except as "unblocked by this
decision".

## 3. Write each item as a decision memo

For each architect-owned item, output:

- **Issue/PR number and title.**
- **Why this needs architect attention** — plain language, assume the architect
  has not read the code or the issue thread recently.
- **Background** — enough context to decide without spelunking. Reference the
  linked epic, ADR, or design doc by number/path.
- **The actual decision to make** — the fork(s) to resolve.
- **Recommended path** — first and prominent. The agent may recommend; the agent
  must not decide.
- **Alternatives considered** — from the issue body and discussion.
- **Consequences if accepted** — what becomes unblocked, which issue/PR follows,
  what constraints the decision imposes.
- **Consequences if deferred** — which work stays blocked or should be re-scoped.
- **Suggested architect action** — approve an option in the issue, request a
  narrowed ADR, merge/reject an escalate PR, or run `/triage apply` for label
  repair.
- **Unblocks** — list of issue/PR numbers that depend on this decision.

## 4. Output format

Use this stable executive format:

```md
# Architect brief — YYYY-MM-DD

## Summary
- Architect-owned decisions: N
- Blocking active work: N
- Deferred / later decisions: N
- Escalate-tier PRs awaiting merge/review: N
- Label drift / triage repair: N

## 1. Decisions blocking active work

### #NNN — Title

**Why this needs you**
...

**What the decision is**
...

**Recommended path**
...

**Alternatives considered**
...

**Consequences if accepted**
...

**Consequences if deferred**
...

**Suggested architect action**
...

**Unblocks**
- #NNN — title

## 2. Escalate-tier PRs awaiting architect action

### PR #NNN — Title
...

## 3. Deferred architect decisions

### #NNN — Title
...

## 4. Triage repair

- #NNN says "Decision needed" but is missing `needs-decision`.
  Suggested repair: `/triage apply` to add the label.
- ...
```

## 5. Be concise but complete

Do not enumerate every open issue. Synthesize.

Use this order:

1. Summary counts.
2. Blocking decisions (highest urgency first).
3. Escalate PRs awaiting architect action.
4. Deferred decisions.
5. Triage repair.
6. Suggested next architect action (one sentence: what to do first).

## 6. Authority boundary

This command is read-only. It must not:
- Apply or remove labels.
- File, close, or edit issues.
- Merge, approve, or request changes on PRs.
- Start implementation of anything.
- Ask the architect to decide via `AskUserQuestion` (the brief *is* the
  deliverable; the architect acts on it outside the command).

If a label repair is needed, say what `/triage apply` should do.
If a decision is accepted, remind that the acceptance must be recorded durably
in the issue, ADR, or PR — not left in chat.
