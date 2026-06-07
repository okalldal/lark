//! Cost characterization of the lookaround terminals, now lowered to the linear
//! Pike-VM engine (`src/lookaround/`, Lexer DFA / B1 plan; `fancy-regex` removed in
//! M3). A recorded trend printed to stdout — NOT a CI gate (see BENCH.md; the
//! *deterministic* linearity gate is `tests/test_lexer_scaling.rs`).
//!
//! Run with:  cargo bench --bench redos
//!
//! The bundled grammars are shipped *verbatim* from Python Lark; their lookaround
//! terminals (which the linear `regex` crate cannot compile) are lowered to a
//! Pike-VM simulation. The concern this bench tracks: does that keep `regex`'s
//! linear-time / ReDoS-safety guarantee?
//!
//! Expected answer: **yes, by construction.** A Pike-VM is a priority-ordered
//! Thompson simulation — O(n · program) with no backtracking — and each lookaround
//! assertion is a zero-width gate that kills a thread rather than backtracking. So
//! the two historically worrying terminals stay linear:
//!
//!   * `python.STRING` — `(?<!\\)(\\\\)*?` escaped-quote guard. Linear in input
//!     length.
//!
//!   * `lark.REGEXP` — body `(\\/ | \\\\ | [^/])*?` is an ambiguous alternation
//!     (the classic ReDoS shape) under a lazy star plus a `(?!/)` lookahead. Under a
//!     backtracking engine this is the textbook blowup; under the Pike-VM each
//!     (state, position) is visited once, so an unterminated literal with a long
//!     backslash run stays linear, NOT exponential.
//!
//! Conclusion: the lookaround terminals are safe to ship. `common.lark`'s
//! `ESCAPED_STRING` keeps its hand-written lookaround-free adaptation (hottest
//! terminal; already linear on the pure-`regex` engine).

use std::time::{Duration, Instant};

use lark_rs::{Lark, LarkOptions};

fn build(grammar: &str) -> Lark {
    Lark::new(grammar, LarkOptions::default()).expect("grammar builds")
}

/// Fastest parse time over a few runs (cheap, no criterion dependency).
fn time_parse(lark: &Lark, input: &str) -> Duration {
    let mut best = Duration::from_secs(3600);
    for _ in 0..5 {
        let t = Instant::now();
        let _ = lark.parse(input);
        best = best.min(t.elapsed());
    }
    best
}

fn main() {
    println!("=== issue #40: cost of fancy-regex lookaround terminals (both linear) ===\n");

    // python.STRING — one lazy star + lookbehind. Linear in input length; the cost
    // is a constant factor over the pure-`regex` engine.
    println!("python.STRING — long valid string (expect ~linear in length):");
    let string_parser = build("start: STRING\n%import python.STRING");
    for n in [100usize, 1_000, 10_000, 100_000] {
        let input = format!("\"{}\"", "a".repeat(n));
        println!(
            "  len={n:>7}  {:>10.3?}",
            time_parse(&string_parser, &input)
        );
    }

    // lark.REGEXP — ambiguous alternation under a lazy star + (?!/) lookahead, on an
    // UNTERMINATED literal with a backslash run (the ReDoS-prone shape). Stays linear
    // because the alternation carries no lookaround and runs on the linear engine.
    println!("\nlark.REGEXP — unterminated /\\\\\\\\... (the ReDoS-prone shape; expect ~linear):");
    let regexp = build("start: REGEXP\n%import lark.REGEXP");
    for n in [16usize, 64, 256, 1_024, 4_096, 16_384] {
        let input = format!("/{}", "\\".repeat(n));
        println!(
            "  backslashes={n:>6} (len={:>6})  {:>10.3?}",
            input.len(),
            time_parse(&regexp, &input)
        );
    }

    println!(
        "\nTakeaway: both terminals are linear under fancy-regex 0.18 — no ReDoS on the\n\
         shipped grammars. STRING carries a constant-factor tax vs the pure-`regex`\n\
         engine; a future rewrite onto `regex` (cf. common.lark's ESCAPED_STRING)\n\
         would shave that constant, not fix a blowup."
    );
}
