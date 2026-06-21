# ADR-0027: Semantic output builders — TreeBuilder becomes one backend; parity is oracle-backed, fast paths are relative-oracle-backed

- **Status:** Proposed (pending architect ratification)
- **Date:** 2026-06-21

> Forward-looking *direction* ADR (architect-directed, 2026-06-21), seeded by the
> product-direction review committed under `docs/notes/`. It adopts a new capability
> epic — **semantic output backends** (syntax-directed translation / a `Transformer`
> equivalent) — and writes its falsifiability story *before* any code, so the area
> is executable under `/next-task` rather than guessed. Direction is the architect's
> (`PRINCIPLES.md` §1); this ADR records the go decision and the auto/escalate split,
> not an implementation.

## Context

The project goal is "10–100× faster than Python Lark." **ADR-0011** found the
LALR ceiling is not algorithmic: parsing is *allocation-bound* (~3 allocs/byte;
~32% of work is the shared `Tree`/`Token` representation), and the named lever is
the tree representation, deferred behind the `TreeBuilder` chokepoint
(ADR-0015 — "consolidate the seam before features") until **Earley exists as a
second consumer to co-design it.** Earley has since landed. The deferral's
precondition is now met.

Three facts make this the ripe next bet:

- **The seam already exists and is documented as the lever.** `TreeBuilder` calls
  itself "the one place where Lark's tree-shaping semantics live … the single
  chokepoint where the node representation could later change (arena / interned
  labels)" (`parsers/tree_builder.rs`). The parse stack carries `NodeValue`
  (`Token | Tree | Inline`); `Token`/`Tree` own `String`s — the exact churn
  ADR-0011 indicts.
- **lark-rs has no semantic-output surface today.** `Lark::parse()` returns
  `ParseTree`; `LarkOptions` has no `transformer` / visitor / output-builder /
  `tree_class` field. The only way to get application data is to build a generic
  tree and walk it — two passes, millions of tiny heap nodes.
- **Python Lark grounds most of it.** It has `Transformer` / `Visitor` /
  `Interpreter`, and an **embedded** `transformer=` that runs reductions into user
  values during the LALR parse (faster, equivalent to post-parse) — *rejected for
  Earley by design*. That embedded-LALR subset is **inside the oracle**.

The fork (the reason this is an ADR, not a `/next-task` pick): the *parity* part is
oracle-backed, but the *point* of the epic — span / arena / event / tape backends
that never build a generic tree — is behaviour Python has **no counterpart** for.
**ADR-0026** says beyond-oracle behaviour is `escalate` *and needs a validation
story*. So the question is not "do we build it" but "what is each slice's
falsifiable acceptance basis, and which slices are autonomous?"

## Decision

**Adopt semantic output backends as the next epic. Refactor `TreeBuilder` into the
default implementation of an internal `OutputBuilder` seam, then add backends.**
Tree-compatibility becomes *one backend, not the parser's identity*. ADR-0025 (no
back-compat, pre-users) makes the surface reshape free; ADR-0015 says do the seam
first.

Ground every slice on **ADR-0026's falsifiability ladder**, which sorts the work
cleanly into `auto` and `escalate`:

1. **Transformer parity (LALR + basic/contextual lexer) — oracle-backed →
   `good-autonomous`.** Python's embedded transformer is the oracle. Acceptance is
   *both* a result-equality oracle **and** a callback-**trace** oracle (ordered
   rule/token callbacks), because result equality alone misses order and
   skipped-callback bugs. Covers the sharp edges Python defines: terminal callbacks
   wired through the lexer, `Discard`, aliases, `?`/`_`/anon shaping,
   `keep_all_tokens`, `maybe_placeholders`. **Earley/CYK embedded transform is out
   of scope** — Python rejects it; post-parse transform stays the only path there.

2. **Fast backends (span / arena / event / tape) — *relative*-oracle-backed →
   `good-autonomous` once the seam + harness exist.** Representation is
   beyond-oracle, but behaviour is pinned by a **projection invariant** (ADR-0026's
   relative oracle): *materialize* the fast output and assert it is byte-identical
   to the transformer/tree oracle over the whole bank. Layered with deterministic
   **perf-counter property gates** (§2.5): a span-only path asserts
   `tree_nodes_built == 0` and `token_value_string_bytes == 0`; one
   `semantic_reduce_call` per parser reduction. The oracle says the value is right;
   the counters say the fast path is actually fast-shaped. Neither is wall-clock.

3. **Public API shape, output-mode taxonomy as product commitments,
   binding (PyO3/WASM/C) surface — `escalate`.** The Rust trait ergonomics have no
   Python counterpart (ADR-0026) and are new public API (§6). Captured in a
   separate API ADR, seeded by the design RFC in
   `docs/notes/semantic-output-builder-design.md`. The architect decides the trait;
   agents implement against it.

**The load-bearing sequencing guardrail: the first deliverable is the oracle
harness, not the API.** Order: (a) transformer result + trace oracle generator →
(b) no-op `TreeBuilder`→`OutputBuilder` refactor, banks green, no public API →
(c) internal *test-only* semantic backend matching the oracle → (d) token-callback
/ `Discard` / child-shaping parity → (e) output perf-counters → **[escalate gate:
API ADR]** → (f) public API → (g) standalone parity → (h) fast backends. Before
(a)/(c) land, an agent would be *inventing* semantics; after, the area is
self-checking.

### Explicit non-goals (falsifiability discipline, §2.7)

- **No "simdjson-class for arbitrary grammars" claim.** Unfalsifiable as stated.
  The defensible bound: a JSON-shaped grammar with a tape/event backend *may* land
  within a constant factor of a dedicated parser; arbitrary grammars get
  allocation-cut speedups, not dedicated-library throughput. Any PR asserting
  throughput parity must cite a deterministic counter + the cross-engine bench,
  never wall-clock alone.
- **SIMD lexing is not this epic.** It is a later, separately-gated experiment that
  attacks the ~55% lexer half, *not* the ~32% allocation half this epic targets —
  and it carries portability cost (WASM/const-bakeability, the §4 lens). It rides
  its own ADR if/when it is funded.

## Consequences

- **The area becomes autonomously executable** the moment the harness lands (§0):
  most slices are `good-autonomous` with written done-whens; only the API ADR and
  any throughput *claim* are `escalate`. `/review-pr` can tier each PR from this
  ADR without reconstructing the session.
- **Pairs cleanly with the existing decision log.** ADR-0011 (*why* — allocation is
  the lever), ADR-0015 (*seam before features*), ADR-0025 (*surface is free to
  reshape*), ADR-0026 (*beyond-oracle behaviour is escalate + needs a validation
  story* — the precedent this ADR applies). This ADR is ADR-0026's first
  application to a *performance* representation rather than an error-recovery shape.
- **Proposed `PRINCIPLES.md` §3 default** (carried on this ADR's PR, per §9
  policy-rides-its-own-PR): *"A new output representation with no Python counterpart
  ships only behind a relative oracle (projection back to the tree/transformer
  oracle) plus a deterministic perf-counter gate — never curated goldens alone."*
- **Tripwire / revisit.** If a fast backend cannot be projection-checked against an
  oracle (e.g. a lossy tape with no faithful materialization), it does **not** ship
  `auto` — it escalates with a named validation story, exactly as ADR-0026 requires.
  If throughput parity with a dedicated library is ever *claimed*, it must be a
  gated counter result, or it is reverted under §9.
