# Contributing

Thanks for your interest in this repository.

## This repository is **not** running a public bug-bounty program

Some internal documents in this repo use terms such as **"bug-bounty findings"**,
**"strike teams"**, **"XFAIL burndown"**, **"good-autonomous"**, or
**"autonomous workflow"**. These are **maintainer planning shorthand** for an
internal differential-audit process that compares the in-progress Rust rewrite
(`lark-rs/`) against Python Lark, which serves as the oracle.

This wording does **not** imply, for anyone outside the maintainer's own tooling:

- any monetary reward or bounty eligibility,
- assignment of an issue to you,
- acceptance of unsolicited or automated pull requests, or
- that opening a PR against a listed "finding" will be reviewed or merged.

**Unsolicited, duplicate, or automated PRs may be closed without review.**

## Before you start work

Please **open or comment on an issue first and wait for a maintainer to confirm**
before writing code. This avoids wasted effort and collisions with in-flight work.
Issues and internal docs that read like ready-made tasks are written for the
maintainer's own automation, not as an open invitation to external contributors.

## Working on upstream Lark (the Python package)

The Python toolkit under `lark/` follows upstream
[lark-parser/lark](https://github.com/lark-parser/lark). For genuine bug fixes and
features there, see the upstream
[development guide](https://github.com/lark-parser/lark#development) and file an
issue describing the problem before sending a patch.

## Security

For anything security-sensitive, see [`SECURITY.md`](SECURITY.md). Do not open a
public issue for a suspected vulnerability.
