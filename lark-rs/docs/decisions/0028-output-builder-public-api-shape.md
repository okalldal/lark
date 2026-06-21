# ADR-0028: Public `OutputBuilder` API shape

- **Status:** Proposed (pending architect ratification)
- **Date:** 2026-06-21
- **Depends on:** ADR-0027 (semantic output direction), ADR-0026 (behaviour scoped
  to the oracle), ADR-0011 (allocation is the lever), ADR-0025 (surface free to reshape)

## Context

ADR-0027 adopted semantic output backends and refactored tree construction behind an
internal `OutputBuilder` seam, but explicitly deferred the *public* API shape as an
architect-ratified call: Rust ergonomics have no Python oracle (ADR-0026, PRINCIPLES.md
§6), so the trait shape is escalate-tier and must not be guessed. The design RFC
(`docs/notes/semantic-output-builder-design.md`) named six open forks and mapped each
to its falsifiable gate, so the choice here is over *ergonomics*, not correctness. This
ADR resolves those six forks so C7 (#232) can implement against a fixed surface.

Tree shaping (per-position punctuation filtering, `?rule` expand-one, transparent
`_rule`/anon helper inlining, placeholders) is structural over the child-value
sequence and never inspects a value's internals. It therefore stays *inside the engine*
and is parametric over the builder's `Value`; the user callback only ever sees the
final, already-shaped child list — mirroring Python Lark's embedded transformer.

## Decision

Adopt the RFC's recommended shape, with a deliberately *narrow* output-mode commitment.
The six forks resolve as:

1. **`Discard` via an `is_discard(&Value) -> bool` hook, default `false`.** Discard is
   an opt-in property of a builder's `Value`, not part of the universal contract.
   Rejected: `token`/`reduce -> Option<Value>`, which taxes the no-discard hot path for
   every builder. The "non-discarding builders pay nothing" claim is a counter-gated
   property (below), not prose.

2. **Per-call `Lark::parse_into<'i, B: OutputBuilder<'i>>(input, &mut builder)`,
   leaving `parse()` untouched.** `Lark` stays non-generic; the builder is parse-call
   state, not parser configuration. `parse()` becomes a thin wrapper over `parse_into`
   with the tree backend. Rejected as the primary Rust seam: a typed `Lark<B>` (pollutes
   the parser type). A `LarkOptions::output(mode)` runtime field is *additionally*
   needed for foreign bindings (fork 3), but is not the Rust extension point.

3. **Output-mode taxonomy — commit narrowly:**
   - **`Tree`** — stable public (Rust + bindings). Oracle-backed today.
   - **Custom (the open `OutputBuilder` trait)** — stable public **Rust only**; an open
     Rust trait cannot cross the FFI boundary.
   - **`SpanTree`** — experimental / feature-gated; the first performance backend, built
     next but not stabilised until its projection + allocation gates pass.
   - **`Event`, `Tape`** — internal experiments only; **not** public commitments until
     each has a concrete consumer *and* a projection gate. They define new public
     semantics (ordering, reduction boundaries, error/lifetime behaviour) that no oracle
     yet pins, so committing their format now would be design fiction.

   The foreign bindings (PyO3/WASM/C) expose a *closed* `enum OutputMode`; which variants
   it carries beyond `Tree`, and whether it offers a callback escape hatch, is a separate
   product decision (filed as a `needs-decision`, blocked on this ADR).

4. **Initial support boundary: LALR + basic/contextual lexers only.** Shift → `token`,
   reduce → `reduce` is exactly the LALR seam. Earley (SPPF ambiguity) and CYK
   (chart reconstruction) stay **post-parse**: "call the user's reducer during the
   parse" is not well-defined over a forest, and Python rejects embedded transform there
   too (ADR-0027).

5. **Span policy: spans internal from day one; no single global owned-vs-borrowed
   policy.** The trait is span-first (`token`/`reduce` receive `span: Range<usize>` +
   `input: &'i str`). The default `parse() -> ParseTree` keeps **owned** strings
   (ergonomic, compatibility-oriented); a distinct `SpanTree<'i>` and custom builders may
   **borrow `&'i str`**, tying the value's lifetime to the input. The builder chooses;
   the API does not force the trade-off on every user.

6. **Metadata via per-call `ctx: &OutputContext`, not an `init(&mut self, meta)`
   lifecycle hook.** The hot path stays interned (`RuleId`/`SymbolId`); `ctx` is a cheap
   borrow of precomputed metadata resolving an id to its callback/terminal name lazily,
   only when a builder asks. The builder is user-constructed before it ever sees the
   parser (the `parse_into(&mut builder)` shape), so a per-call reference avoids stored
   borrows, reset rules, and parser-reuse hazards.

## Consequences

- **C7 (#232) has a fixed surface to implement;** C8 (#233) is scoped to `SpanTree`
  with `Event`/`Tape` carved into gated follow-ups, and the bindings `OutputMode`
  taxonomy is a separate `needs-decision`.
- **The public vocabulary is two things — owned `Tree` and borrowed `SpanTree<'i>` —
  by design,** not accidental complexity: owned is the easy/compatibility story,
  borrowed is the zero-copy performance story.
- **`OutputBuilder::token` is engine token materialization, not Python's visible
  terminal callback** (it runs for *every* shifted terminal). This is a new Rust
  contract with no direct Python analogue; it is pinned against lark-rs behaviour and
  by projection to the transformer oracle, and is documented as a sharp edge (RFC §5).
- **Validation gates (ADR-0026 ladder, restated):**
  - `TreeOutputBuilder` refactor → **full oracle** (tree + compliance/wild banks
    byte-identical).
  - Transformer parity, token-callback/`Discard`/shaping → **oracle** (Python embedded
    value *and* callback-trace fixtures).
  - `SpanTree`/`Event`/`Tape` → **relative oracle** (materialize → byte-identical to the
    oracle output) **plus** deterministic perf-counters (`tree_nodes_built == 0` for
    non-tree builders; `token_value_string_bytes == 0` for no-copy span builders; bounded
    child-buffer reuse; one `semantic_reduce_call` per reduction). The "zero-cost
    `is_discard`" and "no intermediate tree" claims are *counter results*, never prose
    (ADR-0007).
  - Public API ergonomics / mode taxonomy / binding exposure → **escalate** (this ADR;
    no oracle exists).
- **Tripwires that revisit this ADR:** a concrete consumer + projection gate arriving for
  `Event` or `Tape` (promote it from internal to a committed mode); a measured regression
  showing non-discarding builders *do* pay for the `is_discard` branch (reopen fork 1);
  or a binding requirement that the closed `OutputMode` enum cannot express (reopen
  fork 3). A backend that cannot be projected back to an oracle-backed output is not
  autonomous and needs separate architect approval (ADR-0027).
