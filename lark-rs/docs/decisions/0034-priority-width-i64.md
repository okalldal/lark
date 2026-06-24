# ADR-0034: Terminal/rule priority storage is `i64`, not `i32`

- **Status:** Proposed (pending architect ratification)
- **Date:** 2026-06-24

## Context

Terminal and rule priorities were stored as `i32`. The loader parsed a priority
to `i128` and then **clamped to `i32`** (`grammar/loader/tokenizer.rs`,
`try_lex_number`). Python Lark (the oracle) represents priorities as
arbitrary-precision `int`s with no bound.

Two distinct priorities that both exceed `i32::MAX` therefore saturated to the
**same** value (`i32::MAX`) and tied, so lark-rs broke the tie by name order where
Python honours the true ordering (#352, bounty H4-4). Repro:

```
start: A | B
A.5000000000: "x"
B.9000000000: "x"
```

on input `"x"` — Python picks `B` (9e9 > 5e9); lark-rs clamped both to `i32::MAX`,
they tied, and it picked `A`. Confirmed against Python: it picks `B`, and it
**accepts** arbitrarily large priorities (no rejection even at 9e20) because Python
ints are unbounded.

The contract (issue #352): *support-and-match* — store priorities wide enough not
to collide, **or** reject out-of-range like Python. Python does not reject, so the
match is "store wide".

## Decision

Widen the priority storage type from `i32` to **`i64`** throughout:

- the loader clamp (`tokenizer.rs`): `Tok::Number(i64)`, clamp to the `i64` bounds
  instead of the `i32` bounds (saturation is kept as the graceful out-of-range
  behaviour, now at ±9.2e18 — a magnitude no hand-authored grammar reaches);
- the public fields `RuleOptions.priority` and `TerminalDef.priority`, and the
  builders `RuleOptions::with_priority(i64)` / `TerminalDef::new(.., i64)`;
- the loader AST (`RawRule`/`RawTerm.priority`), the template-store tuple, and
- the priority **accumulators** that sum priorities along a derivation: the Earley
  `node_priority`/`packed_priority` machinery (`prio`/`term_priority` maps and the
  `i32::MIN` sentinel) and the CYK `weight` (`saturating_add` chains) — these must
  widen too, or a summed pair of large priorities would re-truncate.

`i64` (not `i128`) is chosen as the minimal widening that resolves every realistic
collision: it covers the full range of any plausibly hand-authored priority,
matches Rust's idiomatic signed default for "wide enough", and keeps the fields a
single machine word. Going to `i128` or an arbitrary-precision type would buy
nothing observable (no grammar reaches ±9.2e18) at a real ergonomic/perf cost.

## Why this is escalate-tier

`RuleOptions` and `TerminalDef` (with their `priority` fields and builder
signatures) are re-exported at the crate root (`lib.rs`) — this is a **public
API/semantics change**. Per ADR-0016 §6 and ADR-0025 (which keeps API changes
`escalate`-tier even pre-users, because they are design/product direction with no
oracle), this rides an `escalate`-tier PR and is the **architect's** to merge, not
an auto-merge. Hence this ADR.

ADR-0025 also means **no backward-compat shim**: the old `i32` signatures are
replaced outright, not kept alongside.

## Consequences

- The named XFAIL `h4_4_priority_i32_saturation_tie` flips to passing: `B` (9e9)
  now outranks `A` (5e9), matching Python.
- Negative control (`h4_4_priority_small_and_boundary_still_order`): ordinary
  small priorities and a pair straddling the old `i32::MAX` boundary still order
  correctly — the widening does not perturb the non-saturating case.
- Out-of-range is still saturated, not rejected — symmetric with the prior
  behaviour and consistent with the existing "don't fail to lex a huge priority"
  comment, now at the `i64` boundary. Python never reaches this boundary, so it is
  unobservable against the oracle; if a future grammar ever needs > `i64`, revisit.
- **Tripwire:** if a new priority-consuming site is added with an `i32` type, a
  large-priority case will silently truncate again. Keep priority types `i64` end
  to end (loader → storage → accumulators).
