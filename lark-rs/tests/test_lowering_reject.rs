//! L2 lowering harness — the **active** layers (`docs/LEXER_DFA_PLAN.md`,
//! "Verification harness").
//!
//! These test the *classifier's* dangerous direction (false-accept), which the
//! lowering's correctness rests on:
//!
//!   * **Reject corpus (layer 4).** Every out-of-shape assertion in the adversarial
//!     corpus MUST be rejected — never accepted/lowered — and with the exact reason.
//!   * **Mutation meta-test (deliverable 4), validated on the reject path.** A
//!     deliberately-wrong classifier that *wrongly accepts* an out-of-shape
//!     assertion MUST be caught (the reject corpus goes red). A surviving mutant is
//!     a hole in the net; this proves the net has teeth.
//!   * **Generator ↔ classifier self-consistency.** Every generated *supported*
//!     terminal classifies as the shape it claims, the entry point lowers every
//!     supported shape (M1/M2/M3) into branches, and every out-of-shape lookaround
//!     terminal is rejected permanently.

mod common;

use common::lowering::{
    long_string_idiom_terminals, reject_cases, reject_path_mutants, supported_terminals,
    wrongly_accepted_rejects,
};
use lark_rs::{classify, lower_terminal, DefaultClassifier, Lowered, ShapeClass, Verdict};

/// Layer 4: the reject corpus is fully active. Every adversarial pattern is rejected
/// with the *expected* reason — no out-of-shape assertion is ever accepted.
#[test]
fn reject_corpus_rejects_every_out_of_shape_assertion() {
    let cases = reject_cases();
    assert!(
        cases.len() >= 20,
        "the adversarial corpus should be substantial, got {}",
        cases.len()
    );

    // Nothing in the corpus is wrongly accepted by the real classifier.
    let wrongly = wrongly_accepted_rejects(&DefaultClassifier, &cases);
    assert!(
        wrongly.is_empty(),
        "the real classifier accepted out-of-shape assertions: {wrongly:?}"
    );

    // And each is rejected for the precise reason the corpus declares.
    let mut mismatches = Vec::new();
    for case in &cases {
        let c = classify(&case.pattern)
            .unwrap_or_else(|e| panic!("classify {:?} errored: {e}", case.pattern));
        match c.first_rejection() {
            Some((_, reason)) if reason == case.expected => {}
            Some((info, reason)) => mismatches.push(format!(
                "{}: {:?} rejected as {reason:?}, expected {:?} (assertion {})",
                case.name, case.pattern, case.expected, info.source
            )),
            None => mismatches.push(format!(
                "{}: {:?} was NOT rejected (expected {:?})",
                case.name, case.pattern, case.expected
            )),
        }
    }
    assert!(
        mismatches.is_empty(),
        "reject-corpus reason mismatches:\n  {}",
        mismatches.join("\n  ")
    );
}

/// Deliverable 4, validated on the reject path: every mutant that wrongly accepts an
/// out-of-shape assertion is caught by the reject corpus (it goes non-empty). If a
/// mutant survived, the net would have a hole.
#[test]
fn mutation_meta_test_catches_wrong_accepts_on_reject_path() {
    let cases = reject_cases();
    for mutant in reject_path_mutants() {
        let caught = wrongly_accepted_rejects(mutant.classifier.as_ref(), &cases);
        assert!(
            !caught.is_empty(),
            "mutant `{}` survived the reject corpus — it wrongly accepted nothing it \
             should have, so the reject corpus would not catch it. This is a hole in \
             the net.",
            mutant.name
        );
    }
}

/// The reject corpus only earns trust if it is *not* vacuous: the real classifier
/// must genuinely reject each case (so "every mutant is caught" isn't because the
/// corpus is empty or trivially-rejected). Cross-check that the corpus exercises all
/// five rejection reasons.
#[test]
fn reject_corpus_covers_every_rejection_reason() {
    use lark_rs::Rejection::*;
    let cases = reject_cases();
    for reason in [
        Unbounded,
        Internal,
        Backref,
        Nested,
        VariableWidthBehind,
        QuantifiedAssertion,
    ] {
        assert!(
            cases.iter().any(|c| c.expected == reason),
            "reject corpus is missing any {reason:?} case"
        );
    }
}

