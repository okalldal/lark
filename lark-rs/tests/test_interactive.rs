//! Interactive-parser oracle + behaviour tests (issue #168).
//!
//! lark-rs's `InteractiveParser` is a 1:1 port of Python Lark's
//! (`lark/parsers/lalr_interactive_parser.py`), so it is oracle-checkable: the
//! `interactive/cases.json` bank records, for a script of operations driven against
//! Python, the sorted `accepts()` set after each step, each feed's success /
//! expected-set, and the final tree. This test replays the same script and asserts
//! the same trace — a step-granular differential. Plus three relative-oracle
//! property tests that need no Python (resume == parse, exhaust+eof == parse,
//! `accepts()` honesty).

mod common;

use common::{load_oracle, tree_matches_oracle};
use lark_rs::{Lark, LarkOptions, LexerType, ParserAlgorithm};

/// Build the interactive grammar LALR + **basic lexer** (interactive v1's
/// configuration; the manual feed surface is lexer-independent, but be explicit).
fn interactive_lark() -> Lark {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/grammars/interactive.lark");
    let text = std::fs::read_to_string(&path).expect("read interactive.lark");
    Lark::new(
        &text,
        LarkOptions {
            parser: ParserAlgorithm::Lalr,
            lexer: LexerType::Basic,
            start: vec!["start".to_string()],
            ..Default::default()
        },
    )
    .expect("interactive.lark loads")
}

#[test]
fn test_interactive_oracle() {
    let lark = interactive_lark();
    let cases = load_oracle("interactive", "cases");
    let cases = cases.as_array().expect("oracle is a JSON array");

    for case in cases {
        let name = case["name"].as_str().unwrap();
        // A fresh interactive parse per case; manual feeds ignore the (empty) input.
        let mut ip = lark
            .parse_interactive("")
            .unwrap_or_else(|e| panic!("case {name}: parse_interactive failed: {e}"));

        for step in case["steps"].as_array().unwrap() {
            match step["op"].as_str().unwrap() {
                "accepts" => {
                    let want: Vec<String> = step["accepts"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|v| v.as_str().unwrap().to_string())
                        .collect();
                    assert_eq!(
                        ip.accepts(),
                        want,
                        "case {name}: accepts() mismatch vs oracle"
                    );
                }
                "feed" => {
                    let ty = step["type"].as_str().unwrap();
                    let val = step["value"].as_str().unwrap();
                    let want_ok = step["ok"].as_bool().unwrap();
                    let res = ip.feed(ty, val);
                    assert_eq!(
                        res.is_ok(),
                        want_ok,
                        "case {name}: feed {ty:?} ok mismatch (got {res:?})"
                    );
                    if !want_ok {
                        assert_expected(&res.unwrap_err(), &step["expected"], name, ty);
                    }
                }
                "feed_eof" => {
                    let want_ok = step["ok"].as_bool().unwrap();
                    let res = ip.feed_eof();
                    assert_eq!(res.is_ok(), want_ok, "case {name}: feed_eof ok mismatch");
                    if want_ok {
                        let tree = res.unwrap().expect("feed_eof reached ACCEPT → Some(tree)");
                        tree_matches_oracle(&tree, &case["tree"])
                            .unwrap_or_else(|e| panic!("case {name}: tree mismatch: {e}"));
                    } else {
                        assert_expected(&res.unwrap_err(), &step["expected"], name, "$END");
                    }
                }
                other => panic!("case {name}: unknown op {other:?}"),
            }
        }
    }
}

/// The `expected` set carried by an `UnexpectedToken`/`UnexpectedEof` must match the
/// oracle's sorted expected set.
fn assert_expected(err: &lark_rs::ParseError, oracle: &serde_json::Value, name: &str, at: &str) {
    use lark_rs::ParseError::*;
    let mut got = match err {
        UnexpectedToken { expected, .. } | UnexpectedEof { expected, .. } => expected.clone(),
        other => panic!("case {name}: feed {at:?} gave unexpected error kind {other:?}"),
    };
    got.sort();
    let want: Vec<String> = oracle
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert_eq!(got, want, "case {name}: expected-set mismatch at {at:?}");
}

