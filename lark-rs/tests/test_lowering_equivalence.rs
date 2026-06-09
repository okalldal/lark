//! L2 lowering harness — **layer 2: terminal-level generative equivalence vs
//! `fancy-regex`** (`docs/LEXER_DFA_PLAN.md`, "Verification harness").
//!
//! For each supported shape, *generate* hundreds of concrete terminals and compare
//! the **lowered** match-length against the `fancy-regex` oracle over an exhaustive
//! small-alphabet corpus — so coverage stops depending on whose imagination (the
//! lesson from missing `DEC_NUMBER`'s length-change until it was *run*).
//!
//! All three shapes have landed (M1 trailing, M2 leading, M3 bounded-lookbehind), so
//! every per-shape equivalence assertion is active and compares the real lowered
//! matcher ([`lowered_prefix`]) against the oracle. The boundary and lookbehind
//! equivalence-layer mutation meta-tests live here too: each deliberately-wrong
//! lowering must diverge from the oracle somewhere on the population, proving the layer
//! has teeth.

mod common;

use common::lowering::{
    boundary_mutations, corpus, fancy_matcher, fancy_prefix, has_guard, has_lookbehind,
    lookbehind_mutations, lowered_prefix, mutant_lookbehind_matcher, mutant_matcher,
    string_idiom_terminals, supported_terminals, BoundaryMutation, GenTerminal,
};
use lark_rs::ShapeClass;

/// The core comparison the per-shape gates run once the lowered matcher exists:
/// for every input in the terminal's exhaustive quotient-alphabet corpus, the
/// lowered match-length must equal the `fancy-regex` oracle. Returns the first
/// divergence, or `None` on full agreement. `lowered_prefix` returning `Err` (a
/// declined terminal) is surfaced as a divergence so it fails loudly rather than
/// silently skipping.
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

/// The mutation meta-test for the **boundary** lowering (leading + trailing): each
/// deliberately-wrong way to lower a guard (forget it, flip its polarity, drop the
/// trailing-EOF case) must be *caught* — it must diverge from the `fancy-regex`
/// oracle somewhere on the boundary population, so a real lowering that made the same
/// mistake would turn the equivalence layers red. A surviving mutant is a hole in the
/// net (`docs/LEXER_DFA_PLAN.md`, "Validate the harness itself").
#[test]
fn boundary_lowering_mutants_are_caught() {
    let terms: Vec<GenTerminal> = supported_terminals()
        .into_iter()
        .filter(|t| {
            matches!(
                t.shape,
                ShapeClass::TrailingBoundary | ShapeClass::LeadingBoundary
            ) && has_guard(&t.pattern)
        })
        .collect();
    assert!(!terms.is_empty(), "no guarded boundary terminals to mutate");

    for mutation in boundary_mutations() {
        let mut caught: Option<String> = None;
        'search: for t in &terms {
            let Some(oracle) = fancy_matcher(&t.pattern) else {
                continue;
            };
            // Compile the mutant matcher once per (terminal, mutation), then reuse it
            // across the corpus.
            let Some(mutant_re) = mutant_matcher(&t.pattern, mutation) else {
                continue;
            };
            for input in corpus(&t.alphabet, t.max_len) {
                let mutant = fancy_prefix(&mutant_re, &input);
                let correct = fancy_prefix(&oracle, &input);
                if mutant != correct {
                    caught = Some(format!(
                        "{} {:?} on {input:?}: mutant={mutant:?} != correct={correct:?}",
                        t.name, t.pattern
                    ));
                    break 'search;
                }
            }
        }
        assert!(
            caught.is_some(),
            "mutant {mutation:?} survived the boundary population — the \
             generative-equivalence layer would NOT catch this wrong lowering. \
             This is a hole in the net."
        );
    }
}

#[test]
fn leading_boundary_lowered_equals_fancy() {
    run_shape(ShapeClass::LeadingBoundary);
}

#[test]
fn bounded_lookbehind_lowered_equals_fancy() {
    run_shape(ShapeClass::BoundedLookbehind);
}

