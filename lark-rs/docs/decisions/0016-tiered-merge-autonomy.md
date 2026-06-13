# ADR-0016: Tiered merge autonomy by blast radius

- **Status:** Proposed (architect to accept/tune — sets how much merge authority agents have)
- **Date:** 2026-06-13

> Forward-looking *policy* ADR (not a backfilled historical record). It defines
> `PRINCIPLES.md` §6 merge tiers and is the one decision in the governance kit that
> is genuinely the architect's to ratify.

## Context

The goal is autonomy at the architect's altitude: the architect should not have to
review and click-merge every PR. But merge is the least reversible delegation, and
the obvious failure mode is **agent-reviews-agent rubber-stamping** — the reviewer
session can rationalize the author session's work.

A blanket "agents may merge" is too risky; a blanket "architect merges everything"
defeats the purpose. The useful question is per-PR: *is this PR's correctness fully
captured by a gate that doesn't care who wrote it?*

## Decision

Two tiers, decided per PR by `/review-pr` from `kind:` (`LABELS.md`) + blast radius:

- **`auto`** — the agent merges once the Definition of Done is met: bugfix-with-
  oracle, xfail burndown, perf fix behind a scaling gate, docs, or a refactor with
  no public-API change and all banks green.
- **`escalate`** — `/review-pr` may approve but the **architect merges**: new
  public API, new grammar-feature semantics, architecture changes, anything
  touching `PRINCIPLES.md`, or anything labeled `needs-decision`.

The dividing line is exactly whether an existing, author-independent gate fully
captures correctness.

## Why this is even defensible here

Rejected alternatives: *blanket auto-merge* (peer design-review is too weak a net
on novel semantics) and *blanket architect-merge* (recreates the bottleneck this
whole effort removes). What makes `auto` safe in *this* repo and not a normal one:
the oracle/compliance banks are an **independent** check that doesn't care who
wrote the code — we lean on *that* net, not on the peer review. Review-side
mitigation: `/code-review` runs in fresh context seeing only the diff
(ADR-0001's oracle discipline is the real backstop; see also ADR-0007).

## Consequences

- Routine correctness-gated work flows without the architect; attention
  concentrates on genuinely novel surface.
- Depends on honest `kind:`/blast-radius classification — mis-tiering an
  `escalate` change as `auto` is the thing to guard against, so `/review-pr` tiers
  *up* when unsure.
- **Tripwire:** start conservative; widen the `auto` set only as the banks prove
  they cover a class. The architect tunes the two lists here as the ADR log
  accumulates evidence.
