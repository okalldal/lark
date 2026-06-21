# Design RFC: the `OutputBuilder` seam and public API shape

- **Status:** Draft RFC — *proposal feeding the escalate-tier API decision*. Not
  ratified. The architect picks the shape; this becomes (or seeds) the API ADR that
  ADR-0027 §Decision-3 defers to. An agent never self-ratifies a public API
  (`PRINCIPLES.md` §6, ADR-0026).
- **Date:** 2026-06-21
- **Context:** ADR-0027 (direction), ADR-0011 (allocation is the lever), ADR-0015
  (`TreeBuilder` is the one shaping seam), ADR-0025 (surface is free to reshape).

This RFC proposes the concrete trait, says what stays *out* of the user's hands
(the shaping engine), names the open forks for the architect, and maps each to its
falsifiable gate so the architect is choosing ergonomics, not correctness.

---

## 1. Goals & non-goals for the *shape*

**Goals.** (1) A single seam every engine path funnels reductions through, with the
current tree behaviour as one implementation (ADR-0015). (2) Zero-cost when a
builder doesn't use a feature (no `Discard` machinery in the hot path for a builder
that never discards). (3) Spans/`&'i str` from day one *internally*, even while the
default backend materializes `String`s (ADR-0011's "design spans from day one").
(4) Tree shaping (filter / `expand1` / transparent-inline / anon-flatten) lives in
the **engine**, never in the user trait — so a user value type can be *anything*.

**Non-goals.** Inline Rust in `.lark` files (kills grammar portability — the whole
Lark USP); Earley/CYK embedded transform (ADR-0027 scope); the binding (PyO3/WASM/C)
surface (those expose a *fixed enum of modes*, not the open Rust trait — §6 below).

## 2. The core insight: shaping stays in the engine

Python Lark's embedded transformer works because the shaping chain (`ChildFilter`,
`expand1`, transparent/anon inlining, placeholders) runs *first* and the user
callback receives the already-shaped child list. Crucially, **every shaping
operation is structural over the sequence of child values** — it decides *which*
child values survive and whether a node is spliced — and **never inspects a value's
internals.** So shaping is parametric over the user's `Value`:

- **Filter** (per-position punctuation) → drop child values at filtered positions.
- **`expand1` (`?rule`, one kept child, no alias)** → don't call `reduce`;
  propagate the single child value unchanged.
- **Transparent (`_rule`) / anon `__anon_*` helper** → don't call `reduce`; return
  the kept children as an *inline group* the parent splices.
- **Normal rule** → call `reduce(rule, &shaped_children, span)`.

The inline-group plumbing is exactly today's `NodeValue::Inline`. It stays **inside**
the seam as `Slot<V> { Value(V) | Inline(Vec<V>) }` on the parse stack; the user's
`reduce` only ever sees a flat, already-spliced, already-filtered `&mut Vec<V>`.
This is what lets the JSON backend return `JsonValue` and the tree backend return
`Child` through *the same* shaping code.

## 3. Proposed trait

```rust
/// One reduction sink. The engine applies all Lark tree-shaping (filtering,
/// expand1, transparent/anon inlining, placeholders) *before* calling `reduce`,
/// so `children` is the final, shaped child list — identical to what Python
/// Lark's embedded transformer hands a rule method.
pub trait OutputBuilder<'i> {
    /// The value carried on the parse stack (Yacc's semantic value).
    type Value;

    /// A shifted terminal — runs for **every** shifted terminal, because the parse
    /// stack needs a value (this is *engine token materialization*, lower-level than
    /// Python's visible terminal callbacks; see §5). `span` indexes `input`; `input`
    /// is the whole source, so a builder can borrow (`&input[span]`) or own
    /// (`input[span].to_owned()`). `ctx` resolves the interned `terminal` to its
    /// Python-side name when the builder needs it.
    fn token(
        &mut self,
        terminal: SymbolId,
        span: core::ops::Range<usize>,
        input: &'i str,
        ctx: &OutputContext<'_>,
    ) -> Self::Value;

    /// A completed reduction of `rule` over its shaped `children`.
    /// `span` covers the whole production (subsumes propagate_positions).
    /// `ctx` resolves `rule` to the *callback name* Python would dispatch on
    /// (alias → template source → origin), the name parity Python's
    /// `create_callback` uses.
    fn reduce(
        &mut self,
        rule: RuleId,
        children: &mut Vec<Self::Value>,
        span: core::ops::Range<usize>,
        input: &'i str,
        ctx: &OutputContext<'_>,
    ) -> Self::Value;

    /// Discard hook (Python's `Discard` sentinel). Default: nothing discards —
    /// the engine skips the check entirely, so non-discarding builders pay zero.
    /// The tree backend overrides it to reach token/rule `Discard` parity.
    #[inline]
    fn is_discard(&self, _value: &Self::Value) -> bool {
        false
    }
}
```

