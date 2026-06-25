---
description: Order the open backlog and apply the label state machine
---

> _Internal maintainer automation — not an invitation for external agents to claim issues or open PRs, and not a public bug-bounty program. See [`/CONTRIBUTING.md`](/CONTRIBUTING.md)._

Keep the backlog's durable state (`lark-rs/docs/LABELS.md`) accurate so
`/next-task` and `/review-pr` can read it instead of re-deriving priority every
run. Triage **labels and orders**; it does not implement anything.

**Dry-run by default.** Labels drive autonomy (task selection *and* merge-tiering),
so a bad mass-triage is high-blast-radius. A bare `/triage` is **report-only**: it
prints the labels it *would* create and the per-issue changes it *would* make, then
stops for the architect. Only **`/triage apply`** mutates anything — and only after
the architect has approved the dry-run.

## 1. Ensure the labels exist (apply mode only)

The process labels may not exist on the repo yet. For each label in
`lark-rs/docs/LABELS.md` that a `mcp__github__get_label` lookup misses: in **apply**
mode create it (`mcp__github__issue_write`/label API, idempotent); in **dry-run**
list it as "would create". Leave the existing topic labels (`lark-rs`, `earley`, …)
untouched.

## 2. Walk the open issues (`mcp__github__list_issues`, state OPEN)

For each, determine the intended labels (**apply:** set them; **dry-run:** report
them):

- **`kind:`** — bug / feat / refactor / perf / docs / infra (drives merge tier).
- **`prio:`** — `now` (next up) / `next` / `later`. Map the existing inline
  "Priority: low" notes to `prio:later`.
- **`status:`** — `triaged` once classified; `blocked` if it names a blocker
  (e.g. #79 blocked on #40); leave `in-progress`/`needs-review` to the task/PR
  commands.
- **Escalation flag** — `needs-decision` if the body has an unresolved fork only
  the architect can settle ("decision needed", "assess & challenge" — #159, #101,
  #95). Otherwise, if it has an oracle-backed done-when and no open fork,
  `good-autonomous` — **subject to the fix-site verification-status check below**.

Flag (don't act on) likely **duplicates** or **stale** items for the architect.

### 2a. `good-autonomous` fix-site verification-status check

This is a **verification status, not a blocking gate** — `good-autonomous` stays
schedulable either way; the check only records whether the named fix site is
load-bearing or exploratory. `good-autonomous` means "schedulable as-is with an
identified fix site," so a named-but-wrong site burns a worker cycle (#272 named
`parsers/lalr.rs`; the real divergence was upstream in `grammar/loader/ebnf.rs` —
re-triaged + escalated #285). Before adding `good-autonomous`, **check the issue's
stated fix site for plausibility against a fast repro** — the failing XFAIL the issue
points at, or a one-line probe that exercises the named module. The bar is
*plausibility*, not a full fix: does the repro actually fail in (or route through) the
named site?

- Site **confirmed** (or the issue records it as `verified` per the bug-hackathon
  filer's hypothesised/verified annotation): apply `good-autonomous` as usual.
- Site **unconfirmed** — no fast repro available, the repro doesn't touch the named
  module, or the filer marked it `hypothesised`: still apply `good-autonomous` if the
  done-when is otherwise groundable, but **annotate the issue body / a triage comment
  "fix site unverified"** so the worker treats the named site as *exploratory* (a
  starting hypothesis to confirm first) rather than churning that file. In **dry-run**,
  report the downgrade; in **apply**, add the note.

### 2b. Decision-label drift check

Search open issue titles and bodies for decision-shaped language even when
`needs-decision` is absent. **Candidate phrases** (cast wide for detection):
`Decision needed`, `needs-decision`, `architect`, `escalate-tier`,
`must not be guessed`, `no Python oracle`, `blocked on the decision`,
`AskUserQuestion`, `unresolved fork`.

**Mutation rule** (narrow — only add `needs-decision` when warranted): a
candidate is a real drift hit only if the issue contains an **explicit
unresolved fork** — a `## Decision needed` section, a concrete "must not be
guessed" / "no Python oracle" statement about an open choice, or multiple
named alternatives with no recorded architect verdict. These alone are
**not** enough to add the label:
- `architect approved` / `architect merges` / `architect ratified` — already
  decided, not an open fork.
- `escalate-tier` — describes the merge tier of a PR, not an open decision.
- A blocked implementation issue (`status:blocked`) that names a decision
  issue as its blocker — the *blocker* is the decision, not the child.

Report discrepancies as:

```
Decision-label drift:
- #N says "Decision needed" but lacks `needs-decision`
- #N has `needs-decision` but no actual fork remains (consider removing)
- #N blocks another issue but the dependent lacks `status:blocked`
```

In **apply** mode, add the missing `needs-decision` label (only where the
mutation rule is met) and set `status:blocked` on dependents. In
**dry-run**, list the repairs only.

## 3. Report the ordered backlog

Output a short ranked list: the `prio:now` / `good-autonomous` picks at the top
(what `/next-task` will pull next), the `needs-decision` queue called out
separately as the architect's inbox, and anything `blocked` with its blocker. In
**dry-run**, list the changes you *would* make and stop for the architect's
go-ahead before `/triage apply`; in **apply**, note what you changed and why. Do not
start implementing — `/next-task` does that.
