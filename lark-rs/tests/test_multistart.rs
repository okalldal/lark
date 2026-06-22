//! Default-start selection must match Python Lark's `_verify_start`
//! (`lark/parser_frontends.py`) on **every** backend — LALR, Earley, and CYK
//! (issues #251, #256). `_verify_start` is shared by every parser frontend in
//! Python, so `parse(text)` with no explicit start on a grammar with >1
//! configured start raises `ConfigurationError("Lark initialized with more than 1
//! possible start rule. Must specify which start rule to parse")` for all three.
//!
//! Before #256, lark-rs's Earley and CYK default-start path silently resolved
//! `start=None` to `grammar.start.first()`, so they wrongly *accepted* a
//! multi-start `parse(text)`; LALR already rejected (after #251). This test pins
//! the Python oracle for all three so they agree.
//!
//! Oracle (Python Lark, verified 2026-06-22):
//!   * `start=['start','other']`, default start → ConfigurationError
//!     ("more than 1 possible start rule") on earley / cyk / lalr;
//!   * single configured start, default → uses it (parses);
//!   * explicit `start=` pick among the configured starts → allowed (uses it);
//!   * explicit start NOT among the configured starts → "Unknown start rule ….
//!     Must be one of […]" on every backend.

use lark_rs::{Lark, LarkOptions, LexerType, ParserAlgorithm};

const MULTI: &str = r#"
    start: A
    other: B
    A: /a+/
    B: /b+/
"#;

const SINGLE: &str = r#"
    start: A
    A: /a+/
"#;

fn lark(grammar: &str, parser: ParserAlgorithm, lexer: LexerType, starts: &[&str]) -> Lark {
    Lark::new(
        grammar,
        LarkOptions {
            parser,
            lexer,
            start: starts.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        },
    )
    .unwrap_or_else(|e| panic!("build failed: {e}"))
}

/// Every backend/lexer combination this issue covers. Earley runs on the basic
/// lexer and the dynamic lexer; CYK and LALR on the basic lexer. All must agree
/// with the Python oracle.
fn backends() -> Vec<(&'static str, ParserAlgorithm, LexerType)> {
    vec![
        ("earley/basic", ParserAlgorithm::Earley, LexerType::Basic),
        (
            "earley/dynamic",
            ParserAlgorithm::Earley,
            LexerType::Dynamic,
        ),
        ("cyk/basic", ParserAlgorithm::Cyk, LexerType::Basic),
        ("lalr/basic", ParserAlgorithm::Lalr, LexerType::Basic),
    ]
}

/// >1 configured start + default (`parse`, no explicit start) → reject with
/// Python's "more than 1 possible start rule" on every backend. This is the
/// headline #256 fix for Earley + CYK (LALR already rejected, #251).
#[test]
fn multi_start_default_rejected_on_every_backend() {
    for (label, parser, lexer) in backends() {
        let l = lark(MULTI, parser, lexer, &["start", "other"]);
        match l.parse("a") {
            Ok(_) => panic!(
                "{label}: multi-start default must reject (Python ConfigurationError), got a tree"
            ),
            Err(e) => {
                let msg = e.to_string();
                assert!(
                    msg.contains("more than 1 possible start rule"),
                    "{label}: expected Python's >1-start message, got: {msg}"
                );
            }
        }
    }
}

/// Duplicate configured start (`['start','start']`) counts as >1, exactly as
/// Python's `len(start) > 1` check.
#[test]
fn duplicate_start_counts_as_multi_on_every_backend() {
    for (label, parser, lexer) in backends() {
        let l = lark(MULTI, parser, lexer, &["start", "start"]);
        match l.parse("a") {
            Ok(_) => panic!("{label}: duplicate start must reject as >1"),
            Err(e) => assert!(
                e.to_string().contains("more than 1 possible start rule"),
                "{label}: duplicate-start should hit the >1 message, got: {e}"
            ),
        }
    }
}

/// Single configured start + default → use it (parses cleanly) on every backend.
#[test]
fn single_start_default_parses_on_every_backend() {
    for (label, parser, lexer) in backends() {
        let l = lark(SINGLE, parser, lexer, &["start"]);
        l.parse("a")
            .unwrap_or_else(|e| panic!("{label}: single-start default should parse, got: {e}"));
    }
}

/// Explicit `start=` pick is ALLOWED with multiple starts — it uses the named
/// one (Python allows this; only the *default* multi-start is rejected).
#[test]
fn explicit_start_pick_allowed_with_multiple_starts() {
    for (label, parser, lexer) in backends() {
        let l = lark(MULTI, parser, lexer, &["start", "other"]);
        l.parse_with_start("a", "start")
            .unwrap_or_else(|e| panic!("{label}: explicit start=start should parse 'a', got: {e}"));
        l.parse_with_start("b", "other")
            .unwrap_or_else(|e| panic!("{label}: explicit start=other should parse 'b', got: {e}"));
    }
}

/// Explicit start NOT among the configured starts → Python's "Unknown start
/// rule …" on every backend (a rule name that isn't a configured start, and a
/// name that isn't a rule at all, both reject).
#[test]
fn explicit_unknown_start_rejected_on_every_backend() {
    for (label, parser, lexer) in backends() {
        // `other` is a real rule but not a configured start here.
        let l = lark(MULTI, parser, lexer, &["start"]);
        match l.parse_with_start("b", "other") {
            Ok(_) => panic!("{label}: non-configured start 'other' must be rejected"),
            Err(e) => assert!(
                e.to_string().contains("Unknown start rule"),
                "{label}: expected Python's unknown-start message, got: {e}"
            ),
        }
        // `nope` is not a rule at all.
        match l.parse_with_start("a", "nope") {
            Ok(_) => panic!("{label}: unknown start 'nope' must be rejected"),
            Err(e) => assert!(
                e.to_string().contains("Unknown start rule"),
                "{label}: expected Python's unknown-start message, got: {e}"
            ),
        }
    }
}
