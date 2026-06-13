# 0001. Python Lark as the oracle (oracle-first testing)

- **Status:** Accepted (retroactive — the founding decision, recorded here to seed the log)
- **Date:** 2026-06-13 (decision predates this record)
- **Deciders:** architect
- **Grounds:** new policy — became PRINCIPLES.md §2 invariants 1–2

## Context

A from-scratch reimplementation of a mature parser has an enormous, mostly-implicit
specification: tree shapes, error conditions, tie-breaks, ambiguity ordering,
dozens of EBNF interactions. Hand-writing expected outputs would re-encode our
*assumptions* about Lark, not Lark's actual behavior — and would make the test
suite agree with our bugs.

## Decision

Python Lark is the ground truth. Expected parse trees and errors are **generated**
from Python Lark (`tools/generate_oracles.py`, the strip-mined compliance banks,
the wild bank) and committed; the Rust output is compared against them. No feature
ships without a failing oracle first; no bug is fixed without a failing
reproduction first.

## Why / alternatives rejected

- *Hand-authored expectations* — rejected: they encode our beliefs, not Lark's
  behavior, and silently ratify divergence.
- *Spec-from-docs* — rejected: Lark's real contract is in its code (tie-breaks,
  forest ordering), not its prose.

The deciding property: parsing is hard to implement but cheap to *verify*. Making
verification falsifiable and automatic is the highest-leverage move available, and
it is what later made autonomous development tractable (see ADR 0003).

## Consequences

- Easier: regression safety (banks fail only on divergence from Lark), and an
  objective, author-independent gate that makes agent-written code reviewable by a
  machine.
- Harder: where lark-rs is arguably *better* than Lark, the divergence is a
  decision to make explicit (e.g. #101, #159), not a free win.
- Tripwire: CI regenerates every oracle generator and fails on drift; the
  oracle-coverage meta-test fails the build if a grammar has no oracle.
