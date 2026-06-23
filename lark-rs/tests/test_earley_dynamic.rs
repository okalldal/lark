//! Earley dynamic-lexer oracle tests (Phase 2, Sprint 5).
//!
//! Curated grammars (tools/generate_oracles.py::generate_earley_dynamic) for the
//! one Phase-2 engine path the basic lexer cannot cover: the **dynamic lexer**,
//! where scanning is folded into the Earley loop so the terminals tried at each
//! position are exactly those the parser predicts. Covers overlapping terminals
//! (`dynamic` greedy vs `dynamic_complete` all-segmentations), `%ignore` through
//! the dynamic scanner, and context-decided keyword/identifier tokenization.
//!
//! Earley is fully implemented, so these always run; the stub-era self-gate is now
//! a hard assertion (`earley_unimplemented()` must be false) — a backend regression
//! fails loudly here instead of silently skipping.

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
    assert!(
        !earley_unimplemented(),
        "Earley backend regressed to 'not yet implemented' — dynamic-lexer oracles must run"
    );

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

/// The flattened `(token_type, value)` of a resolved `start` tree — both canonical
/// `dynamic_complete` resolve cases produce exactly two terminal children.
fn token_pairs(lark: &lark_rs::Lark, input: &str) -> Vec<(String, String)> {
    let tree = match lark.parse(input).expect("should parse") {
        lark_rs::ParseTree::Tree(t) => t,
        other => panic!("expected a `start` tree, got {other:?}"),
    };
    tree.children
        .iter()
        .map(|c| {
            let t = c.as_token().expect("child is a token");
            (t.type_.clone(), t.value.clone())
        })
        .collect()
}

/// #91/#32 regression pin: the `dynamic_complete` resolve cases the old
/// `sorted_families` split-point tie-break protected (`parse:49 / 72`) must still
/// resolve correctly with the heuristic **removed** — now purely via the inlined
/// recurse rule's `rule.order` + insertion order. Were the tie-break still load-
/// bearing, removing it would flip these to the wrong (shorter-first) segmentation.
#[test]
fn dynamic_complete_resolves_longest_segmentation_without_tiebreak() {
    assert!(
        !earley_unimplemented(),
        "Earley backend regressed to 'not yet implemented'"
    );
    // parse:49 — `(A | WORD)+` over "abc": one `A "a"`, then `WORD "bc"` (earliest
    // split first), NOT `A "a"`, `WORD "b"`, `WORD "c"`.
    let g49 = make_earley_dynamic(
        "A.2: \"a\"\nWORD: (\"a\"..\"z\")+\nstart: (A | WORD)+\n",
        "dynamic_complete",
        Ambiguity::Resolve,
    )
    .expect("grammar 49 builds");
    assert_eq!(
        token_pairs(&g49, "abc"),
        vec![
            ("A".to_string(), "a".to_string()),
            ("WORD".to_string(), "bc".to_string()),
        ]
    );

    // parse:72 — `A A?` with `A: "a"+` over "aaa": `A "a"` then `A "aa"` (the
    // order-0 `A A` expansion, earliest split first).
    let g72 = make_earley_dynamic(
        "start: A A?\nA: \"a\"+\n",
        "dynamic_complete",
        Ambiguity::Resolve,
    )
    .expect("grammar 72 builds");
    assert_eq!(
        token_pairs(&g72, "aaa"),
        vec![
            ("A".to_string(), "a".to_string()),
            ("A".to_string(), "aa".to_string()),
        ]
    );
}
