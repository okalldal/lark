//! Error-recovery oracle + behaviour tests (issue #43).
//!
//! lark-rs's panic-mode recovery is single-token-deletion, a token-for-token port
//! of Python Lark's built-in `on_error` driver. The oracle (`recovery/cases.json`,
//! produced by `generate_oracles.py::generate_recovery`) captures, for each input,
//! the tree Python recovers to with `on_error=lambda e: True` and how many tokens
//! it deleted. We assert byte-for-byte tree parity plus the same deletion count.

mod common;

use common::{load_oracle, make_lalr_from_file, tree_matches_oracle};
use lark_rs::{Lark, LarkOptions, LexerType, ParserAlgorithm};

/// The recovery grammar, built LALR (the contextual lexer make_lalr_from_file
/// selects is irrelevant — recovery always lexes with the basic/global lexer).
fn recovery_parser() -> Lark {
    make_lalr_from_file("recovery")
}

#[test]
fn test_recovery_oracle() {
    let lark = recovery_parser();
    let cases = load_oracle("recovery", "cases");
    let cases = cases.as_array().expect("oracle is a JSON array");

    for case in cases {
        let input = case["input"].as_str().unwrap();
        let recovered = case["recovered"].as_bool().unwrap();
        let error_count = case["error_count"].as_u64().unwrap() as usize;

        let result = lark
            .parse_with_recovery(input)
            .unwrap_or_else(|e| panic!("recovery should not hard-error on {input:?}: {e}"));

        assert_eq!(
            result.errors.len(),
            error_count,
            "input {input:?}: deleted {} tokens, oracle deleted {error_count}",
            result.errors.len(),
        );

        if recovered {
            // Python recovered to a full tree — lark-rs must produce the same one.
            tree_matches_oracle(&result.tree, &case["tree"])
                .unwrap_or_else(|e| panic!("input {input:?}: tree mismatch vs oracle: {e}"));
        } else {
            // Premature-EOF: Python re-raises; lark-rs intentionally returns a
            // best-effort partial instead of aborting. Only the recovery itself
            // (a non-empty error list) is asserted, not the partial's shape.
            assert!(
                !result.errors.is_empty(),
                "input {input:?}: expected at least one recovered error"
            );
        }
    }
}

#[test]
fn test_clean_parse_records_no_errors() {
    // A valid input recovers nothing: the tree equals a normal parse and the error
    // list is empty.
    let lark = recovery_parser();
    let result = lark.parse_with_recovery("1 + 2").unwrap();
    assert!(result.errors.is_empty());
    let normal = lark.parse("1 + 2").unwrap();
    assert_eq!(format!("{}", result.tree), format!("{normal}"));
}

#[test]
fn test_on_error_stop_returns_partial() {
    // Returning `false` from the handler stops at the first error and returns the
    // partial tree built so far — without deleting anything further.
    let lark = recovery_parser();
    let mut seen = 0;
    let result = lark
        .parse_on_error("1 + + 2", |_| {
            seen += 1;
            false // stop on the first error
        })
        .unwrap();
    assert_eq!(seen, 1, "handler called exactly once before stopping");
    assert_eq!(result.errors.len(), 1);
}

#[test]
fn test_recovery_never_aborts_on_trailing_error() {
    // The premature-EOF case (`1 + 2 +`) is where Python re-raises. lark-rs returns
    // Ok with a partial tree and the error recorded — the issue's "produce a partial
    // tree on failure rather than aborting".
    let lark = recovery_parser();
    let result = lark.parse_with_recovery("1 + 2 +").unwrap();
    assert_eq!(result.errors.len(), 1);
}

#[test]
fn test_char_level_recovery_records_unexpected_character() {
    // Issue #93: an un-lexable character is skipped (one char at a time) and
    // recorded as an `UnexpectedCharacter` error, rather than aborting. Here the
    // stray `@` is the only error and the surviving `1 + 2` parses to an `add`.
    use lark_rs::ParseError;
    let lark = recovery_parser();
    let result = lark.parse_with_recovery("1 + @ 2").unwrap();
    assert_eq!(
        result.errors.len(),
        1,
        "exactly one un-lexable char skipped"
    );
    match &result.errors[0] {
        ParseError::UnexpectedCharacter { ch, .. } => assert_eq!(*ch, '@'),
        other => panic!("expected UnexpectedCharacter, got {other:?}"),
    }
    // The survivors `1 + 2` still form a valid sum.
    let clean = lark.parse("1 + 2").unwrap();
    assert_eq!(format!("{}", result.tree), format!("{clean}"));
}

#[test]
fn test_char_and_token_deletions_both_counted() {
    // Issue #93: character-level skips and token-level deletions accumulate into
    // one error list. `1 @ 2`: skip the `@` (char), then the surviving `1 2` has a
    // stray NUMBER the parser deletes (token) — two errors total, matching Python.
    let lark = recovery_parser();
    let result = lark.parse_with_recovery("1 @ 2").unwrap();
    assert_eq!(result.errors.len(), 2);
}

#[test]
fn test_on_error_false_stops_at_unlexable_char() {
    // Returning `false` from the handler at an un-lexable position stops lexing
    // there (the lexer-side equivalent of stopping the token loop), returning the
    // tokens collected so far with the single error recorded.
    let lark = recovery_parser();
    let mut seen = 0;
    let result = lark
        .parse_on_error("1 @ 2", |_| {
            seen += 1;
            false
        })
        .unwrap();
    assert_eq!(seen, 1, "handler called once before stopping");
    assert_eq!(result.errors.len(), 1);
}

#[test]
fn test_recovery_unsupported_on_earley() {
    // Recovery is LALR-only; other backends report it clearly rather than silently
    // ignoring the request.
    let lark = Lark::new(
        "start: \"a\"+\n",
        LarkOptions {
            parser: ParserAlgorithm::Earley,
            lexer: LexerType::Basic,
            start: vec!["start".to_string()],
            ..Default::default()
        },
    )
    .unwrap();
    let err = lark.parse_with_recovery("aa").unwrap_err();
    assert!(
        format!("{err}").contains("error recovery requires parser='lalr'"),
        "unexpected error: {err}"
    );
}
