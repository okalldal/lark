//! Deterministic linear-scan gate for the lexer (the regression net PR #104's
//! `\G`-anchoring fix shipped without).
//!
//! The lexer matches one token at a time by asking each candidate terminal to
//! match *exactly at* the current position `pos` (`Scanner::match_at`, shared by
//! both the basic and the contextual lexer). The underlying `regex` /
//! `fancy-regex` searches (`captures_read_at`, `find_from_pos`) are **leftmost**,
//! not anchored: when a terminal does *not* match at `pos` they scan forward toward
//! the next possible match and the result is then rejected by a `start() == pos`
//! check. A low-rank lookaround terminal on the `fancy-regex` side-probe — historically
//! the bundled `python.STRING` / `lark.REGEXP` / `python.LONG_STRING`, before their
//! lowerings landed; today only per-instance declines and unsupported user lookaround
//! ride it — is tried at *every* token boundary, so each failing attempt
//! forward-scans O(remaining input). Over `n` tokens that is **O(n²)** lexing, even
//! though every token is unambiguous. Anchoring the per-position search (so it only
//! ever looks at `pos`) collapses it back to linear.
//!
//! This is the noise-free analog `BENCH.md` prescribes (the same discipline the
//! Earley/CYK scaling gates use): it keys on the **deterministic work counter**
//! [`lark_rs::perf::lexer_scan_steps`] — per per-position attempt, the bytes the
//! search reported skipping past `pos` plus one — and asserts the total stays *flat
//! per byte*, never wall-clock. An anchored scanner records ~1 per attempt (linear
//! in the token count); the unanchored forward-scan makes the per-byte cost climb
//! with `n` and trips the gate.
//!
//! The workload deliberately ends with **one sparse string match**: a no-match
//! returns `None` from both an anchored and an unanchored search, so the forward
//! scan is only *observable* when the search reports a match starting far ahead of
//! `pos`. With the single trailing `"…"`, every earlier position's unanchored STR
//! search returns that distant start (the skip we count) while the `\G`-anchored
//! search keeps failing at `pos` — so the counter cleanly separates the two regimes.
//!
//! The counter only exists when the crate is built with `--features perf-counters`
//! (off by default, zero overhead on the hot path otherwise). When the feature is
//! off this test is a single trivial pass, so `cargo test --all` stays green and
//! fast; CI runs the gating variant separately:
//!
//! ```bash
//! cargo test --features perf-counters --test test_lexer_scaling
//! ```

use lark_rs::{Lark, LarkOptions, LexerType, ParserAlgorithm};

/// A grammar whose only lookaround terminal (`STR`, a trailing negative lookahead →
/// routed to `fancy-regex`) is a *low-rank* candidate — its pattern is longer than
/// `WORD`'s, so the `(-priority, -pattern_len, name)` sort tries it first at every
/// position. Over a run of bare words `STR` does not match at any word position, so
/// an unanchored search scans ahead to the sole (trailing) string — the O(n²) shape.
/// This mirrors the historically fancy-routed bundled lookaround terminals
/// (`python.STRING` / `lark.REGEXP` / `python.LONG_STRING`, all lowered now — the
/// class this models survives in per-instance user declines)
/// exactly, without depending on the stdlib grammars' internals.
const LOOKAROUND_GRAMMAR: &str = r#"
    start: (WORD | STR)+
    WORD: /[a-z]+/
    STR: /"[^"]*"(?![0-9])/
    %ignore " "
"#;

/// `n` bare words separated by spaces, then **one** trailing string. The trailing
/// match is what makes the unanchored forward-scan observable (see the module-level
/// note): at every word position the lookaround `STR` fails at `pos` but its only
/// match lies far ahead, so an unanchored search reports that distant start while a
/// `\G`-anchored search fails immediately. Length ≈ `2n`.
fn words(n: usize) -> String {
    let mut s = vec!["a"; n].join(" ");
    s.push_str(" \"z\"");
    s
}

fn lark(lexer: LexerType) -> Lark {
    Lark::new(
        LOOKAROUND_GRAMMAR,
        LarkOptions {
            start: vec!["start".to_string()],
            parser: ParserAlgorithm::Lalr,
            lexer,
            ..LarkOptions::default()
        },
    )
    .expect("scaling-test grammar must build")
}

/// The `perf` counters are process-global atomics, so two `#[test]` fns reading them
/// in parallel would corrupt each other's measurements. Each gate below locks this
/// mutex for its whole reset→parse→read sweep, so they run mutually exclusive even
/// under the default multi-threaded test runner (the same rationale as the
/// single-function Earley/CYK scaling gates, generalized so the basic/contextual gate
/// and the dynamic-lexer gate #335 can be two named tests without racing).
#[cfg(feature = "perf-counters")]
static PERF_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The basic + contextual lexers share `Scanner::match_at`, so the pathology — and
/// its `\G`-anchoring fix (#104) — live in one place; gate both so neither can
/// regress independently.
#[cfg(feature = "perf-counters")]
#[test]
fn lexer_scan_is_flat_per_byte() {
    use lark_rs::perf;

    let _guard = PERF_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    assert!(
        perf::ENABLED,
        "test built with the perf-counters feature but counters report disabled"
    );

    assert_flat_per_byte("basic", &lark(LexerType::Basic));
    assert_flat_per_byte("contextual", &lark(LexerType::Contextual));
}

/// Measure `lexer_scan_steps` over a size sweep and assert the per-byte cost is
/// flat-or-decreasing: the largest input's scan/byte must be within `1.6×` of the
/// smallest's. A forward-scanning (unanchored) lexer makes scan/byte grow ~linearly
/// with `n` and trips this; an anchored one stays flat.
#[cfg(feature = "perf-counters")]
fn assert_flat_per_byte(label: &str, parser: &Lark) {
    assert_flat_per_byte_with(label, parser, words);
}

