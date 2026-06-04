//! Deterministic profiling target for the LALR hot path (profiling spike).
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
//! Args: <mode: build|parse|parse_arith> <iters>

use lark_rs::{Lark, LarkOptions, LexerType, ParserAlgorithm};
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

fn opts() -> LarkOptions {
    LarkOptions {
        start: vec!["start".to_string()],
        parser: ParserAlgorithm::Lalr,
        lexer: LexerType::Contextual,
        ..LarkOptions::default()
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
        other => {
            eprintln!("unknown mode {other:?} (build|parse|parse_arith)");
            std::process::exit(2);
        }
    }
}
