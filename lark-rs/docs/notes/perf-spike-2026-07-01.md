# Perf spike 2026-07-01 — where the next level actually is

- **Status:** Spike findings (measurements + demonstrated prototypes on branch
  `claude/lark-rs-perf-spike-5ew7ag`). Not policy; production landing of each item
  goes through the normal issue → PR flow, and the public-surface pieces (E4) ride
  the architect's open "default `Tree` gets cheaper" decision
  (`performance-strategy.md` §5). Wall-clock numbers are one box (4-core Xeon @
  2.80 GHz, rustc 1.94.1, release+LTO `bench` profile) — **only the ratios travel**
  (BENCH.md discipline).
- **Method:** profile first (callgrind + DHAT on `examples/profile_parse parse`,
  the 92 KB `json_large` workload), then one experiment per suspected cost, each
  measured on `cargo bench --bench parse` and kept only with the full suite green
  (675/675 after every kept step). The before/after on real grammars is a
  same-box `cargo bench --bench wild` against a baseline worktree at `7959ffa`.

## TL;DR

**The spike lands ~1.75× on synthetic LALR workloads and 1.44–1.71× on the wild
bank's real-world LALR grammars, with the full suite green** — five experiments,
all engine-internal except the `u32`-positions one. An allocator swap (mimalloc)
is worth roughly another 1.15–1.25× on top (≈2× combined on JSON). The
instrument that matters going forward is the new **NullBuilder floor**: even
after these wins, ~40–47% of end-to-end parse time is output materialization,
and the existing opt-in `SpanTree` backend recovers only a fraction of it. The
next level after this branch is (a) allocation *count* (token values, labels,
child vecs — interning/arena territory, the §5 decision), and (b) a
floor-chasing output backend (#242/#243).

## Baseline vs result (same box, same session)

`cargo bench --bench parse` medians:

| workload | baseline | after spike (E1–E5) | ratio | + mimalloc¹ |
|---|---:|---:|---:|---:|
| parse json_small | 62.2 µs / 6.3 MB/s | 43.5 µs / 9.0 MB/s | **1.43×** | 31.9 µs / 12.2 MB/s |
| parse json_medium | 1.56 ms / 5.6 MB/s | 0.91 ms / 9.6 MB/s | **1.71×** | 0.76 ms / 11.5 MB/s |
| parse json_large | 17.67 ms / 5.2 MB/s | 10.14 ms / 9.1 MB/s | **1.74×** | 8.93 ms² / 10.3 MB/s |
| parse arith_small | 12.6 µs / 2.6 MB/s | 6.8 µs / 4.9 MB/s | **1.86×** |  |
| parse arith_large | 803 µs / 2.6 MB/s | 414 µs / 5.1 MB/s | **1.94×** |  |
| parse_earley json_large | 124.7 ms | 117.4 ms | 1.06× | — |

¹ mimalloc column measured at the E1–E4 state (`RUSTFLAGS="--cfg spike_mimalloc"`).
² E1–E4 state; E5 landed after, so the stacked number is conservatively ~8.2 ms.

Earley is essentially unchanged (~1.06×): its cost is the chart/forest, not the
token plumbing — an honest negative result that says the next Earley win is a
different spike.

Wild bank (real grammars, corpus rows, baseline worktree → spike, median):

| project | engine | baseline | spike | ratio |
|---|---|---:|---:|---:|
| cel | LALR | 3.02 ms | 1.87 ms | **1.62×** |
| lark_lark | LALR | 6.06 ms | 3.66 ms | **1.66×** |
| mappyfile | LALR | 20.19 ms | 14.01 ms | **1.44×** |
| matter_idl | LALR | 71.7 ms | 44.7 ms | **1.60×** |
| poetry_markers | LALR | 62.4 µs | 36.9 µs | **1.69×** |
| poetry_pep508 | LALR | 63.8 µs | 37.5 µs | **1.70×** |
| pylogics_ltl | LALR | 66.3 µs | 38.7 µs | **1.71×** |
| pyquil | LALR | 2.39 ms | 1.55 ms | **1.54×** |
| tartiflette | LALR | 5.09 ms | 3.35 ms | **1.52×** |
| vyper | LALR | 6.64 ms | 4.14 ms | **1.60×** |
| dotmotif / mistql | Earley | 6.21 / 22.4 ms | 6.16 / 22.2 ms | ~1.0× |

Profile deltas (json_large, 5 parses): instructions 1103 M → **712 M** (−35%);
SipHash 5% → ~0%; heap churn 183 MB → 123 MB per parse (−33%); memcpy share
31.5% → 26.2% of the smaller pie. Allocation **block count is unchanged**
(242.7 K/parse) — the count, not the bytes, is now the frontier.

## What was actually costing (found ≠ guessed)

1. **Every token was materialized three times.** `Contextual::lex_next` builds
   the `Token` (type_ clone + value alloc), `TokenSource::peek` **cloned** it out
   of the cache (the trait returned `Token` by value), and the SHIFT arm
   **cloned** it again into the builder. Six String allocations per token where
   two suffice.
2. **Four per-token SipHash probes** that should be array reads: `names[&id]`
   (`HashMap<SymbolId, String>`) in `build_token`, `state_to_scanner.get(&state)`
   in the contextual lexer, `ignore.contains(&id)` in the skip loop, and
   `unless.get(&id)` in both scanner backends' `match_at`.
3. **Every reduction `drain().collect()`ed the popped slots** into a fresh `Vec`
   before shaping, and grew its `kept` buffer from zero (4→8→16… reallocs).
4. **The value-stack element was 264 bytes.** `Child` = 152 (a `Tree` is inline),
   `Meta` = **104** (six `Option<usize>` at 16 bytes each): every push/pop/
   drain/splice memcpys the lot. This was the single biggest memcpy source.

## The experiments

| id | change | where | json_large effect (cumulative) |
|---|---|---|---:|
| E1 | `TokenSource::peek_type`/`take_current` — dispatch on the cached token's id; SHIFT moves the token, never clones | `token_source.rs`, `lalr.rs` | 17.67 → 16.02 ms |
| E2 | dense name table, dense state→scanner table, `%ignore` bitset | `lexer/mod.rs`, `lexer/dynamic.rs` | → 14.87 ms |
| E3 | `shape_reduction` drains the value-stack tail in place (no per-reduction collect) | `tree_builder.rs`, `lalr.rs` | → 14.81 ms |
| E4 | `u32` positions: `Token` fields + `Meta` fields `usize`→`u32`; stack element 264 → ~168 B | `tree.rs` + mechanical casts | → **11.10 ms** |
| E5 | `kept` pre-sized to rule arity; dense `unless` retype table | `tree_builder.rs`, `dfa.rs`, `scanner.rs`, `plan.rs` | → **10.14 ms** |
| E0 | mimalloc as `#[global_allocator]` (opt-in probe: `--cfg spike_mimalloc`) | `benches/parse.rs` | −20% baseline, −12% on E4 state |

E1/E2/E3/E5 are engine-internal and landable as-is (auto-tier candidates,
counter gates per ADR-0007). **E4 changes public field types**
(`Token.line: u32`, `Meta.line: Option<u32>`, …) — it is the measured argument
for §5, not a unilateral landing. Inputs ≥ 4 GiB would overflow u32 positions;
Python Lark practically cannot parse such inputs either, but the bound must be
documented if E4 lands.

## The ceiling: NullBuilder floor (new instrument, `examples/spike_floor.rs`)

Parse a 146 KB JSON through public `parse_into` with a do-nothing builder
(`Value = ()`): pays lexing + LALR dispatch + all shaping control flow,
materializes nothing.

| path | baseline session | after E4 |
|---|---:|---:|
| `parse()` (owned tree) | 28.45 ms | 18.98 ms |
| `parse_span()` (C8 backend) | 23.32 ms | 15.58 ms |
| `parse_into(NullBuilder)` — floor | 11.51 ms | 10.09 ms |
| materialization share of `parse()` | **59.5%** | **46.9%** |

Two standing conclusions:

- Even post-spike, **~47% of parse time is output materialization** (ADR-0011
  still holds). The remaining materialization cost is allocation *count*:
  per-token `value`/`type_` Strings, per-node label `to_string()`, per-node
  child `Vec` — interning / arena / tape territory (#242/#243 and the §5
  decision), now with a hard number attached.
- **`SpanTree` sits mid-gap, not at the floor.** It still rides the generic
  `GSlot`/`GElem` machinery and allocates `SpanNode`s. The floor is the honest
  target for a "fast output" backend.

## Identified, not yet demonstrated (next spike's shopping list)

- **Double char-scan per token:** `build_token` walks the value to compute
  end line/col + char count; `advance_by_chars` then re-walks the same text to
  move the cursor. Carry the post-token cursor alongside the cached token
  (O(1) advance). Small but universal.
- **`Child::Tree(Box<Tree>)`:** post-E4 the win shrank (`Child` 112 → 88 B,
  but +1 malloc per node) — measure before believing either direction.
- **Interned labels / `Arc<str>` token type names:** kills 2 allocs per
  node/token on the default path; public-surface (§5).
- **Earley:** untouched by this spike's wins; needs its own profile-first pass
  (chart/forest representation, `SmallVec` for item lists, the explicit-walk
  streaming fix BENCH.md already tracks).
- **Allocator guidance:** document mimalloc in BENCH.md/README for embedders;
  consider shipping it in the PyO3/WASM bindings (their users can't choose).

## Correctness

Full suite green after every kept experiment (675 passed / 0 failed, incl.
compliance banks, JSONTestSuite corpus, wild bank, event-stream differential).
No oracle regenerated, no XFAIL touched, no grammar changed. The scanner
`unless` change is covered by the L0 differential's own gates in-suite; the
perf-counter scaling gates and the fancy-oracle differential were run
separately on the final state (see PR/commit notes).