/// Same gate as [`assert_flat_per_byte`] but with a caller-supplied input generator,
/// so the dynamic-lexer gate (#335) can reuse the identical measurement loop with its
/// own `#z#`-terminated workload.
#[cfg(feature = "perf-counters")]
fn assert_flat_per_byte_with(label: &str, parser: &Lark, input_of: fn(usize) -> String) {
    use lark_rs::perf;

    let sizes = [64usize, 256, 1024, 4096];
    let mut per_byte: Vec<(usize, f64)> = Vec::new();
    for &n in &sizes {
        let input = input_of(n);
        perf::reset();
        parser
            .parse(&input)
            .unwrap_or_else(|e| panic!("{label}: n={n} must parse: {e:?}"));
        let scan = perf::lexer_scan_steps();
        assert!(
            scan > 0,
            "{label}: n={n} recorded zero scan steps — the counter is not wired into \
             the scanner (or the grammar is not actually lexing)"
        );
        per_byte.push((input.len(), scan as f64 / input.len() as f64));
    }

    let first = per_byte.first().unwrap().1;
    let last = per_byte.last().unwrap().1;
    assert!(
        last <= first * 1.6,
        "{label}: lexer scan is NOT flat per byte — grew from {first:.3} to {last:.3} \
         scan/byte across the sweep (per-byte rows: {per_byte:?}). A lookaround \
         terminal that fails at a position is forward-scanning the rest of the input \
         instead of matching anchored at `pos` (O(n²) lexing); anchor the \
         per-position search."
    );
}

// ─── #335: the Earley DYNAMIC lexer's per-terminal scan ───────────────────────
//
// The dynamic lexer (`parser=earley, lexer=dynamic`) does NOT use `Scanner::match_at`
// above — it matches each predicted terminal individually through
// `DynamicMatcher::match_at` (`src/lexer/dynamic.rs`). That path historically used
// the unanchored `regex::Regex::find_at`, so a *sparse* terminal in the per-position
// scan set forward-scanned to its next match at every boundary it missed — O(n) per
// byte ⇒ O(n²) total, even though `\G`-anchoring landed for basic/contextual in #104.
// The fix anchors the per-terminal search at `pos` (`Anchored::Yes`), the per-terminal
// analog of `\G` and of Python's anchored `re.Pattern.match(text, pos)`.

/// The issue #335 repro grammar, verbatim: a plain (non-lookaround) `STR` terminal
/// whose pattern is longer than `WORD`'s, so the dynamic scan tries it at every word
/// boundary. Over a run of bare words `STR` never matches at a word position; the
/// sole `STR` lies at the trailing `#z#`, so an unanchored search reports that distant
/// start (the skip we count) at every earlier position.
const DYNAMIC_GRAMMAR: &str = r#"
    start: (WORD | STR)+
    WORD: /[a-z]+/
    STR: /\#[^\#]*\#/
    %ignore " "
"#;

/// `n` bare words separated by spaces, then **one** trailing `#z#` string. Same shape
/// as `words` above; the trailing sparse `STR` match is what makes the unanchored
/// forward-scan observable in the work counter. Length ≈ `2n`.
#[cfg(feature = "perf-counters")]
fn dynamic_words(n: usize) -> String {
    let mut s = vec!["a"; n].join(" ");
    s.push_str(" #z#");
    s
}

#[cfg(feature = "perf-counters")]
fn dynamic_lark() -> Lark {
    Lark::new(
        DYNAMIC_GRAMMAR,
        LarkOptions {
            start: vec!["start".to_string()],
            parser: ParserAlgorithm::Earley,
            lexer: LexerType::Dynamic,
            ..LarkOptions::default()
        },
    )
    .expect("dynamic scaling-test grammar must build")
}

/// #335: the Earley dynamic lexer's `lexer_scan_steps` must grow **linearly** (flat
/// per byte), not quadratically. Before the anchoring fix, `lexer_scan_steps`/byte
/// climbed ~linearly with `n` (67 → 259 → 1027 → 4099 total across n=64→4096), which
/// trips the same flat-per-byte assertion the basic/contextual gate uses.
#[cfg(feature = "perf-counters")]
#[test]
fn h11_dynamic_lexer_scan_is_flat_per_byte() {
    use lark_rs::perf;

    let _guard = PERF_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    assert!(
        perf::ENABLED,
        "test built with the perf-counters feature but counters report disabled"
    );

    // First confirm the token stream is what we expect (the fix must not change WHAT
    // is matched, only how the scan is anchored): n WORDs + one STR.
    let parser = dynamic_lark();
    let small = parser.parse("a b #z#").expect("dynamic grammar must parse");
    assert_eq!(
        small.to_string(),
        r##"Tree(start, [Token(WORD, "a"), Token(WORD, "b"), Token(STR, "#z#")])"##,
        "dynamic-lexer token output changed — the anchoring fix must be token-identical"
    );

    assert_flat_per_byte_with("dynamic", &parser, dynamic_words);
}

/// Without the `perf-counters` feature the counter is a no-op, so the gate cannot
/// run. Keep a visible placeholder documenting how to run it, so the file is never
/// silently empty (mirrors the Earley/CYK scaling gates).
#[cfg(not(feature = "perf-counters"))]
#[test]
fn lexer_scaling_requires_perf_counters_feature() {
    // Intentionally trivial: `cargo test --all` stays fast; CI runs the real gate
    // with `cargo test --features perf-counters --test test_lexer_scaling`.
    assert!(!lark_rs::perf::ENABLED);
}
