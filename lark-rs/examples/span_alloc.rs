//! Allocation demo: does the C8.1 span-lexer path actually cut heap allocations?
//!
//! The `SpanTree` epic (#225) claims a zero-copy output path: `parse_span` builds
//! no owned `Tree`/`Token` graph and — since C8.1 (#582) — allocates no owned
//! `Token.value: String` in the lexer either. The unit gates prove that as a
//! *deterministic counter* result (`tests/test_span_tree.rs`: `tree_nodes_built ==
//! 0`, `token_value_string_bytes == 0`, `lexer_token_value_bytes == 0`). This
//! example answers the follow-on question those counters don't: does the counter
//! win show up as *real* heap allocations, and how much wall-clock does it buy?
//!
//! It wraps the system allocator in a counting shim and parses one large JSON input
//! both ways, reporting real allocations (count + bytes), the deterministic perf
//! counters (the gated claim), and a wall-clock trend (noisy — ADR-0007 keeps
//! wall-clock a trend, never a gate; read the allocation counts).
//!
//! ```text
//! cargo run --release --features "span-tree,perf-counters" --example span_alloc
//! ```
//!
//! Reference figures (594 KB JSON, dev box): `parse()` ≈ 2.2 allocs/byte,
//! `parse_span()` ≈ 1.3 allocs/byte — the span path removes ~41% of allocations
//! (the per-token value String + the whole owned tree). Alloc *bytes* barely move:
//! the removed allocations are many but tiny; the byte volume is dominated by
//! working buffers both paths share (parser stacks, the token `Vec`, the not-yet-
//! reused per-node child `Vec` #583 tracks). The remaining allocation *volume* is
//! what the arena/`Tape` follow-ups (#242/#243) target.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering};

/// A pass-through allocator that counts allocations (and grow-reallocs) so a parse's
/// real heap traffic is measurable. Deterministic for a single-threaded parse. Only
/// this example binary installs it — a `#[global_allocator]` in an example does not
/// affect the library, tests, or other binaries.
struct Counting;

static ALLOC_COUNT: AtomicU64 = AtomicU64::new(0);
static ALLOC_BYTES: AtomicU64 = AtomicU64::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOC_COUNT.fetch_add(1, Ordering::Relaxed);
        ALLOC_BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        System.alloc(layout)
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout)
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // A grow-realloc is a fresh allocation of the delta.
        if new_size > layout.size() {
            ALLOC_COUNT.fetch_add(1, Ordering::Relaxed);
            ALLOC_BYTES.fetch_add((new_size - layout.size()) as u64, Ordering::Relaxed);
        }
        System.realloc(ptr, layout, new_size)
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

#[cfg(all(feature = "span-tree", feature = "perf-counters"))]
mod demo {
    use super::{ALLOC_BYTES, ALLOC_COUNT};
    use lark_rs::{Lark, LarkOptions, LexerType, ParserAlgorithm};
    use std::sync::atomic::Ordering;
    use std::time::Instant;

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

