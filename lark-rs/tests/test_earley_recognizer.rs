//! Earley recognizer accept/reject parity (Phase 2, Sprint 1).
//!
//! Sprint 1 lands the Earley *recognizer* — boolean accept/reject, no parse
//! forest yet. The tree-comparing oracle tests (`test_earley_oracle`,
//! `test_earley_compliance`) only activate once the Earley *frontend* produces
//! trees (Sprint 2), so they stay gated for now. This test exercises the
//! recognizer directly through [`common::make_earley_recognizer`], asserting it
//! accepts exactly the inputs Python Lark accepts — the Sprint 1 exit criterion:
//! accept/reject parity on the Sprint-0 curated grammars (unambiguous + the two
//! ambiguous ones) plus the existing unambiguous JSON and arithmetic grammars.
//!
//! Ambiguity does not change the *language* a grammar accepts, only the forest it
//! builds — so an ambiguous grammar must still recognize exactly what the oracle
//! recognizes here; the trees are Sprints 2–4.

mod common;

use common::{
    earley_accepts, load_oracle, make_earley_recognizer, make_earley_recognizer_from_file,
};
use serde_json::Value;

/// Expected acceptance for an oracle case, or `None` when the oracle itself did
/// not produce a clean verdict (Python Lark errored on a case it was meant to
/// pass — a known oracle limitation that is skipped, mirroring `test_oracle.rs`).
///
/// Handles both field spellings: `should_parse` (the Earley cases) and
/// `should_pass` (the arithmetic/JSON cases).
fn expected_accept(case: &Value) -> Option<bool> {
    let should = case["should_parse"]
        .as_bool()
        .or_else(|| case["should_pass"].as_bool())
        .unwrap_or(false);
    let ok = case["ok"].as_bool().unwrap_or(false);
    match (should, ok) {
        (true, true) => Some(true),    // oracle accepts → recognizer must accept
        (false, false) => Some(false), // oracle rejects → recognizer must reject
        _ => None,                     // oracle had no clean verdict → skip
    }
}

#[test]
fn test_earley_recognizer_curated() {
    // The Sprint-0 curated Earley grammars: an unambiguous arithmetic-style
    // grammar, a root-ambiguous grammar (`!start: start start | "a"`), and a
    // nested-ambiguous grammar. Each appears twice (resolve / explicit); the
    // recognizer ignores ambiguity mode, so both must agree with the oracle.
    let oracle = load_oracle("earley", "cases");
    let mut failures = Vec::new();

    for group in oracle.as_array().expect("array of groups") {
        let name = group["name"].as_str().unwrap_or("?");
        let ambiguity = group["ambiguity"].as_str().unwrap_or("resolve");
        let grammar = group["grammar"].as_str().unwrap_or("");
        let (parser, lexer) = make_earley_recognizer(grammar);

        for case in group["cases"].as_array().map(Vec::as_slice).unwrap_or(&[]) {
            let Some(expected) = expected_accept(case) else {
                continue;
            };
            let input = case["input"].as_str().unwrap_or("");
            let got = earley_accepts(&parser, &lexer, input);
            if got != expected {
                failures.push(format!(
                    "[{name}/{ambiguity}] input={input:?}: recognizer={got}, oracle={expected}"
                ));
            }
        }
    }

    assert!(
        failures.is_empty(),
        "Earley recognizer parity failures:\n{}",
        failures.join("\n")
    );
}

/// Accept/reject parity on an existing unambiguous grammar file: Earley must
/// accept exactly what the oracle accepts — the same language LALR handles.
fn check_grammar_file(name: &str) {
    let (parser, lexer) = make_earley_recognizer_from_file(name);
    let oracle = load_oracle(name, "cases");
    let mut failures = Vec::new();

    for case in oracle.as_array().expect("array of cases") {
        let Some(expected) = expected_accept(case) else {
            continue;
        };
        let input = case["input"].as_str().unwrap_or("");
        let got = earley_accepts(&parser, &lexer, input);
        if got != expected {
            failures.push(format!(
                "[{name}] input={input:?}: recognizer={got}, oracle={expected}"
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "Earley recognizer parity failures:\n{}",
        failures.join("\n")
    );
}

#[test]
fn test_earley_recognizer_arithmetic() {
    check_grammar_file("arithmetic");
}

#[test]
fn test_earley_recognizer_json() {
    check_grammar_file("json");
}
