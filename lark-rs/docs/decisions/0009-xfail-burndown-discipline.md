# ADR-0009: Known gaps are XFAIL allow-lists that only shrink

- **Status:** Accepted
- **Date:** backfilled 2026-06-13 (decision made with the compliance banks, Phase 1–2)

## Context

We replay large captured corpora against lark-rs: Python Lark's own test suite
strip-mined into compliance banks (LALR, Earley, dynamic-lexer Earley, CYK), plus
the wild-grammar bank. At any moment some cases fail — features not yet built,
known divergences. We need CI to stay green on *expected* failures while still
failing hard on **regressions**, and we need a forcing function that drives the
gap count toward zero instead of letting failures accumulate silently.

## Decision

Each bank carries an explicit **XFAIL allow-list** (`*_xfail.json`) of the cases
known to fail. The harness fails the build only when a case that is *not*
allow-listed fails (a regression) — or when an allow-listed case unexpectedly
*passes* (the list must shrink). Regenerating the list
(`LARK_*_WRITE_XFAIL=1 cargo test --test …`) and committing the **shrunk** file
is the ritual after a fix; see `/xfail-burndown`.

Process-aborting grammars (that would crash the test runner) go in a separate
`skip.json`, not the XFAIL list.

## Consequences

- Green CI means "no regressions," and the XFAIL files are an honest, reviewable
  ledger of exactly what doesn't work yet — the gap is visible, not hidden.
- The discipline is ratchet-like: the lists can only shrink, so progress can't
  silently reverse. An accidental *fix* that isn't recorded fails the build,
  forcing the list to be tightened.
- The wild bank tightens this further: an `alt_grammar` workaround's success
  namespaces are **not** XFAIL-able — it must be tree-identical to the original
  grammar's oracle on every input.
- Same discipline applies uniformly across all banks (`test_compliance.rs`,
  `test_earley_compliance.rs`, `test_cyk_compliance.rs`, `test_wild.rs`, the
  standalone bank), so there is one mental model for "known gap."
