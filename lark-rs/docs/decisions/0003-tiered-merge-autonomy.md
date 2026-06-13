# 0003. Tiered merge autonomy by blast radius

- **Status:** Proposed (architect to accept/tune — this sets how much merge authority agents have)
- **Date:** 2026-06-13
- **Deciders:** architect (pending)
- **Grounds:** new policy — defines PRINCIPLES.md §6 merge tiers

## Context

The goal is autonomy at the architect's altitude: the architect should not have to
review and click-merge every PR. But merge is the least reversible delegation, and
the obvious failure mode is **agent-reviews-agent rubber-stamping** — the reviewer
session can rationalize the author session's work.

A blanket "agents may merge" is too risky; a blanket "architect merges everything"
defeats the purpose. The useful question is per-PR: *is this PR's correctness fully
captured by a gate that doesn't care who wrote it?*

## Decision

Two tiers, decided per PR by `/review-pr` from `kind:` + blast radius:

- **`auto`** — the agent merges once the Definition of Done is met: bugfix-with-
  oracle, xfail burndown, perf fix behind a scaling gate, docs, or a refactor with
  no public-API change and all banks green.
- **`escalate`** — `/review-pr` may approve but the **architect merges**: new
  public API, new grammar-feature semantics, architecture changes, anything
  touching `PRINCIPLES.md`, or anything labeled `needs-decision`.

The dividing line is exactly whether an existing, author-independent gate fully
captures correctness.

## Why / alternatives rejected

- *Blanket auto-merge* — rejected: design-review by a peer session is not a strong
  enough net on novel semantics; rubber-stamping risk is real.
- *Blanket architect-merge* — rejected: for fully-gated changes a human merge adds
  latency, not safety, and recreates the bottleneck this whole effort removes.

Why `auto` is even defensible here (and wouldn't be in a normal repo): the
oracle/compliance banks are an **independent** check that doesn't care who wrote
the code. We lean on *that* net, not on the peer design-review. Mitigations on the
review side: `/code-review` runs in fresh context seeing only the diff, and is
instructed adversarially.

## Consequences

- Easier: routine correctness-gated work flows without the architect; the architect's
  attention concentrates on genuinely novel surface.
- Harder: requires the `kind:`/blast-radius classification to be honest;
  mis-tiering an `escalate` change as `auto` is the thing to guard against — when
  in doubt, `/review-pr` tiers up.
- Tripwire: start conservative (tier up when unsure); widen the `auto` set only as
  the banks prove they cover a class. The architect tunes the two lists here as the
  ADR log accumulates evidence.
