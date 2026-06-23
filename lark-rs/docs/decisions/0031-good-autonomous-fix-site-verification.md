# ADR-0031: `good-autonomous` carries a fix-site verification status

- **Status:** Accepted (2026-06-23) — architect ratified with keep-label-and-annotate
  semantics; the "fix-site **gate**" framing is renamed to a "fix-site
  **verification-status check**" so `good-autonomous` stays schedulable while an
  unverified site is explicitly exploratory.
- **Date:** 2026-06-23

> **Decision (architect, 2026-06-23):** Accept. `good-autonomous` is **not** gated on
> verification — verified sites are load-bearing; unverified sites still get the label
> but must be explicitly annotated as exploratory. Wording changed from "gate" to
> "verification-status check" so the label contract is not misread as blocking.

## Context

`good-autonomous` means "schedulable as-is with an identified fix site" — an
unattended `/next-task` pick whose done-when is groundable and whose stated fix
site tells the worker where to start. But the label's contract said nothing about
whether that fix site had been *checked*. Issue #272 named its fix site as
`parsers/lalr.rs` (the conflict detector) and was labelled `good-autonomous`; the
real divergence lived upstream in `grammar/loader/ebnf.rs` helper sharing
(ADR-0013). The worker that claimed it churned the wrong file, then had to park the
issue `needs-decision` and escalate (#285). The cost: a `good-autonomous` label
implied a load-bearing site, but the site was unverified.

Two surfaces mint `good-autonomous` candidates and name fix sites: `/triage`
(applies the label) and `/bug-hackathon` (files fix-site issues from a findings
catalog). Neither distinguished a *confirmed* site from a *guessed* one.

## Decision

`good-autonomous` carries a **fix-site verification status**, not a blocking gate.
Before applying the label, `/triage` checks the issue's stated fix site against a fast
repro — the failing XFAIL or a one-line probe that exercises the named module. The bar
is plausibility (does the repro fail in / route through the named site?), not a full
fix:

- a **verified** site is load-bearing — the worker can trust it and start there;
- an **unverified** site (no fast repro available, the repro doesn't touch the named
  module, or the upstream filer marked it `hypothesised`) does **not** block the label:
  the issue still gets `good-autonomous` when its done-when is otherwise groundable, but
  it carries a **"fix site unverified"** note so the worker treats the named file as an
  exploratory starting hypothesis rather than churning it.

Correspondingly, `/bug-hackathon`'s findings filer marks each fix site **hypothesised**
vs **verified**, so `/triage` knows whether the site is load-bearing.

The label's *meaning* — groundable, oracle-backed, no open fork, schedulable as-is — is
unchanged, and `good-autonomous` stays schedulable even when the site is unverified;
this strengthens the contract by making the "identified fix site" clause a *falsifiable
status* (verified vs exploratory) rather than an unstated assumption.

## Consequences

- A worker reading `good-autonomous` can trust a named fix site is repro-confirmed,
  or is explicitly flagged unverified — no silent wrong-site churn, no mid-sprint
  re-triage + escalation (the #272 → #285 failure mode).
- Triage costs a fast-repro check per `good-autonomous` candidate. Cheap by
  construction: the XFAIL or probe usually already exists, and a candidate without
  any repro is exactly the one whose site we most want flagged.
- The hypothesised/verified annotation flows from the hackathon filer through the
  issue body into the triage verification-status check — one consistent signal across
  the two surfaces that mint these candidates.
- Tripwire to revisit: if "fix site unverified" notes become the common case rather
  than the exception, the upstream filers aren't supplying confirmable sites and the
  check is just relabelling churn — reconsider where verification should live.
