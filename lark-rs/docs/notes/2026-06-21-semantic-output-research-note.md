# Research note: SIMD vs. semantic output (2026-06-21)

Non-normative provenance for ADR-0027. The durable decision is the ADR; the trait
design is `semantic-output-builder-design.md`. This note records *why* we picked
the direction we did, so the alternatives aren't silently re-litigated.

## Question asked

Could lark-rs compile a generic grammar into a SIMD-accelerated parser "on par with
dedicated libraries like simdjson"? And separately: what is the standard way to bolt
an application-specific *output format* onto a parser, and is that an ultra-speed
lever?

## What we found

- **The ceiling is allocation, not the parse loop.** Independently confirmed by the
  repo's own profiling (ADR-0011: ~3 allocs/byte; the LALR loop is already dense
  array-indexed table lookup over interned ids). A vectorized LR stack machine is
  aiming at the wrong cost.
- **The "output format" feature is semantic actions / syntax-directed translation**
  — Python Lark's `Transformer`, with the LALR *embedded* transformer being a
  during-parse syntax-directed translation. lark-rs has no equivalent today;
  `TreeBuilder` (ADR-0015) is the seam where one belongs.
- Therefore **direct semantic output is the higher-value lever than SIMD**: it
  avoids building millions of generic tree nodes the caller never wanted, where SIMD
  only finds tokens faster.

## Alternatives considered (and why not)

- **"Generic grammar → simdjson-class parser."** Rejected as an honest *claim*: it's
  unfalsifiable for arbitrary grammars (contextual lexing, ambiguity, rich tree
  semantics). Defensible bound only: a JSON-shaped grammar + tape/event backend may
  land within a constant factor of a dedicated parser.
- **SIMD lexer prefilter first.** Deferred. It attacks the lexer (~55%) half, but the
  *structural* ceiling is the allocation (~32%) half; and SIMD intrinsics complicate
  the WASM/const-bakeable distribution story. Out of the semantic-output epic; its
  own ADR if/when funded.
- **Embed semantic actions in `.lark` files (Yacc-style inline code).** Rejected: it
  kills the grammar-portability USP (one grammar → LALR/Earley/CYK/standalone/WASM).
  Bind actions externally, keyed by rule alias/name; grammars stay pure.

## Conclusion

Adopt semantic output backends (ADR-0027), oracle-first: transformer parity is
oracle-backed; fast span/event/tape backends are relative-oracle-backed (projection
+ counters); public API shape is architect-ratified. SIMD is explicitly **out** of
this epic.
