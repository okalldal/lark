# lark-rs — Performance Baseline (recorded trend, not a gate)

This is the **performance analog of the correctness oracle**: a number for a
representation or algorithm change to move, established against the working LALR
engine *before* Earley (Phase 2) so the second engine has a baseline to be
measured against. It is deliberately **not a CI gate** — wall-clock on shared
runners is too noisy to enforce, and a flaky red perf gate gets muted. The
nightly `.github/workflows/lark-rs-bench.yml` records and uploads the numbers as a
trend; humans read regressions off the trend.

## Performance discipline (profile first — the #54/#55/#56 lesson)

Three rules, learned the hard way: #54 named a culprit (completer / Joop-Leo), #55's
profiler found a different one (the forest→tree walk), and #56 showed *both* halves
of a hypothesis can be wrong at once — its guessed explicit-walk culprit (a clone
loop) turned out linear, while a suspicion it had down-weighted (the completer
rescan) turned out real.

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

## Deterministic scaling counters (the #56 gate)

Wall-clock is a recorded trend; the **gateable** signal is a set of deterministic
work counters in `lark_rs::perf`, compiled in only under the `perf-counters`
feature (zero overhead otherwise — the increments sit in the Earley hot path). They
make a suspected super-linearity reproducible as a *flat-per-byte* (or capped n²)
assertion that a shared runner can actually enforce:

```bash
# Demonstrate: print the counters across a size sweep for each #56 workload.
cargo run --release --features perf-counters --example profile_parse scaling
# Gate: the committed scaling regression net (CI runs this).
cargo test --features perf-counters --test test_earley_scaling
```

`completer_scan_steps` (Arm 1), `explicit_prefix_copies` (Arm 2, the *named* clone
loop — kept as a committed disproof that it is linear), and `explicit_node_children`
(Arm 2, the *real* O(n²) cost). Adding a new suspicion means adding a counter + a
sweep here, never a wall-clock threshold.

## Running it

```bash
cd lark-rs
cargo bench --bench parse           # Rust LALR/Earley internal numbers + scaling
python3 tools/bench_compare.py      # Python Lark on parse.rs's JSON/arith grammars
cargo bench --bench vs_python_lark  # cross-engine JSON/Python/SQL/NL-CYK, prints the speedup
cargo bench --bench lex_backends    # lexer: regex Scanner vs regex-automata DfaScanner (L1)
```

`vs_python_lark` is the **cross-engine end-to-end comparison** (issue #50, the
"10–100×" headline) and is the single command that reports the speedup ratio: it
times lark-rs on four real workloads, then shells out to
`benches/vs_python_lark.py` to time the *byte-identical* inputs through the in-tree
Python Lark and prints `python_median / rust_median` per workload. See
"Cross-engine end-to-end" below. The `parse` bench and `tools/bench_compare.py`
remain the *internal* trend (LALR vs Earley, scaling shapes).

Both print the same columns; compare row-by-row:

```
BENCH<TAB>kind<TAB>name<TAB>bytes<TAB>median_ns<TAB>min_ns<TAB>mb_per_s
```

The `min_ns` column is the least-noise estimator (use it for run-to-run
comparison); `median_ns` drives the MB/s. Speedup on a row is
`python_median / rust_median`.

## The three comparisons (and what each means)

1. **Rust-LALR vs Python-Lark-LALR** — the defensible "faster than Python Lark"
   story. `tools/bench_compare.py` computes it on `parse.rs`'s grammars, and
   `cargo bench --bench vs_python_lark` reports it directly on the JSON/Python/SQL/NL
   workloads (see "Cross-engine end-to-end" below).
