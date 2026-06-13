# ADR-0014: Recover Python's PatternStr/PatternRE split structurally via a `string_type` flag

- **Status:** Accepted
- **Date:** backfilled 2026-06-13 from PRs #76, #103

## Context

Python Lark distinguishes a *string* terminal (`IF: "if"`, a `PatternStr`) from a
*regex* terminal (`/[a-z]+/`, a `PatternRE`). lark-rs initially blurred them —
everything became a regex. That blur breaks two unrelated subsystems, and the fix
turns out to be a single structural invariant worth recording.

## Decision

Carry a `TerminalDef::string_type` flag that recovers the PatternStr/PatternRE
distinction in the loader, and treat a single-string named terminal as
`Pattern::Str`. This is load-bearing in two places:

- **Strict regex-collision (#76):** *"a new `TerminalDef::string_type` flag
  recovers Python's `PatternStr`/`PatternRE` distinction structurally in the
  loader … so a keyword like `IF: "if"` is never reported as colliding with
  `/[a-z]+/`."* (The collision check only applies between regex terminals.)
- **`unless` keyword-retyping (#103):** a single-string named terminal *must* be
  `Pattern::Str` so the keyword-retyping fires. Getting this wrong *"broke every
  async/await construct in python.lark."*

## Consequences

- The string-vs-regex flag is a cross-cutting classification invariant, not a
  collision-checker detail: it also drives `unless` retyping and feeds terminal
  ordering. A change that recomputes terminal kinds must keep all three consumers
  in sync.
- It is the structural analog of Python's two `Pattern` subclasses — so when in
  doubt about a terminal's treatment, the oracle's PatternStr/PatternRE behavior
  is the answer.
