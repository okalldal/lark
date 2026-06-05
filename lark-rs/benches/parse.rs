//! Parse/build micro-benchmarks for the LALR engine — the perf baseline Earley
//! (Phase 2) is measured against.
//!
//! **Recorded trend, not a gate.** This is the performance analog of the
//! correctness oracle: it exists so a representation or algorithm change has a
//! *number* to move, not a red/green CI check (wall-clock on shared runners is
//! too noisy to gate on — see `.github/workflows/lark-rs-bench.yml`). It uses no
//! benchmarking crate on purpose: a self-contained `std::time` loop keeps the
//! harness dependency-free and fully under our control, which is all a recorded
//! trend needs.
//!
//! Run with `cargo bench --bench parse` (the `bench` profile inherits release
//! optimizations). Each workload prints a stable, greppable `BENCH<TAB>…` line
//! plus a human table. Compare against Python Lark with
//! `python3 tools/bench_compare.py` (the 10–100× story).
//!
//! Wired as `harness = false` in Cargo.toml, so `main()` runs directly.

use lark_rs::{Lark, LarkOptions, LexerType, ParserAlgorithm};
use std::hint::black_box;
use std::time::{Duration, Instant};

const JSON_GRAMMAR: &str = r#"
    ?start: value
    ?value: object
          | array
          | string
          | SIGNED_NUMBER  -> number
          | "true"         -> true
          | "false"        -> false
          | "null"         -> null
    array  : "[" [value ("," value)*] "]"
    object : "{" [pair ("," pair)*] "}"
    pair   : string ":" value
    string : ESCAPED_STRING
    %import common.ESCAPED_STRING
    %import common.SIGNED_NUMBER
    %import common.WS
    %ignore WS
"#;

const ARITH_GRAMMAR: &str = r#"
    ?start : expr
    ?expr  : expr "+" term  -> add
           | expr "-" term  -> sub
           | term
    ?term  : term "*" factor -> mul
           | term "/" factor -> div
           | factor
    ?factor : "+" factor    -> pos
            | "-" factor    -> neg
            | atom
    ?atom  : NUMBER
           | NAME
           | "(" expr ")"
    %import common.NUMBER
    %import common.CNAME -> NAME
    %import common.WS_INLINE
    %ignore WS_INLINE
"#;

fn lalr_options() -> LarkOptions {
    LarkOptions {
        start: vec!["start".to_string()],
        parser: ParserAlgorithm::Lalr,
        lexer: LexerType::Contextual,
        ..LarkOptions::default()
    }
}

/// Earley with the basic lexer — the engine path the unambiguous cost-of-generality
/// ratio (P2-1) is measured on. `ambiguity` stays at its `Resolve` default so the
/// trees are identical to LALR's and the comparison is apples-to-apples.
fn earley_options() -> LarkOptions {
    LarkOptions {
        start: vec!["start".to_string()],
        parser: ParserAlgorithm::Earley,
        lexer: LexerType::Basic,
        ..LarkOptions::default()
    }
}

fn build(grammar: &str) -> Lark {
    Lark::new(grammar, lalr_options()).expect("benchmark grammar must build")
}

fn build_earley(grammar: &str) -> Lark {
    Lark::new(grammar, earley_options()).expect("benchmark grammar must build (earley)")
}

/// A JSON array of `records` flat objects, each with `fields` key/value pairs —
/// scales linearly in size and exercises object/array/string/number rules.
fn gen_json(records: usize, fields: usize) -> String {
    let mut s = String::from("[");
    for r in 0..records {
        if r > 0 {
            s.push(',');
        }
        s.push('{');
        for f in 0..fields {
            if f > 0 {
                s.push(',');
            }
            s.push_str(&format!(
                "\"key{f}\": {}, \"name{f}\": \"value{r}_{f}\"",
                r * 10 + f
            ));
        }
        s.push('}');
    }
    s.push(']');
    s
}

