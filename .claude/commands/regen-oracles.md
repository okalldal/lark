---
description: Regenerate Python-Lark oracle fixtures and verify freshness
---

> _Internal maintainer automation — not an invitation for external agents to claim issues or open PRs, and not a public bug-bounty program. See [`/CONTRIBUTING.md`](/CONTRIBUTING.md)._

Regenerate the committed oracle fixtures from Python Lark (the ground truth).
**Never hand-edit anything under `lark-rs/tests/fixtures/oracles/`** — the
`.claude/settings.json` deny rules enforce this; the generators are the only
legitimate writers.

From `lark-rs/` (needs `pip install lark`):

```bash
python3 tools/generate_oracles.py           # fixtures/oracles/**/*.json (curated + Earley)
python3 tools/extract_lark_compliance.py    # compliance/bank.json + the 3 other banks
python3 tools/generate_wild_oracles.py      # oracles/wild/ (needs `pip install regex`)
```

Then:

1. Run `git diff --stat lark-rs/tests/fixtures/oracles/` and sanity-check the
   churn matches what you changed — an unexpected diff in an unrelated oracle
   means the generator or the environment drifted; investigate before committing.
2. Run `cargo test` so the Rust side is gated against the fresh oracles.
3. Commit the oracle JSON **together with** the implementation/test change that
   motivated it (project rule: oracle and implementation land in one commit).

CI regenerates all generators and fails if the committed JSON drifts, so a
stale oracle that slips through is caught — but regen locally when you touched
`tools/` or the fixtures so it doesn't cost a CI round trip.

For the standalone-parser fixtures (`lark-rs/tests/standalone/*.rs`, also
deny-listed for hand edits):

```bash
LARK_STANDALONE_WRITE=1 cargo test --test test_standalone
```
