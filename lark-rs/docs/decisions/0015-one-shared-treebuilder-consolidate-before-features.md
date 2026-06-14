# ADR-0015: One shared tree shaper across all engines; consolidate seams before adding algorithms

- **Status:** Accepted
- **Date:** backfilled 2026-06-13 from PRs #9, #10, #11

## Context

Before Earley (the second of three parser algorithms) landed, three load-bearing
abstractions were deliberately extracted *first*: the terminal algebra (#9), the
`TokenSource` trait (#10), and a shared `TreeBuilder` (#11). The explicit
philosophy (#9): *"in a parsing toolkit the architecture is the product, so we
consolidate the load-bearing abstractions before stacking more parser
algorithms."*

## Decision

Maintain **one** rule→tree shaper (`parsers::tree_builder::TreeBuilder::assemble`)
and **one** lexer⇄parser seam (`TokenSource`), shared by every engine, rather
than letting each algorithm grow its own. Settle a seam before the next algorithm
forces a second, divergent copy of it. The `TreeBuilder` rationale (#11):
*"Settle one tree-builder before Earley's SPPF materializes a second one shaped
differently."*

## Consequences

- LALR, Earley, and CYK all call the same `TreeBuilder::assemble`, so an
  unambiguous parse is byte-identical across engines by construction — this is
  what makes the multi-algorithm guarantee (ARCHITECTURE.md differentiator #1)
  cheap to keep true, and it underpins [ADR-0008](0008-standalone-shares-one-runtime.md)'s
  "share, don't copy" approach.
- A rejected alternative is preserved here so it isn't re-litigated (#11):
  interning the `Tree`'s string label to an id was floated and **declined** —
  *"a `Tree` is the public output and must stay self-contained … Replacing its
  owned `String` label with an interned id would force every consumer to carry
  the symbol table — for a perf win no profiler has asked for."* (Contrast
  [ADR-0003](0003-intern-symbols-to-ids-with-flags.md): the *engine* interns
  aggressively; the *public output* deliberately does not.)
- The cost is up-front refactoring before feature work — accepted because the
  shared seam is cheaper than reconciling N divergent copies later.