// ─── Relative oracles (no Python needed) ─────────────────────────────────────

/// `parse_interactive(text).resume()` must equal a normal `parse(text)` for valid
/// input — re-grounds the whole interactive path against the already-oracle'd parser.
#[test]
fn test_resume_equals_parse() {
    let lark = interactive_lark();
    for input in ["7", "1 + 2", "1 + 2 + 3", "10 + 20"] {
        let interactive = lark
            .parse_interactive(input)
            .unwrap()
            .resume()
            .unwrap_or_else(|e| panic!("resume failed on {input:?}: {e}"));
        let normal = lark.parse(input).unwrap();
        assert_eq!(
            format!("{interactive}"),
            format!("{normal}"),
            "resume() diverged from parse() on {input:?}"
        );
    }
}

/// `exhaust_lexer()` then `feed_eof()` must equal a normal parse, and `exhaust_lexer`
/// returns exactly the lexed tokens.
#[test]
fn test_exhaust_then_eof_equals_parse() {
    let lark = interactive_lark();
    let mut ip = lark.parse_interactive("1 + 2").unwrap();
    let fed = ip.exhaust_lexer().unwrap();
    assert_eq!(fed.len(), 3, "1 + 2 lexes to NUMBER PLUS NUMBER");
    let tree = ip.feed_eof().unwrap().expect("ACCEPT after eof");
    assert_eq!(
        format!("{tree}"),
        format!("{}", lark.parse("1 + 2").unwrap())
    );
}

/// `accepts()` honesty: on a fork, every terminal it lists feeds without error, and
/// a terminal it does *not* list errors. (This is how Python *computes* accepts(),
/// so it is a tautology there — but an independent check of our value-free version.)
#[test]
fn test_accepts_is_honest() {
    let lark = interactive_lark();
    let mut ip = lark.parse_interactive("").unwrap();
    ip.feed("NUMBER", "1").unwrap();
    let accepts = ip.accepts(); // {"$END", "PLUS"} after a number
    assert!(accepts.iter().any(|t| t == "PLUS"));

    // Each accepted terminal feeds OK on an independent fork.
    for term in &accepts {
        if term == "$END" {
            assert!(ip.fork().feed_eof().is_ok(), "$END should feed");
        } else {
            assert!(
                ip.fork().feed(term, "+").is_ok(),
                "accepted terminal {term:?} should feed without error"
            );
        }
    }
    // A non-accepted terminal (NUMBER after a number) errors — and the fork leaves
    // the original untouched.
    assert!(ip.fork().feed("NUMBER", "9").is_err());
    assert_eq!(ip.accepts(), accepts, "fork must not mutate the original");
}

/// Lazy lexing: `parse_interactive` over text with a *later* lexical error must
/// succeed (the caller gets the steering wheel), and the error surfaces only when
/// `resume`/`exhaust_lexer` drives into it — matching Python's lazy lexer.
#[test]
fn test_lazy_lexing_defers_error() {
    use lark_rs::ParseError;
    let lark = interactive_lark();

    // Construction succeeds despite the stray '@' later in the input.
    let mut ip = lark
        .parse_interactive("1 + @ 2")
        .expect("parse_interactive must not eagerly lex / fail on a later bad char");
    // The caller can inspect before driving into the bad region.
    assert_eq!(ip.accepts(), vec!["NUMBER".to_string()]);

    // Driving the lexer into the '@' raises an UnexpectedCharacter, not a panic.
    let err = match ip.exhaust_lexer() {
        Ok(_) => panic!("exhaust_lexer should raise at '@'"),
        Err(e) => e,
    };
    match err {
        ParseError::UnexpectedCharacter { ch, .. } => assert_eq!(ch, '@'),
        other => panic!("expected UnexpectedCharacter at '@', got {other:?}"),
    }
}

