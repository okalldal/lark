//! Deterministic **child-buffer boundedness** gate (semantic-output C8.2, #583).
//!
//! The honest close-out of #233's last done-when line — *"bounded child-buffer
//! reuse"*. C8 (PR #581) shipped the `SpanTree` output backend and drove the two
//! output counters (`tree_nodes_built == 0`, `token_value_string_bytes == 0`) to
//! zero, but it did **not** implement or gate child-buffer reuse: the reduction path
//! (`shape_reduction`) still allocates a **fresh** owned child `Vec` per reduction.
//! That per-reduction allocation is *bounded* (O(children) per node, never
//! quadratic) but neither *reused* nor *counter-gated*, so the claim was unproven.
//! This gate proves the **bounded** half deterministically (ADR-0007: a work
//! counter, never wall-clock — BENCH.md), and documents the *not-reused* half
//! honestly.
//!
//! It keys on [`lark_rs::perf::child_vec_allocs`] — one child-buffer allocation
//! charged **per reduction** the `parse_into` path shapes (the reduction's owned
//! `kept` child buffer; a per-node unit, not a raw allocator count — see the
//! `perf.rs` doc) — and asserts:
//!
//! 1. **Flat per node.** `child_vec_allocs == 2n+1` — the grammar's known closed-form
//!    reduction count — so the child-buffer work is exactly one buffer per node and
//!    scales linearly with the output shape. A super-linear blowup (a buffer per
//!    child, or per span cell) would push the count past `2n+1`. `child_vec_allocs ==
//!    semantic_reduce_calls` is also pinned as the *reuse-ratio == 1* invariant (see
//!    assertion note below).
//! 2. **Linear in output shape, no super-linear blowup.** Across a size sweep the
//!    per-item allocation rate `(child_vec_allocs - 1) / n` is flat (exactly `2`),
//!    so doubling the input doubles the buffers — the output-shape analog of the
//!    Earley/CYK per-unit-flat scaling nets.
//!
//! The gate is deliberately keyed on the *reuse* frontier: today the ratio
//! `child_vec_allocs / semantic_reduce_calls` is exactly `1` (one fresh buffer per
//! node, no reuse). An owned-per-node representation like `SpanBranch` inherently
//! *cannot* reuse the buffer it retains, so a genuine reuse win (allocations `<` node
//! count) needs the arena/`Tape` backend (#242/#243), not `SpanTree`. When that
//! lands, this counter is exactly the gate that shows the reuse — the ratio drops
//! below `1`. Scope of *this* issue is the counter + boundedness gate only; pooling
//! rides the arena work.
//!
//! Like every other scaling gate the counter only exists under
//! `--features perf-counters` (zero overhead otherwise), so `cargo test --all` runs
//! the trivial placeholder and CI runs the real gate with:
//!
//! ```bash
//! cargo test --features perf-counters --test test_child_vec_scaling
//! ```

#[cfg(feature = "perf-counters")]
use lark_rs::{perf, Lark, LarkOptions, LexerType, ParserAlgorithm};

/// A list grammar with a **known, closed-form reduction count** — the same shape
/// `test_output_counters.rs` uses. For an input of `n` items (`"a a a …"`, `n ≥ 1`)
/// under LALR the user-rule reductions are:
///
/// * `item: "a"`        → `n`;
/// * `list: item`       → `1`;
/// * `list: list item`  → `n - 1`;
/// * `start: list`      → `1`.
///
/// Total = `2n + 1`. Each reduction routes through `shape_reduction` (the
/// value-parametric `parse_into`/`parse()` path), which allocates exactly one child
/// buffer, so `child_vec_allocs == 2n + 1` too. The augmented `$root_start → start`
/// accept does **not** route through `shape_reduction`, so it is not counted.
#[cfg(feature = "perf-counters")]
const LIST_GRAMMAR: &str = r#"
start: list
list: list item | item
item: ITEM
ITEM: "a"
%ignore " "
"#;

/// Parse `n` space-separated `a` items on the default (tree) `parse()` path — which
/// drives the value-parametric `run_into` + `shape_reduction` seam — and return
/// `(child_vec_allocs, semantic_reduce_calls)` for that single parse.
#[cfg(feature = "perf-counters")]
fn parse_items(parser: &Lark, n: usize) -> (u64, u64) {
    assert!(n >= 1, "grammar needs at least one item");
    let input = vec!["a"; n].join(" ");
    perf::reset();
    parser
        .parse(&input)
        .unwrap_or_else(|e| panic!("list parse of {n} items must succeed: {e}"));
    (perf::child_vec_allocs(), perf::semantic_reduce_calls())
}

