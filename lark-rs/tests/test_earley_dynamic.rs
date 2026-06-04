//! Earley dynamic-lexer oracle tests (Phase 2, Sprint 5).
//!
//! Curated grammars (tools/generate_oracles.py::generate_earley_dynamic) for the
//! one Phase-2 engine path the basic lexer cannot cover: the **dynamic lexer**,
//! where scanning is folded into the Earley loop so the terminals tried at each
//! position are exactly those the parser predicts. Covers overlapping terminals
//! (`dynamic` greedy vs `dynamic_complete` all-segmentations), `%ignore` through
//! the dynamic scanner, and context-decided keyword/identifier tokenization.
//!
//! Self-gates on [`common::earley_unimplemented`] exactly like the basic-lexer
//! Earley oracle, so it enforces the moment the engine builds.

mod common;

use common::{earley_unimplemented, load_oracle, make_earley_dynamic, tree_matches_oracle};
use lark_rs::Ambiguity;

fn ambiguity_from_str(s: &str) -> Ambiguity {
    match s {
        "explicit" => Ambiguity::Explicit,
        "forest" => Ambiguity::Forest,
        _ => Ambiguity::Resolve,
    }
}

#[test]
fn test_earley_dynamic_oracle() {
    if earley_unimplemented() {
        eprintln!("Earley backend not implemented yet — skipping dynamic-lexer oracle tests");
        return;
    }

    let oracle = load_oracle("earley", "dynamic_cases");
    let groups = oracle
        .as_array()
        .expect("oracle must be an array of groups");

    let mut failures = Vec::new();

    for group in groups {
        let name = group["name"].as_str().unwrap_or("?");
        let grammar = group["grammar"].as_str().unwrap_or("");
        let lexer = group["lexer"].as_str().unwrap_or("dynamic");
        let ambiguity = ambiguity_from_str(group["ambiguity"].as_str().unwrap_or("resolve"));

        let lark = match make_earley_dynamic(grammar, lexer, ambiguity.clone()) {
            Ok(l) => l,
            Err(e) => {
                failures.push(format!(
                    "[{name}/{lexer}/{ambiguity:?}] grammar failed to build: {e}"
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
                        failures.push(format!(
                            "[{name}/{lexer}] input={input:?}: tree mismatch: {msg}"
                        ));
                    }
                }
                (true, Err(e)) => {
                    failures.push(format!(
                        "[{name}/{lexer}] input={input:?}: expected parse, got: {e}"
                    ));
                }
                (false, Ok(_)) => {
                    failures.push(format!(
                        "[{name}/{lexer}] input={input:?}: expected parse failure, but it parsed"
                    ));
                }
                (false, Err(_)) => {}
            }
        }
    }

    if !failures.is_empty() {
        panic!(
            "Earley dynamic-lexer oracle failures:\n{}",
            failures.join("\n")
        );
    }
}
