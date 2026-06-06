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

    let lark = try_build(grammar).expect("dangling-else must build under true LALR(1) (BUG-1)");

    for case in oracle["cases"].as_array().unwrap() {
        let input = case["input"].as_str().unwrap();
        let should_pass = case["should_pass"].as_bool().unwrap();
        let result = lark.parse(input);
        if should_pass {
            let tree = result.unwrap_or_else(|e| panic!("expected {input:?} to parse: {e}"));
            tree_matches_oracle(&tree, &case["tree"])
                .unwrap_or_else(|e| panic!("tree mismatch for {input:?}: {e}"));
        } else {
            assert!(result.is_err(), "expected {input:?} to fail to parse");
        }
    }
}

/// BUG-6 (updated for Phase 3): every requested backend must *build and parse*
/// with its own engine, never silently fall back to another. Earley (Phase 2) and
/// CYK (Phase 3) are both implemented now, so requesting either must use it — the
/// original "no silent fallback" guarantee, now pinned across all three backends.
#[test]
fn test_each_backend_builds_with_its_own_engine() {
    // Earley builds and parses (it accepts even grammars LALR cannot build).
    let earley = Lark::new(
        "start: \"a\"",
        LarkOptions {
            parser: ParserAlgorithm::Earley,
            ..Default::default()
        },
    )
    .expect("Earley backend should build (Phase 2)");
    assert!(
        earley.parse("a").is_ok(),
        "Earley should parse a trivial grammar"
    );

    // CYK builds and parses too (Phase 3).
    let cyk = Lark::new(
        "start: \"a\"",
        LarkOptions {
            parser: ParserAlgorithm::Cyk,
            ..Default::default()
        },
    )
    .expect("CYK backend should build (Phase 3)");
    assert!(cyk.parse("a").is_ok(), "CYK should parse a trivial grammar");
    assert!(
        cyk.parse("b").is_err(),
        "CYK should reject input outside the grammar"
    );
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
                Err(e) => panic!("expected GrammarError::Conflict for {name:?}, got: {e}"),
            }
        }
    }
}

/// The two `TokenSource` frontends — the basic lexer's pre-lexed stream and the
/// contextual lexer's lazy stream — must drive the shared LALR loop to identical
/// trees. This pins the contract that the lexer/parser interface refactor only
/// changes *how* a token is sourced, never the parse result.
#[test]
fn test_basic_and_contextual_lexers_agree() {
    let grammar = r#"
start: list
list: "[" [item ("," item)*] "]"
item: NUMBER | list
NUMBER: /[0-9]+/
%ignore /[ \t]+/
"#;
    let build = |lexer: LexerType| {
        Lark::new(
            grammar,
            LarkOptions {
                parser: ParserAlgorithm::Lalr,
                lexer,
                start: vec!["start".to_string()],
                ..Default::default()
            },
        )
        .expect("grammar builds")
    };
    let basic = build(LexerType::Basic);
    let contextual = build(LexerType::Contextual);

    for input in ["[]", "[1]", "[1, 2, 3]", "[1, [2, 3], [ ]]"] {
        let b = basic
            .parse(input)
            .unwrap_or_else(|e| panic!("basic {input:?}: {e}"));
        let c = contextual
            .parse(input)
            .unwrap_or_else(|e| panic!("contextual {input:?}: {e}"));
        assert_eq!(
            b.to_string(),
            c.to_string(),
            "basic vs contextual disagree on {input:?}"
        );
    }

    // Both frontends must also reject the same malformed input.
    for bad in ["[", "1]", "[1,]", "[1 2]"] {
        assert_eq!(
            basic.parse(bad).is_err(),
            contextual.parse(bad).is_err(),
            "basic vs contextual disagree on rejecting {bad:?}"
        );
    }
}