/// Premature-EOF via `resume` carries the real input position, not the old `0,0`
/// default (the positioned `$END` is built from the lazy cursor).
#[test]
fn test_eof_position_preserved() {
    use lark_rs::ParseError;
    let lark = interactive_lark();
    let err = match lark.parse_interactive("1 +").unwrap().resume() {
        Ok(_) => panic!("'1 +' is incomplete — resume must error"),
        Err(e) => e,
    };
    match err {
        ParseError::UnexpectedEof { line, col, .. } => {
            assert_eq!(line, 1, "EOF line should be the real position, not 0");
            assert!(col > 1, "EOF col should be past the input, got {col}");
        }
        other => panic!("expected UnexpectedEof, got {other:?}"),
    }
}

/// The public `feed_token(Token)` path must work with a hand-built `Token::new`
/// (no interned id) — resolving the terminal *name* like Python's `feed_token`.
/// This pins the exact API surface the oracle replay drives through `feed`.
#[test]
fn test_feed_token_resolves_user_built_token() {
    use lark_rs::Token;
    let lark = interactive_lark();
    let mut ip = lark.parse_interactive("").unwrap();

    // A bare user token (type_id unset) is accepted and resolved by name.
    assert!(ip.feed_token(Token::new("NUMBER", "1")).unwrap().is_none());
    assert_eq!(ip.accepts(), vec!["$END".to_string(), "PLUS".to_string()]);
    let tree = ip.feed_eof().unwrap().expect("ACCEPT");
    assert_eq!(format!("{tree}"), format!("{}", lark.parse("1").unwrap()));

    // An unknown terminal name errors clearly rather than silently mis-parsing.
    let mut ip2 = lark.parse_interactive("").unwrap();
    assert!(ip2.feed_token(Token::new("NOPE", "x")).is_err());
}

/// `parse_interactive_with_start` drives from an explicit start symbol.
#[test]
fn test_interactive_with_start() {
    let lark = interactive_lark();
    let tree = lark
        .parse_interactive_with_start("1 + 2", "start")
        .unwrap()
        .resume()
        .unwrap();
    assert_eq!(
        format!("{tree}"),
        format!("{}", lark.parse("1 + 2").unwrap())
    );
}

/// Forking after wiring the lexer (not just after manual feeds): a fork carries an
/// independent copy of the input cursor, so both the fork and the original resume to
/// the same tree without consuming each other.
#[test]
fn test_fork_preserves_independent_lexer_cursor() {
    let lark = interactive_lark();
    let ip = lark.parse_interactive("1 + 2").unwrap();
    let want = format!("{}", lark.parse("1 + 2").unwrap());

    let forked = ip.fork().resume().unwrap();
    assert_eq!(format!("{forked}"), want);
    // The original still has its full cursor and resumes to the same tree.
    let original = ip.resume().unwrap();
    assert_eq!(format!("{original}"), want);
}

/// A trailing `%ignore`d run must advance the lazy cursor before `feed_eof`, so
/// `resume` over input with trailing whitespace still completes (and matches parse).
#[test]
fn test_ignored_tokens_advance_cursor_before_eof() {
    let lark = interactive_lark();
    let mut ip = lark.parse_interactive("7   ").unwrap();
    let fed = ip.exhaust_lexer().unwrap();
    assert_eq!(
        fed.len(),
        1,
        "only NUMBER is a real token; the spaces are ignored"
    );
    let tree = ip
        .feed_eof()
        .unwrap()
        .expect("ACCEPT past the trailing spaces");
    assert_eq!(format!("{tree}"), format!("{}", lark.parse("7").unwrap()));
}

#[test]
fn test_interactive_unsupported_on_contextual() {
    // v1 is basic-lexer only; the default contextual config returns a typed error.
    let lark = common::make_lalr("start: \"a\"+\n");
    let err = match lark.parse_interactive("aa") {
        Ok(_) => panic!("contextual config should refuse interactive parsing"),
        Err(e) => e,
    };
    assert!(
        format!("{err}").contains("interactive parsing requires"),
        "unexpected error: {err}"
    );
}
