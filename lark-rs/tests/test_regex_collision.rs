//! `strict=True` regex-collision parity (compliance M7b, bank ids 57/58).
//!
//! Two same-priority *regex* terminals that can both fully match a common string
//! must be rejected at construction in strict mode, exactly as Python Lark does
//! via `interegular` (`lexer.py::_check_regex_collisions`). In the default mode
//! the lexer resolves the overlap arbitrarily and the grammar builds.
//!
//! Oracle: `Lark("start: A | B\nA: /e?rez/\nB: /erez?/", parser="lalr",
//! strict=True)` raises `LexError` (both match "erez"); builds fine without strict.

use lark_rs::{Lark, LarkOptions, LexerType, ParserAlgorithm};

fn opts(strict: bool, lexer: LexerType) -> LarkOptions {
    LarkOptions {
        parser: ParserAlgorithm::Lalr,
        lexer,
        start: vec!["start".to_string()],
        strict,
        ..Default::default()
    }
}

/// `/e?rez/` matches {rez, erez}; `/erez?/` matches {ere, erez}. Both match
/// "erez", same (default) priority → a strict-mode collision.
const COLLIDING: &str = "start: A | B\nA: /e?rez/\nB: /erez?/\n";

#[test]
fn colliding_regexes_build_in_default_mode() {
    assert!(
        Lark::new(COLLIDING, opts(false, LexerType::Contextual)).is_ok(),
        "default mode must not run the collision check"
    );
    assert!(Lark::new(COLLIDING, opts(false, LexerType::Basic)).is_ok());
}

#[test]
fn colliding_regexes_are_fatal_in_strict_mode() {
    // Both lexer types — bank records this once per lexer (ids 57 contextual,
    // 58 basic).
    for lexer in [LexerType::Contextual, LexerType::Basic] {
        let err = Lark::new(COLLIDING, opts(true, lexer.clone()));
        assert!(
            err.is_err(),
            "strict mode must reject the regex collision ({lexer:?})"
        );
        let msg = format!("{}", err.err().unwrap());
        assert!(
            msg.contains("Collision"),
            "strict-mode error should mention the collision, got: {msg}"
        );
    }
}

#[test]
fn disjoint_regexes_build_under_strict() {
    // No over-rejection: terminals that cannot share a string must still build.
    let g = "start: A | B\nA: /abc/\nB: /xyz/\n";
    assert!(
        Lark::new(g, opts(true, LexerType::Contextual)).is_ok(),
        "strict mode must not reject disjoint terminals"
    );
}

#[test]
fn different_priority_overlap_builds_under_strict() {
    // interegular groups by priority — overlapping terminals at *different*
    // priorities are disambiguated by priority, not a collision.
    let g = "start: A | B\nA.2: /ab/\nB: /ab/\n";
    assert!(
        Lark::new(g, opts(true, LexerType::Contextual)).is_ok(),
        "only same-priority pairs collide"
    );
}
