---
description: Regenerate a compliance/wild bank XFAIL allow-list after a fix
---

> _Internal maintainer automation — not an invitation for external agents to claim issues or open PRs, and not a public bug-bounty program. See [`/CONTRIBUTING.md`](/CONTRIBUTING.md)._

After fixing an engine gap, regenerate the affected bank's XFAIL allow-list and
commit the **shrunk** file. The banks fail the build only on *regressions*; the
xfail files encode the known-failure set, and the burndown discipline is that
they only ever shrink (a fix that grows one is a regression elsewhere — stop
and investigate).

Pick the bank(s) the fix touches; run from `lark-rs/`:

```bash
# LALR compliance bank
LARK_COMPLIANCE_WRITE_XFAIL=1 cargo test --test test_compliance

# Earley banks (basic lexer / dynamic lexer)
LARK_COMPLIANCE_WRITE_XFAIL=1 cargo test --test test_earley_compliance
LARK_COMPLIANCE_WRITE_XFAIL=1 cargo test --test test_earley_dynamic_compliance

# CYK bank (currently 0 xfails — keep it that way)
LARK_COMPLIANCE_WRITE_XFAIL=1 cargo test --test test_cyk_compliance

# Wild-grammar bank
LARK_WILD_WRITE_XFAIL=1 cargo test --test test_wild

# Standalone bank
LARK_STANDALONE_WRITE_XFAIL=1 cargo test
```

Then:

1. `git diff lark-rs/tests/fixtures/oracles/**/*xfail*.json` — verify entries
   were **removed**, not added or reworded. Wild-bank note: `build-alt:` /
   `parse-alt:` / `panic-alt:` namespaces are never xfail-able and are never
   written by the regen; if one appears, the alt grammar is divergent and must
   be removed, not allow-listed.
2. If a wild-bank fix cleared a root cause, add a distilled pin for it to
   `tests/test_wild_gap_pins.rs` and update the findings in `docs/STATUS.md`.
3. Commit the shrunk xfail JSON together with the fix.

Debugging helpers: `LARK_COMPLIANCE_TRACE=1` prints each grammar before it runs
(finds process-aborting grammars); `LARK_WILD_TRACE=1` prints per-project
timing; `LARK_WILD_DETAILS=1` prints each failure's build/parse error.