/// A left-deep arithmetic expression with `terms` operands: `1 + 2 * 3 - 4 + …`.
fn gen_arith(terms: usize) -> String {
    let ops = ["+", "*", "-", "/"];
    let mut s = String::from("1");
    for i in 0..terms {
        s.push(' ');
        s.push_str(ops[i % ops.len()]);
        s.push(' ');
        s.push_str(&(i % 9 + 2).to_string());
    }
    s
}

/// Timing result for one workload, in nanoseconds per iteration.
struct Stat {
    min_ns: f64,
    median_ns: f64,
}

/// Time `f` with min/median over samples. Calibrates the inner iteration count so
/// one batch clears the timer resolution, then takes the min (least-noise
/// estimator) and median across samples, capped at ~1.5 s wall time per workload.
fn measure<F: FnMut()>(mut f: F) -> Stat {
    // Warm up and calibrate: grow `iters` until a batch takes >= 1 ms.
    let mut iters = 1usize;
    loop {
        let t = Instant::now();
        for _ in 0..iters {
            f();
        }
        let elapsed = t.elapsed();
        if elapsed >= Duration::from_millis(1) || iters >= 1 << 22 {
            break;
        }
        iters = (iters * 2).max(1);
    }

    let mut samples: Vec<f64> = Vec::new();
    let overall = Instant::now();
    while samples.len() < 50 && overall.elapsed() < Duration::from_millis(1500) {
        let t = Instant::now();
        for _ in 0..iters {
            f();
        }
        samples.push(t.elapsed().as_nanos() as f64 / iters as f64);
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    Stat {
        min_ns: samples[0],
        median_ns: samples[samples.len() / 2],
    }
}

/// Time one parse workload, emit the `BENCH<TAB>…` trend line + the human row, and
/// return the median ns/iter (so callers can compute the Earley/LALR ratio).
fn run_parse(kind: &str, name: &str, parser: &Lark, input: &str) -> f64 {
    let bytes = input.len();
    let stat = measure(|| {
        black_box(
            parser
                .parse(black_box(input))
                .expect("benchmark input must parse"),
        );
    });
    // bytes/ns * 1e9 = bytes/s, /1e6 = MB/s  ==>  bytes/ns * 1e3 = MB/s
    let mb_per_s = bytes as f64 / stat.median_ns * 1e3;
    println!(
        "BENCH\t{kind}\t{name}\t{bytes}\t{:.0}\t{:.0}\t{mb_per_s:.1}",
        stat.median_ns, stat.min_ns
    );
    println!(
        "  {kind:<6} {name:<16} {bytes:>8} B   {:>10.0} ns/iter (min {:>10.0})   {mb_per_s:>7.1} MB/s",
        stat.median_ns, stat.min_ns
    );
    stat.median_ns
}

fn run_build(name: &str, grammar: &str) {
    let stat = measure(|| {
        black_box(build(black_box(grammar)));
    });
    println!(
        "BENCH\tbuild\t{name}\t{}\t{:.0}\t{:.0}\t0",
        grammar.len(),
        stat.median_ns,
        stat.min_ns
    );
    println!(
        "  build  {name:<16} {:>8} B   {:>10.0} ns/iter (min {:>10.0})",
        grammar.len(),
        stat.median_ns,
        stat.min_ns
    );
}

fn main() {
    println!("# lark-rs parse benchmarks (LALR + contextual lexer)");
    println!("# columns: BENCH<TAB>kind<TAB>name<TAB>bytes<TAB>median_ns<TAB>min_ns<TAB>mb_per_s");
    println!();

    // --- Grammar construction (LALR table build) -----------------------------
    // The deferred LALR optimizations (quadratic lr1_closure snapshotting) would
    // show up here first.
    println!("Construction (Lark::new):");
    run_build("json", JSON_GRAMMAR);
    run_build("arithmetic", ARITH_GRAMMAR);
    println!();

    // --- Parsing throughput (LALR) -------------------------------------------
    // Build the unambiguous workloads once; keep each input + its LALR median so
    // the Earley section below can compute the cost-of-generality ratio per row.
    println!("Parsing — LALR (build once, parse many):");
    let json = build(JSON_GRAMMAR);
    let arith = build(ARITH_GRAMMAR);
    let mut workloads: Vec<(&str, String)> = Vec::new();
    for (name, records, fields) in [
        ("json_small", 4, 3),
        ("json_medium", 64, 4),
        ("json_large", 512, 5),
    ] {
        workloads.push((name, gen_json(records, fields)));
    }
    for (name, terms) in [("arith_small", 8), ("arith_large", 512)] {
        workloads.push((name, gen_arith(terms)));
    }

    let mut lalr_median: std::collections::HashMap<&str, f64> = std::collections::HashMap::new();
    for (name, input) in &workloads {
        let parser = if name.starts_with("json") {
            &json
        } else {
            &arith
        };
        lalr_median.insert(name, run_parse("parse", name, parser, input));
    }

    // --- Cost of generality: Earley vs LALR on the SAME unambiguous input -----
    // Earley solves a strictly harder problem (O(n^3) worst case), so it is
    // expected to be slower than LALR. We re-run every workload above under
    // parser='earley' and print the per-row Earley/LALR ratio.
    //
    // REPORTED, NOT GATED. P2-1 originally proposed asserting a single constant K×
    // ceiling here ("Earley within K× of LALR on unambiguous input"). Wiring the
    // measurement up disproved that premise: the ratio *grew* with input size
    // (≈15×→35×→193× as JSON scaled 0.4K→8.7K→92K). The growth was first guessed to
    // be the completer rescanning the origin column (Joop-Leo omitted) — but that is
    // NOT what profiling found (#54/#55): chart construction is linear on these
    // workloads (the completer scans a constant ~5 items/completion), and the
    // super-linearity lived entirely in the resolve-mode forest→tree walk — two
    // quadratics, copying the `Inline` child list of transparent left-recursive
    // helpers (`x*`/`x+`/`_rule`) and deep-cloning each growing left subtree on memo
    // (`expr: expr "+" term`). #55 fixed both (streaming append + lazy memoization),
    // so the resolve-mode ratio now *stops growing* with input size (the large cases
    // are cheaper per byte than the small ones). A constant-K ceiling is still not
    // asserted: wall-clock is too noisy to gate (see BENCH.md). The completer/Joop-Leo
    // claim is unverified (shown linear on JSON/arith only, not adversarial shapes),
    // and that residual suspicion — plus the still-quadratic ambiguity='explicit'
    // walk — is tracked in #56. We print the ratios so the trend stays visible.
    println!();
    println!("Parsing — Earley (basic lexer), cost-of-generality vs LALR (reported, NOT gated):");
    let json_e = build_earley(JSON_GRAMMAR);
    let arith_e = build_earley(ARITH_GRAMMAR);
    let mut worst_ratio = 0.0f64;
    let mut worst_name = "";
    for (name, input) in &workloads {
        let parser = if name.starts_with("json") {
            &json_e
        } else {
            &arith_e
        };
        let earley_ns = run_parse("parse_earley", name, parser, input);
        let ratio = earley_ns / lalr_median[name];
        println!("  ratio  {name:<16} earley/lalr = {ratio:>6.1}x");
        if ratio > worst_ratio {
            worst_ratio = ratio;
            worst_name = name;
        }
    }
    println!("BENCH\tratio\tearley_over_lalr_max\t0\t{worst_ratio:.2}\t0\t0   ({worst_name})");

    // --- Pathological ambiguous workload (REPORTED, never gated) --------------
    // S -> S S | "b" has a Catalan number of parses for n b's; the SPPF stays
    // cubic but the work grows fast. This is the cost-of-generality story, not a
    // regression — reading a cubic-Earley-on-ambiguous-input number as "slow" is
    // a category error (BENCH.md §3). Reported so the O(n^3) growth is visible.
    println!();
    println!("Parsing — Earley pathological ambiguity (reported, NOT gated):");
    let ambig = build_earley("start: a\na: a a | \"b\"\n");
    for n in [4usize, 8, 12, 16] {
        let input = "b".repeat(n);
        run_parse("parse_earley_ambig", &format!("ambig_{n}"), &ambig, &input);
    }
}
