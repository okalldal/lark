# lark-rs — Performance Baseline (recorded trend, not a gate)

This is the **performance analog of the correctness oracle**: a number for a
representation or algorithm change to move, established against the working LALR
engine *before* Earley (Phase 2) so the second engine has a baseline to be
measured against. It is deliberately **not a CI gate** — wall-clock on shared
runners is too noisy to enforce, and a flaky red perf gate gets muted. The
nightly `.github/workflows/lark-rs-bench.yml` records and uploads the numbers as a
trend; humans read regressions off the trend.

## Performance discipline (profile first — the #54/#55 lesson)

Three rules, learned the hard way: #54 named a culprit (completer / Joop-Leo), and
#55's profiler found a different one (the forest→tree walk).

1. **Demonstrate before fixing.** A suspected super-linearity gets a committed,
   size-parametrized workload that *exhibits* it before any fix is written — the perf
   analog of "every bug reproducible as a test failure first" (`CLAUDE.md`).
   "Couldn't reproduce a pathology" is a valid, documented outcome (it closes the
   suspicion with evidence).
2. **Profile the root cause; don't guess it.** Fix the phase the profiler indicts,
   not the one a hypothesis names, and attach the profile to the change. #54
   attributed the growth to the completer; the cost was in the forest→tree walk.
3. **Regress on a deterministic signal, never wall-clock.** Gate on allocation-block
   counts (DHAT) or an instrumented copy/clone/rebuild counter, asserting *flat
   per-byte scaling* — not absolute time. Wall-clock on shared runners is too noisy
   to gate, and a flaky perf gate gets muted (the reason this whole bench is a
   recorded trend, not a gate).

## Running it

```bash
cd lark-rs
cargo bench --bench parse        # Rust LALR numbers (no benchmarking crate needed)
python3 tools/bench_compare.py   # Python Lark on the same grammars+inputs
```

Both print the same columns; compare row-by-row:

```
BENCH<TAB>kind<TAB>name<TAB>bytes<TAB>median_ns<TAB>min_ns<TAB>mb_per_s
```

The `min_ns` column is the least-noise estimator (use it for run-to-run
comparison); `median_ns` drives the MB/s. Speedup on a row is
`python_median / rust_median`.

## The three comparisons (and what each means)

1. **Rust-LALR vs Python-Lark-LALR** — the defensible "faster than Python Lark"
   story. This is what `tools/bench_compare.py` lets you compute today.
2. **Rust-Earley vs Python-Lark-Earley** (once the engine lands) — the same story
   for the second engine.
3. **Rust-Earley vs Rust-LALR** — the *cost of generality*, not "slowness."
   Earley is O(n³) worst case and solves a strictly harder problem; reading a
   cubic-Earley-on-pathological-input number as a regression against LALR is a
   category error. `cargo bench --bench parse` now wires this up: it re-runs the
   unambiguous workloads under `parser='earley'`, prints the per-row Earley/LALR
   ratio (`parse_earley` rows + a `ratio` line), and adds a reported-only
   pathological-ambiguous workload (`parse_earley_ambig`).

   **Reported, not gated — and the ratio was *not* a constant.** Sprint 2 originally
   meant to assert "...and within K× of LALR" here. Wiring the measurement up
   disproved that premise: the Earley/LALR ratio **grew with input size**
   (≈15×→32×→196× as JSON scaled 0.4K→8.7K→92K on the reference box). That growth was
   first attributed to the completer rescanning the origin column (Joop-Leo
   transitives omitted) — but **profiling did not bear that hypothesis out.** #54/#55
   found chart construction is linear on these workloads (the completer scans a
   constant number of items per completion), and the super-linearity lived entirely
   in the **resolve-mode forest→tree walk**: two quadratics — copying the `Inline`
   child list of transparent left-recursive helpers (`x*`/`x+`/`_rule`), and
   deep-cloning each growing left subtree on memo for left-recursive real rules
   (`expr: expr "+" term`). #55 fixed both (streaming append + lazy memoization), so
   the resolve-mode ratio now **stops growing with input size** —
   `earley_over_lalr_max` fell 311.8× → 17.9×, and the largest cases are now cheaper
   per byte than the smallest. A single-K ceiling is still not asserted: wall-clock
   is too noisy to gate. The completer/Joop-Leo claim is **unverified** (shown linear
   on JSON/arith only, not on adversarial grammar shapes), and that residual
   suspicion — together with the still-quadratic `ambiguity='explicit'` walk,
   untouched by #55 — is tracked in **#56** (profile-first, regression-backed). The
   ratios are printed so the trend stays visible.

## Baseline snapshot

Machine-specific — capture fresh numbers on your own box; only **ratios and
trends** travel. Reference run:

- `Linux x86_64`, 4 cores, `rustc 1.94.1`, in-tree Python Lark, release + LTO.

