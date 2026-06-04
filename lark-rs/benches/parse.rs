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

fn build(grammar: &str) -> Lark {
    Lark::new(grammar, lalr_options()).expect("benchmark grammar must build")
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

fn run_parse(name: &str, parser: &Lark, input: &str) {
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
        "BENCH\tparse\t{name}\t{bytes}\t{:.0}\t{:.0}\t{mb_per_s:.1}",
        stat.median_ns, stat.min_ns
    );
    println!(
        "  parse  {name:<16} {bytes:>8} B   {:>10.0} ns/iter (min {:>10.0})   {mb_per_s:>7.1} MB/s",
        stat.median_ns, stat.min_ns
    );
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

    // --- Parsing throughput --------------------------------------------------
    println!("Parsing (build once, parse many):");
    let json = build(JSON_GRAMMAR);
    for (name, records, fields) in [
        ("json_small", 4, 3),
        ("json_medium", 64, 4),
        ("json_large", 512, 5),
    ] {
        let input = gen_json(records, fields);
        run_parse(name, &json, &input);
    }

    let arith = build(ARITH_GRAMMAR);
    for (name, terms) in [("arith_small", 8), ("arith_large", 512)] {
        let input = gen_arith(terms);
        run_parse(name, &arith, &input);
    }

    println!();
    println!("# Earley/SPPF workloads (Phase 2) land here once the engine exists:");
    println!("#   - the unambiguous grammars above re-run under parser='earley'");
    println!("#     (cost-of-generality vs LALR, must stay within K x),");
    println!("#   - plus a pathological ambiguous grammar to expose O(n^3) growth.");
}
