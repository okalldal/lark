//! Core LALR(1) correctness: true LALR lookaheads (BUG-1) and conflict
//! detection with rule-priority resolution (BUG-2), both verified against
//! Python Lark as the oracle.

mod common;

use common::{load_oracle, tree_matches_oracle};
use lark_rs::{Lark, LarkError, LarkOptions, LexerType, ParserAlgorithm};

/// Build a LALR + contextual-lexer parser, surfacing grammar errors instead of
/// panicking (so conflict cases can be asserted).
fn try_build(grammar: &str) -> Result<Lark, LarkError> {
    Lark::new(
        grammar,
        LarkOptions {
            parser: ParserAlgorithm::Lalr,
            lexer: LexerType::Contextual,
            start: vec!["start".to_string()],
            ..Default::default()
        },
    )
}

/// BUG-1: the dangling-else grammar is LALR(1) but not SLR(1). An SLR table
/// reports a spurious shift/reduce conflict on it; with true LALR lookaheads it
/// builds cleanly and parses exactly as Python Lark does.
#[test]
fn test_dangling_else_is_lalr_not_slr() {
    let oracle = load_oracle("lalr_core", "dangling_else");
    let grammar = oracle["grammar"].as_str().expect("oracle grammar");

    let lark = try_build(grammar)
        .expect("dangling-else must build under true LALR(1) (BUG-1)");

    for case in oracle["cases"].as_array().unwrap() {
        let input = case["input"].as_str().unwrap();
        let should_pass = case["should_pass"].as_bool().unwrap();
        let result = lark.parse(input);
        if should_pass {
            let tree = result
                .unwrap_or_else(|e| panic!("expected {input:?} to parse: {e}"));
            tree_matches_oracle(&tree, &case["tree"])
                .unwrap_or_else(|e| panic!("tree mismatch for {input:?}: {e}"));
        } else {
            assert!(result.is_err(), "expected {input:?} to fail to parse");
        }
    }
}

/// BUG-6: requesting the (unimplemented) Earley backend must fail loudly rather
/// than silently substituting LALR, which would accept fewer grammars.
#[test]
fn test_earley_errors_instead_of_silent_fallback() {
    let result = Lark::new(
        "start: \"a\"",
        LarkOptions {
            parser: ParserAlgorithm::Earley,
            ..Default::default()
        },
    );
    match result {
        Ok(_) => panic!("Earley must not silently fall back to LALR (BUG-6)"),
        Err(LarkError::Grammar(lark_rs::GrammarError::Other { msg })) => {
            assert!(msg.contains("Earley"), "unexpected error message: {msg}");
        }
        Err(e) => panic!("expected an Earley-not-implemented error, got: {e}"),
    }
}

/// BUG-2: grammar construction must fail loudly on unresolvable reduce/reduce
/// collisions and resolve them by rule priority — matching Python Lark's
/// raise/no-raise outcome on each grammar.
#[test]
fn test_conflict_detection_matches_oracle() {
    let oracle = load_oracle("lalr_core", "conflicts");

    for case in oracle.as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let grammar = case["grammar"].as_str().unwrap();
        let lark_raised = case["construct_error"].as_bool().unwrap();

        let result = try_build(grammar);
        assert_eq!(
            result.is_err(),
            lark_raised,
            "conflict outcome parity mismatch for {name:?}: \
             rust_errored={}, python_lark_errored={lark_raised}",
            result.is_err(),
        );

        // When we do error, it should be a structured Conflict, not a generic one.
        if lark_raised {
            match result {
                Ok(_) => unreachable!("asserted above"),
                Err(LarkError::Grammar(lark_rs::GrammarError::Conflict { .. })) => {}
                Err(e) => panic!(
                    "expected GrammarError::Conflict for {name:?}, got: {e}"
                ),
            }
        }
    }
}
