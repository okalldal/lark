//! Deterministic **dense-DFA build-cost** gate for the lookaround lowering
//! (`docs/LEXER_DFA_PLAN.md`, the determinization-blowup risk).
//!
//! Lowering a bounded assertion into the combined DFA is paid at **build** time, not
//! per lex: the `DfaScanner` compiles each plain/guarded base and each guard body to a
//! `dense::DFA`. The **L5 bake** (`to_bytes`) needs that fully-determinized dense DFA,
//! so a lowering that blows up determinization — parity duplication, spliced branches,
//! an interacting union of python.lark's many per-state contextual scanners — inflates
//! the bake target even though per-lex throughput looks fine. That cost is invisible to
//! the runtime differential, so it needs its own gate.
//!
//! Like the Earley/CYK/lexer scaling gates, this keys on a **deterministic work
//! counter** — [`lark_rs::perf::dense_build_bytes`], the summed `dense::DFA`
//! `memory_usage()` (state-count × stride proxy) over a scanner build — never
//! wall-clock. Two sweeps, two pathologies:
//!
//!   * **per terminal** — add lowered lookaround terminals to one scanner and assert
//!     the determinized size stays *flat per terminal*. A union that determinizes to a
//!     product (super-linear in the terminal count) trips it.
//!   * **per guard width** — grow a single lookbehind's window width and assert the
//!     size stays *flat per width*. A future window-carry lowering that built `2^W`
//!     states instead of `O(W)` trips it.
//!
//! The counter only exists under `--features perf-counters` (zero overhead otherwise),
//! so `cargo test --all` runs the trivial placeholder and CI runs the real gate with:
//!
//! ```bash
//! cargo test --features perf-counters --test test_lexer_dfa_build_scaling
//! ```

#[cfg(feature = "perf-counters")]
use lark_rs::{basic_lexer_conf, load_grammar, lower, BasicLexer, LexerBackend};

/// Encode `i` as a distinct lowercase prefix over `{a,b,c}` (base-3, fixed width), so
/// `n` generated terminals have `n` distinct languages that union into one combined
/// DFA without colliding.
#[cfg(feature = "perf-counters")]
fn marker(i: usize) -> String {
    let mut s = String::new();
    let mut v = i;
    for _ in 0..4 {
        s.push((b'a' + (v % 3) as u8) as char);
        v /= 3;
    }
    s
}

/// A grammar of `n` **lowered lookaround** terminals — alternating a trailing-boundary
/// guard and a bounded lookbehind, each with a distinct marker prefix so all `n` lower
/// (none declined) into one combined `DfaScanner`.
#[cfg(feature = "perf-counters")]
fn grammar_with_n_terminals(n: usize) -> String {
    let mut g = String::from("start: (");
    g.push_str(
        &(0..n)
            .map(|i| format!("T{i}"))
            .collect::<Vec<_>>()
            .join(" | "),
    );
    g.push_str(")+\n");
    for i in 0..n {
        let m = marker(i);
        // Even: a trailing-boundary guard (`X(?![0-9])`); odd: a fixed-offset bounded
        // lookbehind (`[a-z](?<!a)X`). Both lower; both are greedy-monotone.
        let pat = if i % 2 == 0 {
            format!("{m}[a-z]+(?![0-9])")
        } else {
            format!("{m}[a-z](?<!a)z")
        };
        g.push_str(&format!("T{i}: /{pat}/\n"));
    }
    g
}

/// A single lowered lookbehind terminal whose guard window is `w` chars wide
/// (`[a-z](?<![a-z]{{w}})z`). The lookbehind body `[a-z]{{w}}` determinizes to `O(w)`
/// states, so the build cost must grow *linearly* in `w`.
#[cfg(feature = "perf-counters")]
fn grammar_with_lookbehind_width(w: usize) -> String {
    format!("start: T+\nT: /[a-z](?<![a-z]{{{w}}})z/\n")
}

