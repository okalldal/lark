# ADR-0038: `maybe_placeholders` None and `Discard` for a generic `OutputBuilder::Value`

- **Status:** Proposed (agent-drafted decision memo; architect ratifies at C7 merge)
- **Date:** 2026-07-01
- **Depends on:** ADR-0029 (public `OutputBuilder` API shape), ADR-0027 (semantic
  output direction), ADR-0026 (behaviour scoped to the oracle), ADR-0011 (allocation
  is the lever). Amends the ADR-0029 fork-1 (`is_discard`) resolution with the
  position-semantics detail it deferred.
- **Issue:** #232 (C7). RFC §5 flagged both edges as "settled by the trace oracle,
  not the RFC."

## Context

C7 makes the `OutputBuilder` seam value-parametric: `reduce` receives
`&mut Vec<Self::Value>` for an arbitrary `Value`, and `token` returns one. Two of
Python Lark's shaping behaviours currently lean on the fact that `Value` is
concretely `Child`, and do not survive genericization unchanged:

1. **`maybe_placeholders` inserts a literal `None` child.** Today the engine pushes
   `Child::None` into the child list (and the `?rule` lone-`None` collapse is
   `Slot::Inline([Child::None])`). For a `Value` that is *not* `Child`, the engine
   cannot synthesize a placeholder — there is no universal "nothing" value for an
   arbitrary type. Python gets away with it because every value is `Optional`.
