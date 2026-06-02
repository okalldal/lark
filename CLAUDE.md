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

## Python Lark is Our Oracle

When working on `lark-rs`, use Python Lark as the ground truth:
```bash
python3 lark-rs/tools/generate_oracles.py   # regenerate expected-output JSON
```
