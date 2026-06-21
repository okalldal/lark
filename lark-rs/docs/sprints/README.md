# Sprint / kaizen ledger archive

One file per `/start-sprint` or `/kaizen-sweep` run, named for the run's
integration branch (`<sprint-id>.md`, e.g. `kaizen-20260621-0830.md`).

Per **ADR-0023**, the orchestrator does **not** maintain a live ledger by rewriting
the omnibus PR body every stage. Instead:

- The **staging table** (which child PR staged, the issue(s) it covered, its tier) is
  **reconstructed at finalize** from the kept integration branch's merge history + the
  child PR bodies (`Refs #N`) + labels — it is not stored here.
- This file is the **append-only residue**: only the state with no other durable home —
  the orchestrator's and review sub-agents' `RETRO:` notes and any synced-`master` SHAs.
  It is appended + committed as those arise, so a summarize/roll-over loses nothing.
- A worker's own `RETRO:` already lives in its child PR body; parked-decision memos on
  the issue; follow-ups as filed issues — those are **not** duplicated here.

The file lands on `master` with the omnibus merge as the run's permanent dated record
(consistent with keeping the integration branch). If these archives ever prove noisy,
the ADR-0023 tripwire applies: strip before merge or relocate, and revisit the decision.
