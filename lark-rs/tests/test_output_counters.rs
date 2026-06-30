//! Deterministic **output-shape** gate (semantic-output C5, #230).
//!
//! The "fast path builds the right *shape* of output" claim must be falsifiable
//! without wall-clock (ADR-0007, BENCH.md: shared-runner timing is a recorded
//! trend, not a gate). The shared [`TreeOutputBuilder`] is the one place that
//! materializes the output tree, so we instrument exactly what it builds:
//!
//! * [`perf::tree_nodes_built`] — every `Tree` node built (`build_node`);
//! * [`perf::token_value_string_bytes`] — the owned token-value bytes copied into
//!   the output (`build_token`);
//! * [`perf::semantic_reduce_calls`] — one per reduction shaped through `assemble`
//!   (the LALR/CYK reducers and the Earley *explicit* walk; the Earley *resolve*
//!   walk shapes via `shape` directly, so this gate is LALR-scoped).
//!
//! This test pins two properties of the *default* `TreeOutputBuilder` backend:
//!
//! 1. it **builds** trees — `tree_nodes_built > 0` and `token_value_string_bytes >
//!    0` on a normal parse, and all three output-shape counters **scale with output
//!    shape** (a parse over `2n` items builds ~2× the nodes/bytes/reductions of one
//!    over `n`);
//! 2. `semantic_reduce_calls == number_of_reductions` for a known input.
//!
//! The two assertions the issue defers to the span backend (C8, blocked on
//! C7/#232) — `tree_nodes_built == 0` and `token_value_string_bytes == 0` for a
//! *zero-tree* backend — are NOT gated here: the C3 fixture backend that landed
//! (PR #261) walks an already-built `ParseTree`, so it builds trees by
//! construction. They land with C8 alongside the `token_value_string_bytes == 0`
//! gate the issue body already defers. This file lays the counter infrastructure
//! those gates will key on.
//!
//! Like every other scaling gate the counters only exist under
//! `--features perf-counters` (zero overhead otherwise), so `cargo test --all` runs
//! the trivial placeholder and CI runs the real gate with:
//!
//! ```bash
//! cargo test --features perf-counters --test test_output_counters
//! ```

#[cfg(feature = "perf-counters")]
use lark_rs::{perf, Lark, LarkOptions, LexerType, ParserAlgorithm};

/// A list grammar with a **known, closed-form reduction count**. For an input of
/// `n` items (`"a a a …"`, `n ≥ 1`) under LALR:
///
/// * `item: "a"`        reduces `n` times;
/// * `list: item`       reduces `1` time   (the left-most base);
/// * `list: list item`  reduces `n - 1` times;
/// * `start: list`      reduces `1` time.
///
/// Total user-rule reductions = `n + 1 + (n - 1) + 1 = 2n + 1`. The augmented
/// `$root_start → start` accept does **not** route through `assemble`, so it is not
/// counted (that is exactly the contract `semantic_reduce_calls` documents).
///
/// `item` keeps its `"a"` token (it is named `ITEM`, not a filtered anonymous
/// punctuation terminal), so each item contributes one kept token of 1 byte.
#[cfg(feature = "perf-counters")]
const LIST_GRAMMAR: &str = r#"
start: list
list: list item | item
item: ITEM
ITEM: "a"
%ignore " "
"#;

/// Build a default LALR parser and parse `n` space-separated `a` items, returning
/// the three output-shape counters `(tree_nodes_built, token_value_string_bytes,
/// semantic_reduce_calls)` for that single parse.
#[cfg(feature = "perf-counters")]
fn parse_items(parser: &Lark, n: usize) -> (u64, u64, u64) {
    assert!(n >= 1, "grammar needs at least one item");
    let input = vec!["a"; n].join(" ");
    perf::reset();
    parser
        .parse(&input)
        .unwrap_or_else(|e| panic!("list parse of {n} items must succeed: {e}"));
    (
        perf::tree_nodes_built(),
        perf::token_value_string_bytes(),
        perf::semantic_reduce_calls(),
    )
}

