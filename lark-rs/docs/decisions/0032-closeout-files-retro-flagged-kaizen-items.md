# ADR-0032: Sprint/kaizen/task close-out must file every retro-flagged kaizen item

- **Status:** Proposed (pending architect ratification)
- **Date:** 2026-06-23

## Context

PRINCIPLES §7 already requires that out-of-scope finds be filed as issues, never
silently dropped. But the `/start-sprint` §9 close-out treated *filing retro-flagged
kaizen/process items* as an implicit obligation, not a checkable deliverable. Sprint
`20260622-2013` (omnibus #284) reliably filed its **product** follow-ups yet silently
dropped **every** kaizen/process follow-up its own retro flagged — several carrying the
literal instruction "file as follow-up at close-out." They were recovered only by a later
ad-hoc audit and filed as #309–#314. The close-out step — the one place meant to enforce
"never silently drop" — was itself the §7 violator. Prior sprints did file their kaizen
items, so the discipline exists, but nothing made it mechanically guaranteed.

The options were: (a) leave it implicit and rely on diligence (status quo — demonstrably
lapses); (b) make filing a **required, checkable** close-out deliverable with an explicit
checklist line and a dual-arm report that makes a zero-kaizen close-out conspicuous.

## Decision

The sprint/kaizen §9 close-out (and the single-task `/finish-task` close-out) treats
"file every retro-flagged kaizen item" as a **required, checkable** deliverable: for each
`RETRO:` note tagged kaizen / KIT BUG / "file as follow-up," a `kaizen`-labelled tracking
issue must exist (or the note be explicitly marked already-tracked/duplicate) before the
close-out reports done, and the close-out report enumerates follow-ups in **two separate
arms — product follow-ups filed AND kaizen follow-ups filed** — so a zero-kaizen arm must
be justified, not left silent.

## Consequences

Makes an existing §7 obligation falsifiable at the close-out gate: a dropped kaizen item
is now a visible DoD failure (an empty *kaizen follow-ups filed* arm) rather than an
invisible omission, closing the lapse that produced #309–#314. Cost: a small amount of
extra close-out bookkeeping (walking the aggregated retro, confirming each issue). It does
not change product behavior or any code path — it strengthens the close-out Definition of
Done only. It **reinforces**, and does not contradict, the existing Retrospective rule to
file kit fixes as their own follow-up issues rather than smuggling them into the omnibus.
Tripwire to revisit: if a future close-out still drops a flagged item, the checklist line
was insufficient and the gate needs to move earlier (e.g. into the §7 finalize confirm
list). Enforced by the command text in `.claude/commands/start-sprint.md` §9 +
Retrospective, `.claude/commands/kaizen-sweep.md` §7–9, and `.claude/commands/finish-task.md`.
