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

1. Verified the right way for its type (§6 DoD-1): *code* → a failed-first
   oracle/repro/scaling gate now passes (in the diff); *docs/governance* → the
   policy/command path was walked through and any contradictions resolved (no
   failed-first test is expected).
2. CI is green — the `pull_request` run (the full gate).
3. `/code-review` ran and findings are addressed. **If the PR doesn't evidence a
   review, run `/code-review` on the diff now** (fresh context, adversarial) and
   require the findings handled before proceeding.
4. Oracles / `STATUS.md` fresh (no drift-gate red).
5. Out-of-scope discoveries filed as issues, not buried in the diff.
6. An ADR exists if a §3 default was deviated from.
7. The issue's done-when is met and the PR says `Closes #N`.
8. **Differential-audit checkpoint (recorded, not optional).** Does this change
   touch a behavior whose *full* input space the standing banks do **not**
   exhaustively cover (e.g. nullable / EBNF-expansion edges, ambiguity dedup,
   recovery resync, lexer tie-breaks)? The banks are a *regression* net, not a
   *completeness* net — a change can pass every bank green and still diverge from
   the oracle (cf. #101: `start: A (B*)~2`). If yes, require evidence the PR ran a
   **targeted differential audit against Python Lark** over a handful of
   adversarial inputs in that space (not just the committed bank) and pinned any
   new case found. If no oracle makes the audit impractical, the PR must *say so
   explicitly* and treat banks-green as **necessary but not sufficient**. A silent
   "banks are green" is a DoD gap here, not a pass.

Also confirm no §2 **invariant** is violated even if CI is green — if one is and
CI didn't catch it, the gate is buggy: say so and file an issue.

## 3. Decide the merge tier (`PRINCIPLES.md` §6, ADR-0016)

Compute, don't look up — tier from `kind:` + blast radius:

- **`auto`** → bugfix-with-oracle, xfail burndown, perf-fix-behind-a-gate, refactor
  with no public-API change and banks green, or **trivial docs** (typo / link /
  status-refresh / non-normative clarification). **When in doubt, tier up.**
- **`escalate`** → new public API, new grammar-feature semantics, architecture
  change, **any governance/policy doc** (`PRINCIPLES.md`, ADRs, command behavior,
  `LABELS.md`, roadmap, `CLAUDE.md`, public claims, responsibility boundaries), or
  anything labeled `needs-decision`.

## 4. Act

> **While ADR-0016 is `Proposed`, `/review-pr` is verdict-only:** even for an
> `auto`-tier PR you post the recommendation and the *architect* merges. The
> "agent merges" path below activates only once ADR-0016 is `Accepted`.

- **DoD not met** → post a concise change request (only what's missing) and set
  `status:needs-review`. Don't merge.
- **DoD met, `auto`** → post the verdict (`auto` + the DoD checklist). *If ADR-0016
  is Accepted:* merge (`mcp__github__merge_pull_request`), confirm the issue
  auto-closed via `Closes #N` (close it if not), report the green outcome. *If
  ADR-0016 is Proposed:* hand to the architect to merge.
- **DoD met, `escalate`** → post an approval summary (DoD checklist + why it's
  escalate-tier + the merge verdict) and hand off with `AskUserQuestion` so the
  architect merges. Do **not** merge it yourself.

Keep commentary frugal (the harness guidance on GitHub replies applies): one
verdict, not a running narration.
