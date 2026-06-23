//! Earley + SPPF oracle tests (Phase 2, Sprint 0).
//!
//! Curated grammars (tools/generate_oracles.py::generate_earley) covering the
//! forest→tree path Earley must get right: an unambiguous grammar (Earley must
//! produce the *same* single tree LALR does), and ambiguous grammars at
//! `ambiguity='resolve'` (one tree) and `ambiguity='explicit'` (an `_ambig`
//! forest, compared unordered by [`common::tree_matches_oracle`]).
//!
//! **Enforced, not self-gated.** Earley is fully implemented (Phase 2 complete), so
//! these oracles always run. The stub-era self-gate that used to `return` early is
//! now a hard assertion (`earley_unimplemented()` must be false): a regression that
//! broke the Earley build would surface as a loud failure here, never a silent skip.

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
    assert!(
        !earley_unimplemented(),
        "Earley backend regressed to 'not yet implemented' — these oracles must run, not skip"
    );

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
