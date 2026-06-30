//! Deterministic density gate for the in-process LALR `ParseTable` (#367, H5-9).
//!
//! The in-process `ParseTable` historically allocated a fully **dense**
//! `action[state][terminal]` matrix (`vec![vec![None; n_terminals]; n_states]`,
//! `src/parsers/lalr.rs`). For a grammar whose state *and* terminal counts both
//! grow with size, that matrix is `O(states × terminals) = O(n²)` cells, while
//! the semantic content — the `Some` actions, matching Python Lark's sparse
//! dict-of-dicts — is only `O(n)`. Not a wrong-answer bug: a memory/build-cost
//! pathology. The standalone emitter already bakes the sparse `&[(u32, Action)]`
//! row; this gate pins the *in-process* table to the same `O(filled)` shape.
//!
//! The repro is the issue's size sweep verbatim: `start: r0 | … | rn` with each
//! arm `ri: Ai Bi Ci` referencing three distinct terminals, so states grow ~`2n`
//! and terminals ~`3n` (the issue measured n=4 → 18 states / 234 dense cells;
//! n=128 → 514 states / 197,890 dense cells). The deterministic work counter
//! [`lark_rs::perf::parse_table_action_cells`] records the ACTION cells the table
//! actually *stores*:
//!
//!   * dense  → `n_states × n_terminals` per build  (`Θ(n²)`)
//!   * sparse → only the `Some` actions             (`Θ(filled)`, `Θ(n)`)
//!
//! The grammar size `n` (its arm count) is the deterministic "unit": both the
//! state count and the terminal count are `Θ(n)`, so the *dense* cell count is
//! `Θ(n²)` ⇒ `cells/n` climbs ~linearly, while the *sparse* cell count is `Θ(n)`
//! ⇒ `cells/n` stays flat. Asserting `cells/n` is flat-or-decreasing over the
//! sweep separates the two regimes — it FAILS on dense and PASSES on sparse. This
//! keys on a deterministic count, never wall-clock (`BENCH.md`, PRINCIPLES.md
//! §2.5), exactly like the Earley/CYK/lexer gates.
//!
//! The counter only exists under `--features perf-counters` (off by default, zero
//! overhead otherwise), so `cargo test --all` runs the trivial placeholder below
//! and stays fast; CI runs the gating variant:
//!
//! ```bash
//! cargo test --features perf-counters --test test_lalr_table_scaling
//! ```

#[cfg(feature = "perf-counters")]
use lark_rs::{Lark, LarkOptions, LexerType, ParserAlgorithm};

/// Build the issue's repro grammar at size `n`: `start: r0 | … | r{n-1}` with each
/// arm `ri: Ai Bi Ci` referencing three terminals distinct per arm. The terminals
/// are unique literals, so each arm contributes its own states and its own three
/// terminals — states ~`2n`, terminals ~`3n` (the shape the issue measured).
#[cfg(feature = "perf-counters")]
fn repro_grammar(n: usize) -> String {
    use std::fmt::Write;
    let mut g = String::new();
    // start: r0 | r1 | … | r{n-1}
    g.push_str("start: ");
    for i in 0..n {
        if i > 0 {
            g.push_str(" | ");
        }
        let _ = write!(g, "r{i}");
    }
    g.push('\n');
    // ri: Ai Bi Ci   (three distinct terminals per arm)
    for i in 0..n {
        let _ = writeln!(g, "r{i}: A{i} B{i} C{i}");
    }
    // Distinct terminal definitions — unique literals so each is its own terminal.
    for i in 0..n {
        let _ = writeln!(g, "A{i}: \"a{i} \"");
        let _ = writeln!(g, "B{i}: \"b{i} \"");
        let _ = writeln!(g, "C{i}: \"c{i} \"");
    }
    g
}

#[cfg(feature = "perf-counters")]
fn build_lalr(grammar: &str) -> Lark {
    Lark::new(
        grammar,
        LarkOptions {
            start: vec!["start".to_string()],
            parser: ParserAlgorithm::Lalr,
            // Basic lexer: building the LALR table is what we measure; the
            // contextual lexer would lazily build per-state scanners we don't need.
            lexer: LexerType::Basic,
            ..LarkOptions::default()
        },
    )
    .expect("repro grammar must build under LALR")
}

/// The `perf` counters are process-global atomics; serialize the reset→build→read
/// sweep so a parallel test can't corrupt the measurement (same rationale as the
/// other scaling gates).
#[cfg(feature = "perf-counters")]
static PERF_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The ACTION table cells must scale `Θ(filled) = Θ(n)`, not `Θ(states ×
/// terminals) = Θ(n²)`.
///
/// Measures `parse_table_action_cells` (the cells the representation *stores*) over
/// the issue's size sweep and asserts the per-arm cost `cells/n` is
/// flat-or-decreasing. On the dense matrix `cells = n_states × n_terminals = Θ(n²)`
/// so `cells/n` climbs ~linearly with `n`; on the sparse `(terminal id, action)`
/// rows `cells = filled = Θ(n)` so `cells/n` stays flat — so the gate FAILS on the
/// dense representation and PASSES on the sparse one.
#[cfg(feature = "perf-counters")]
#[test]
fn parse_table_action_cells_scale_with_filled_not_dense() {
    use lark_rs::perf;

    let _guard = PERF_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    assert!(
        perf::ENABLED,
        "test built with the perf-counters feature but counters report disabled"
    );

    // The issue's size sweep (a superset of its n=4/64/128 measurement table).
    let sizes = [4usize, 16, 64, 128];

    // (n, cells, cells/n) per build.
    let mut rows: Vec<(usize, u64, f64)> = Vec::new();
    for &n in &sizes {
        let grammar = repro_grammar(n);

        perf::reset();
        let lark = build_lalr(&grammar);
        let cells = perf::parse_table_action_cells();
        assert!(
            cells > 0,
            "n={n}: counter recorded zero cells — the counter is not wired into the \
             LALR table build"
        );

        // Prove the table is functional: parsing a valid arm must still work (no
        // parse-result change is the hard constraint of #367).
        let tree = lark
            .parse("a0 b0 c0 ")
            .unwrap_or_else(|e| panic!("n={n}: repro grammar must parse a valid arm: {e:?}"));
        assert_eq!(
            tree.to_string(),
            r#"Tree(start, [Tree(r0, [Token(A0, "a0 "), Token(B0, "b0 "), Token(C0, "c0 ")])])"#,
            "n={n}: the table must produce the byte-identical tree for arm 0"
        );

        rows.push((n, cells, cells as f64 / n as f64));
    }

    let first = rows.first().unwrap().2;
    let last = rows.last().unwrap().2;
    assert!(
        last <= first * 1.6,
        "LALR ACTION table is NOT O(filled) — cells/arm grew from {first:.1} to \
         {last:.1} across the sweep (rows n,cells,cells/n = {rows:?}). The dense \
         `vec![vec![None; n_terminals]; n_states]` matrix stores Θ(states × \
         terminals) = Θ(n²) cells where only Θ(n) are semantic; switch ACTION/GOTO \
         to a sparse per-state representation (the standalone emitter's \
         `&[(u32, Action)]` row)."
    );
}

/// Without the `perf-counters` feature the counter is a no-op, so the gate cannot
/// run. A trivial placeholder keeps `cargo test --all` fast and the file non-empty
/// (mirrors the Earley/CYK/lexer scaling gates); CI runs the real gate with
/// `--features perf-counters`.
#[cfg(not(feature = "perf-counters"))]
#[test]
fn lalr_table_scaling_requires_perf_counters_feature() {
    assert!(!lark_rs::perf::ENABLED);
}