Why these choices:

- **`&mut Vec<Self::Value>`, not `Vec`/slice.** Lets a builder *drain* children into
  its own structure (the array/object case) with no extra copy, and lets the engine
  reuse the buffer across reductions (bounded child-buffer reuse — a perf-counter
  target in ADR-0027 slice (h)).
- **`span: Range<usize>` not `Token`/line-col.** Line/column derive lazily from
  `span + input` only when asked; the hot path never computes them. This is the
  zero-copy substrate even though `TreeOutputBuilder` materializes `String`s today.
- **`is_discard` hook, not `Option<Self::Value>`.** Returning `Option` from every
  `reduce`/`token` pushes a branch and a discard concept onto the 99% path that
  never discards. The default-`false` hook is monomorphized away for those builders
  and gives the tree backend full `Discard` parity. *(Alternative weighed in §7.)*
- **`ctx: &OutputContext` carries the ID→name path.** The hot path stays interned
  (CLAUDE.md: "an array index per token, never a string hash"), but a builder can't
  be written — or oracle-tested — without resolving an `RuleId`/`SymbolId` back to a
  name. `OutputContext` is a cheap borrow of precomputed metadata exposing
  `callback_name(RuleId)` (the alias→template→origin resolution Python dispatches
  on), `rule_name`/`rule_alias`, and `terminal_name(SymbolId)`. It is passed
  per-call rather than injected at builder construction because the builder is
  user-constructed *before* it ever sees the parser (the `parse_into(&mut builder)`
  shape, §6); a reference costs nothing and keeps name resolution lazy. *(The
  injection alternative — an `init(&mut self, meta)` lifecycle hook — is a §7 fork.)*

## 4. Default & test backends

- **`TreeOutputBuilder` (`Value = Child`)** — *the* compatibility backend. The
  no-op refactor (ADR-0027 slice b) must make this byte-for-byte reproduce today's
  `ParseTree`, proven by the existing tree-oracle + compliance/wild banks staying
  green. This is the safe refactor: behaviour is fully gated.
- **`PythonTransformerOracleBuilder` (test-only) — the Python-compat adapter.**
  Deliberately Python-*shaped*, not Rust-ergonomic: it holds the action spec keyed
  by `callback_name`/`terminal_name` (`rule_methods`, `token_methods`,
  `default_rule`, `default_token`), resolves every incoming `RuleId`/`SymbolId`
  through `ctx` to that name world, and emits **both** the final value (vs the Python
  transformer value oracle, ADR-0027 slice c) and an ordered callback `trace` (vs the
  Python trace oracle). It is the single place the engine-vs-visible distinction
  (§5) lives: it logs a *visible terminal callback* only when the spec defines a
  method for that terminal, and otherwise materializes a Token-compatible value
  silently. Result equality alone misses order, missed callbacks, and callbacks
  firing for filtered punctuation — the trace is why this adapter exists.
- **Later, speed:** `SpanTreeBuilder` (`Value` borrows `&'i str`), event sink, JSON
  tape. Each ships behind the §projection invariant + perf-counters of ADR-0027.

## 5. The parity sharp edges (settled by the trace oracle, not this RFC)

These are behaviour, so they are *not* RFC decisions — the Python trace oracle pins
them. Flagged here so the implementer expects them:

- **Engine token materialization ≠ Python-visible terminal callback (the load-bearing
  one).** The trait's `token()` runs for *every* shifted terminal because the stack
  needs a value. Python is narrower: `_get_lexer_callbacks` wires a terminal callback
  **only** for terminals the transformer actually defines a method for — every other
  terminal is kept as a plain `Token`, with no visible callback. So the oracle adapter
  must **not** log every `token()` call as a Python callback: it logs a visible
  terminal callback iff the action spec defines a method for that terminal, else it
  materializes a Token-compatible value silently (this is exactly what
  `PythonTransformerOracleBuilder` does, §4). Naïvely tracing every `token()` would
  manufacture false mismatches — for kept terminals, not just filtered ones.
- **Does the embedded path observe `__default_token__`?** Uncertain — Python's
  embedded transformer may not invoke a default token handler the post-parse path
  does. `default_token` in the adapter is wired *only if the oracle shows the
  embedded path observes it*; the fixture settles it, the RFC does not guess.
- **Filtered punctuation.** A filtered token's value is dropped at shaping; whether
  its `token()`/visible-callback fired first is pinned by the fixtures.
