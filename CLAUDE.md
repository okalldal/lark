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

## Autonomous Development — the governance kit

This repo is developed autonomously under a written constitution. The grounding
doc is **[`lark-rs/docs/PRINCIPLES.md`](lark-rs/docs/PRINCIPLES.md)** (invariants,
defaults, decision taxonomy, escalation, Definition of Done, merge tiers); the
decision log is [`lark-rs/docs/decisions/`](lark-rs/docs/decisions/) and the
backlog label schema is [`lark-rs/docs/LABELS.md`](lark-rs/docs/LABELS.md). The
commands that operate on it: **`/roadmap`** (propose next epics for approval),
**`/triage`** (label & order the backlog), **`/next-task`** (pick & execute),
**`/finish-task`** (review → gate → PR → close-out), **`/review-pr`** (gate &
merge-tier a PR). Agents cite `PRINCIPLES.md`; only the architect edits it.

## Python Lark is Our Oracle

When working on `lark-rs`, use Python Lark as the ground truth:
```bash
python3 lark-rs/tools/generate_oracles.py   # regenerate expected-output JSON
```