/// The whole net is ONE test: the `perf` counters are process-global atomics, so a
/// second `#[test]` racing in parallel would corrupt the reads (same rationale as
/// the Earley/CYK/lexer/grammar-build scaling gates).
#[cfg(feature = "perf-counters")]
#[test]
fn output_counters_track_tree_shape() {
    assert!(
        perf::ENABLED,
        "test built with the perf-counters feature but counters report disabled"
    );

    let parser = Lark::new(
        LIST_GRAMMAR,
        LarkOptions {
            parser: ParserAlgorithm::Lalr,
            lexer: LexerType::Contextual,
            start: vec!["start".to_string()],
            ..Default::default()
        },
    )
    .expect("list grammar must build under LALR");

    // ── Assertion 1a: the default backend BUILDS trees. ────────────────────────
    let n = 4;
    let (nodes, bytes, reduces) = parse_items(&parser, n);
    eprintln!(
        "n={n}: tree_nodes_built={nodes}, token_value_string_bytes={bytes}, \
         semantic_reduce_calls={reduces}"
    );
    assert!(
        nodes > 0,
        "default TreeOutputBuilder must build Tree nodes (tree_nodes_built={nodes}); \
         a zero is the C8 span-backend gate, not the default backend"
    );
    assert!(
        bytes > 0,
        "default TreeOutputBuilder must copy token-value bytes \
         (token_value_string_bytes={bytes}); a zero is the C8 span-backend gate"
    );

    // ── Assertion 2: semantic_reduce_calls == number_of_reductions. ────────────
    // Closed form for this grammar/input: 2n + 1 (see LIST_GRAMMAR doc).
    let expected_reductions = (2 * n + 1) as u64;
    assert_eq!(
        reduces, expected_reductions,
        "semantic_reduce_calls must equal the parser's reduction count for a known \
         input: n={n} items over the list grammar performs exactly {expected_reductions} \
         user-rule reductions (2n+1), got {reduces}"
    );

    // For this grammar the output shape is also exactly known: one `start`, one
    // `list` per item (left-recursive nesting), and one `item` per item → 1 + n + n
    // = 2n + 1 Tree nodes; and each of the n kept `ITEM` tokens is 1 byte → n bytes.
    assert_eq!(
        nodes,
        (2 * n + 1) as u64,
        "list grammar over n={n} items builds 1 start + n list + n item = 2n+1 nodes"
    );
    assert_eq!(
        bytes, n as u64,
        "list grammar keeps n={n} one-byte ITEM tokens → n value bytes"
    );

    // ── Assertion 1b: every counter SCALES with output shape. ──────────────────
    // Doubling the input doubles the nodes/bytes/reductions (the shape is linear in
    // n for this grammar). A backend that stopped building per-item output — or a
    // counter wired to something constant — would break the ~2× ratio. We assert
    // the exact closed forms across a sweep so the scaling is pinned, not merely
    // "grows".
    let sweep = [1usize, 2, 4, 8, 16];
    let mut rows: Vec<(usize, u64, u64, u64)> = Vec::new();
    for &k in &sweep {
        let (nodes, bytes, reduces) = parse_items(&parser, k);
        rows.push((k, nodes, bytes, reduces));
    }
    eprintln!("output-shape sweep (n, nodes, bytes, reduces) = {rows:?}");
    for &(k, nodes, bytes, reduces) in &rows {
        assert_eq!(
            nodes,
            (2 * k + 1) as u64,
            "tree_nodes_built must scale as 2n+1 with output shape (n={k})"
        );
        assert_eq!(
            bytes, k as u64,
            "token_value_string_bytes must scale as n with kept-token payload (n={k})"
        );
        assert_eq!(
            reduces,
            (2 * k + 1) as u64,
            "semantic_reduce_calls must scale as 2n+1 with the reduction count (n={k})"
        );
    }

    // Direct doubling check across the sweep: nodes and reduces roughly double as n
    // doubles (the +1 constant makes it not exactly 2×, so assert the per-item rate
    // is flat — the deterministic "flat per unit" envelope the gate discipline
    // demands, BENCH.md).
    let (n_small, nodes_small, _bytes_small, _r_small) = (2u64, rows[1].1, rows[1].2, rows[1].3);
    let (n_big, nodes_big) = (16u64, rows[4].1);
    // (nodes - 1) / n is exactly 2 for every n → flat per-item node rate.
    assert_eq!(
        (nodes_small - 1) / n_small,
        (nodes_big - 1) / n_big,
        "per-item node-build rate must be flat (output shape scales linearly, no \
         super-linear node blowup)"
    );
}

/// Without the `perf-counters` feature the counters are no-ops, so the gate cannot
/// run. Keep a visible placeholder documenting how to run it (mirrors the other
/// scaling gates), so `cargo test --all` stays fast and the file is never silently
/// empty.
#[cfg(not(feature = "perf-counters"))]
#[test]
fn output_counters_require_perf_counters_feature() {
    assert!(!lark_rs::perf::ENABLED);
}
