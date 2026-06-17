# Lark — Repository Overview for Claude Code

This is the [Lark](https://github.com/lark-parser/lark) Python parsing toolkit repo.
The Python source lives in `lark/` and its tests in `tests/`.

## Active Work: Rust Rewrite (`lark-rs/`)

All current development is in `lark-rs/` — a ground-up Rust rewrite of Lark's core.
**See [`lark-rs/CLAUDE.md`](lark-rs/CLAUDE.md) for architecture, status, testing guide, and roadmap.**

Quick start:
```bash
cd lark-rs
cargo test                    # run the full test suite
cargo test test_json_corpus   # run the 293-file JSONTestSuite corpus
```

## Picking Work

When asked to find or take on the most valuable / highest-value work (or given
no specific task), run **`/next-task`** — it codifies the backlog survey
(open PRs/issues, STATUS.md follow-ups, xfail burndown) and the selection rubric.

## Autonomous Development — Operating Rules (binding)

This repo is developed autonomously under a written constitution,
**[`lark-rs/docs/PRINCIPLES.md`](lark-rs/docs/PRINCIPLES.md)** (full text, rationale,
Definition of Done, §-level detail — *only the architect edits it*). The rules below
are the always-in-context core; cite PRINCIPLES.md for the depth.

- **Thesis.** Safe autonomy extends exactly as far as we've made things
  *falsifiable*. Decide what you can ground; escalate the rest.
- **Decide / ADR / escalate** (§4). Grounded by an oracle/gate/bank → **decide
  freely**, self-check. Grounded only by a written rule + judgement → **decide, and
  record an ADR** if you deviate from a §3 default. Nothing falsifiable (product
  direction, taste, a real trade-off) → **escalate**, don't guess. Unsure which →
  escalate.
- **Invariants** (§2, never violate): oracle-first; Python Lark is the oracle; never
  hand-edit generated oracles/standalone parsers; never regress a green corpus; perf
  claims are deterministic work-counters, not wall-clock; upstream grammars verbatim.
- **Escalate** via a `needs-decision` issue (the architect's inbox); use
  `AskUserQuestion` only when it blocks the session. Never resolve a `needs-decision`
  by picking the easiest-to-implement option.
- **Out-of-scope finds → file an issue** (never silently fix or drop), and
  **governance/policy changes ride their own PR** (§9).
- **Merge authority is staged.** ADR-0016 is *Proposed*, so **the architect merges
  every PR**; `/review-pr` only posts a verdict (`auto`/`escalate`) until the
  architect accepts ADR-0016.

Operated by **`/roadmap`** (propose epics for approval), **`/triage`** (label &
order; dry-run by default), **`/next-task`** (claim & execute), **`/finish-task`**
(review → gate → PR → close-out), **`/review-pr`** (DoD gate + merge tier). Backlog
labels: [`lark-rs/docs/LABELS.md`](lark-rs/docs/LABELS.md); decision log:
[`lark-rs/docs/decisions/`](lark-rs/docs/decisions/).

## Python Lark is Our Oracle

When working on `lark-rs`, use Python Lark as the ground truth:
```bash
python3 lark-rs/tools/generate_oracles.py   # regenerate expected-output JSON
```
