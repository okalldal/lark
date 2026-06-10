//! One-off build-cost attribution for the wild bank: times `Lark::new` for a
//! wild grammar under each lexer backend × lexer type, to localize where the
//! multi-second build cost lives (per-state DFA builds vs LALR table vs loader).
//!
//! Usage: cargo run --release --example wild_build_cost <project> [reps]

use lark_rs::{Lark, LarkOptions, LexerBackend, LexerType, ParserAlgorithm};
use serde_json::Value;
use std::path::PathBuf;
use std::time::Instant;

fn main() {
    let project = std::env::args().nth(1).expect("project name");
    let reps: usize = std::env::args()
        .nth(2)
        .map(|s| s.parse().unwrap())
        .unwrap_or(3);
    let pdir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/wild")
        .join(&project);
    let meta: Value =
        serde_json::from_str(&std::fs::read_to_string(pdir.join("meta.json")).unwrap()).unwrap();
    let grammar =
        std::fs::read_to_string(pdir.join(meta["entry_grammar"].as_str().unwrap())).unwrap();
    let opts = &meta["lark_options"];
    let start = opts["start"].as_str().unwrap().to_string();

    for (backend, bname) in [(LexerBackend::Dfa, "dfa"), (LexerBackend::Regex, "regex")] {
        for (lexer, lname) in [
            (LexerType::Contextual, "contextual"),
            (LexerType::Basic, "basic"),
        ] {
            let mut best = f64::INFINITY;
            for _ in 0..reps {
                let o = LarkOptions {
                    start: vec![start.clone()],
                    parser: ParserAlgorithm::Lalr,
                    lexer: lexer.clone(),
                    maybe_placeholders: opts["maybe_placeholders"].as_bool().unwrap_or(true),
                    propagate_positions: opts["propagate_positions"].as_bool().unwrap_or(false),
                    keep_all_tokens: opts["keep_all_tokens"].as_bool().unwrap_or(false),
                    base_path: Some(pdir.join("grammar")),
                    lexer_backend: backend,
                    ..Default::default()
                };
                let t = Instant::now();
                let r = Lark::new(&grammar, o);
                let dt = t.elapsed().as_secs_f64();
                match r {
                    Ok(_) => best = best.min(dt),
                    Err(e) => {
                        println!("{project} {bname:>5} {lname:<10} BUILD ERROR: {e:?}");
                        best = f64::NAN;
                        break;
                    }
                }
            }
            if !best.is_nan() {
                println!(
                    "{project} {bname:>5} {lname:<10} {:.1} ms (min of {reps})",
                    best * 1e3
                );
            }
        }
    }
}
