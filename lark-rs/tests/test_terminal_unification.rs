//! Compliance milestone M6: an inline pattern identical to a named terminal's
//! pattern is *unified* with it (the token adopts the named terminal's type), while
//! tree filtering stays per rule-symbol occurrence — so a literal occurrence is
//! dropped even when it lexes to the same terminal a sibling reference keeps.
//!
//! Expected values come from Python Lark (the oracle); the compliance bank covers
//! these too (ids 155, 194/195), but this file pins the behavior readably and
//! exercises both the basic and contextual lexers.

mod common;

use lark_rs::{Child, Lark, LarkOptions, LexerType, ParserAlgorithm};

fn build(grammar: &str, lexer: LexerType) -> Lark {
    Lark::new(
        grammar,
        LarkOptions {
            parser: ParserAlgorithm::Lalr,
            lexer,
            start: vec!["start".to_string()],
            ..Default::default()
        },
    )
    .unwrap_or_else(|e| panic!("Grammar failed to load: {e}"))
}

/// (type, value) of the single token child of a one-child `start` tree.
fn only_token(lark: &Lark, input: &str) -> (String, String) {
    let tree = lark.parse(input).expect("parse").as_tree().unwrap().clone();
    assert_eq!(
        tree.children.len(),
        1,
        "expected exactly one child: {:?}",
        tree.children
    );
    match &tree.children[0] {
        Child::Token(t) => (t.type_.clone(), t.value.clone()),
        other => panic!("child is not a token: {other:?}"),
    }
}

#[test]
fn test_literal_unifies_with_named_terminal_basic_lexer() {
    // `start: "a" A` / `A: "a"` — the inline `"a"` and `A` share a pattern, so they
    // unify to one terminal `A`. The literal occurrence (position 0) is filtered;
    // the `A` reference (position 1) is kept. Input "aa" → start[A:"a"]. The basic
    // lexer (no per-state narrowing) can only parse this once the terminals unify.
    for lexer in [LexerType::Basic, LexerType::Contextual] {
        let label = format!("{lexer:?}");
        let lark = build("start: \"a\" A\nA: \"a\"", lexer);
        assert_eq!(
            only_token(&lark, "aa"),
            ("A".to_string(), "a".to_string()),
            "{label}"
        );
    }
}

#[test]
fn test_inline_regex_adopts_named_terminal_type() {
    // `start: /a/` / `A: /a/` — the inline regex adopts the named terminal's type,
    // so the token is `A`, not `__ANON_0`.
    let lark = build("start: /a/\nA: /a/", LexerType::Contextual);
    assert_eq!(only_token(&lark, "a"), ("A".to_string(), "a".to_string()));
}

#[test]
fn test_keep_all_overrides_per_position_filter() {
    // With `!start`, the otherwise-filtered literal occurrence is kept too, so both
    // unified `A` tokens survive — proving keep_all_tokens still overrides the
    // per-position drop after unification.
    let lark = build("!start: \"a\" A\nA: \"a\"", LexerType::Contextual);
    let tree = lark.parse("aa").expect("parse").as_tree().unwrap().clone();
    let kinds: Vec<&str> = tree
        .children
        .iter()
        .map(|c| match c {
            Child::Token(t) => t.type_.as_str(),
            _ => "?",
        })
        .collect();
    assert_eq!(kinds, vec!["A", "A"]);
}
