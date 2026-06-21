# ADR-0023: Sprint/kaizen ledger durability — reconstruct-at-finalize + a committed append-only residue, not a churned PR body

- **Status:** Accepted (2026-06-21)
- **Date:** 2026-06-21

## Context

ADR-0018 made the omnibus PR **body** the "live ledger": the orchestrator rewrote
the whole body on every stage so a roll-over could always recover what had happened.
The justification was real — a staged child PR is no longer *open*, so without a
durable write nothing records it was staged. Two costs at scale:

- **Per-stage whole-body rewrite is token-heavy.** Rewriting the entire body every
  stage is felt at 8+ stages; a real sprint hits 11+.
- **The retrospective rides the same body with the same churn**, doubling the
  rewrite pressure.

Most of what the body recorded is **reconstructable**: which child PR staged, the
issue(s) it covered, and its tier are all derivable from the integration branch's
merge history (each squash merge names `…(#PR)`), the child PR bodies (`Refs #N`),
and the issue labels. Only a small residue — `RETRO:` notes and synced-`master`
SHAs — has no other durable home. The agent workspace is the *least* durable place:
the container is reclaimed on inactivity, so an uncommitted scratch file dies on
the exact roll-over the ledger exists to survive.

## Decision

Stop treating the PR body as the per-stage live ledger. Specifically:

1. **The PR body holds a stable pointer + a short living summary**, written *lightly*
   (at wave boundaries and at finalize), never rewritten per stage.
2. **The staging table is reconstructed at finalize**, not maintained live — from the
   kept integration branch's merge history + child PR bodies (`Refs #N`) + labels.
3. **An append-only committed ledger file — `lark-rs/docs/sprints/<sprint-id>.md` — on
   the integration branch carries only the irreducible residue:** state with no other
   durable home (the orchestrator's and review sub-agents' `RETRO:` notes; synced-`master`
   SHAs). It is appended sparsely and committed+pushed when produced. Worker `RETRO:`
   notes already persist in their child PR bodies; parked-decision memos already live on
   the issue; follow-ups are already filed issues.
4. **A workspace scratch file may serve as the running session's convenience cache**, but
   is never the system of record — anything that must survive a roll-over is either
   reconstructable (per 2) or committed to the residue file (per 3).
5. **Applies identically to `/start-sprint` and `/kaizen-sweep`**, and to **both** the
   staging ledger and the retrospective.

## Consequences

- Removes the per-stage whole-body rewrite (the #191.3 churn) while *preserving*
  resumability: durable state lives in the branch (merge history + committed residue),
  child PRs, and labels — never the ephemeral workspace. Resume reconstructs the
  issue→state table from `(open child PRs + branch merge history + labels + residue file)`
  rather than from a churned body.
- The residue file lands on `master` with the omnibus merge as a **permanent dated record**
  of the run — consistent with keeping the integration branch (#190.2) and with ADRs as
  durable dated records. **Tripwire:** if these archives prove noisy, strip the file before
  merge (branch-only) or relocate it out of the merged tree — revisit then.
- Refines **ADR-0018** (the "omnibus body is the live ledger" mechanism); ADR-0018's
  invariants (only the orchestrator stages; only the architect merges the omnibus; nothing
  reaches `master` but the omnibus) are unchanged.
- Enforced by the revised §2 / §6 / §7 / Retrospective / Guardrails of
  `.claude/commands/start-sprint.md` and the `/kaizen-sweep` deltas that point at them.