2. **`Discard`** (Python's transformer sentinel) drops a value from its parent's
   child list. ADR-0029 fork 1 chose an `is_discard(&Value) -> bool` hook (default
   `false`), but deferred *where in the pipeline the drop happens* — specifically its
   interaction with placeholder positions — to the trace oracle.

Neither edge affects the `TreeOutputBuilder` (`Value = Child`) or the `SpanTree`
backend's *tree-shaped* output: the tree backend keeps `Child::None` and never
discards (its punctuation filtering is `filter_pos`, a separate mechanism, not
`Discard`). So **both edges are invisible to the compliance/wild/JSONTestSuite/
propagate-positions banks** — they bite only a *user semantic builder* whose `Value`
is a custom type. That is exactly ADR-0026's "no Python oracle for Rust ergonomics"
zone, hence this memo rather than a banks-gated auto decision.

## Decision

### 1. Placeholder — add `fn placeholder(&mut self, ctx) -> Self::Value` to the trait

A placeholder is a value the *builder* mints, exactly as a token or a reduction is —
so it is a builder responsibility, not an engine-synthesized sentinel.

```rust
/// The value for an absent `maybe_placeholders` optional (`[...]`). Python Lark
/// inserts a literal `None` child here; a builder maps that to its own "absent"
/// value. Default: unreachable unless the grammar uses `maybe_placeholders` — a
/// builder used with such a grammar MUST override this.
fn placeholder(&mut self, _ctx: &OutputContext<'_>) -> Self::Value {
    panic!(
        "OutputBuilder::placeholder called: this builder was used with a \
         maybe_placeholders grammar but does not implement placeholder()"
    );
}
```

- `TreeOutputBuilder` overrides → `Child::None`. `SpanTree` overrides → its own
  none/leaf. Byte-identical tree output preserved.
- **Rejected — required method (no default):** taxes every builder, including the
  vast majority whose grammars never use `maybe_placeholders`, with a method they
  never reach. The panicking default keeps the common builder a three-method impl
  (`token`/`reduce`/optionally `is_discard`) and turns misuse into a clear,
  discoverable error, mirroring the engine's existing `unreachable!` guards.
- **Rejected — `reduce(&mut Vec<Option<Value>>)`:** pushes `Option` onto the 99%
  hot path that has no placeholders, the same tax ADR-0029 fork 1 rejected for
  `Discard`. The panicking-default hook is the parallel choice to `is_discard`.

The `?rule` lone-`None` collapse and the root `Inline([None]) -> ParseTree::None`
carve-out (RC9/#289/ADR-0033) stay engine-internal for the tree/`ParseTree` path;
they are expressed in terms of `placeholder()` values, not a hardcoded `Child::None`.

### 2. `Discard` — engine drops `is_discard` children *after* shaping, *before* the
parent node is built; placeholder positions are fixed by the grammar, not shifted by
a sibling's discard

`is_discard` stays as ADR-0029 fork 1 resolved it (hook, default `false`,
monomorphized away for non-discarding builders). This memo pins the *ordering* it
deferred:

- The engine builds the shaped child list (filter `filter_pos` punctuation, splice
  transparent inlines, insert `maybe_placeholders` `placeholder()` values at their
  grammar-determined positions), **then** drops any child `v` with
  `is_discard(&v) == true`, **then** applies `expand1`/transparent/`reduce`.
- **Placeholder positions are a property of the production, not of sibling values.**
  A discarded *sibling* does not renumber a `[...]` slot: the placeholder count and
  offsets come from `RuleOptions.nones_before` / `placeholder_count`, computed at
  lowering from the grammar alternative, and are inserted before the discard sweep.
  This matches Python: `maybe_placeholders` is resolved structurally during
  tree-building; `Discard` is a transformer-time value drop layered on top.

This ordering is **oracle-gated the moment a semantic builder exists** (the C4/#229
trace-oracle harness, extended with a `Discard` + `maybe_placeholders` fixture in
C7b/beyond). Until then it changes no observable tree output, so no bank moves.

### 3. `token()` input — refines ADR-0029 fork 5 (span-first) for byte-identical C7

ADR-0029 fork 5 sketched `token(terminal, span: Range<usize>, input, ctx)`, with
line/column "derived lazily from span + input." Implementing C7 byte-identically
surfaces two reasons the *engine* must hand positions/value through rather than have
the builder recompute:

- **Line/column are lexer state, not a pure function of the byte span.** The
  `BasicLexer` cursor (`src/lexer/mod.rs`) tracks 1-based line/col with a running
  char counter (newline resets col), and multi-line tokens get an `end_line`/
  `end_column` the cursor advanced. Re-deriving these in the builder from `span +
  input` is a reimplementation with a divergence hazard the position + propagate-
  positions oracles would (rightly) trip. The lexer already paid for them, so the
  engine passes them through — *more* aligned with fork 5's "never recompute on the
  hot path," not less.
- **The value string is already allocated by the lexer.** Rebuilding it from
  `input[span]` in the builder would double-allocate (lexer string built, dropped,
  rebuilt) — an ADR-0011 regression on the default path. C7 moves the lexer's owned
  value into the tree builder instead.

**Decision for C7:** `token()` receives the lexer's token record (interned
`terminal`, `span`, precomputed line/col, and the owned value) plus `input: &'i str`.
`TreeOutputBuilder` moves the value → `Child::Token` (byte-identical, no extra
alloc). **This is the C7 intermediate, not the end state:** C8 changes the lexer to
emit spans without building the value string, at which point the value field narrows
to a borrowed `&'i str` and the signature converges on fork 5's span-only ideal
(that flip is where `token_value_string_bytes == 0` becomes achievable — C8's gate,
not C7's). Documented here so the escalate review sees the intended trajectory.

## Consequences

- **C7's `TreeOutputBuilder` reshape stays byte-identical and banks-gated** — both
  edges are no-ops for `Value = Child` (placeholder → `Child::None`; nothing
  discards). The reshape's safety argument is unchanged from C2's.
- **The public trait gains one method (`placeholder`) beyond ADR-0029's sketch.**
  That is a public-API-shape addition (ADR-0029 §6 / PRINCIPLES §6), which is why
  this rides an ADR and merges escalate-tier with C7, not as an autonomous call.
- **The `Discard`/placeholder ordering is a claim to be pinned, not yet pinned.** It
  is asserted here on Python-parity reasoning and becomes falsifiable when the
  semantic-builder trace fixtures land; if the oracle contradicts the ordering, this
  ADR is amended (the tripwire).
- **Tripwires that revisit this ADR:** a trace fixture showing Python *does* shift
  placeholder positions by a sibling `Discard`; a measured cost from the
  `placeholder()` virtual call on a hot maybe_placeholders grammar (revisit the
  panicking-default vs. an inlined sentinel); or a binding (`OutputMode`) that cannot
  express `placeholder`.
