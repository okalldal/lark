//! Cost characterization of the lookaround terminals routed to `fancy-regex`
//! (issue #40). A recorded trend printed to stdout — NOT a CI gate (see BENCH.md).
//!
//! Run with:  cargo bench --bench redos
//!
//! The bundled grammars are shipped *verbatim* from Python Lark; their lookaround
//! terminals (which the linear `regex` crate cannot compile) are routed to
//! `fancy-regex`. The concern this bench was written to investigate: does that lose
//! `regex`'s linear-time / ReDoS-safety guarantee?
//!
//! Measured answer (fancy-regex 0.18): **no catastrophic blowup on the shipped
//! terminals.** `fancy-regex` splits a pattern at its lookaround boundaries and runs
//! the lookaround-*free* portions on the linear NFA engine, only backtracking around
//! the assertions themselves. The terminals here have just a fixed leading
//! assertion (`(?<!\\)`, `(?!/)`, `(?![1-9])`), so their ambiguous bodies stay on
//! the linear engine. The two terminals measured:
//!
//!   * `python.STRING` — `(?<!\\)(\\\\)*?` escaped-quote guard. Linear in input
//!     length; the only cost is a constant factor over the pure-`regex` engine.
//!
//!   * `lark.REGEXP` — body `(\\/ | \\\\ | [^/])*?` is an ambiguous alternation
//!     (the classic ReDoS shape) under a lazy star plus a `(?!/)` lookahead. Because
//!     the alternation carries no lookaround, `fancy-regex` runs it on the linear
//!     engine even on an unterminated literal with a long backslash run — it stays
//!     linear, NOT exponential. (An earlier hand-extracted spike that ran the body
//!     under a pure backtracker saw blowup; the integrated engine does not.)
//!
//! Conclusion: the lookaround terminals are safe to ship as-is. Rewriting them back
//! onto the pure-`regex` engine (as `common.lark`'s `ESCAPED_STRING` already is)
//! would only shave the constant factor — a perf nicety, not a safety fix.

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
