# ADR-0034: Terminal/rule priority storage is `i64`, not `i32`

- **Status:** Accepted (2026-06-30; was Proposed — architect ratified)
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

The contract (issue #352): resolve the realistic-magnitude collisions of #352. The
honest framing is **not** unbounded "support-and-match" with Python's
arbitrary-precision ints — lark-rs deliberately narrows to a **bounded `i64`
priority domain** (below). Within that domain it matches Python; beyond it,
lark-rs saturates where Python would keep growing.

## Decision

lark-rs intentionally supports a **bounded `i64` priority domain**, saturating
beyond ±`i64::MAX`. This is a deliberate bounded narrowing of Python's
arbitrary-precision priority semantics, **not** unbounded parity. The bound applies
end to end:

- **Storage** is `i64` (was `i32`). The loader (`tokenizer.rs::try_lex_number`)
  parses to `i128` and **clamps to the `i64` bounds** — two distinct declared
  priorities that both exceed `i64::MAX` therefore both saturate to `i64::MAX` and
  **tie** (the tie then breaks by the engine's normal rule/name order). This is the
  documented bounded behavior, pinned by
  `h4_4_priority_beyond_i64_saturates_to_bounded_domain`.
- **Accumulation** is **saturating**. Both backends sum priorities along a
  derivation, and both saturate at the `i64` boundary rather than wrap or panic: CYK
  uses `weight.saturating_add` (table fill + unit-rule folding) and Earley's forest
  accumulator (`packed_priority_value`) uses `saturating_add` for the
  rule-priority-plus-children sum — they are consistent. A derivation whose summed
  priorities exceed `i64::MAX` pins at `i64::MAX` and still wins deterministically,
  pinned by `h4_4_earley_priority_accumulation_saturates_no_overflow`.

Within the bounded domain (any realistic hand-authored magnitude), ordering matches
Python exactly. The widening from `i32` resolves every realistic collision of #352:

- the loader clamp (`tokenizer.rs`): `Tok::Number(i64)`, clamp to the `i64` bounds
  instead of the `i32` bounds (saturation is kept as the graceful out-of-range
  behaviour, now at ±9.2e18 — a magnitude no hand-authored grammar reaches);
- the public fields `RuleOptions.priority` and `TerminalDef.priority`, and the
  builders `RuleOptions::with_priority(i64)` / `TerminalDef::new(.., i64)`;
- the loader AST (`RawRule`/`RawTerm.priority`), the template-store tuple, and
- the priority **accumulators** that sum priorities along a derivation: the Earley
  `node_priority`/`packed_priority` machinery (`prio`/`term_priority` maps and the
  `i32::MIN` sentinel) and the CYK `weight` (`saturating_add` chains) — these must
  widen too, or a summed pair of large priorities would re-truncate. The accumulator
  sums are **saturating, not plain `+`**: Earley's `packed_priority_value` uses
  `saturating_add` for `base + child.left + child.right`, mirroring CYK's existing
  `weight.saturating_add` — so a derivation summing priorities past `i64::MAX`
  saturates instead of wrapping or panicking, the bounded-domain policy applied to
  accumulation as well as storage.

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
  comment, now at the `i64` boundary. **This is the bounded-`i64` policy, not a
  Python match:** Python's unbounded ints never saturate, so beyond ±`i64::MAX`
  lark-rs *diverges by design* — two distinct beyond-boundary priorities tie where
  Python would order them, and a summed derivation pins at `i64::MAX` where Python
  would keep growing. **The tradeoff:** we trade exactness at extreme (>9.2e18,
  unreachable by any hand-authored grammar) magnitudes for a single-word,
  panic-free, deterministic accumulator. No Python oracle exists past the boundary
  (Python is unbounded), so the bounded behavior is pinned to *our stated policy*
  (no panic + deterministic + saturating), not to the oracle — by
  `h4_4_priority_beyond_i64_saturates_to_bounded_domain` and
  `h4_4_earley_priority_accumulation_saturates_no_overflow`. If a future grammar
  ever needs > `i64`, revisit.
- **Tripwire:** if a new priority-consuming site is added with an `i32` type, a
  large-priority case will silently truncate again; and if an accumulator sums
  priorities with a plain `+` instead of `saturating_add`, a large summed derivation
  will wrap/panic. Keep priority types `i64` and accumulator sums *saturating* end to
  end (loader → storage → accumulators).
