//! Differential tests for the interactive LALR parser (issues #168, #222).
//!
//! Each test replays an oracle trace produced by Python Lark's `InteractiveParser`
//! (via `tools/generate_oracles.py`) and asserts that lark-rs produces identical
//! `accepts()` sets, token sequences, and result trees at every step.

mod common;

use common::{load_oracle, tree_matches_oracle};
use lark_rs::{Lark, LarkOptions, LexerType, ParserAlgorithm};

// ─── Helpers ────────────────────────────────────────────────────────────────

fn load_grammar_file(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/grammars")
        .join(format!("{name}.lark"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("Cannot read {}: {e}", path.display()))
}

fn make_interactive_parser(grammar_name: &str, lexer: &str) -> Lark {
    let text = load_grammar_file(grammar_name);
    let lexer_type = match lexer {
        "basic" => LexerType::Basic,
        "contextual" => LexerType::Contextual,
        other => panic!("unsupported lexer type in oracle: {other}"),
    };
    Lark::new(
        &text,
        LarkOptions {
            parser: ParserAlgorithm::Lalr,
            lexer: lexer_type,
            start: vec!["start".to_string()],
            ..Default::default()
        },
    )
    .unwrap_or_else(|e| panic!("Grammar {grammar_name} failed to build: {e}"))
}

fn json_str_vec(val: &serde_json::Value) -> Vec<String> {
    val.as_array()
        .map(|a| {
            a.iter()
                .map(|v| v.as_str().unwrap_or("").to_string())
                .collect()
        })
        .unwrap_or_default()
}

// ─── Oracle replay: exhaust_lexer cases ─────────────────────────────────────

#[test]
fn test_interactive_exhaust_oracle() {
    let oracle = load_oracle("interactive", "cases");
    let cases = oracle.as_array().expect("oracle must be an array");

    let mut failures = Vec::new();

    for case in cases {
        let name = case["name"].as_str().unwrap_or("?");
        // Skip fork cases (tested separately) and manual-feed cases
        if name.contains("fork") || name.contains("manual") {
            continue;
        }

        let lexer = case["lexer"].as_str().unwrap_or("basic");
        let grammar = case["grammar"].as_str().unwrap_or("arithmetic");
        let text = case["text"].as_str().unwrap_or("");

        let lark = make_interactive_parser(grammar, lexer);
        let mut p = match lark.parse_interactive(text) {
            Ok(p) => p,
            Err(e) => {
                failures.push(format!("{name}: parse_interactive failed: {e}"));
                continue;
            }
        };

        // Check initial accepts
        let expected_initial = json_str_vec(&case["initial_accepts"]);
        let actual_initial = p.accepts();
        if actual_initial != expected_initial {
            failures.push(format!(
                "{name}: initial accepts mismatch:\n  expected: {expected_initial:?}\n  actual:   {actual_initial:?}"
            ));
        }

        // exhaust_lexer
        let tokens = match p.exhaust_lexer() {
            Ok(t) => t,
            Err(e) => {
                failures.push(format!("{name}: exhaust_lexer failed: {e}"));
                continue;
            }
        };

        // Check tokens fed
        let expected_steps = case["steps"].as_array().expect("steps must be array");
        if tokens.len() != expected_steps.len() {
            failures.push(format!(
                "{name}: token count mismatch: expected {}, got {}",
                expected_steps.len(),
                tokens.len()
            ));
        } else {
            for (i, (tok, step)) in tokens.iter().zip(expected_steps.iter()).enumerate() {
                let exp_term = step["terminal"].as_str().unwrap_or("?");
                let exp_val = step["value"].as_str().unwrap_or("?");
                if tok.type_ != exp_term {
                    failures.push(format!(
                        "{name}: token[{i}] type mismatch: expected {exp_term:?}, got {:?}",
                        tok.type_
                    ));
                }
                if tok.value != exp_val {
                    failures.push(format!(
                        "{name}: token[{i}] value mismatch: expected {exp_val:?}, got {:?}",
                        tok.value
                    ));
                }
            }
        }

        // Check accepts after exhaust
        if let Some(expected_after) = case.get("accepts_after_exhaust") {
            let expected = json_str_vec(expected_after);
            let actual = p.accepts();
            if actual != expected {
                failures.push(format!(
                    "{name}: accepts_after_exhaust mismatch:\n  expected: {expected:?}\n  actual:   {actual:?}"
                ));
            }
        }

        // feed_eof and check result tree
        match p.feed_eof() {
            Ok(Some(tree)) => {
                if let Some(oracle_result) = case.get("result") {
                    if !oracle_result.is_null() {
                        if let Err(msg) = tree_matches_oracle(&tree, oracle_result) {
                            failures.push(format!("{name}: tree mismatch: {msg}"));
                        }
                    }
                }
            }
            Ok(None) => {
                failures.push(format!("{name}: feed_eof returned None (expected a tree)"));
            }
            Err(e) => {
                failures.push(format!("{name}: feed_eof failed: {e}"));
            }
        }
    }

    if !failures.is_empty() {
        panic!(
            "Interactive exhaust oracle failures ({}):\n{}",
            failures.len(),
            failures.join("\n")
        );
    }
}

// ─── Oracle replay: manual feed cases ───────────────────────────────────────

#[test]
fn test_interactive_manual_feed_oracle() {
    let oracle = load_oracle("interactive", "cases");
    let cases = oracle.as_array().expect("oracle must be an array");

    let mut failures = Vec::new();

    for case in cases {
        let name = case["name"].as_str().unwrap_or("?");
        if !name.contains("manual") {
            continue;
        }

        let lexer = case["lexer"].as_str().unwrap_or("basic");
        let grammar = case["grammar"].as_str().unwrap_or("arithmetic");
        let text = case["text"].as_str().unwrap_or("");

        let lark = make_interactive_parser(grammar, lexer);
        let mut p = match lark.parse_interactive(text) {
            Ok(p) => p,
            Err(e) => {
                failures.push(format!("{name}: parse_interactive failed: {e}"));
                continue;
            }
        };

        // Check initial accepts
        let expected_initial = json_str_vec(&case["initial_accepts"]);
        let actual_initial = p.accepts();
        if actual_initial != expected_initial {
            failures.push(format!(
                "{name}: initial accepts mismatch:\n  expected: {expected_initial:?}\n  actual:   {actual_initial:?}"
            ));
        }

        // Feed each step
        let steps = case["steps"].as_array().expect("steps must be array");
        for (i, step) in steps.iter().enumerate() {
            let terminal = step["terminal"].as_str().unwrap_or("?");
            let value = step["value"].as_str().unwrap_or("?");

            // Check accepts_before
            if let Some(expected_before) = step.get("accepts_before") {
                let expected = json_str_vec(expected_before);
                let actual = p.accepts();
                if actual != expected {
                    failures.push(format!(
                        "{name}: step[{i}] accepts_before mismatch:\n  expected: {expected:?}\n  actual:   {actual:?}"
                    ));
                }
            }

            match p.feed(terminal, value) {
                Ok(_) => {}
                Err(e) => {
                    failures.push(format!(
                        "{name}: step[{i}] feed({terminal:?}, {value:?}) failed: {e}"
                    ));
                    break;
                }
            }

            // Check accepts_after
            if let Some(expected_after) = step.get("accepts_after") {
                let expected = json_str_vec(expected_after);
                let actual = p.accepts();
                if actual != expected {
                    failures.push(format!(
                        "{name}: step[{i}] accepts_after mismatch:\n  expected: {expected:?}\n  actual:   {actual:?}"
                    ));
                }
            }
        }

        // feed_eof
        match p.feed_eof() {
            Ok(Some(tree)) => {
                if let Some(oracle_result) = case.get("result") {
                    if !oracle_result.is_null() {
                        if let Err(msg) = tree_matches_oracle(&tree, oracle_result) {
                            failures.push(format!("{name}: tree mismatch: {msg}"));
                        }
                    }
                }
            }
            Ok(None) => {
                failures.push(format!("{name}: feed_eof returned None (expected a tree)"));
            }
            Err(e) => {
                failures.push(format!("{name}: feed_eof failed: {e}"));
            }
        }

        // Check final_accepts (after eof, should be empty)
        if let Some(expected_final) = case.get("final_accepts") {
            let expected = json_str_vec(expected_final);
            let actual = p.accepts();
            if actual != expected {
                failures.push(format!(
                    "{name}: final_accepts mismatch:\n  expected: {expected:?}\n  actual:   {actual:?}"
                ));
            }
        }
    }

    if !failures.is_empty() {
        panic!(
            "Interactive manual-feed oracle failures ({}):\n{}",
            failures.len(),
            failures.join("\n")
        );
    }
}

// ─── Oracle replay: fork cases ──────────────────────────────────────────────

#[test]
fn test_interactive_fork_oracle() {
    let oracle = load_oracle("interactive", "cases");
    let cases = oracle.as_array().expect("oracle must be an array");

    let mut failures = Vec::new();

    for case in cases {
        let name = case["name"].as_str().unwrap_or("?");
        if !name.contains("fork") {
            continue;
        }

        let lexer = case["lexer"].as_str().unwrap_or("basic");
        let grammar = case["grammar"].as_str().unwrap_or("arithmetic");
        let text = case["text"].as_str().unwrap_or("");

        let lark = make_interactive_parser(grammar, lexer);
        let mut p = match lark.parse_interactive(text) {
            Ok(p) => p,
            Err(e) => {
                failures.push(format!("{name}: parse_interactive failed: {e}"));
                continue;
            }
        };

        // exhaust_lexer
        if let Err(e) = p.exhaust_lexer() {
            failures.push(format!("{name}: exhaust_lexer failed: {e}"));
            continue;
        }

        // Fork
        let mut fork = p.fork();

        // Check accepts on both
        if let Some(expected) = case.get("main_accepts_before_eof") {
            let exp = json_str_vec(expected);
            let main_accepts = p.accepts();
            if main_accepts != exp {
                failures.push(format!(
                    "{name}: main accepts_before_eof mismatch:\n  expected: {exp:?}\n  actual:   {main_accepts:?}"
                ));
            }
        }
        if let Some(expected) = case.get("fork_accepts_before_eof") {
            let exp = json_str_vec(expected);
            let fork_accepts = fork.accepts();
            if fork_accepts != exp {
                failures.push(format!(
                    "{name}: fork accepts_before_eof mismatch:\n  expected: {exp:?}\n  actual:   {fork_accepts:?}"
                ));
            }
        }

        // Feed eof on both independently
        match p.feed_eof() {
            Ok(Some(tree)) => {
                if let Some(oracle_result) = case.get("main_result") {
                    if !oracle_result.is_null() {
                        if let Err(msg) = tree_matches_oracle(&tree, oracle_result) {
                            failures.push(format!("{name}: main tree mismatch: {msg}"));
                        }
                    }
                }
            }
            Ok(None) => {
                failures.push(format!("{name}: main feed_eof returned None"));
            }
            Err(e) => {
                failures.push(format!("{name}: main feed_eof failed: {e}"));
            }
        }
        match fork.feed_eof() {
            Ok(Some(tree)) => {
                if let Some(oracle_result) = case.get("fork_result") {
                    if !oracle_result.is_null() {
                        if let Err(msg) = tree_matches_oracle(&tree, oracle_result) {
                            failures.push(format!("{name}: fork tree mismatch: {msg}"));
                        }
                    }
                }
            }
            Ok(None) => {
                failures.push(format!("{name}: fork feed_eof returned None"));
            }
            Err(e) => {
                failures.push(format!("{name}: fork feed_eof failed: {e}"));
            }
        }
    }

    if !failures.is_empty() {
        panic!(
            "Interactive fork oracle failures ({}):\n{}",
            failures.len(),
            failures.join("\n")
        );
    }
}

// ─── Relative-oracle property tests ─────────────────────────────────────────
//
// These do not need a Python oracle — they test structural properties of the
// interactive parser that must hold regardless of the specific grammar.

/// An interactive parse that feeds the same tokens `resume` would feed must
/// produce the same tree as a batch `parse`.
#[test]
fn test_interactive_resume_matches_batch_basic() {
    let grammar = load_grammar_file("arithmetic");
    let lark = Lark::new(
        &grammar,
        LarkOptions {
            parser: ParserAlgorithm::Lalr,
            lexer: LexerType::Basic,
            start: vec!["start".to_string()],
            ..Default::default()
        },
    )
    .unwrap();

    for input in ["1 + 2", "1 + 2 * 3", "(1 + 2) * 3", "-1", "42"] {
        let batch = lark.parse(input).unwrap();
        let interactive = lark.parse_interactive(input).unwrap();
        let resumed = interactive.resume().unwrap();
        assert_eq!(
            format!("{batch:?}"),
            format!("{resumed:?}"),
            "batch vs resume mismatch on {input:?}"
        );
    }
}

/// Same property, contextual lexer.
#[test]
fn test_interactive_resume_matches_batch_contextual() {
    let grammar = load_grammar_file("recovery_contextual");
    let lark = Lark::new(
        &grammar,
        LarkOptions {
            parser: ParserAlgorithm::Lalr,
            lexer: LexerType::Contextual,
            start: vec!["start".to_string()],
            ..Default::default()
        },
    )
    .unwrap();

    for input in ["[hello] {foo}", "[a b c] {x y z}", "[one two] {three}"] {
        let batch = lark.parse(input).unwrap();
        let interactive = lark.parse_interactive(input).unwrap();
        let resumed = interactive.resume().unwrap();
        assert_eq!(
            format!("{batch:?}"),
            format!("{resumed:?}"),
            "batch vs resume mismatch on {input:?}"
        );
    }
}

/// `accepts()` is empty after a successful `feed_eof`.
#[test]
fn test_accepts_empty_after_accept() {
    let grammar = load_grammar_file("arithmetic");
    let lark = Lark::new(
        &grammar,
        LarkOptions {
            parser: ParserAlgorithm::Lalr,
            lexer: LexerType::Basic,
            start: vec!["start".to_string()],
            ..Default::default()
        },
    )
    .unwrap();

    let mut p = lark.parse_interactive("1").unwrap();
    p.exhaust_lexer().unwrap();
    p.feed_eof().unwrap();
    assert!(
        p.accepts().is_empty(),
        "accepts() must be empty after ACCEPT"
    );
    assert!(p.result().is_some(), "result() must be Some after ACCEPT");
}

/// Feeding after ACCEPT errors (nothing is acceptable).
#[test]
fn test_feed_after_accept_errors() {
    let grammar = load_grammar_file("arithmetic");
    let lark = Lark::new(
        &grammar,
        LarkOptions {
            parser: ParserAlgorithm::Lalr,
            lexer: LexerType::Basic,
            start: vec!["start".to_string()],
            ..Default::default()
        },
    )
    .unwrap();

    let mut p = lark.parse_interactive("1").unwrap();
    p.exhaust_lexer().unwrap();
    p.feed_eof().unwrap();
    assert!(
        p.feed("NUMBER", "2").is_err(),
        "feeding after ACCEPT must error"
    );
}

/// `fork()` produces an independent cursor: feeding one doesn't affect the other.
#[test]
fn test_fork_independence() {
    let grammar = load_grammar_file("arithmetic");
    let lark = Lark::new(
        &grammar,
        LarkOptions {
            parser: ParserAlgorithm::Lalr,
            lexer: LexerType::Basic,
            start: vec!["start".to_string()],
            ..Default::default()
        },
    )
    .unwrap();

    let mut p = lark.parse_interactive("").unwrap();
    p.feed("NUMBER", "1").unwrap();
    let accepts_before_fork = p.accepts();

    let mut fork = p.fork();
    // Feed different tokens on each
    p.feed("PLUS", "+").unwrap();
    p.feed("NUMBER", "2").unwrap();

    fork.feed("STAR", "*").unwrap();
    fork.feed("NUMBER", "3").unwrap();

    // Both should still accept the same set of terminals at their respective
    // states (both just fed a number after an operator)
    let p_accepts = p.accepts();
    let fork_accepts = fork.accepts();
    assert_eq!(
        p_accepts, fork_accepts,
        "after feeding number-after-op, both should accept the same set"
    );
    assert_eq!(p_accepts, accepts_before_fork);

    // But the results should differ
    let r_p = p.feed_eof().unwrap().unwrap();
    let r_fork = fork.feed_eof().unwrap().unwrap();
    assert_ne!(
        format!("{r_p:?}"),
        format!("{r_fork:?}"),
        "fork results must differ (1+2 vs 1*3)"
    );
}

/// The contextual lexer correctly types AWORD vs BWORD by parser state.
/// This is the load-bearing property of #222.
#[test]
fn test_contextual_lexer_types_by_state() {
    let grammar = load_grammar_file("recovery_contextual");
    let lark = Lark::new(
        &grammar,
        LarkOptions {
            parser: ParserAlgorithm::Lalr,
            lexer: LexerType::Contextual,
            start: vec!["start".to_string()],
            ..Default::default()
        },
    )
    .unwrap();

    let mut p = lark.parse_interactive("[hello] {world}").unwrap();
    let tokens = p.exhaust_lexer().unwrap();

    // The contextual lexer must type the same pattern as different terminals
    // depending on parser state: "hello" -> AWORD inside [...], "world" -> BWORD
    // inside {...}.
    let token_types: Vec<&str> = tokens.iter().map(|t| t.type_.as_str()).collect();
    assert!(
        token_types.contains(&"AWORD"),
        "contextual lexer must produce AWORD: {token_types:?}"
    );
    assert!(
        token_types.contains(&"BWORD"),
        "contextual lexer must produce BWORD: {token_types:?}"
    );

    // Verify exact sequence
    assert_eq!(
        token_types,
        vec!["LSQB", "AWORD", "RSQB", "LBRACE", "BWORD", "RBRACE"],
        "contextual token sequence"
    );
}

/// `pretty()` returns a non-empty debug string.
#[test]
fn test_pretty() {
    let grammar = load_grammar_file("arithmetic");
    let lark = Lark::new(
        &grammar,
        LarkOptions {
            parser: ParserAlgorithm::Lalr,
            lexer: LexerType::Basic,
            start: vec!["start".to_string()],
            ..Default::default()
        },
    )
    .unwrap();

    let p = lark.parse_interactive("1 + 2").unwrap();
    let pretty = p.pretty();
    assert!(
        pretty.contains("InteractiveParser"),
        "pretty() should contain 'InteractiveParser': {pretty:?}"
    );
    assert!(
        pretty.contains("accepts"),
        "pretty() should contain 'accepts': {pretty:?}"
    );
}

/// LALR (basic and contextual, without postlex) supports interactive parsing.
#[test]
fn test_interactive_supported_lalr() {
    let grammar = "start: \"hello\"";
    for lexer in [LexerType::Basic, LexerType::Contextual] {
        let lark = Lark::new(
            grammar,
            LarkOptions {
                parser: ParserAlgorithm::Lalr,
                lexer: lexer.clone(),
                start: vec!["start".to_string()],
                ..Default::default()
            },
        )
        .unwrap();
        assert!(
            lark.parse_interactive("hello").is_ok(),
            "LALR + {lexer:?} should support parse_interactive"
        );
    }
}

/// Earley returns a typed error (not a panic) for parse_interactive.
#[test]
fn test_interactive_unsupported_earley() {
    let grammar = "start: \"hello\"";
    let lark = Lark::new(
        grammar,
        LarkOptions {
            parser: ParserAlgorithm::Earley,
            lexer: LexerType::Basic,
            start: vec!["start".to_string()],
            ..Default::default()
        },
    )
    .unwrap();

    match lark.parse_interactive("hello") {
        Ok(_) => panic!("Earley must refuse parse_interactive"),
        Err(e) => {
            let msg = e.to_string();
            assert!(
                msg.contains("interactive") && msg.contains("lalr"),
                "Earley error should mention interactive + lalr, got: {msg}"
            );
        }
    }
}

/// CYK returns a typed error (not a panic) for parse_interactive.
#[test]
fn test_interactive_unsupported_cyk() {
    let grammar = "start: \"hello\"";
    let lark = Lark::new(
        grammar,
        LarkOptions {
            parser: ParserAlgorithm::Cyk,
            lexer: LexerType::Basic,
            start: vec!["start".to_string()],
            ..Default::default()
        },
    )
    .unwrap();

    match lark.parse_interactive("hello") {
        Ok(_) => panic!("CYK must refuse parse_interactive"),
        Err(e) => {
            let msg = e.to_string();
            assert!(
                msg.contains("interactive") && msg.contains("lalr"),
                "CYK error should mention interactive + lalr, got: {msg}"
            );
        }
    }
}

/// LALR + postlex (Indenter) returns a typed error for parse_interactive.
#[test]
fn test_interactive_unsupported_lalr_postlex() {
    use lark_rs::Indenter;

    // Use the real indenter grammar that the oracle tests rely on.
    let grammar = load_grammar_file("indent");
    let lark = Lark::new(
        &grammar,
        LarkOptions {
            parser: ParserAlgorithm::Lalr,
            lexer: LexerType::Basic,
            start: vec!["start".to_string()],
            postlex: Some(Indenter {
                nl_type: "_NL".to_string(),
                open_paren_types: vec![],
                close_paren_types: vec![],
                indent_type: "_INDENT".to_string(),
                dedent_type: "_DEDENT".to_string(),
                tab_len: 8,
            }),
            ..Default::default()
        },
    )
    .unwrap();

    match lark.parse_interactive("hello\n") {
        Ok(_) => panic!("LALR + postlex must refuse parse_interactive"),
        Err(e) => {
            let msg = e.to_string();
            assert!(
                msg.contains("interactive") && msg.contains("postlex"),
                "LALR+postlex error should mention interactive + postlex, got: {msg}"
            );
        }
    }
}

/// `parse_interactive_with_start` selects an alternative start symbol.
#[test]
fn test_interactive_with_start() {
    let grammar = r#"
        start: A
        other: B
        A: /a+/
        B: /b+/
    "#;
    let lark = Lark::new(
        grammar,
        LarkOptions {
            parser: ParserAlgorithm::Lalr,
            lexer: LexerType::Basic,
            start: vec!["start".to_string(), "other".to_string()],
            ..Default::default()
        },
    )
    .unwrap();

    // Explicit start "start": expects "a"
    let p = lark.parse_interactive_with_start("a", "start").unwrap();
    let res = p.resume();
    assert!(res.is_ok(), "'start' should parse 'a': {:?}", res.err());

    // Explicit start "other": expects "b"
    let p2 = lark.parse_interactive_with_start("b", "other").unwrap();
    let res2 = p2.resume();
    assert!(
        res2.is_ok(),
        "'other' start should parse 'b': {:?}",
        res2.err()
    );

    // Cross-check: "a" fails under "other" start
    let p3 = lark.parse_interactive_with_start("a", "other").unwrap();
    assert!(p3.resume().is_err(), "'a' should fail under 'other' start");
}
