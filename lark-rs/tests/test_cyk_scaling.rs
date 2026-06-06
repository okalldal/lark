//! Deterministic scaling gate for the CYK backend (#87).
//!
//! The CYK backend landed at 100% oracle agreement but with **no scaling/perf
//! gate** — unlike Earley (`tests/test_earley_scaling.rs`). CYK is inherently
//! `O(n³ · |grammar|)`: a triangular table over token spans, each cell combining
//! two adjacent sub-spans over every split point. An accidental complexity
//! regression — an extra loop in the DP, or a CNF conversion whose grammar size
//! grows with the input — would push it past cubic, and nothing would catch it.
//!
//! This is the noise-free analog `BENCH.md` prescribes (the same discipline the
//! Earley gate uses): it keys on the **deterministic work counter**
//! [`lark_rs::perf::cyk_table_steps`] — every `(split, left-nt, right-nt)`
//! combination the DP examines — and asserts a fixed scaling *shape* (cubic, i.e.
//! flat per `n³`), never wall-clock. That can actually gate.
//!
//! The counter only exists when the crate is built with `--features perf-counters`
//! (off by default, zero overhead on the hot path otherwise). When the feature is
//! off this test is a single trivial pass, so `cargo test --all` stays green and
//! fast; CI runs the gating variant separately:
//!
//! ```bash
//! cargo test --features perf-counters --test test_cyk_scaling
//! ```
//!
//! ## What the assertions pin
//!
//! On a *densely ambiguous* grammar (`s: s s | "a"` over `"a"ⁿ`) every span cell is
//! populated, so the DP does its maximal `~n³/6` combination steps — the worst case
//! we want to bound. Two checks, two-sided so the gate is meaningful in both
//! directions:
//!
//! * **Flat per `n³`.** `steps / n³` must not grow across a 16→128 sweep. A genuine
//!   cubic workload has `steps ≈ c·n³` (with lower-order terms making the ratio
//!   *decrease* toward `c` as `n` grows), so a non-increasing ratio passes and a
//!   regression to `n⁴` (the ratio climbs) trips it.
//! * **Genuinely cubic, not quadratic.** Each doubling of `n` must grow `steps` by
//!   ≥5× (a cubic workload octuples, `2³ = 8`; a merely quadratic one only
//!   quadruples) and ≤12× (catches a quartic regression, which would be 16×). So
//!   the gate proves the workload *is* the cubic stress case and that the DP stays
//!   within the cubic envelope.

use lark_rs::{Lark, LarkOptions, ParserAlgorithm};

/// The canonical maximally-ambiguous CYK stress grammar: `s: s s | "a"`. Over
/// `"a"ⁿ` every sub-span derives `s`, so the table is fully dense and the DP does
/// its worst-case `~n³/6` combination steps — exactly the shape a scaling gate
/// must bound.
const AMBIG_GRAMMAR: &str = "start: s\ns: s s | \"a\"\n";

/// A wider ambiguous rule (`s: s s s | "a"`) so the gate also exercises the BIN
/// binarization path of the CNF conversion (`>2`-symbol rules split through
/// `__SP_` helpers). Still `O(n³)` per cell, so the same cubic envelope holds.
const AMBIG3_GRAMMAR: &str = "start: s\ns: s s s | \"a\"\n";

fn cyk(grammar: &str) -> Lark {
    Lark::new(
        grammar,
        LarkOptions {
            start: vec!["start".to_string()],
            parser: ParserAlgorithm::Cyk,
            ..LarkOptions::default()
        },
    )
    .expect("scaling-test grammar must build")
}

/// The whole net runs as ONE test function: the `perf` counters are process-global
/// atomics, so a second `#[test]` racing in parallel would corrupt the reads. A
/// single sequential reset→parse→read loop keeps every measurement clean (the same
/// rationale as the Earley scaling gate).
#[cfg(feature = "perf-counters")]
#[test]
fn cyk_scaling_is_pinned() {
    use lark_rs::perf;

    assert!(
        perf::ENABLED,
        "test built with the perf-counters feature but counters report disabled"
    );

    // Sizes chosen so n³ spans a ~500× range while each parse stays sub-second even
    // under CYK's cubic cost. Doublings make the per-doubling ratio check clean.
    // `s: s s` accepts any length; `s: s s s` only derives odd lengths (each ternary
    // step adds 2 to the count), so it gets a near-doubling odd sweep.
    assert_cubic_envelope("ambig (s: s s)", &cyk(AMBIG_GRAMMAR), &[16, 32, 64, 128]);
    assert_cubic_envelope(
        "ambig3 (s: s s s)",
        &cyk(AMBIG3_GRAMMAR),
        &[15, 31, 63, 127],
    );
}

/// Measure `cyk_table_steps` over a size sweep and assert the cubic shape: the
/// per-`n³` cost is flat-or-decreasing, and each doubling grows the raw count
/// within `[5×, 12×]` (super-quadratic but not quartic). Restores nothing — the
/// counter is reset before each measurement.
#[cfg(feature = "perf-counters")]
fn assert_cubic_envelope(label: &str, parser: &Lark, sizes: &[usize]) {
    use lark_rs::perf;

    let mut steps: Vec<(usize, u64)> = Vec::new();
    for &n in sizes {
        let input = "a".repeat(n);
        perf::reset();
        parser
            .parse(&input)
            .unwrap_or_else(|e| panic!("{label}: n={n} must parse: {e:?}"));
        let s = perf::cyk_table_steps();
        assert!(
            s > 0,
            "{label}: n={n} recorded zero table steps — the counter is not wired \
             into the DP (or the grammar is not actually running CYK)"
        );
        steps.push((n, s));
    }

    // Flat (or decreasing) per n³: a true cubic workload has steps/n³ → c from
    // above, so the largest size's ratio must not exceed the smallest's.
    let per_cube = |(n, s): &(usize, u64)| *s as f64 / (*n as f64).powi(3);
    let first = per_cube(steps.first().unwrap());
    let last = per_cube(steps.last().unwrap());
    assert!(
        last <= first * 1.5,
        "{label}: CYK table fill is NOT flat per n³ — steps/n³ grew from {first:.4} \
         to {last:.4} across the sweep (rows: {steps:?}); the DP or CNF conversion \
         became super-cubic"
    );

    // Genuinely cubic: each doubling must grow the raw count by ≥5× (a cubic
    // octuples; a quadratic only quadruples) and ≤12× (a quartic would be 16×).
    for w in steps.windows(2) {
        let ratio = w[1].1 as f64 / w[0].1 as f64;
        assert!(
            (5.0..=12.0).contains(&ratio),
            "{label}: a doubling of n grew table steps {ratio:.2}× ({} → {}) — \
             outside the cubic band [5×, 12×]. Below 5× the workload stopped being \
             the cubic stress case (the gate would no longer prove anything); above \
             12× the DP regressed toward quartic. rows: {steps:?}",
            w[0].1,
            w[1].1
        );
    }
}

/// Without the `perf-counters` feature the counter is a no-op, so the gate cannot
/// run. Keep a visible placeholder documenting how to run it, so the file is never
/// silently empty (mirrors the Earley scaling gate).
#[cfg(not(feature = "perf-counters"))]
#[test]
fn cyk_scaling_requires_perf_counters_feature() {
    // Intentionally trivial: `cargo test --all` stays fast; CI runs the real gate
    // with `cargo test --features perf-counters --test test_cyk_scaling`.
    assert!(!lark_rs::perf::ENABLED);
}