    /// Read-and-zero the allocation counters (isolates one parse's heap traffic).
    fn snapshot() -> (u64, u64) {
        (
            ALLOC_COUNT.swap(0, Ordering::Relaxed),
            ALLOC_BYTES.swap(0, Ordering::Relaxed),
        )
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

    fn median(mut v: Vec<u128>) -> u128 {
        v.sort_unstable();
        v[v.len() / 2]
    }

    pub fn run() {
        let parser = Lark::new(
            JSON_GRAMMAR,
            LarkOptions {
                parser: ParserAlgorithm::Lalr,
                lexer: LexerType::Contextual,
                ..Default::default()
            },
        )
        .expect("json grammar builds");

        let records = 2000;
        let input = gen_json(records, 8);
        let nbytes = input.len();
        println!("input: {nbytes} bytes ({records} records × 8 fields)\n");

        // Warm up caches / lazy scanners (not measured).
        for _ in 0..3 {
            let _ = parser.parse(&input).unwrap();
            let _ = parser.parse_span(&input).unwrap();
        }

        // ── Allocations: reset right before ONE parse, read after it returns
        //    (the result is still alive, so its allocations are counted; freeing
        //    is dealloc, which we don't count). Deterministic. ──────────────────
        lark_rs::perf::reset();
        let _ = snapshot();
        let owned = parser.parse(&input).unwrap();
        let (owned_allocs, owned_bytes) = snapshot();
        let owned_lex = lark_rs::perf::lexer_token_value_bytes();
        let owned_out = lark_rs::perf::token_value_string_bytes();
        let owned_nodes = lark_rs::perf::tree_nodes_built();
        std::hint::black_box(&owned);
        drop(owned);

        lark_rs::perf::reset();
        let _ = snapshot();
        let span = parser.parse_span(&input).unwrap();
        let (span_allocs, span_bytes) = snapshot();
        let span_lex = lark_rs::perf::lexer_token_value_bytes();
        let span_out = lark_rs::perf::token_value_string_bytes();
        let span_nodes = lark_rs::perf::tree_nodes_built();
        std::hint::black_box(&span);
        drop(span);

        // ── Wall-clock (trend only, noisy). ──────────────────────────────────
        let iters = 200;
        let time = |f: &dyn Fn()| {
            let mut ts = Vec::with_capacity(iters);
            for _ in 0..iters {
                let t = Instant::now();
                f();
                ts.push(t.elapsed().as_nanos());
            }
            median(ts)
        };
        let owned_med = time(&|| {
            std::hint::black_box(parser.parse(&input).unwrap());
        });
        let span_med = time(&|| {
            std::hint::black_box(parser.parse_span(&input).unwrap());
        });

        // ── Report. ──────────────────────────────────────────────────────────
        let per_byte = |n: u64| n as f64 / nbytes as f64;
        println!("── Real heap allocations (counting allocator) ──");
        println!(
            "{:<13}{:>12}{:>14}{:>16}",
            "path", "allocs", "allocs/byte", "bytes"
        );
        println!(
            "{:<13}{:>12}{:>14.3}{:>16}",
            "parse()",
            owned_allocs,
            per_byte(owned_allocs),
            owned_bytes
        );
        println!(
            "{:<13}{:>12}{:>14.3}{:>16}",
            "parse_span()",
            span_allocs,
            per_byte(span_allocs),
            span_bytes
        );
        let pct = |from: u64, to: u64| 100.0 * (from as f64 - to as f64) / from as f64;
        println!(
            "→ span removes {:.1}% of allocations ({:.1}% of alloc bytes)\n",
            pct(owned_allocs, span_allocs),
            pct(owned_bytes, span_bytes)
        );

        println!("── Deterministic perf counters (the gated claim) ──");
        println!(
            "{:<13}{:>18}{:>20}{:>13}",
            "path", "lexer_tok_bytes", "output_tok_bytes", "tree_nodes"
        );
        println!(
            "{:<13}{:>18}{:>20}{:>13}",
            "parse()", owned_lex, owned_out, owned_nodes
        );
        println!(
            "{:<13}{:>18}{:>20}{:>13}",
            "parse_span()", span_lex, span_out, span_nodes
        );
        println!();

        println!("── Wall-clock median over {iters} iters (trend, noisy) ──");
        let mbps = |ns: u128| nbytes as f64 / (ns as f64 / 1e3);
        println!(
            "parse()      {:>8.3} ms  {:>6.2} MB/s",
            owned_med as f64 / 1e6,
            mbps(owned_med)
        );
        println!(
            "parse_span() {:>8.3} ms  {:>6.2} MB/s",
            span_med as f64 / 1e6,
            mbps(span_med)
        );
        println!(
            "→ span is {:.2}× the speed of owned",
            owned_med as f64 / span_med as f64
        );
    }
}

#[cfg(all(feature = "span-tree", feature = "perf-counters"))]
fn main() {
    demo::run();
}

#[cfg(not(all(feature = "span-tree", feature = "perf-counters")))]
fn main() {
    eprintln!(
        "span_alloc needs the `span-tree` (for parse_span) and `perf-counters` (for the \
         allocation-counter readout) features:\n\n    cargo run --release --features \
         \"span-tree,perf-counters\" --example span_alloc"
    );
}
