//! L2 lowering harness — **layer 2: terminal-level generative equivalence vs
//! `fancy-regex`** (`docs/LEXER_DFA_PLAN.md`, "Verification harness").
//!
//! For each supported shape, *generate* hundreds of concrete terminals and compare
//! the **lowered** match-length against the `fancy-regex` oracle over an exhaustive
//! small-alphabet corpus — so coverage stops depending on whose imagination (the
//! lesson from missing `DEC_NUMBER`'s length-change until it was *run*).
//!
//! The equivalence assertion is `#[ignore]`'d **pending the first shape**: there is
//! no lowered matcher yet ([`lowered_prefix`] returns `Err`), so the comparison
//! cannot run. The moment the first-shape session implements the lowered matcher,
//! drop the per-shape `#[ignore]` and the generators + oracle below pin it. The
//! oracle, the generators, and the corpus enumeration are all exercised *now* by the
//! active smoke test, so a bug in the net itself surfaces before the lowering lands.

mod common;

use common::lowering::{
    corpus, fancy_matcher, fancy_prefix, lowered_prefix, supported_terminals, GenTerminal,
};
use lark_rs::ShapeClass;

/// The core comparison the per-shape gates run once the lowered matcher exists:
/// for every input in the terminal's exhaustive quotient-alphabet corpus, the
/// lowered match-length must equal the `fancy-regex` oracle. Returns the first
/// divergence, or `None` on full agreement. `lowered_prefix` returning `Err` (the
/// pending stub) is surfaced as a divergence so an un-ignored run fails loudly.
fn equivalence_divergence(t: &GenTerminal) -> Option<String> {
    let oracle_re = fancy_matcher(&t.pattern)?;
    let inputs = corpus(&t.alphabet, t.max_len);
    for input in &inputs {
        let oracle = fancy_prefix(&oracle_re, input);
        let lowered = match lowered_prefix(&t.name, &t.pattern, input) {
            Ok(v) => v,
            Err(e) => return Some(format!("{}: lowered matcher unavailable: {e}", t.name)),
        };
        if lowered != oracle {
            return Some(format!(
                "{} {:?} on input {:?}: lowered={lowered:?} != fancy={oracle:?}",
                t.name, t.pattern, input
            ));
        }
    }
    None
}

/// Run the generative equivalence over every generated terminal of `shape`.
fn run_shape(shape: ShapeClass) {
    let terms: Vec<GenTerminal> = supported_terminals()
        .into_iter()
        .filter(|t| t.shape == shape)
        .collect();
    assert!(!terms.is_empty(), "no generated terminals for {shape:?}");

    let mut failures = Vec::new();
    for t in &terms {
        if let Some(d) = equivalence_divergence(t) {
            failures.push(d);
        }
    }
    assert!(
        failures.is_empty(),
        "{:?}: {} generative-equivalence divergence(s):\n  {}",
        shape,
        failures.len(),
        failures
            .iter()
            .take(20)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n  ")
    );
}

#[test]
fn trailing_boundary_lowered_equals_fancy() {
    run_shape(ShapeClass::TrailingBoundary);
}

#[test]
fn leading_boundary_lowered_equals_fancy() {
    run_shape(ShapeClass::LeadingBoundary);
}

#[test]
#[ignore = "pending first shape — bounded-lookbehind lowering not yet implemented"]
fn bounded_lookbehind_lowered_equals_fancy() {
    run_shape(ShapeClass::BoundedLookbehind);
}

/// Active **now**: the oracle, the generators, and the exhaustive corpus all work,
/// so the net itself is exercised before the lowering exists. We assert the
/// `fancy-regex` oracle compiles every generated pattern and that the corpora
/// actually drive matches (otherwise an "equivalence" could pass vacuously on an
/// all-None oracle).
#[test]
fn oracle_and_generators_are_well_formed() {
    let terms = supported_terminals();
    let mut total_matches = 0usize;
    for t in &terms {
        // The oracle must compile the generated pattern (a generator bug otherwise).
        let re = fancy_matcher(&t.pattern)
            .unwrap_or_else(|| panic!("fancy-regex rejected generated pattern {:?}", t.pattern));
        // The corpus must exist and be non-trivial.
        let inputs = corpus(&t.alphabet, t.max_len);
        assert!(
            inputs.len() > 1,
            "{} has a degenerate corpus (alphabet={:?})",
            t.name,
            t.alphabet
        );
        total_matches += inputs
            .iter()
            .filter(|i| fancy_prefix(&re, i).is_some())
            .count();
    }
    // Across the whole population the corpora exercise *many* real matches, so the
    // ignored equivalence layers will not be vacuous when activated.
    assert!(
        total_matches > terms.len(),
        "corpora barely match anything ({total_matches} matches over {} terminals) — \
         the generative layer would be near-vacuous",
        terms.len()
    );
}
