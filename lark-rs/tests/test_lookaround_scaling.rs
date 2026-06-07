//! Deterministic linearity gate for the lookaround lowering engine — the Pike-VM
//! analog of `tests/test_lexer_scaling.rs`, closing "gap #2" of the Lexer DFA / B1
//! plan's testing strategy.
//!
//! `tests/test_lexer_scaling.rs` proves the *driver* scans linearly per byte (each
//! terminal is tried anchored at `pos`, no forward scan). It does NOT prove the
//! *per-terminal matcher* can't blow up internally. That is the whole reason
//! `fancy-regex` was removed: a backtracking engine has no work bound, so the
//! ambiguous alternation under a lazy star in `lark.REGEXP`
//! (`(\\\/|\\\\|[^\/])*?` with a `(?!\/)` lookahead) is a textbook ReDoS — its step
//! count explodes on an input that forces maximal backtracking.
//!
//! The replacement is a Pike-VM (`src/lookaround/matcher.rs`): a priority-ordered
//! Thompson simulation that visits each `(instruction, input position)` at most once,
//! so its work is bounded by `program_size · match_length` — **linear in the input**
//! no matter how ambiguous the pattern. This test pins that property deterministically
//! via the [`lark_rs::perf::pike_vm_steps`] work counter, never wall-clock: it feeds
//! the genuine `lark.REGEXP`-shaped terminal the adversarial inputs that would make a
//! backtracker explode and asserts the VM step count stays *flat per byte*.
//!
//! The counter only exists under `--features perf-counters` (off by default, zero
//! overhead otherwise); CI runs the gating variant separately:
//!
//! ```bash
//! cargo test --features perf-counters --test test_lookaround_scaling
//! ```

use lark_rs::{Lark, LarkOptions, LexerType, ParserAlgorithm};

/// The bundled `lark.REGEXP` terminal verbatim: an ambiguous alternation
/// (`\\\/ | \\\\ | [^\/]` — a backslash matches both the `\\\\` branch *and* the
/// `[^\/]` branch) under a lazy star, guarded by a `(?!\/)` lookahead. Under a
/// backtracking engine this is the ReDoS shape; under the Pike-VM it is linear.
const REGEXP_GRAMMAR: &str = r#"
    start: REGEXP
    REGEXP: /\/(?!\/)(\\\/|\\\\|[^\/])*?\/[imslux]*/
"#;

/// A valid regex literal: `/` + `n` escaped-backslash pairs + `/`. One long token
/// the VM matches in full — the common "big token" case (≈ the python `STRING`
/// shape), which must also be linear.
fn valid_regexp(n: usize) -> String {
    format!("/{}/", "\\\\".repeat(n))
}

/// The ReDoS adversary: `/` + a long run of backslashes and **no closing slash**.
/// The body can segment the run many ways and never reaches the required closing
/// `/`, so a backtracker tries exponentially many segmentations before failing. The
/// Pike-VM traverses each position once and reports no-match in linear time.
fn unterminated_regexp(n: usize) -> String {
    format!("/{}", "\\".repeat(n))
}

fn lark(lexer: LexerType) -> Lark {
    Lark::new(
        REGEXP_GRAMMAR,
        LarkOptions {
            start: vec!["start".to_string()],
            parser: ParserAlgorithm::Lalr,
            lexer,
            ..LarkOptions::default()
        },
    )
    .expect("scaling-test grammar must build")
}

/// One sequential test: the `perf` counters are process-global atomics, so a second
/// `#[test]` racing in parallel would corrupt the reads (same rationale as the
/// Earley/CYK/lexer scaling gates).
#[cfg(feature = "perf-counters")]
#[test]
fn pike_vm_work_is_flat_per_byte() {
    use lark_rs::perf;

    assert!(
        perf::ENABLED,
        "test built with the perf-counters feature but counters report disabled"
    );

    // Both lexers drive the same lowered matcher; gate both.
    for lexer in [LexerType::Basic, LexerType::Contextual] {
        let parser = lark(lexer.clone());
        let tag = format!("{lexer:?}");
        // Valid long token: matches in full, must be linear.
        assert_flat_per_byte(&format!("{tag}/valid"), &parser, valid_regexp, true);
        // ReDoS adversary: forces maximal work then fails — the backtracking blowup
        // the Pike-VM linearizes. The parse errors (no valid token), which is fine;
        // we measure the VM work, not the parse result.
        assert_flat_per_byte(
            &format!("{tag}/adversary"),
            &parser,
            unterminated_regexp,
            false,
        );
    }
}

/// Measure `pike_vm_steps` over a size sweep and assert the per-byte cost is
/// flat-or-decreasing: the largest input's steps/byte must be within `1.6×` of the
/// smallest's. A backtracking engine would make this climb super-linearly (in fact
/// explode) on the adversary; the Pike-VM keeps it flat.
#[cfg(feature = "perf-counters")]
fn assert_flat_per_byte(
    label: &str,
    parser: &Lark,
    make_input: fn(usize) -> String,
    expect_parse_ok: bool,
) {
    use lark_rs::perf;

    let sizes = [64usize, 256, 1024, 4096];
    let mut per_byte: Vec<(usize, f64)> = Vec::new();
    for &n in &sizes {
        let input = make_input(n);
        perf::reset();
        let result = parser.parse(&input);
        assert_eq!(
            result.is_ok(),
            expect_parse_ok,
            "{label}: n={n} parse outcome unexpected (ok={})",
            result.is_ok()
        );
        let steps = perf::pike_vm_steps();
        assert!(
            steps > 0,
            "{label}: n={n} recorded zero Pike-VM steps — the counter is not wired \
             into the lowered matcher (or the terminal is not actually lowered)"
        );
        per_byte.push((input.len(), steps as f64 / input.len() as f64));
    }

    let first = per_byte.first().unwrap().1;
    let last = per_byte.last().unwrap().1;
    // Upper bound: catches super-linearity / ReDoS (steps/byte climbing with n).
    assert!(
        last <= first * 1.6,
        "{label}: Pike-VM work is NOT flat per byte — grew from {first:.3} to \
         {last:.3} steps/byte across the sweep (per-byte rows: {per_byte:?}). The \
         lowered matcher is doing super-linear work; a backtracking path has crept \
         back into the engine."
    );
    // Lower bound: the work must genuinely scale with input (steps/byte stays
    // roughly constant), so an O(1) early-bail can't trivially satisfy the upper
    // bound and falsely "prove" linearity. Measured steps/byte is ~constant (≈11).
    assert!(
        last >= first * 0.5,
        "{label}: Pike-VM steps/byte collapsed from {first:.3} to {last:.3} — the \
         matcher is doing ~constant work, so this sweep does not actually exercise \
         input-proportional matching (per-byte rows: {per_byte:?})."
    );
}

/// Without `perf-counters` the counter is a no-op, so the gate cannot run. A visible
/// placeholder documents how to run it (mirrors the other scaling gates).
#[cfg(not(feature = "perf-counters"))]
#[test]
fn lookaround_scaling_requires_perf_counters_feature() {
    assert!(!lark_rs::perf::ENABLED);
}