/// The whole net is ONE test: the `perf` counters are process-global atomics, so a
/// second `#[test]` racing in parallel would corrupt the reads (same rationale as
/// the Earley/CYK/lexer/output-shape scaling gates).
#[cfg(feature = "perf-counters")]
#[test]
fn child_vec_allocs_are_bounded() {
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

    // ── The counter is actually wired. ────────────────────────────────────────
    let (allocs, reduces) = parse_items(&parser, 4);
    eprintln!("n=4: child_vec_allocs={allocs}, semantic_reduce_calls={reduces}");
    assert!(
        allocs > 0,
        "child_vec_allocs recorded zero — the counter is not wired into \
         shape_reduction (or this input never reduces)"
    );

    // ── Assertion 1: reuse ratio == 1 (one child buffer per reduction). ────────
    // `child_vec_allocs == semantic_reduce_calls` pins the *current* (bounded,
    // not-reused) one-tick-per-reduction state: today the reuse ratio is exactly 1.
    // The two counters are charged adjacently in `shape_reduction`, so this equality
    // is the tripwire on the counter *placement* itself — a future pooling/arena
    // strategy (#242/#243) that moves the tick to charge fewer buffers than nodes
    // drives allocs BELOW reduces (the reuse win the gate is laid to show), and a
    // regression that charged per child would drive it ABOVE. The load-bearing
    // *boundedness* claim is the `== 2n+1` closed form in assertion 2.
    assert_eq!(
        allocs, reduces,
        "child_vec_allocs must equal semantic_reduce_calls (one child buffer charged \
         per reduction — the bounded, not-yet-reused reuse-ratio-1 state #583 \
         documents). allocs<reduces would be a reuse win (#242/#243); allocs>reduces \
         a per-child blowup"
    );

    // ── Assertion 2: linear in output shape across a sweep, no super-linear blowup.
    // The grammar's closed form is 2n+1 (see LIST_GRAMMAR), so we pin the exact
    // count and the flat per-item rate across the sweep — the deterministic
    // "flat per unit" envelope the gate discipline demands (BENCH.md).
    let sweep = [1usize, 2, 4, 8, 16, 32];
    let mut rows: Vec<(usize, u64, u64)> = Vec::new();
    for &n in &sweep {
        let (allocs, reduces) = parse_items(&parser, n);
        rows.push((n, allocs, reduces));
    }
    eprintln!("child-vec sweep (n, allocs, reduces) = {rows:?}");

    for &(n, allocs, reduces) in &rows {
        let expected = (2 * n + 1) as u64;
        assert_eq!(
            allocs, expected,
            "child_vec_allocs must scale as the closed-form reduction count 2n+1 \
             (flat per node) with output shape (n={n}); a super-linear blowup would \
             exceed it"
        );
        assert_eq!(
            allocs, reduces,
            "the one-buffer-per-reduction invariant must hold at every size (n={n})"
        );
    }

    // Flat per-item allocation rate: `(allocs - 1) / n` is exactly 2 for every n, so
    // doubling n doubles the buffers (linear), never super-linearly. Compare the
    // smallest and largest sweep points explicitly — a per-child or per-span-cell
    // blowup would make the big-n rate climb.
    let per_item = |&(n, allocs, _): &(usize, u64, u64)| (allocs - 1) / n as u64;
    let small = per_item(&rows[1]); // n=2
    let big = per_item(rows.last().unwrap()); // n=32
    assert_eq!(
        small, big,
        "per-item child-buffer allocation rate must be flat (child-buffer work is \
         linear in output shape, no super-linear blowup): (allocs-1)/n = {small} at \
         n=2 vs {big} at n=32"
    );
}

/// Without the `perf-counters` feature the counter is a no-op, so the gate cannot
/// run. Keep a visible placeholder documenting how to run it (mirrors the other
/// scaling gates), so `cargo test --all` stays fast and the file is never silently
/// empty.
#[cfg(not(feature = "perf-counters"))]
#[test]
fn child_vec_scaling_requires_perf_counters_feature() {
    assert!(!lark_rs::perf::ENABLED);
}
