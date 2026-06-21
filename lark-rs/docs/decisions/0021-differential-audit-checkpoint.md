# ADR-0021: Differential-audit checkpoint in the review discipline

- **Status:** Accepted
- **Date:** 2026-06-21

## Context

The standing compliance banks (LALR / Earley / dynamic / CYK) and the wild bank
are a **regression** net: they replay cases someone already captured. They are
*not* a **completeness** net — a change to a behavior the banks under-sample can
pass every bank green and still diverge from Python Lark, our oracle (ADR-0001).

This bit us concretely in sprint `20260619-0714` (#101): a candidate CYK fix
passed all four compliance banks yet silently over-rejected `start: A (B*)~2`, a
grammar Python Lark *accepts*. The banks carry no `~n`-over-nullable-group case, so
"banks green" was satisfied while the oracle was violated. Only an ad-hoc
differential audit against Python Lark caught it.

`/code-review`, `/finish-task`, `/review-pr`, and the `/start-sprint` verdict-only
review all treated banks-green as sufficient, with no recorded prompt to ask
whether the change touched a behavior the banks under-sample. The decision to diff
against the oracle beyond the bank was left implicit — an afterthought, not a gate.

## Decision

Make "is a differential audit vs Python Lark warranted?" an **explicit, recorded
checkpoint** in the review kit. For any change touching a behavior whose full input
space the standing banks do not exhaustively cover (nullable / EBNF-expansion
edges, ambiguity dedup, recovery resync, lexer tie-breaks, …), the author/reviewer
must either run a **targeted differential audit against Python Lark** over a
handful of adversarial inputs in that space — pinning any new case found — or
explicitly note that no oracle makes the audit impractical and treat banks-green as
**necessary but not sufficient**. A silent banks-green on such a change is a DoD
gap, not a pass.

The checkpoint is added to the editable review surfaces: `review-pr.md` §2 (the
merge gate's DoD), `finish-task.md` step 1 (the worker pre-PR review), and
`start-sprint.md` §5 (the verdict-only sprint review). (The `/code-review` skill is
a built-in, not a repo file, so it is not edited here.)

## Consequences

- Oracle divergences in under-sampled behaviors become a *conscious, recorded*
  decision at review time rather than something that surfaces only via an
  occasional ad-hoc audit.
- A genuinely new adversarial case found by an audit is pinned (fed into the banks
  via the existing XFAIL/oracle discipline, ADR-0009 / ADR-0012), so completeness
  ratchets toward regression coverage rather than staying ephemeral.
- Cost: a small extra judgement call per review for changes in the named behavior
  classes. It is bounded to those classes, not every PR.
- Tripwire to revisit: if the active differential fuzzer (ADR-0012) is extended to
  cover these behavior classes exhaustively, the manual checkpoint becomes
  redundant for them and can be narrowed.
