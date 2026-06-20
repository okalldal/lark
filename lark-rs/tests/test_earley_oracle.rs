//! Earley + SPPF oracle tests (Phase 2, Sprint 0).
//!
//! Curated grammars (tools/generate_oracles.py::generate_earley) covering the
//! forest→tree path Earley must get right: an unambiguous grammar (Earley must
//! produce the *same* single tree LALR does), and ambiguous grammars at
//! `ambiguity='resolve'` (one tree) and `ambiguity='explicit'` (an `_ambig`
//! forest, compared unordered by [`common::tree_matches_oracle`]).
//!
//! **Self-gating.** Until the Phase-2 engine lands, building an Earley parser
//! returns "not yet implemented", so this test skips itself (see
//! [`common::earley_unimplemented`]). The moment Sprint 1 wires up a real Earley
//! frontend the probe flips and these oracles start being enforced — no edit to
//! this file required. This mirrors the fuzz corpus's self-activating carve-out.

mod common;

use common::{earley_unimplemented, load_oracle, make_earley_mp, tree_matches_oracle};
use lark_rs::Ambiguity;

fn ambiguity_from_str(s: &str) -> Ambiguity {
    match s {
        "explicit" => Ambiguity::Explicit,
        "forest" => Ambiguity::Forest,
        _ => Ambiguity::Resolve,
    }
}

#[test]
fn test_earley_oracle() {
    if earley_unimplemented() {
        eprintln!(
            "Earley backend not implemented yet — skipping Earley oracle tests \
             (Phase 2, Sprint 1+). The harness and oracles are in place; this test \
             will enforce them automatically once Earley builds."
        );
        return;
    }

    let oracle = load_oracle("earley", "cases");
    let groups = oracle
        .as_array()
        .expect("oracle must be an array of groups");

    let mut failures = Vec::new();

    for group in groups {
        let name = group["name"].as_str().unwrap_or("?");
        let grammar = group["grammar"].as_str().unwrap_or("");
        let ambiguity = ambiguity_from_str(group["ambiguity"].as_str().unwrap_or("resolve"));
        let maybe_placeholders = group["maybe_placeholders"].as_bool().unwrap_or(false);

        let lark = match make_earley_mp(grammar, ambiguity.clone(), maybe_placeholders) {
            Ok(l) => l,
            Err(e) => {
                failures.push(format!(
                    "[{name}/{ambiguity:?}] grammar failed to build: {e}"
                ));
                continue;
            }
        };

        for case in group["cases"].as_array().map(Vec::as_slice).unwrap_or(&[]) {
            let input = case["input"].as_str().unwrap_or("");
            let should_parse = case["should_parse"].as_bool().unwrap_or(false);
            let oracle_ok = case["ok"].as_bool().unwrap_or(false);
            let result = lark.parse(input);

            match (should_parse && oracle_ok, &result) {
                (true, Ok(tree)) => {
                    if let Err(msg) = tree_matches_oracle(tree, &case["tree"]) {
                        failures.push(format!("[{name}] input={input:?}: tree mismatch: {msg}"));
                    }
                }
                (true, Err(e)) => {
                    failures.push(format!(
                        "[{name}] input={input:?}: expected parse, got: {e}"
                    ));
                }
                (false, Ok(_)) => {
                    failures.push(format!(
                        "[{name}] input={input:?}: expected parse failure, but it parsed"
                    ));
                }
                (false, Err(_)) => {}
            }
        }
    }

    if !failures.is_empty() {
        panic!("Earley oracle failures:\n{}", failures.join("\n"));
    }
}