/// Build the `DfaScanner`-backed basic lexer for `grammar`, returning the
/// `dense_build_bytes` counted during the build (the scanner determinizes its dense
/// DFAs in `BasicLexer::new`).
#[cfg(feature = "perf-counters")]
fn build_cost(grammar: &str) -> u64 {
    use lark_rs::perf;
    let g = load_grammar(grammar, &["start".to_string()], false, false)
        .expect("build-scaling grammar must load");
    let cg = lower(&g);
    let conf = basic_lexer_conf(&cg, 0).with_backend(LexerBackend::Dfa);
    perf::reset();
    let _lexer = BasicLexer::new(&conf).expect("DfaScanner must build");
    perf::dense_build_bytes()
}

/// The whole net is ONE test: the `perf` counters are process-global atomics, so a
/// second `#[test]` racing in parallel would corrupt the reads (same rationale as the
/// Earley/CYK/lexer scaling gates).
#[cfg(feature = "perf-counters")]
#[test]
fn dense_dfa_build_cost_is_flat() {
    use lark_rs::perf;
    assert!(
        perf::ENABLED,
        "test built with the perf-counters feature but counters report disabled"
    );

    // Sweep 1 — flat per terminal. The combined base engine is one DFA whose size grows
    // ~linearly in the terminal count (plus a small fixed guard DFA each), so cost/term
    // is flat-or-decreasing. A union that determinizes to a product would make it climb.
    assert_flat(
        "per-terminal",
        &[8usize, 16, 32, 64],
        |n| grammar_with_n_terminals(n),
        |n| n as f64,
    );

    // Sweep 2 — flat per guard width. The lookbehind body `[a-z]{w}` determinizes to
    // O(w) states, so cost/width is flat. A window-carry lowering that built 2^w states
    // would make cost/width explode.
    assert_flat(
        "per-width",
        &[1usize, 2, 4, 8],
        |w| grammar_with_lookbehind_width(w),
        |w| w as f64,
    );
}

/// Measure `dense_build_bytes` over a size sweep and assert the cost *per unit*
/// (terminal or guard width) is flat-or-decreasing: the largest size's cost/unit must
/// be within `1.6×` of the smallest's. A super-linear blowup makes cost/unit climb and
/// trips this.
#[cfg(feature = "perf-counters")]
fn assert_flat(
    label: &str,
    sizes: &[usize],
    grammar: impl Fn(usize) -> String,
    unit: impl Fn(usize) -> f64,
) {
    let mut per_unit: Vec<(usize, f64)> = Vec::new();
    for &n in sizes {
        let cost = build_cost(&grammar(n));
        assert!(
            cost > 0,
            "{label}: size {n} recorded zero dense-build bytes — the counter is not \
             wired into the DfaScanner build (or nothing lowered)"
        );
        per_unit.push((n, cost as f64 / unit(n)));
    }

    eprintln!("dense-build {label}: bytes/unit across sweep = {per_unit:?}");
    let first = per_unit.first().unwrap().1;
    let last = per_unit.last().unwrap().1;
    assert!(
        last <= first * 1.6,
        "{label}: dense-DFA build cost is NOT flat — grew from {first:.1} to {last:.1} \
         bytes/unit across the sweep (rows: {per_unit:?}). A lowering is blowing up \
         dense-DFA determinization (parity duplication, a spliced/product union); this \
         is the L5 bake-target cost, paid at standalone generation."
    );
}

/// Without the `perf-counters` feature the counter is a no-op, so the gate cannot run.
/// Keep a visible placeholder documenting how to run it (mirrors the other scaling
/// gates), so `cargo test --all` stays fast and the file is never silently empty.
#[cfg(not(feature = "perf-counters"))]
#[test]
fn dense_dfa_build_scaling_requires_perf_counters_feature() {
    assert!(!lark_rs::perf::ENABLED);
}