- **`Discard` shifts sibling positions** for placeholders — the `maybe_placeholders`
  fixtures pin the interaction.
- **Aliases / templates / `?`/`_`** decide the *callback name* (`-> alias` vs origin
  vs template source) — the `ctx.callback_name` resolution above. The shaping engine
  resolves it; fixtures pin it.

## 6. Public entry point & binding implications (the escalate part)

**Recommended surface** — a generic method, leaving `parse()` untouched:

```rust
impl Lark {
    /// Existing: parse to the default Lark tree (uses TreeOutputBuilder).
    pub fn parse(&self, input: &str) -> Result<ParseTree, LarkError>;

    /// New: parse directly into a builder's values, no generic tree built.
    pub fn parse_into<'i, B: OutputBuilder<'i>>(
        &self,
        input: &'i str,
        builder: &mut B,
    ) -> Result<B::Value, LarkError>;
}
```

`parse_into` keeps `Lark` non-generic (the builder is per-call, not per-parser),
mirrors Python's embedded `transformer=` ergonomically, and `parse` stays a
thin `parse_into(_, &mut TreeOutputBuilder)`.

**Binding implication (genuinely escalate / product).** An open Rust trait cannot
cross the PyO3/WASM/C boundary. Those bindings expose a *closed* `enum OutputMode {
Tree, SpanTree, Event, Tape }` selecting built-in backends — which makes the output
*mode taxonomy* a product commitment, not just an implementation detail. That
taxonomy, and whether the foreign bindings get a callback escape hatch at all, is
the architect's call.

## 7. Open forks for the architect (what to ratify)

1. **`Discard` representation.** Recommended: the `is_discard` hook (§3,
   zero-cost default). Alternative: `reduce`/`token -> Option<Value>` (more
   obvious, but taxes the no-discard hot path). Pick one.
2. **Entry shape.** Recommended: per-call `parse_into<B>` (§6). Alternatives:
   a typed `Lark<B>`, or a `LarkOptions::output(mode)` runtime field (needed anyway
   for bindings — do both?).
3. **Output-mode taxonomy as a product commitment.** Which of
   `Tree | SpanTree | Event | Tape | Custom` are *committed* surfaces vs.
   experiments — and which the foreign bindings expose (§6).
4. **Initial support boundary.** Recommended: LALR + basic/contextual only
   (matches Python's embedded limit, ADR-0027). Confirm Earley/CYK stay post-parse.
5. **Span policy at the public boundary.** Internal spans from day one (settled,
   ADR-0011); does the *first public* builder hand back owned `String`s or borrowed
   `&'i str`? (Borrowing ties the value's lifetime to the input — an ergonomics
   call.)
6. **Metadata path form.** *That* the builder gets an ID→name path is settled (§3 —
   without it the API is unusable and untestable). The form is the fork: recommended
   per-call `ctx: &OutputContext` (fits `parse_into(&mut builder)`, keeps resolution
   lazy); alternative is an `init(&mut self, meta: &OutputMetadata)` lifecycle hook
   the parser calls before the first reduction (smaller per-call signature, but the
   builder must store the borrow).

## 8. Where it plugs in (orientation, not a mandate)

- `parsers/tree_builder.rs` — becomes `TreeOutputBuilder`, the default `impl`.
- `parsers/lalr.rs` — the reduce loop gains the `OutputBuilder` type parameter; the
  `Slot<V>` stack replaces the `NodeValue` stack (a new driver wiring, not match
  arms — §3). Shaping logic moves beside the seam, value-parametric.
- `parsers/token_source.rs` / `tree.rs` — spans already available; `Token`'s owned
  strings become *one builder's* choice, not the engine's.
- `lib.rs` — `parse_into`; `parse` delegates.
- `standalone/runtime.rs` — gets the parallel seam *after* the main crate
  (ADR-0027 slice g); today it bakes its own `Token`/`Tree`/`Child`.

## 9. Validation story (ADR-0026 ladder, restated for this surface)

| Slice | Gate |
|---|---|
| `TreeOutputBuilder` refactor | **Full oracle** — existing tree + compliance/wild banks byte-identical |
| Transformer parity | **Oracle** — Python embedded-transformer value **and** trace fixtures |
| Token-callback / `Discard` / shaping | **Oracle** — targeted trace fixtures (§5) |
| Span / event / tape backends | **Relative oracle** — materialize → byte-identical to the tree/transformer oracle, + perf-counter property gates (`tree_nodes_built == 0`, `token_value_string_bytes == 0`) |
| Public API ergonomics, mode taxonomy | **Escalate** — architect ADR; no oracle exists, by ADR-0026 |
