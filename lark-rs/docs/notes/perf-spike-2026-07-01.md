# Perf spike 2026-07-01 — where the next level actually is

- **Status:** Spike findings (measurements + demonstrated prototypes on branch
  `claude/lark-rs-perf-spike-5ew7ag`). Not policy; production landing of each item
  goes through the normal issue → PR flow. Wall-clock numbers below are one box
  (4-core Xeon @ 2.80 GHz, rustc 1.94.1, release+LTO `bench` profile) — **only the
  ratios travel** (BENCH.md discipline).
- **Method:** profile first (callgrind + DHAT on `examples/profile_parse parse`,
  the 92 KB `json_large` workload), then one experiment per suspected cost, each
  measured on `cargo bench --bench parse` and kept only if the full suite stays
  green. Deterministic-counter gates for anything that lands for real (ADR-0007);
  this note reports wall-clock because a spike's job is to *find* the levers.

## TL;DR

The LALR hot path was still spending **>60% of instructions in memcpy + malloc/free
+ SipHash** (callgrind, pre-spike). Four safe, engine-internal changes on this
branch remove the SipHash entirely and a third of the allocations, for
**−16…−23% wall-clock** across the parse workloads. An allocator swap (mimalloc)
is worth another **−20%** on top, for ~**1.4–1.5× combined**. Beyond that, a
**NullBuilder floor measurement** shows **59.5% of remaining parse time is output
materialization** — and the existing opt-in `SpanTree` backend recovers only a
fifth of that gap, so the ratified "zero-copy backend" plan is leaving most of its
own prize unclaimed. The next level is the *value-stack element size* and the
*owned `Tree`/`Token` representation*, quantified below.

## Baseline (this box, before the spike)

| workload | bytes | median | MB/s |
|---|---:|---:|---:|
| parse json_small | 390 | 62.2 µs | 6.3 |
| parse json_medium | 8.7 K | 1.56 ms | 5.6 |
| parse json_large | 92 K | 17.67 ms | 5.2 |
| parse arith_small | 33 | 12.6 µs | 2.6 |
| parse arith_large | 2.1 K | 0.80 ms | 2.6 |

Callgrind (5× json_large): memcpy **31.5%**, malloc/free machinery **~25%**,
SipHash **~5%** — the engine's own logic (DFA search, table dispatch, shaping
control flow) is single-digit percent. DHAT: ~301 K allocation blocks per 92 KB
parse (≈3.3 blocks/input byte).

## What was actually costing (found ≠ guessed)

1. **Every token was materialized three times.** `Contextual::lex_next` builds the
   `Token` (type_ clone + value alloc), `TokenSource::peek` **cloned** it out of the
   cache (the trait returned `Token` by value), and the SHIFT arm **cloned** it
   again into the builder (`builder.token(token.clone(), …)`). Six String
   allocations per token where two suffice.
2. **Three per-token SipHash probes** that should be array reads:
   `names[&id]` (`HashMap<SymbolId, String>`) in `build_token`,
   `state_to_scanner.get(&state)` (`HashMap<usize, usize>`) in the contextual
   lexer, and `ignore.contains(&id)` (`HashSet<SymbolId>`) in the skip loop.
   (A fourth remains: `unless.get(&id)` in `DfaScanner::match_at` — identified,
   not yet converted.)
3. **Every reduction `drain().collect()`ed the popped slots** into a fresh `Vec`
   before shaping — one allocation + one full memcpy of the popped elements per
   reduction, purely to satisfy `shape_reduction`'s owned-`Vec` signature.
4. **The value-stack element is enormous.** `Child` = 152 bytes (a `Tree` is
   inline), `Meta` = **104 bytes** (six `Option<usize>` at 16 bytes each), so the
   generic stack element `GSlot<Child>` ≈ 264 bytes. Every push/pop/drain/splice
   memcpys that. This is the single biggest source of the 31% memcpy share.

## Experiments and results

