---
description: Adversarially review a PR against the constitution and decide its merge tier
---

The autonomous-merge gate. Review a PR **against `PRINCIPLES.md` + the Definition
of Done**, then either merge it (auto tier) or hand it to the architect (escalate
tier). Usage: `/review-pr <number>`.

This is deliberately distinct from `/code-review` (which hunts for bugs in a diff).
`/review-pr` asks the *governance* question: **is this PR done, and who may merge
it?**

## 1. Gather

`mcp__github__pull_request_read` for the PR + its diff + CI status, and the
originating issue (`Closes #N`). Note `kind:` and the changed paths.

## 2. Check the Definition of Done (`PRINCIPLES.md` §6)

Verify each, and say which are satisfied vs missing:

1. A test that failed first now passes (the oracle/repro is in the diff).
2. CI is green — the `pull_request` run (the full gate).
3. `/code-review` ran and findings are addressed. **If the PR doesn't evidence a
   review, run `/code-review` on the diff now** (fresh context, adversarial) and
   require the findings handled before proceeding.
4. Oracles / `STATUS.md` fresh (no drift-gate red).
5. Out-of-scope discoveries filed as issues, not buried in the diff.
6. An ADR exists if a §3 default was deviated from.
7. The issue's done-when is met and the PR says `Closes #N`.

Also confirm no §2 **invariant** is violated even if CI is green — if one is and
CI didn't catch it, the gate is buggy: say so and file an issue.

## 3. Decide the merge tier (`PRINCIPLES.md` §6, ADR-0016)

Compute, don't look up — tier from `kind:` + blast radius:

- **`auto`** → bugfix-with-oracle, xfail burndown, perf-fix-behind-a-gate, docs, or
  refactor with no public-API change and banks green. **When in doubt, tier up.**
- **`escalate`** → new public API, new grammar-feature semantics, architecture
  change, anything touching `PRINCIPLES.md`, or anything labeled `needs-decision`.

## 4. Act

- **DoD not met** → post a concise change request (only what's missing) and set
  `status:needs-review`. Don't merge.
- **DoD met, `auto`** → merge (`mcp__github__merge_pull_request`), confirm the
  issue auto-closed via `Closes #N` (close it if not), and report the green
  outcome. This IS the deliverable — not a no-op.
- **DoD met, `escalate`** → post an approval summary (DoD checklist + why it's
  escalate-tier + the merge verdict) and hand off with `AskUserQuestion` so the
  architect merges. Do **not** merge it yourself.

Keep commentary frugal (the harness guidance on GitHub replies applies): one
verdict, not a running narration.
