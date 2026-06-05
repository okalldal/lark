//! Deterministic profiling target for the LALR hot path (profiling spike) and the
//! Earley super-linearity demonstrations (#56).
//!
//! Unlike `benches/parse.rs` (adaptive timing loop — noisy under a profiler),
//! this does a fixed, predictable amount of work so callgrind/DHAT get clean
//! attribution. Mirrors the bench's `json_large` / `arith_large` workloads.
//!
//!   cargo build --release --example profile_parse
//!   valgrind --tool=callgrind ./target/release/examples/profile_parse parse 20
//!   valgrind --tool=dhat      ./target/release/examples/profile_parse parse 1
//!   ./target/release/examples/profile_parse build 5   # isolate construction
//!
//! ## #56 scaling demonstrations
//!
//! The `scaling` mode sweeps the two #56 candidate workloads and prints the
//! **deterministic work counters** (`lark_rs::perf`) per size — the committed
//! artifact that *demonstrates* each pathology before any fix, and the noise-free
//! signal the regression test (`tests/test_earley_scaling.rs`) gates on. It needs
//! the counters compiled in:
//!
//!   cargo run --release --features perf-counters --example profile_parse scaling
//!
//! The `earley_rightrec` / `earley_explicit` modes do fixed work on those same
//! grammars for a clean DHAT/callgrind profile.
//!
//! Args: <mode: build|parse|parse_arith|scaling|earley_rightrec|earley_explicit> <iters|n>

use lark_rs::{Ambiguity, Lark, LarkOptions, LexerType, ParserAlgorithm};
use std::hint::black_box;

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

/// `a: X a | X` — right recursion. Non-Leo Earley builds O(n²) completed items
/// here; the Joop-Leo optimization (#58) collapses the forest to O(n) nodes.
const RIGHTREC_GRAMMAR: &str = "start: a\na: X a | X\nX: \"x\"\n";

/// `X+` — a transparent left-recursive helper. Under `ambiguity='explicit'` its
/// per-node derivation-value rebuild is O(n²) (the Arm-2 real cost #56 demonstrates;
/// the `expand_packed` clone loop the issue guessed is, by contrast, linear).
const LIST_GRAMMAR: &str = "start: X+\nX: \"x\"\n";

fn opts() -> LarkOptions {
    LarkOptions {
        start: vec!["start".to_string()],
        parser: ParserAlgorithm::Lalr,
        lexer: LexerType::Contextual,
        ..LarkOptions::default()
    }
}

fn earley_opts(ambiguity: Ambiguity) -> LarkOptions {
    LarkOptions {
        start: vec!["start".to_string()],
        parser: ParserAlgorithm::Earley,
        lexer: LexerType::Basic,
        ambiguity,
        ..LarkOptions::default()
    }
}

/// Print the #56 work counters across a size sweep so each pathology is visible as
/// a per-size table. The deterministic analog of a profiler trace — committed so
/// "demonstrate before fixing" is reproducible without valgrind.
fn scaling() {
    if !lark_rs::perf::ENABLED {
        eprintln!(
            "scaling mode needs the work counters — rebuild with:\n  \
             cargo run --release --features perf-counters --example profile_parse scaling"
        );
        std::process::exit(2);
    }
    use lark_rs::perf;

    println!("# Arm 1 — completer scan steps (waiting-index fix keeps realistic shapes flat)");
    let json = Lark::new(JSON_GRAMMAR, earley_opts(Ambiguity::Resolve)).unwrap();
    for &(rec, fld) in &[(8usize, 3usize), (64, 4), (256, 5), (512, 5)] {
        let input = gen_json(rec, fld);
        perf::reset();
        json.parse(&input).unwrap();
        let s = perf::completer_scan_steps();
        println!(
            "  json   bytes={:>6}  scan={:>9}  scan/byte={:.3}",
            input.len(),
            s,
            s as f64 / input.len() as f64
        );
    }
    println!(
        "# Joop-Leo (#58) — right recursion `a: X a | X`, forest size OFF (O(n²)) vs ON (O(n))"
    );
    let rr = Lark::new(RIGHTREC_GRAMMAR, earley_opts(Ambiguity::Resolve)).unwrap();
    for &n in &[64usize, 128, 256, 512] {
        let input = "x".repeat(n);
        perf::set_leo_disabled(true);
        perf::reset();
        rr.parse(&input).unwrap();
        let off = perf::forest_nodes();
        perf::set_leo_disabled(false);
        perf::reset();
        rr.parse(&input).unwrap();
        let on = perf::forest_nodes();
        println!(
            "  rightrec n={n:>5}  nodes_off={off:>8} ({:.2}/n²)  nodes_on={on:>6} ({:.2}/n)",
            off as f64 / (n * n) as f64,
            on as f64 / n as f64
        );
    }
    println!("# Arm 2 — `X+` explicit: clone loop LINEAR (disproof) vs node rebuild O(n²) (real)");
    let lst = Lark::new(LIST_GRAMMAR, earley_opts(Ambiguity::Explicit)).unwrap();
    for &n in &[64usize, 128, 256, 512, 1024] {
        let input = "x".repeat(n);
        perf::reset();
        lst.parse(&input).unwrap();
        let loop_copies = perf::explicit_prefix_copies();
        let materialized = perf::explicit_node_children();
        println!(
            "  list   n={n:>5}  clone_loop={loop_copies:>6} (={:.2}/n)  \
             node_children={materialized:>9} (={:.3}/n²)",
            loop_copies as f64 / n as f64,
            materialized as f64 / (n * n) as f64
        );
    }
}

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

fn main() {
    let mut args = std::env::args().skip(1);
    let mode = args.next().unwrap_or_else(|| "parse".into());
    let iters: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(20);

    match mode.as_str() {
        "build" => {
            for _ in 0..iters {
                black_box(Lark::new(black_box(JSON_GRAMMAR), opts()).unwrap());
            }
        }
        "parse" => {
            let parser = Lark::new(JSON_GRAMMAR, opts()).unwrap();
            let input = gen_json(512, 5); // ~92 KB, the bench's json_large
            for _ in 0..iters {
                black_box(parser.parse(black_box(&input)).unwrap());
            }
        }
        "parse_arith" => {
            let parser = Lark::new(ARITH_GRAMMAR, opts()).unwrap();
            let input = gen_arith(512);
            for _ in 0..iters {
                black_box(parser.parse(black_box(&input)).unwrap());
            }
        }
        // #56 scaling sweep, printing the deterministic work counters.
        "scaling" => scaling(),
        // Fixed-work Earley targets for a clean DHAT/callgrind profile. `iters` is
        // reused as the input size n (default 256) since the cost is super-linear in n.
        "earley_rightrec" => {
            let n = if iters == 20 { 256 } else { iters };
            let parser = Lark::new(RIGHTREC_GRAMMAR, earley_opts(Ambiguity::Resolve)).unwrap();
            let input = "x".repeat(n);
            black_box(parser.parse(black_box(&input)).unwrap());
        }
        "earley_explicit" => {
            let n = if iters == 20 { 256 } else { iters };
            let parser = Lark::new(LIST_GRAMMAR, earley_opts(Ambiguity::Explicit)).unwrap();
            let input = "x".repeat(n);
            black_box(parser.parse(black_box(&input)).unwrap());
        }
        other => {
            eprintln!(
                "unknown mode {other:?} \
                 (build|parse|parse_arith|scaling|earley_rightrec|earley_explicit)"
            );
            std::process::exit(2);
        }
    }
}