Cumulative `bench parse` medians (same box, same session):

| change | json_large | arith_large | notes |
|---|---:|---:|---|
| baseline | 17.67 ms | 803 µs | |
| E1 token plumbing (peek_type/take_current — no peek-clone, no SHIFT-clone) | 16.02 ms | 770 µs | −9% / −4% |
| E2 dense per-token tables (names Vec, state→scanner Vec, ignore bitset) | 14.87 ms | 649 µs | −7% / −16% more |
| E3 in-place reduction (shape_reduction drains the stack tail itself) | 14.81 ms | 616 µs | mins −4% / −5% |
| **E1–E3 vs baseline** | **−16%** | **−23%** | suite green after each |
| E0 allocator: mimalloc (measured on baseline) | 14.16 ms | 662 µs | **−20% for free**, orthogonal |

Post-E3 callgrind: total instructions −18.5% (1103 M → 899 M for 5 parses);
SipHash gone from the profile (5% → 0.5%); DHAT blocks 242.7 K (−20%).
memcpy is now **35.7%** of a smaller pie — the representation, not the logic,
is what remains.

## The ceiling: NullBuilder floor measurement (new instrument)

`examples/spike_floor.rs` parses a 146 KB JSON through the public `parse_into`
with a do-nothing builder (`Value = ()`): the run pays lexing, LALR dispatch, and
all shaping control flow, but materializes nothing.

| path | time | MB/s |
|---|---:|---:|
| `parse()` (owned tree) | 28.45 ms | 5.1 |
| `parse_span()` (C8 span backend) | 23.32 ms | 6.2 |
| `parse_into(NullBuilder)` — the floor | 11.51 ms | 12.7 |

Two conclusions:

- **Output materialization is 59.5% of end-to-end parse time** (post-E3!). The
  ADR-0011 "allocation-bound" diagnosis still holds after the cheap fixes.
- **`SpanTree` recovers only ~0.2 of the available 2.5×.** The span backend still
  rides the same `GSlot<V>`/`GElem<V>` machinery (a 104-byte `Meta` per element,
  per-reduction child buffers, `SpanNode` allocations), so most of the
  "zero-copy" prize is unclaimed. The floor is the honest target for #242/#243.

## Demonstrated next levers (in value order)

1. **Shrink the stack element** (this branch, E4): `u32` positions in
   `Token`/`Meta` (inputs < 4 GiB) take `Meta` 104 → 56 and `Token` 104 → 80
   bytes, `GSlot<Child>` 264 → ~168 — a pure-memcpy win every engine shares,
   including the floor itself. Public-surface change (field types), so it rides
   the architect's §5 "default Tree gets cheaper" decision. Measured below.
2. **Allocator** (E0): mimalloc −20% wall-clock end-to-end. A `#[global_allocator]`
   choice belongs to the *embedding application*, not the library — but the
   finding says the README/BENCH should document it, PyO3/WASM builds should
   consider shipping it, and every future "×N vs Python" claim should state the
   allocator.
3. **Tape/arena output backend** (#242/#243, now with a number): the floor says a
   backend that skips per-node materialization entirely is worth up to 2.5× on
   JSON-shaped output. The existing event-stream backend (#242) is the natural
   carrier.
4. **Remaining per-token costs** (identified, unconverted): the `unless`
   HashMap probe; the double char-scan per token (`build_token` counts
   line/col/chars, then `advance_by_chars` re-scans the same text — fuse by
   carrying the post-token cursor alongside the cached token); `Tree.data`
   label `to_string()` per node and `Token.type_` clone per token (only fixable
   by interning — `Arc<str>`/id-only — i.e. the §5 decision).

## Correctness

Full suite (`cargo test --release`, all targets incl. compliance banks, corpus,
wild bank) green after each kept experiment. No oracle regenerated, no XFAIL
touched. The spike changes are engine-internal except E4 (public field types) —
E4 is explicitly a *measurement*, not a landing.