2. **Rust-Earley vs Python-Lark-Earley** — the same story for the second engine,
   now wired into `cargo bench --bench vs_python_lark` (JSON + SQL; ~13–16× on the
   reference box, since Python Lark's Earley is much slower in absolute terms).
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
   (≈15×→32×→196× as JSON scaled 0.4K→8.7K→92K on the reference box). #55 fixed the
   **resolve-mode forest→tree walk** (two quadratics: copying the `Inline` child list
   of transparent left-recursive helpers, and deep-cloning each growing left subtree
   on memo), so the resolve-mode ratio on JSON/arith **stops growing with input
   size** — `earley_over_lalr_max` fell 311.8× → 17.9×. A single-K ceiling is still
   not asserted: wall-clock is too noisy to gate. The ratios are printed so the trend
   stays visible.

   **#56 — the residual suspicions, now resolved under the demonstrate-first
   discipline.** Each was taken through a committed, *deterministic* scaling artifact
   (`lark_rs::perf` work counters via `examples/profile_parse.rs scaling`, gated by
   `tests/test_earley_scaling.rs` — never wall-clock). The verdicts:

   - **Completer origin-column rescan — was real, now fixed.** The earlier "linear on
     JSON/arith" reading did *not* generalize: the completer rescanned the *whole*
     origin column with an O(column) `.filter` per completion, which is super-linear
     on a right-recursive grammar (`a: X a | X`) where later columns hold O(n)
     completed items. A per-column `waiting` index (expected-symbol → waiters) makes
     it O(matches); JSON/arith/nested/left-recursion now hold flat per-byte completer
     scan, gated. (So the old "the ratio grows *because* the completer rescans the
     origin column" claim was directionally right about the mechanism but was never
     verified — #56 verified and fixed it.)
   - **Right-recursion — linearized by Joop-Leo (#58).** Even with the index,
     `a: X a | X` stayed O(n²): non-Leo Earley builds O(n²) completed items
     regardless of the rescan (Python Lark still does — its Leo transitives are dead
     code, `create_leo_transitives` commented out; the upstream completer even
     references a nonexistent field, see lark-parser/lark#397). #58 implemented the
     Joop-Leo deterministic-reduction-path optimization with a lazy, reachability-
     bounded SPPF spine reconstruction over a forest-global `(key,start,end)` index.
     The forest drops from O(n²) to O(n) nodes. The gate now proves this **before
     vs after** on three grammars — the canonical `a: X a | X` plus two that people
     hand-write as right recursion and *cannot* express with `+` (a right-associative
     operator `?a: NAME "=" a | NAME` and a separated list `lst: ITEM "," lst | ITEM`,
     since `+` expands to flat *left* recursion): with the Leo toggle off the forest
     is super-linear (≥3× per doubling), with it on it is linear (≤2.3×). Wall-clock
     on the `=` chain (measured 2026-06-05, `--features perf-counters`): **17× @ n=256,
     38× @ n=512, 90× @ n=1024** (671 ms → 7.5 ms), the speedup growing ~linearly in
     n exactly as O(n²)→O(n) predicts. This is where lark-rs is now *faster than the
     Python oracle*. Restricted to strict right recursion (recognized symbol is the
     rule's last); nullable-tail recursion falls back to the regular completer.
   - **`ambiguity='explicit'` walk — guessed cause disproved.** The suspected culprit
     was `expand_packed`'s `l = list.clone(); l.push(rv)` loop. Measured, that loop is
     **linear** (its prefix is bounded by the rule arity). The genuine O(n²) is the
     per-node derivation-value rebuild in `symbol_derivations`: a transparent helper
     materializes Inlines of size 1,2,…,n — exactly the cost #55 streamed away for
     resolve, still present in explicit. Both are gated (loop stays linear; rebuild
     stays within its n² ceiling); the streaming fix is a **tracked follow-up**.

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

## Cross-engine end-to-end: JSON / Python / SQL / NL-CYK (issues #50, #87)

`cargo bench --bench vs_python_lark` is the **cross-engine comparison** — the
number behind the project's "10–100×" goal — over four real workloads. It is the
throughput analog of the oracle: lark-rs and Python Lark parse the **same grammar**
over the **same bytes**, so the ratio is apples-to-apples. JSON / Python / SQL run
on **LALR + the contextual lexer** (Lark's primary USP); JSON and SQL *also* run on
**Earley**, and the NL workload runs on **CYK** — so all three engines have a
cross-engine number (see "Earley arm" and "CYK arm" below).

- **JSON** — the canonical JSON grammar over a ~92 KB array of records (the
  `json_large` shape from `parse.rs`).
- **Python** — the **real upstream `python.lark`** (issue #79), driven by the
  `Indenter`/`PythonIndenter` postlex hook over a generated source file that
  exercises the full language: classes + decorators, `async def`/`await`/`async
  with`/`async for`, list/dict/set comprehensions, the walrus `:=`, f-strings,
  `*args`/`**kwargs`, `lambda`, ternary, `try`/`except`/`finally`, `with`, slices,
  `del`/`assert`/`while`, augmented + annotated assignment, and `import`s. This is
  no longer a curated subset: lark-rs and Python Lark load the *same* in-tree
  `python.lark` (start `file_input`) and parse the byte-identical input. It became
  parseable end-to-end once #98 (EBNF-helper dedup → builds under LALR), #97/#100
  (leading-nullable distribution → parses), and the named-keyword-terminal
  `PatternStr` fix (async/await) landed; the lookaround terminals route to
  `fancy-regex` (#40). One construct stays off the generator — star-params *after*
  a positional in a `def` header (`def f(self, *a)`), which lark-rs's LALR table
  does not yet accept where Python Lark does — so def-site `*args`/`**kwargs` is
  exercised via a no-positional top-level function (call-site unpacking, which both
  accept, is used everywhere else). The bench both *builds* and *parse-checks* the
  workload on each engine before timing, so any divergence fails loudly.

  > **Perf note (2026-06-06): an O(n²) lexer pathology, found and fixed by this
  > swap.** Swapping in the real `python.lark` first exposed a quadratic: the
  > `fancy-regex`-routed lookaround terminals (`STRING`/`LONG_STRING`/`DEC_NUMBER`)
  > were matched per position with `find_from_pos`, an *unanchored forward search*,
  > so trying a sparse terminal like `STRING` at every offset scanned ahead to the
  > next quote — O(n²) over the file (a 124 KB parse took ~177 s; JSON/SQL, which
  > use no lookaround terminals, were unaffected, which is what localized it). The
  > fix anchors the per-position fancy match to the search start with `\G`
  > (`src/lexer.rs`, `Scanner::build`), so the search fails immediately when nothing
  > matches at `pos`. Behaviour-preserving by construction — `match_end_at` already
  > required `m.start() == pos`, so the match set is identical, only the forward
  > scan is gone — and verified green across the full compliance/oracle/stdlib
  > suite. Parsing dropped from ~177 s to ~0.24 s on the 124 KB workload and is now
  > linear per byte. (Follow-up: a committed deterministic lexer-scan-step gate, the
  > analog of the Earley/CYK scaling nets, would pin this so it can't silently
  > regress — the current net is only this wall-clock row.)
- **SQL** — a `SELECT`/`INSERT`/`UPDATE`/`DELETE` grammar (joins, `WHERE`,
  `GROUP BY`, `ORDER BY`, `BETWEEN`/`IN`) over a batch of statements.
- **NL** (CYK) — a small ambiguous natural-language grammar (PP-attachment +
  coordination) over one short sentence. This is the *realistic* CYK/CKY use case:
  unlike JSON/SQL/Python (all LALR-parseable), it genuinely needs a general-CFG
  engine, and parse count grows ~Catalan in the number of prepositional phrases.
  Bounded to ~40 tokens since CYK is O(n³·|grammar|) — the niche/last-resort
  backend, deliberately stressed but kept small.

The grammars live byte-identical in `benches/vs_python_lark.rs` and
`benches/vs_python_lark.py`. The Rust harness generates each input once, writes it
to a temp dir, and passes it to the Python script with `--inputs`, so both engines
time the exact same bytes (no generator-drift risk). Both sides assert the
workload parses before timing, so grammar drift fails loudly rather than silently
measuring an error path.

### Earley arm (JSON + SQL)

JSON and SQL also run under `parser='earley'` — the second engine — so the
"Rust-Earley vs Python-Earley" comparison has a number, not just the
"Rust-Earley vs Rust-LALR" cost-of-generality one in `parse.rs`. The lexer is the
one each workload needs under Earley: **JSON → basic**, **SQL → dynamic** (the
basic lexer can't tell the assignment `=` from the comparison `=` in the SQL
grammar — true in *both* engines, so it's a fair constraint, not a lark-rs gap).

**Python has no Earley row.** Its `Indenter` postlex hook is LALR-only in lark-rs,
and Python Lark itself refuses postlex with the dynamic lexer
(`Can't use postlex with a dynamic lexer`) — so there is simply no apples-to-apples
Earley configuration for a significant-whitespace grammar to compare. Lifting
postlex onto the Earley engine is future work.

### CYK arm (NL) — issue #87

The **NL** workload runs under `parser='cyk'` on both engines, the like-for-like
"Rust-CYK vs Python-CYK" comparison. It is the one workload here whose grammar
genuinely *needs* a general-CFG parser: JSON/SQL/Python are all LALR-parseable, so
running them under CYK would be a contrived choice, whereas an ambiguous
phrase-structure grammar (PP-attachment) is the textbook realistic CYK/CKY use
case. Both engines run the **same** CNF conversion + O(n³) DP (lark-rs's `cyk.rs`
is a faithful port of Python Lark's `cyk.py`), so the ratio is a clean
implementation-vs-implementation number, not an algorithm difference.

The input is a single ambiguous sentence (~40 tokens), not a file — CYK is the
niche/last-resort backend and is `O(n³·|grammar|)`, so the arm is deliberately
small (the bound the issue prescribes). The deterministic *shape* of that cubic
cost is separately gated by `tests/test_cyk_scaling.rs` (#87); this arm is the
throughput trend.

### Reference run

Machine-specific — **only ratios travel**; capture fresh numbers on your own box.

- `Linux x86_64`, Intel Xeon @ 2.80 GHz, 4 cores, `rustc 1.94.1`, release + LTO.
- **Python Lark 1.3.1**, CPython 3.11.15 (the in-tree copy). LALR rows use
  `lexer='contextual'`; Earley rows use `lexer='basic'` (JSON) / `'dynamic'` (SQL).
  Measured 2026-06-06 (the Python row re-measured against the **real upstream
  `python.lark`** — issue #79; see below).

| engine | workload | bytes | Rust MB/s | Python MB/s | speedup |
|--------|----------|------:|----------:|------------:|--------:|
| LALR   | JSON   | ~92 KB  | ~4.3 | ~0.9 | **~4.5×** |
| LALR   | Python | ~122 KB | ~0.5 | ~0.3 | **~1.8×** |
| LALR   | SQL    | ~57 KB  | ~3.2 | ~0.8 | **~4.2×** |
| Earley | JSON   | ~92 KB  | ~0.5 | ~0.1 | **~10.2×** |
| Earley | SQL    | ~57 KB  | ~0.1 | ~0.01 | **~13.5×** |
| CYK    | NL     | ~186 B  | ~0.6 | ~0.02 | **~29.5×** |

**Reading.** Three separate stories:

- **LALR** — lark-rs is ~4–5× faster than Python Lark on JSON/SQL, consistent with
  the internal baseline above (~4–5× on JSON/arith). The **Python** row is lower
  (~1.8×) and is the honest outlier: the real upstream `python.lark` routes its
  `STRING`/`LONG_STRING`/`DEC_NUMBER` terminals through `fancy-regex` (lookaround —
  see the "Key Design Decisions" note in `CLAUDE.md`), which carries a per-token
  constant-factor tax the pure-`regex` JSON/SQL scanners do not, and it is a far
  larger grammar. Still a real win on the *actual* Python grammar (issue #79), not a
  curated subset. The general gap to the "10–100×" headline is the
  deliberately-deferred tree-representation work (`Box<str>`/arena labels, zero-copy
  spans — see the profiling findings below; parse throughput is allocation-bound,
  ~3 allocations per input byte, not algorithm-bound).
- **Earley** — the margin is *larger* (~13–16×), because Python Lark's Earley is
  dramatically slower in absolute terms (multiple seconds per parse here) while
  lark-rs's Earley stays in the tens-to-hundreds of ms. This is the second engine
  paying its cost-of-generality (Earley is much slower than LALR *within* lark-rs
  too — see `parse.rs`), but doing so far more cheaply than the reference
  implementation. SQL's dynamic lexer is the most expensive configuration, which is
  exactly where the gap is widest.
- **CYK** — the widest margin (~29×). Same story as Earley but more pronounced:
  Python Lark's pure-Python CYK DP pays a large constant factor per table cell,
  while lark-rs's port keeps the same O(n³) shape in native code. The absolute
  numbers are tiny (a ~40-token sentence), so read this as "the general-CFG
  fallback is not a Python-speed cliff in lark-rs," not a throughput headline — CYK
  remains the last-resort backend.

This bench turns that remaining LALR headroom into a tracked delta: **re-run it
after each significant engine change and update the table.**

## Lexer backends: regex Scanner vs regex-automata DfaScanner (L1)

`cargo bench --bench lex_backends` times the **lexer in isolation** (`BasicLexer::lex`)
under each of the two combined-scanner engines behind the `ScannerBackend` seam
(`src/lexer.rs`): the original `regex`-crate `Scanner` (combined alternation +
capture groups) and the L1 `DfaScanner` (a `regex-automata` multi-pattern DFA over
the plain terminals, `docs/LEXER_DFA_PLAN.md`). The two are *correctness*-identical —
the L0 differential oracle (`tests/test_scanner_differential.rs`) is the gate — so
this is purely the throughput comparison the plan calls for, isolating the scanner
from parsing. It prints each backend's MB/s and the `dfa / regex` ratio (<1.0 = DFA
faster).

The DFA wins on the all-plain path for two structural reasons: it returns a bare
`PatternID` (no capture-group tracking — the per-token cost the 2026-06-04 profiling
spike below localized), and it searches **anchored at `pos`** (never forward-scans).
The plan's "re-add a literal prefilter so the common path doesn't regress" worry is
addressed by an explicit start-byte prefilter on `DfaScanner` (and the measured
common path *improves*, it does not regress). On a grammar dominated by the
`fancy-regex` lookaround side-probe (`python.lark`'s `STRING`/`LONG_STRING`), both
backends share that probe, so the ratio is ~1.0 — the swap neither helps nor hurts
the part it doesn't touch.

> **Staleness note (2026-06-10).** The "shared probe, ratio ~1.0" premise above (and
> the `python_8k` row's reading below) describes the recorded runs of 2026-06-08/09.
> Since the Stage-B idioms + the flag-wrapper strip landed, the **Dfa** backend lexes
> `python.lark` fully lowered (zero fancy side-probes) while the `Regex` reference
> still pays them — the workload now measures lowered-vs-fancy, and the python ratio
> is expected to move in the Dfa backend's favor on the next recorded run.

### Reference run

Machine-specific — **only ratios travel**; capture fresh numbers on your own box.

- `Linux x86_64`, release + LTO (the `bench` profile). Measured 2026-06-08.

| workload | bytes | regex MB/s | dfa MB/s | dfa/regex |
|----------|------:|-----------:|---------:|----------:|
| json_small  |   390 |  ~8.2 | ~22.5 | **~0.36×** |
| json_medium | ~8.7K |  ~8.7 | ~23.9 | **~0.36×** |
| json_large  |  ~92K |  ~9.7 | ~20.6 | **~0.47×** |
| expr_small  |   385 |  ~8.0 | ~12.1 | **~0.66×** |
| expr_large  |  ~28K |  ~9.6 | ~14.6 | **~0.66×** |
| python_8k   | ~8.0K |  ~0.5 |  ~0.5 | **~0.97×** |

### Default-flip re-run (DFA is now the default)

Re-run on the **flip** that makes `LexerBackend::Dfa` the default
(`LexerBackend::default()` / `LarkOptions.lexer_backend`). Same box family, the
`bench` profile (release + LTO). Measured 2026-06-09 — a shared runner, so the
absolute MB/s sit lower than the 2026-06-08 reference box; **only the ratios
travel**, and they reproduce the picture (DFA decisively faster on the all-plain
common path, a wash on the `fancy-regex`-dominated `python.lark`).

| workload | bytes | regex MB/s | dfa MB/s | dfa/regex |
|----------|------:|-----------:|---------:|----------:|
| json_small  |   390 |  ~6.2 | ~19.3 | **~0.32×** |
| json_medium | ~8.7K |  ~6.7 | ~21.6 | **~0.31×** |
| json_large  |  ~92K |  ~7.1 | ~22.9 | **~0.31×** |
| expr_small  |   385 |  ~6.1 | ~10.1 | **~0.60×** |
| expr_large  |  ~28K |  ~7.2 | ~12.7 | **~0.57×** |
| python_8k   | ~8.0K |  ~0.4 |  ~0.4 | **~0.96×** |

**Reading.** On all-plain grammars the DFA scanner is ~1.7–3.2× faster (JSON ~3.2×,
the identifier/number/operator stream ~1.7×) — the lexer is ~55% of LALR parse time
(profiling spike below), so this is a real end-to-end lever, not a micro-win. On
`python.lark` the shared `fancy-regex` `STRING` probe dominates both backends
(~0.4 MB/s either way), so the plain-engine swap is a wash there (ratio ~0.96×) —
exactly as expected, and the throughput-and-bakeability payoff for *those* terminals
is what later phases (L3 lowering, L5 baking) deliver. Because the swap is
correctness-identical (the L0 differential oracle is 0 divergences over the full
bank + JSON + python/lark corpora) and never slower, **`LexerBackend::Dfa` is now the
default**; `LexerBackend::Regex` remains selectable
(`LarkOptions.lexer_backend = LexerBackend::Regex` / `LexerConf::with_backend`) and
the differential keeps both engines gated against each other.

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

**Internal trend** (`parse`): edit `benches/parse.rs` (Rust) and mirror it in
`tools/bench_compare.py` (Python) so the rows line up. Keep generators
size-parameterized so a workload can scale to expose super-linear behavior. The
Earley workloads (the unambiguous grammars re-run under `parser='earley'`, plus a
pathological ambiguous grammar) light up when the engine lands.

**Cross-engine** (`vs_python_lark`): add the grammar + a size-parameterized
generator to **both** `benches/vs_python_lark.rs` and `benches/vs_python_lark.py`,
keeping the grammar strings byte-identical, then add the workload name to the three
arrays in each `main()`. The Rust harness writes the generated input to a temp file
and the Python script reads it via `--inputs`, so the two engines always time the
same bytes even if the generators ever drift.
