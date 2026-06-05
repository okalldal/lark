//! Strict-mode regex-collision detection (issue #35).
//!
//! Under `strict=True`, Python Lark (via `interegular`) rejects two same-priority
//! *regex* terminals whose languages share a string. lark-rs reproduces this with
//! a product-construction emptiness test over each terminal's DFA. These tests
//! pin both halves of the contract:
//!
//!   * a real overlap is reported (and *only* in strict mode), and
//!   * non-overlapping / different-priority / string terminals are **not**
//!     over-rejected — the documented risk the issue calls out.

use lark_rs::{Lark, LarkOptions, LexerType, ParserAlgorithm};

fn strict_opts(lexer: LexerType) -> LarkOptions {
    LarkOptions {
        parser: ParserAlgorithm::Lalr,
        lexer,
        strict: true,
        ..Default::default()
    }
}

/// The exact grammar from the compliance bank (construct:57 / :58): `/e?rez/` and
/// `/erez?/` both match `"erez"`, so strict mode must refuse to build.
const COLLIDING: &str = r#"
start: A | B
A: /e?rez/
B: /erez?/
"#;

#[test]
fn overlapping_regex_terminals_collide_in_strict_mode() {
    for lexer in [LexerType::Contextual, LexerType::Basic] {
        let err = Lark::new(COLLIDING, strict_opts(lexer.clone()))
            .err()
            .unwrap_or_else(|| panic!("expected a collision error for lexer {lexer:?}"));
        let msg = err.to_string();
        assert!(
            msg.contains('A') && msg.contains('B') && msg.contains("Collision"),
            "error should name both colliding terminals: {msg}"
        );
    }
}

#[test]
fn overlap_is_only_rejected_in_strict_mode() {
    // The same grammar builds fine without strict — Python Lark only warns there.
    let opts = LarkOptions {
        parser: ParserAlgorithm::Lalr,
        lexer: LexerType::Basic,
        strict: false,
        ..Default::default()
    };
    assert!(
        Lark::new(COLLIDING, opts).is_ok(),
        "non-strict build must not be rejected"
    );
}

#[test]
fn disjoint_regex_terminals_do_not_collide() {
    // `[0-9]+` and `[a-z]+` share no string — must not be over-rejected.
    let grammar = r#"
start: A | B
A: /[0-9]+/
B: /[a-z]+/
"#;
    assert!(
        Lark::new(grammar, strict_opts(LexerType::Basic)).is_ok(),
        "disjoint terminals must build under strict mode"
    );
}

#[test]
fn overlap_at_different_priorities_is_not_a_collision() {
    // Same languages as COLLIDING but different priorities: Python groups by
    // priority and never compares across groups, so this builds.
    let grammar = r#"
start: A | B
A.2: /e?rez/
B.1: /erez?/
"#;
    assert!(
        Lark::new(grammar, strict_opts(LexerType::Basic)).is_ok(),
        "different-priority terminals must not be compared"
    );
}

#[test]
fn string_terminals_are_not_collision_checked() {
    // Two string terminals that the lexer disambiguates via `unless`/ordering are
    // not regex collisions — interegular only ever sees `pattern.type == "re"`.
    let grammar = r#"
start: A B
A: "if"
B: /if|else/
"#;
    // A is a string keyword fully matched by B; Python's collision check skips
    // string terminals, so strict mode must still build.
    assert!(
        Lark::new(grammar, strict_opts(LexerType::Basic)).is_ok(),
        "string terminal must not trigger a regex collision"
    );
}
