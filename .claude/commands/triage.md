---
description: Order the open backlog and apply the label state machine
---

Keep the backlog's durable state (`lark-rs/docs/LABELS.md`) accurate so
`/next-task` and `/review-pr` can read it instead of re-deriving priority every
run. Triage **labels and orders**; it does not implement anything.

## 1. Ensure the labels exist

The process labels may not exist on the repo yet. For each label in
`lark-rs/docs/LABELS.md` that a `mcp__github__get_label` lookup misses, create it
(`mcp__github__issue_write`/label API) before use — idempotent, one-time in
practice. Leave the existing topic labels (`lark-rs`, `earley`, …) untouched.

## 2. Walk the open issues (`mcp__github__list_issues`, state OPEN)

For each, set/confirm:

- **`kind:`** — bug / feat / refactor / perf / docs / infra (drives merge tier).
- **`prio:`** — `now` (next up) / `next` / `later`. Map the existing inline
  "Priority: low" notes to `prio:later`.
- **`status:`** — `triaged` once classified; `blocked` if it names a blocker
  (e.g. #79 blocked on #40); leave `in-progress`/`needs-review` to the task/PR
  commands.
- **Escalation flag** — `needs-decision` if the body has an unresolved fork only
  the architect can settle ("decision needed", "assess & challenge" — #159, #101,
  #95). Otherwise, if it has an oracle-backed done-when and no open fork,
  `good-autonomous`.

Flag (don't act on) likely **duplicates** or **stale** items for the architect.

## 3. Report the ordered backlog

Output a short ranked list: the `prio:now` / `good-autonomous` picks at the top
(what `/next-task` will pull next), the `needs-decision` queue called out
separately as the architect's inbox, and anything `blocked` with its blocker. Note
what labels you changed and why. Do not start implementing — `/next-task` does that.
