//! L2 lowering harness — **layer 3: the Route-1 DFA-equivalence proof**
//! (`docs/LEXER_DFA_PLAN.md`, "Verification harness" + `TERMINAL_REDUCTION_DIAGNOSIS.md`
//! "What a proof of equivalence would require").
//!
//! Generative equivalence (layer 2) is *exhaustive to a bound* — strong evidence,
//! not a proof. Route-1 closes the gap with the **decidable** decision procedure:
//! lower the bounded assertion to a lookaround-free automaton, compile both the
//! lowered terminal and the `fancy-regex` reference to anchored match-DFAs, and
//! decide match-length equality by **product construction** (unequal ⇒ shortest
//! counterexample). A shape is **not "supported" until its representative proof is
//! committed** (the plan's per-shape proof obligation).
//!
//! This is a **skeleton**: the proof needs the lowered automaton, which no shape has
//! yet, so [`prove_route1`] is the pending hook and every proof obligation is
//! `#[ignore]`'d. What is active now is the *obligation registry* — each supported
//! shape has at least one committed representative whose proof must be discharged
//! before the shape ships. That registry is the contract the first-shape session
//! inherits.

mod common;

use lark_rs::{classify, ShapeClass, Verdict};

/// A representative terminal whose Route-1 equivalence must be proven before its
/// shape is declared supported. The bundled six are here by name, plus a synthetic
/// representative per shape.
struct ProofObligation {
    name: &'static str,
    pattern: &'static str,
    shape: ShapeClass,
}

/// The committed proof obligations, one+ per supported shape (the bundled six map
/// onto these). Recognizing `python.STRING`'s nested-leading guard is a first-shape
/// classifier refinement, so STRING is represented here by the *top-level* leading
/// form its lowering must reproduce, not its raw nested pattern.
fn obligations() -> Vec<ProofObligation> {
    vec![
        // Trailing boundary — the bundled OP / DEC_NUMBER guards.
        ProofObligation {
            name: "OP",
            pattern: r"[?](?![a-z])",
            shape: ShapeClass::TrailingBoundary,
        },
        ProofObligation {
            name: "DEC_NUMBER",
            pattern: r"0(?![1-9])",
            shape: ShapeClass::TrailingBoundary,
        },
        // Leading boundary — reserved-word exclusion + the STRING-style opening guard.
        ProofObligation {
            name: "RESERVED",
            pattern: r"(?!if|else)[a-z]+",
            shape: ShapeClass::LeadingBoundary,
        },
        ProofObligation {
            name: "STRING_OPEN",
            pattern: r#"(?!"")[^"]*"#,
            shape: ShapeClass::LeadingBoundary,
        },
        // Bounded lookbehind — the LONG_STRING even-backslash close + a fixed-width
        // lookbehind representative.
        ProofObligation {
            name: "LONG_STRING_CLOSE",
            pattern: r#"a(?<!\\)b"#,
            shape: ShapeClass::BoundedLookbehind,
        },
        ProofObligation {
            name: "FIXED_BEHIND",
            pattern: r"(?<=ab)c",
            shape: ShapeClass::BoundedLookbehind,
        },
    ]
}

/// **Pending proof hook.** Decide Route-1 match-length equivalence between the
/// lowered terminal and the `fancy-regex` reference by product construction.
/// Returns `Ok(())` when proven equivalent, `Err(counterexample)` otherwise. Stubbed
/// to the pending state — there is no lowered automaton to build the product against
/// yet. The first-shape session implements this against the lowered `DfaScanner`
/// (its dense DFA) and `fancy-regex`'s compiled automaton.
fn prove_route1(name: &str, _pattern: &str) -> Result<(), String> {
    Err(format!(
        "Route-1 equivalence proof for `{name}` is not implemented — pending the \
         lowered automaton (docs/LEXER_DFA_PLAN.md L2)"
    ))
}

fn discharge(shape: ShapeClass) {
    let obs: Vec<_> = obligations()
        .into_iter()
        .filter(|o| o.shape == shape)
        .collect();
    assert!(
        !obs.is_empty(),
        "no proof obligation registered for {shape:?}"
    );
    for o in obs {
        prove_route1(o.name, o.pattern)
            .unwrap_or_else(|cex| panic!("Route-1 proof failed for {}: {cex}", o.name));
    }
}

#[test]
#[ignore = "pending first shape — Route-1 proof needs the lowered automaton"]
fn route1_proof_trailing_boundary() {
    discharge(ShapeClass::TrailingBoundary);
}

#[test]
#[ignore = "pending first shape — Route-1 proof needs the lowered automaton"]
fn route1_proof_leading_boundary() {
    discharge(ShapeClass::LeadingBoundary);
}

#[test]
#[ignore = "pending first shape — Route-1 proof needs the lowered automaton"]
fn route1_proof_bounded_lookbehind() {
    discharge(ShapeClass::BoundedLookbehind);
}

/// Active now: the proof-obligation registry is the per-shape contract. Every
/// supported shape has at least one committed representative, and each representative
/// genuinely classifies as its shape (so the obligation targets the right thing).
/// This fails the moment a shape is added without a committed proof representative.
#[test]
fn every_supported_shape_has_a_committed_proof_obligation() {
    for shape in [
        ShapeClass::TrailingBoundary,
        ShapeClass::LeadingBoundary,
        ShapeClass::BoundedLookbehind,
    ] {
        let obs: Vec<_> = obligations()
            .into_iter()
            .filter(|o| o.shape == shape)
            .collect();
        assert!(
            !obs.is_empty(),
            "supported shape {shape:?} has no committed Route-1 proof obligation"
        );
        for o in &obs {
            let c = classify(o.pattern)
                .unwrap_or_else(|e| panic!("classify proof rep {:?} errored: {e}", o.pattern));
            assert!(
                c.assertions
                    .iter()
                    .any(|a| a.verdict() == Verdict::Supported(shape)),
                "proof representative {} ({:?}) does not classify as {shape:?}",
                o.name,
                o.pattern
            );
        }
    }
}