| workload | bytes | Rust median | Python median | speedup |
|----------|------:|------------:|--------------:|--------:|
| build json        |   462 |  4.41 ms |  12.5 ms | ~2.8× |
| build arithmetic  |   462 |  6.12 ms |  11.9 ms | ~1.9× |
| parse json_small  |  ~390 |  0.11 ms |  0.54 ms | ~4.8× |
| parse json_medium | ~8.7K |  2.31 ms | 10.96 ms | ~4.7× |
| parse json_large  | ~92K  | 26.9 ms  | 118.8 ms | ~4.4× |
| parse arith_small |    33 |  0.02 ms |  0.11 ms | ~5.7× |
| parse arith_large | ~2.1K |  1.21 ms |  6.22 ms | ~5.1× |

**Reading of the baseline.** lark-rs LALR is currently ~4–5× faster than Python
Lark on parsing — real, but short of the project's "10–100×" headline. The gap is
expected headroom: the deliberately-deferred optimizations (see `CLAUDE.md` "defer
deliberately") have not been done, and parse throughput (~3.5 MB/s) is
allocation-bound, not algorithm-bound — now **measured**, not assumed (see below).
This harness is what makes that headroom measurable and turns each future
optimization into a tracked delta.

## Profiling findings (spike, 2026-06-04)

A one-off spike with `valgrind --tool=callgrind` (per-function instruction cost)
and `--tool=dhat` (allocations), on `json_large` (~92 KB), build with debug symbols
and LTO off. Reproduce with the committed `examples/profile_parse.rs`:

```bash
CARGO_PROFILE_RELEASE_DEBUG=true CARGO_PROFILE_RELEASE_LTO=false \
  cargo build --release --example profile_parse
valgrind --tool=callgrind ./target/release/examples/profile_parse parse 10
valgrind --tool=dhat      ./target/release/examples/profile_parse parse 1
```

**Headline: it is allocation-bound, decisively.** One parse of a 92 KB input does
**~301K allocations / 105 MB of churn** (≈3 allocations per input byte, >1000× the
input size). In the instruction profile, ~40% of all instructions are in
`memcpy` + `malloc`/`free`, and another ~10% in SipHash (`hash_one`).

**Where the time goes (inclusive, callgrind):**

| region | share | what |
|--------|------:|------|
| lexing (`Contextual::peek` → `next_token` → `Scanner::match_at`) | **~55%** | dominated by the `regex` engine + capture handling, *not* our logic |
| reduce / tree-building (`reduce` → `TreeBuilder::assemble` → `Tree::new`) | **~32%** | `String` clones, `Tree` label + children `Vec` allocation |

**Two concrete, localized root causes in the lexer** (`src/lexer.rs::match_at`),
both **shared by the future Earley engine** (it lexes through the same
`TokenSource`/`Scanner`) — **both now FIXED (perf sprint, 2026-06-04):**

1. ✅ **Capture group resolved by *name* per token.** `match_at` looped over groups
   calling `caps.name(group)` (string-keyed → SipHash) on every token — the ~2.5M
   `hash_one` calls. Fixed: each terminal's capture-group *index* is resolved once
   at `Scanner::build` (from `re.capture_names()`, robust to inner groups in a
   terminal's own pattern) and read by number in `match_at`.
2. ✅ **A fresh `Captures` allocated per match.** `captures_at` made the regex
   backtracker `malloc` per token. Fixed: a single `CaptureLocations` scratch
   buffer (held in the `Scanner` behind a `RefCell`, since the hot contextual path
   runs through `&self`) is reused across matches via `captures_read_at`.

**Measured result (same box, `examples/profile_parse`).** Allocations per
`json_large` parse fell **300,957 → 271,892 blocks** (DHAT), and the per-token
SipHash group-name lookups are gone entirely. End-to-end this is a **~17–20%
wall-clock speedup** on the contextual LALR path across every parse workload
(e.g. `json_large` 27.8 → 22.9 ms, ~3.3 → 4.0 MB/s; `arith_large` 1.21 → 0.97 ms),
lifting the speedup-vs-Python column accordingly. No public type changed; the full
oracle suite + compliance bank stayed green. The remaining lexer cost is now the
`regex` engine itself, not our capture handling.

**The other ~32% is the shared tree representation** — `Tree::data: String`,
`Token` owned strings, per-node child `Vec`s. This is the "load-bearing
abstraction" change (`Box<str>`/arena labels, zero-copy spans) that `CLAUDE.md`
defers behind the `TreeBuilder` chokepoint until a profiler justifies it. It now
does — but it is the change best made once Earley is a second consumer of that
representation.

**Sequencing implication.** The single cheapest, highest-leverage, lowest-risk win
was the lexer pair (1)+(2): it attacks the larger (~55%) half, is purely local to
`Scanner`, touches no public type, and benefits both engines — so it was safe to do
*before* Earley. ✅ **Landed (perf sprint, 2026-06-04)** — see the measured result
above. The tree-representation half is still deferred until Earley exists to
co-design it (see `PHASE_2_PLAN.md` §10).

## Adding a workload

Edit `benches/parse.rs` (Rust) and mirror it in `tools/bench_compare.py` (Python)
so the rows line up. Keep generators size-parameterized so a workload can scale to
expose super-linear behavior. The Earley workloads (the unambiguous grammars
re-run under `parser='earley'`, plus a pathological ambiguous grammar) are stubbed
in both files and light up when the engine lands.
