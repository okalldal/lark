//! Deterministic **grammar-build cross-product** gate (#404, H6-7).
//!
//! `compile_expansion` (`grammar/loader/ebnf.rs`) lowers a rule body by a
//! per-position cartesian fold: each inline group/optional contributes a set of
//! alternatives, and the running product `acc` is `concat`-ed with the next
//! position's choices. A chain of `k` duplicate-arm inline groups
//! (`(X|X) (X|X) … (X|X)`) folded *without* per-step dedup materializes the full
//! `m^k` product before a single trailing dedup collapses it to one alternative — a
//! deterministic `2^k` build blowup (measured before the fix: ~2× wall-clock per +1
//! `k`, hanging for seconds by k=20). The fix folds with `concat_alts_dedup`, which
//! bounds the running product to the *distinct* alternatives at each prefix length —
//! here exactly one — producing the byte-identical final alternative set. Python
//! Lark's `SimplifyRule_Visitor` dedups each group's arms before the product and
//! builds the identical grammar in flat linear time.
//!
//! Like the Earley/CYK/lexer/dense-DFA scaling gates, this keys on a **deterministic
//! work counter** — [`lark_rs::perf::expansion_alts`], the size of the running
//! product after each fold step, summed over the rule body — never wall-clock
//! (`BENCH.md`: shared-runner timing is a recorded trend, not a gate). The counter
//! only exists under `--features perf-counters` (zero overhead otherwise), so
//! `cargo test --all` runs the trivial placeholder and CI runs the real gate with:
//!
//! ```bash
//! cargo test --features perf-counters --test test_grammar_build_scaling
//! ```

#[cfg(feature = "perf-counters")]
use lark_rs::{load_grammar, perf};

/// Count the alternatives `compile_expansion` materializes while loading `grammar`
/// (the `expansion_alts` perf counter, summed across every fold step of every
/// rule). The grammar must load successfully — these workloads all build to a single
/// surface rule.
#[cfg(feature = "perf-counters")]
fn fold_alts(grammar: &str) -> u64 {
    perf::reset();
    load_grammar(grammar, &["start".to_string()], false, false)
        .expect("build-scaling grammar must load");
    perf::expansion_alts()
}

/// `(X|X) (X|X) … (X|X)` — `k` inline groups, each two **byte-identical** arms. The
/// cross-product of duplicate arms collapses to one distinct alternative at every
/// prefix length, so the deduping fold's running product stays size 1 and the total
/// folded-alternative count is linear in `k`. The non-deduping fold made it `2^k`.
#[cfg(feature = "perf-counters")]
fn duplicate_group_chain(k: usize) -> String {
    let body = vec!["(X|X)"; k].join(" ");
    format!("start: {body}\nX: \"x\"\n")
}

/// `(X|Y) (X|Y) … (X|Y)` — the **distinct-arm control**. Its final alternative set is
/// genuinely `2^k` distinct sequences (Python materializes them too), so the fold
/// cost is exponential in *both* engines by design — the dedup cannot and must not
/// collapse it. Used to prove the fix targets only *duplicate*-arm collapse and does
/// not silently linearize a legitimately exponential grammar.
#[cfg(feature = "perf-counters")]
fn distinct_group_chain(k: usize) -> String {
    let body = vec!["(X|Y)"; k].join(" ");
    format!("start: {body}\nX: \"x\"\nY: \"y\"\n")
}

/// The whole net is ONE test: the `perf` counters are process-global atomics, so a
/// second `#[test]` racing in parallel would corrupt the reads (same rationale as the
/// Earley/CYK/lexer/dense-DFA scaling gates).
#[cfg(feature = "perf-counters")]
#[test]
fn grammar_build_cross_product_is_subexponential() {
    assert!(
        perf::ENABLED,
        "test built with the perf-counters feature but counters report disabled"
    );

    // ── The fix: the duplicate-arm chain folds flat. ───────────────────────────
    // Each fold step's running product is size 1 (the two arms are byte-identical),
    // so the summed alternative count is O(k). We assert it stays *strictly bounded*
    // (well under any exponential): a `2^k` fold would materialize 2^k alternatives;
    // the deduping fold materializes ~k. A generous linear envelope (`8*k`) catches
    // any reintroduction of the exponential while tolerating the exact per-step
    // bookkeeping.
    //
    // The sweep is **capped at k=12 on purpose**: the gate's signal (`expansion_alts`)
    // can only be read *after* `load_grammar` returns, so a reverted fix must still
    // build the cross-product before the assertion can fire. At k=12 the non-deduping
    // fold materializes only 2^12 = 4096 alternatives — already 43× over the `8*12=96`
    // envelope, so the gate fails *fast and cleanly*. Larger k (the wall-clock pin in
    // `test_bounty_findings_h6.rs` uses k=20 behind a worker-thread timeout) would add
    // no discriminating power here and would turn a reverted-fix failure into a
    // multi-second hang / OOM rather than a clean assertion.
    let ks = [6usize, 8, 10, 12];
    let mut rows: Vec<(usize, u64)> = Vec::new();
    for &k in &ks {
        let alts = fold_alts(&duplicate_group_chain(k));
        rows.push((k, alts));
    }
    eprintln!("duplicate-arm chain: (k, folded_alts) = {rows:?}");
    for &(k, alts) in &rows {
        assert!(
            alts <= 8 * k as u64,
            "H6-7 (#404): (X|X)^{k} materialized {alts} fold alternatives — a deduping \
             fold is O(k) (≤ {} here). A {alts} that tracks 2^{k} means the per-position \
             fold regressed to the non-deduping `concat_alts` and the cross-product \
             blowup is back.",
            8 * k as u64
        );
    }

    // The final surface grammar is still exactly one `start` rule (byte-identical to
    // the pre-fix output): the dedup only avoids the intermediate explosion.
    let g = load_grammar(
        &duplicate_group_chain(12),
        &["start".to_string()],
        false,
        false,
    )
    .expect("(X|X)^12 must load");
    let start_rules = g.rules.iter().filter(|r| r.origin.name == "start").count();
    assert_eq!(
        start_rules, 1,
        "H6-7 (#404): (X|X)^12 must collapse to a single `start` alternative (got {start_rules})"
    );

    // ── The control: the distinct-arm chain is genuinely exponential. ──────────
    // `(X|Y)^k` has `2^k` distinct final alternatives — Python materializes them too
    // — so the dedup cannot collapse it. Confirm the fold count *grows* (roughly
    // doubles per +1 k), i.e. the fix did not silently linearize a legitimately
    // exponential grammar. Kept small (k ≤ 12 → ≤ 4096 alternatives) so it builds
    // fast.
    let small = fold_alts(&distinct_group_chain(8));
    let large = fold_alts(&distinct_group_chain(12));
    eprintln!("distinct-arm control: alts(k=8)={small}, alts(k=12)={large}");
    assert!(
        large >= small * 8,
        "control (X|Y)^k must stay exponential in both engines (alts grew {small} → \
         {large} from k=8 to k=12; a genuine 2^k fold grows ~16×). If the dedup \
         collapsed this it would be wrongly merging *distinct* alternatives — a \
         correctness bug, not a perf win."
    );
}

/// Without the `perf-counters` feature the counter is a no-op, so the gate cannot run.
/// Keep a visible placeholder documenting how to run it (mirrors the other scaling
/// gates), so `cargo test --all` stays fast and the file is never silently empty.
#[cfg(not(feature = "perf-counters"))]
#[test]
fn grammar_build_scaling_requires_perf_counters_feature() {
    assert!(!lark_rs::perf::ENABLED);
}
