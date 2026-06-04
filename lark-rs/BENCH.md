# lark-rs — Performance Baseline (recorded trend, not a gate)

This is the **performance analog of the correctness oracle**: a number for a
representation or algorithm change to move, established against the working LALR
engine *before* Earley (Phase 2) so the second engine has a baseline to be
measured against. It is deliberately **not a CI gate** — wall-clock on shared
runners is too noisy to enforce, and a flaky red perf gate gets muted. The
nightly `.github/workflows/lark-rs-bench.yml` records and uploads the numbers as a
trend; humans read regressions off the trend.

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
   category error. Sprint 2's exit criterion ("Earley produces trees identical to
   LALR on unambiguous grammars") is the place to also assert "...and within K× the
   speed" — pick K off these numbers.

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
expected headroom: the deliberately-deferred LALR optimizations (the quadratic
`lr1_closure` map-snapshot, FIRST/FOLLOW bitsets, zero-copy token spans — see
`CLAUDE.md` "defer deliberately") have not been done, and parse throughput
(~3.5 MB/s) is allocation-bound, not algorithm-bound. This harness is what makes
that headroom measurable and turns each future optimization into a tracked delta.

## Adding a workload

Edit `benches/parse.rs` (Rust) and mirror it in `tools/bench_compare.py` (Python)
so the rows line up. Keep generators size-parameterized so a workload can scale to
expose super-linear behavior. The Earley workloads (the unambiguous grammars
re-run under `parser='earley'`, plus a pathological ambiguous grammar) are stubbed
in both files and light up when the engine lands.