/// Generator ↔ classifier self-consistency: every generated *supported* terminal
/// classifies as exactly the shape it advertises. A drift between the generators and
/// the classifier (either side wrong) fails here.
#[test]
fn generated_supported_terminals_match_their_declared_shape() {
    let terms: Vec<_> = supported_terminals()
        .into_iter()
        .chain(long_string_idiom_terminals())
        .collect();
    assert!(
        terms.len() >= 200,
        "expected hundreds of supported terminals, got {}",
        terms.len()
    );

    let mut bad = Vec::new();
    let (mut lead, mut trail, mut behind) = (0, 0, 0);
    for t in &terms {
        let c = classify(&t.pattern)
            .unwrap_or_else(|e| panic!("classify generated {:?} errored: {e}", t.pattern));
        let got: Vec<Verdict> = c.assertions.iter().map(|a| a.verdict()).collect();
        let ok = !got.is_empty() && got.iter().all(|v| *v == Verdict::Supported(t.shape));
        if ok {
            match t.shape {
                ShapeClass::LeadingBoundary => lead += 1,
                ShapeClass::TrailingBoundary => trail += 1,
                ShapeClass::BoundedLookbehind => behind += 1,
            }
        } else {
            bad.push(format!(
                "{} {:?}: classified {got:?}, want {:?}",
                t.name, t.pattern, t.shape
            ));
        }
    }
    assert!(
        bad.is_empty(),
        "{} generated terminals misclassified:\n  {}",
        bad.len(),
        bad.iter()
            .take(20)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n  ")
    );
    // Every shape is actually represented (the generator isn't lopsided).
    assert!(
        lead > 0 && trail > 0 && behind > 0,
        "shape coverage: lead={lead} trail={trail} behind={behind}"
    );
}

/// The entry point lowers every implemented shape and rejects out-of-shape
/// assertions permanently. With M1/M2/M3 landed, all three supported shapes lower:
///
///   * a plain terminal lowers (`Lowered::Plain`);
///   * every generated **trailing** / **leading** / **lookbehind** terminal lowers
///     to non-empty branches (the generated lookbehind population is all fixed-offset,
///     so none is declined to fancy);
///   * every adversarial out-of-shape terminal is rejected permanently.
#[test]
fn lowering_entry_point_lowers_landed_shapes_and_rejects_the_rest() {
    use lark_rs::lookaround::classify::is_pending_shape_error;

    // Plain terminal: lowers (no lookaround).
    assert!(matches!(
        lower_terminal("PLAIN", r"[A-Za-z_][A-Za-z0-9_]*"),
        Ok(Lowered::Plain)
    ));

    for t in supported_terminals()
        .into_iter()
        .chain(long_string_idiom_terminals())
    {
        // All three supported shapes lower for real now (M1/M2/M3).
        let lowered = lower_terminal(&t.name, &t.pattern).unwrap_or_else(|e| {
            panic!(
                "supported {:?} terminal {:?} must lower now, got: {e}",
                t.shape, t.pattern
            )
        });
        assert!(
            matches!(lowered, Lowered::Branches(ref b) if !b.is_empty()),
            "supported terminal {:?} must lower to branches, got {lowered:?}",
            t.pattern
        );
    }

    // Every adversarial terminal is rejected (permanently, not pending).
    for case in reject_cases() {
        let err = lower_terminal(&case.name, &case.pattern)
            .err()
            .unwrap_or_else(|| panic!("entry point unexpectedly lowered {:?}", case.pattern));
        assert!(
            !is_pending_shape_error(&err),
            "an out-of-shape assertion must reject permanently, not as pending: {err}"
        );
    }
}