/// The mutation meta-test for the **bounded-lookbehind** lowering (M3): each
/// deliberately-wrong way to lower a lookbehind (ignore it, flip its polarity, an
/// off-by-one window width) must be *caught* — it must diverge from the `fancy-regex`
/// oracle somewhere on the (biting) lookbehind population. A surviving mutant is a
/// hole in the net (`docs/LEXER_DFA_PLAN.md`, "Validate the harness itself"). The
/// biting generator cases (`\w(?<!_)x`, …) are what keep the *ignore-the-lookbehind*
/// mutant from passing vacuously.
#[test]
fn bounded_lookbehind_lowering_mutants_are_caught() {
    let terms: Vec<GenTerminal> = supported_terminals()
        .into_iter()
        .filter(|t| t.shape == ShapeClass::BoundedLookbehind && has_lookbehind(&t.pattern))
        .collect();
    assert!(!terms.is_empty(), "no lookbehind terminals to mutate");

    for mutation in lookbehind_mutations() {
        let mut caught: Option<String> = None;
        'search: for t in &terms {
            let Some(oracle) = fancy_matcher(&t.pattern) else {
                continue;
            };
            let Some(mutant_re) = mutant_lookbehind_matcher(&t.pattern, mutation) else {
                continue;
            };
            for input in corpus(&t.alphabet, t.max_len) {
                let mutant = fancy_prefix(&mutant_re, &input);
                let correct = fancy_prefix(&oracle, &input);
                if mutant != correct {
                    caught = Some(format!(
                        "{} {:?} on {input:?}: mutant={mutant:?} != correct={correct:?}",
                        t.name, t.pattern
                    ));
                    break 'search;
                }
            }
        }
        assert!(
            caught.is_some(),
            "lookbehind mutant {mutation:?} survived the population — the \
             generative-equivalence layer would NOT catch this wrong lowering. \
             This is a hole in the net."
        );
    }
}

/// The **string-literal opening-guard idiom** (python.STRING's real nested/prefixed
/// shape, the marquee L2 splice): every generated idiom terminal's lowered match-length
/// must equal the `fancy-regex` oracle over its exhaustive corpus — which includes the
/// `""""` boundary, escaped quotes, and escaped backslashes. `lowered_prefix` returning
/// `Err` (a declined terminal) is surfaced as a divergence, so a terminal that *failed*
/// to lower fails loudly rather than passing vacuously.
#[test]
fn string_idiom_lowered_equals_fancy() {
    let terms = string_idiom_terminals();
    assert!(!terms.is_empty(), "no string-idiom terminals generated");
    let mut failures = Vec::new();
    for t in &terms {
        if let Some(d) = equivalence_divergence(t) {
            failures.push(d);
        }
    }
    assert!(
        failures.is_empty(),
        "string-idiom generative-equivalence divergence(s):\n  {}",
        failures.join("\n  ")
    );
}

/// **The drop-the-`(?!"")`-guard equivalence mutant** (deliverable 4). The `(?!"")`
/// splice's residual is the trailing `(?!")` guard on the empty arm; a lowering that
/// *forgot* it would accept `""""` (one empty string) where the oracle rejects it. The
/// meta-test asserts this `ForgetGuard` mutant is **caught** — and that the witness is
/// exactly the `""""`-shape (oracle matches nothing, the mutant matches a leading empty
/// string at an over-long quote-run) — so the canary genuinely defends the splice. A
/// surviving mutant would mean the equivalence layer could not tell a guard-less STRING
/// from the real one: a hole in the net.
#[test]
fn dropping_the_opening_guard_is_caught_by_the_quad_quote() {
    let terms = string_idiom_terminals();
    let mut caught_by_quad_quote = false;
    for t in &terms {
        let Some(oracle) = fancy_matcher(&t.pattern) else {
            continue;
        };
        // The wrong lowering: the empty arm's trailing guard dropped.
        let Some(mutant) = mutant_matcher(&t.pattern, BoundaryMutation::ForgetGuard) else {
            continue;
        };
        for input in corpus(&t.alphabet, t.max_len) {
            let correct = fancy_prefix(&oracle, &input);
            let wrong = fancy_prefix(&mutant, &input);
            if correct == wrong {
                continue;
            }
            // The divergence must be the canary: an over-long quote-run the real STRING
            // rejects (None) but the guard-less mutant accepts as an empty string.
            let q = if t.alphabet.contains(&'"') { '"' } else { '\'' };
            if correct.is_none()
                && wrong.is_some()
                && input.starts_with(q)
                && input[1..].starts_with(q)
            {
                caught_by_quad_quote = true;
            }
        }
    }
    assert!(
        caught_by_quad_quote,
        "the drop-(?!\"\")-guard mutant was NOT caught by a `\"\"\"\"`-shape input — the \
         opening-guard splice would be undefended (a hole in the net)"
    );
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
