# ADR-0039: The embedded transformer is C8b's named event-sink consumer

- **Status:** Proposed (pending architect ratification)
- **Date:** 2026-07-01
- **Depends on:** ADR-0029 (public `OutputBuilder` API shape), ADR-0027 (semantic
  output direction), ADR-0026 (behaviour scoped to the oracle), ADR-0011 (allocation
  is the lever)

## Context

C8b (#242) is the internal event-stream output backend under the semantic-output epic
(#225): a builder that folds a parse into a value through the `OutputBuilder`
`token`/`reduce` seam without materializing a generic `Tree`/`Token` graph. ADR-0029
named its tripwire: an event stream defines *new public semantics* (event ordering,
reduction boundaries, `Discard`/placeholder behaviour) that have **no Python oracle**,
so committing an event format before a real consumer exists is design fiction — the
promotion from internal to a committed surface is an architect call, and #242 sat
`needs-decision` on it.

The one candidate consumer that already has a Python oracle is the **embedded
transformer** — Python Lark's `Lark(..., transformer=T)`, which applies the transformer
*during* the LALR parse instead of building a tree. The C1/C3/C4 work already froze that
oracle: `tools/generate_transformer_oracles.py` records, per case × {LALR basic,
contextual}, the ordered callback **trace** and folded **value** for both the post-parse
and the embedded paths (the embedded block pins the `__default_token__` divergence, RFC
§5 / #229), and `tests/test_transformer_oracle.rs` already drives an event-sink
`OutputBuilder` (`EmbeddedTransformBuilder`) through `parse_into` proven byte-identical to
that oracle over the whole bank.

## Decision

The **embedded transformer is C8b's named consumer**. This resolves the ADR-0029
tripwire for #242: the event-sink backend is grounded against the embedded-transformer
oracle (value + ordered trace over the bank) plus the deterministic zero-tree counter
(`tree_nodes_built == 0`).

The backend stays **internal — no public API commitment**. It is realized on the
existing `OutputBuilder` seam and gated in tests; a *public* transformer / `OutputMode`
surface (`Lark(transformer=…)`-style) remains the separate escalate-tier call tracked as
C10 (#244). Naming the consumer here does **not** bless a public event format.

## Consequences

- **Buys:** #242 becomes autonomously groundable and closeable — it is now backed by an
  oracle (embedded value + trace, `tests/test_transformer_oracle.rs`) and a deterministic
  perf gate (`tests/test_transform_counters.rs`: an embedded transformer driven through
  `parse_into` materializes no generic tree — `tree_nodes_built == 0` /
  `token_value_string_bytes == 0` — with the default `parse()` tree backend as the `> 0`
  positive control).
- **Costs / rules out:** no public transformer surface ships from this decision; the
  event ordering and `Discard`/placeholder semantics are committed only *internally*,
  against the embedded-transformer oracle. Nothing here commits an event *format* for an
  arbitrary external consumer.
- **Tripwire (unchanged from ADR-0029):** exposing this backend as a public `OutputMode`
  or `transformer=` parameter is a new public-API/semantics decision — escalate under
  #244, do not fold it in here.
- **Sibling:** C8c (#243, JSON tape) keeps its own `needs-decision`; it has no
  oracle-backed consumer named and is untouched by this ADR.
