//! Lexing throughput: the `regex`-crate `Scanner` vs the `regex-automata`
//! `DfaScanner` (`docs/LEXER_DFA_PLAN.md`, phase L1).
//!
//! **Recorded trend, not a gate** — like `benches/parse.rs`, a self-contained
//! `std::time` loop (no benchmarking crate), so a representation change has a number
//! to move. Both backends are *correctness*-identical (the L0 differential oracle,
//! `tests/test_scanner_differential.rs`, is the gate); this is purely the speed
//! comparison the plan calls for, so the engine swap can't silently regress the
//! common path.
//!
//! It times the **lexer in isolation** (`BasicLexer::lex` under each backend), not a
//! full parse, so the number is the scanner's and nothing else. Run with
//! `cargo bench --bench lex_backends`. Each workload prints a greppable `BENCH<TAB>…`
//! line plus a human row, and a `ratio` line (`dfa / regex`; <1.0 means the DFA is
//! faster).
//!
//! Wired as `harness = false` in Cargo.toml, so `main()` runs directly.

use std::hint::black_box;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use lark_rs::{basic_lexer_conf, load_grammar, lower, BasicLexer, Lexer, LexerBackend};

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

// An identifier/number/operator-dense grammar: all-plain terminals, the path where
// the combined-engine swap is the whole story (no fancy-regex side-probe to share).
const EXPR_GRAMMAR: &str = r#"
    start: (NAME | NUMBER | OP | WS)+
    NAME: /[a-zA-Z_][a-zA-Z0-9_]*/
    NUMBER: /[0-9]+(\.[0-9]+)?/
    OP: /[-+*\/%=<>!&|^~.,;:()\[\]{}]/
    WS: /[ \t\n]+/
"#;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn build_lexer(grammar: &str, start: &str, backend: LexerBackend) -> BasicLexer {
    let g = load_grammar(grammar, &[start.to_string()], true, false)
        .expect("benchmark grammar must load");
    let cg = lower(&g);
    let conf = basic_lexer_conf(&cg, 0).with_backend(backend);
    BasicLexer::new(&conf).expect("benchmark lexer must build")
}

/// A JSON array of `records` flat objects (the `parse.rs` `gen_json` shape).
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

/// A dense stream of identifiers, numbers, and operators for `EXPR_GRAMMAR`.
fn gen_expr(terms: usize) -> String {
    let mut s = String::new();
    for i in 0..terms {
        if i > 0 {
            s.push_str(" + ");
        }
        s.push_str(&format!("var_{i} * {} - func{i}(x)", i % 97 + 1));
    }
    s.push('\n');
    s
}

struct Stat {
    min_ns: f64,
    median_ns: f64,
}

/// Min/median ns-per-iteration over samples (copied from `parse.rs`'s `measure`).
fn measure<F: FnMut()>(mut f: F) -> Stat {
    let mut iters = 1usize;
    loop {
        let t = Instant::now();
        for _ in 0..iters {
            f();
        }
        if t.elapsed() >= Duration::from_millis(1) || iters >= 1 << 22 {
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

fn time_lex(backend_tag: &str, name: &str, lexer: &BasicLexer, input: &str) -> f64 {
    let bytes = input.len();
    let stat = measure(|| {
        black_box(
            lexer
                .lex(black_box(input))
                .expect("benchmark input must lex"),
        );
    });
    let mb_per_s = bytes as f64 / stat.median_ns * 1e3;
    println!(
        "BENCH\tlex_{backend_tag}\t{name}\t{bytes}\t{:.0}\t{:.0}\t{mb_per_s:.1}",
        stat.median_ns, stat.min_ns
    );
    println!(
        "  {backend_tag:<5} {name:<14} {bytes:>8} B   {:>10.0} ns/iter (min {:>10.0})   {mb_per_s:>7.1} MB/s",
        stat.median_ns, stat.min_ns
    );
    stat.median_ns
}

/// Lex `input` under both backends of the same grammar; print both rows + the
/// `dfa / regex` ratio.
fn compare(name: &str, regex: &BasicLexer, dfa: &BasicLexer, input: &str) {
    // Sanity: the two backends must agree on this input (the oracle is the real
    // gate, but a benchmark that times divergent output is meaningless).
    assert_eq!(
        regex.lex(input).is_ok(),
        dfa.lex(input).is_ok(),
        "backends disagree on {name}"
    );
    let r = time_lex("regex", name, regex, input);
    let d = time_lex("dfa", name, dfa, input);
    let ratio = d / r;
    let faster = if ratio < 1.0 {
        "dfa faster"
    } else {
        "regex faster"
    };
    println!("  ratio {name:<14} dfa/regex = {ratio:>5.2}x   ({faster})");
    println!("BENCH\tlex_ratio\t{name}\t0\t{ratio:.3}\t0\t0");
    println!();
}

fn main() {
    println!("# lark-rs lexing throughput — regex Scanner vs regex-automata DfaScanner");
    println!("# columns: BENCH<TAB>kind<TAB>name<TAB>bytes<TAB>median_ns<TAB>min_ns<TAB>mb_per_s");
    println!();

    // --- JSON: all-plain terminals, the common pure-`regex` path ----------------
    println!("JSON (all plain terminals — the common path):");
    let json_re = build_lexer(JSON_GRAMMAR, "start", LexerBackend::Regex);
    let json_dfa = build_lexer(JSON_GRAMMAR, "start", LexerBackend::Dfa);
    for (name, records, fields) in [
        ("json_small", 4, 3),
        ("json_medium", 64, 4),
        ("json_large", 512, 5),
    ] {
        compare(name, &json_re, &json_dfa, &gen_json(records, fields));
    }

    // --- Expr: identifier/number/operator-dense, also all-plain -----------------
    println!("Expr (identifier/number/operator stream — all plain terminals):");
    let expr_re = build_lexer(EXPR_GRAMMAR, "start", LexerBackend::Regex);
    let expr_dfa = build_lexer(EXPR_GRAMMAR, "start", LexerBackend::Dfa);
    for (name, terms) in [("expr_small", 16), ("expr_large", 1024)] {
        compare(name, &expr_re, &expr_dfa, &gen_expr(terms));
    }

    // --- Python: mixed plain + lookaround terminals ------------------------------
    // Historically STRING/LONG_STRING/DEC_NUMBER routed to fancy-regex in BOTH
    // backends (a shared side-probe; the recorded ratio was ~1.0 here). The Dfa side
    // lexes python.lark fully LOWERED; under this bench's required `fancy-oracle`
    // feature the Regex reference still pays the historical fancy probes — so this
    // workload measures lowered-vs-fancy (BENCH.md).
    println!("Python (python.lark — mixed plain + lookaround terminals):");
    let py_grammar = std::fs::read_to_string(manifest_dir().join("src/grammars/python.lark"));
    match py_grammar {
        Ok(grammar) => {
            let py_re = build_lexer(&grammar, "file_input", LexerBackend::Regex);
            let py_dfa = build_lexer(&grammar, "file_input", LexerBackend::Dfa);
            // A capped real Python source (the Regex reference side still pays the
            // fancy STRING probe, so keep it modest).
            let src = std::fs::read_to_string(manifest_dir().join("tools/generate_oracles.py"))
                .unwrap_or_default();
            let cut = src[..src.len().min(8_000)]
                .rfind('\n')
                .map(|i| i + 1)
                .unwrap_or(0);
            compare("python_8k", &py_re, &py_dfa, &src[..cut]);
        }
        Err(_) => println!("  (python.lark not found — skipped)"),
    }
}
