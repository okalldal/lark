//! Compliance milestones M5 (nested `maybe_placeholders`) and M8 (oversized
//! terminal priority). Expected values come from Python Lark (the oracle); the
//! compliance bank covers these too, but this file pins the behavior in a
//! readable form.
//!
//! - M5: an absent `[...]` emits one `None` per kept slot of its widest
//!   alternative, counted *recursively* — a `[...]` nested inside another `[...]`
//!   contributes its own slot count, mirroring Lark's `FindRuleSize`.
//! - M8: a terminal priority too large for `i32` (Lark uses arbitrary-precision
//!   ints) saturates instead of failing to lex.

mod common;

use lark_rs::{Child, Lark, LarkOptions, LexerType, ParserAlgorithm};

fn build(grammar: &str, maybe_placeholders: bool) -> Lark {
    Lark::new(
        grammar,
        LarkOptions {
            parser: ParserAlgorithm::Lalr,
            lexer: LexerType::Contextual,
            start: vec!["start".to_string()],
            maybe_placeholders,
            ..Default::default()
        },
    )
    .unwrap_or_else(|e| panic!("Grammar failed to load: {e}"))
}

/// Summarize a child list as a compact string: `A`/`B` for tokens, `_` for a
/// `None` placeholder, `(..)` for a subtree.
fn shape(children: &[Child]) -> String {
    children
        .iter()
        .map(|c| match c {
            Child::Token(t) => t.value.clone(),
            Child::None => "_".to_string(),
            Child::Tree(t) => format!("({})", t.data),
        })
        .collect::<Vec<_>>()
        .join(",")
}

#[test]
fn test_nested_maybe_placeholders_compose() {
    // `!start: ["a" ["b" "c"]]` — the inner `["b" "c"]` is two kept slots, so the
    // outer absent group must emit three Nones, not one (compliance ids 123/124).
    let lark = build("!start: [\"a\" [\"b\" \"c\"]] ", true);
    let cases = [
        ("", "_,_,_"),    // outer absent: 1 (for "a") + 2 (nested) = 3 placeholders
        ("a", "a,_,_"),   // outer present, inner absent
        ("abc", "a,b,c"), // both present
    ];
    for (input, expected) in cases {
        let tree = lark.parse(input).expect("parse").as_tree().unwrap().clone();
        assert_eq!(shape(&tree.children), expected, "input {input:?}");
    }
}

#[test]
fn test_single_maybe_unaffected() {
    // A single (non-nested) `[...]` still emits exactly its own kept-slot count.
    let lark = build("!start: [\"a\" \"b\"]", true);
    assert_eq!(
        shape(&lark.parse("").unwrap().as_tree().unwrap().children),
        "_,_"
    );
    assert_eq!(
        shape(&lark.parse("ab").unwrap().as_tree().unwrap().children),
        "a,b"
    );
}

#[test]
fn test_non_final_maybe_distributes_under_placeholders() {
    // Issue #106, distilled from `python.lark`'s `parameters` rule: a non-final
    // `[...]` under `maybe_placeholders` must be *distributed* into the parent's
    // alternatives (Python's `_EMPTY` markers → `empty_indices`), not kept as a
    // nullable helper rule. The helper form hides the following branch from the
    // LR(0) closure: after `A ("," A)*`, the `,` that starts the *second*
    // optional is reachable only through the first helper's ε-reduce, which the
    // shift-over-reduce conflict resolution silently drops — so `a, *`
    // (`def f(a, *b)` in python.lark) was a parse error although Python Lark
    // accepts it. The distribution must also recurse: the first `[...]`'s
    // present form ends in a `("," A)*` that lands mid-rule when spliced, so it
    // distributes too (or `a, /, *` dies the same way one branch later).
    //
    // Expected shapes are Python Lark 1.3.1 (`lalr` and `earley` agree).
    let grammar = "start: A (\",\" A)* [\",\" SLASH (\",\" A)*] [\",\" [STAR]]\n\
                   SLASH: \"/\"\n\
                   STAR: \"*\"\n\
                   A: \"a\"\n\
                   %ignore \" \"";
    let cases = [
        ("a", "a,_,_"),
        ("a, a", "a,a,_,_"),
        ("a, *", "a,_,*"),
        ("a, a, *", "a,a,_,*"),
        ("a, /", "a,/,_"),
        ("a, /, a", "a,/,a,_"),
        ("a, /, *", "a,/,*"),
        ("a, /, a, *", "a,/,a,*"),
        ("a,", "a,_,_"),
        ("a, /, a,", "a,/,a,_"),
    ];
    for parser in [ParserAlgorithm::Lalr, ParserAlgorithm::Earley] {
        let lark = Lark::new(
            grammar,
            LarkOptions {
                parser: parser.clone(),
                start: vec!["start".to_string()],
                maybe_placeholders: true,
                ..Default::default()
            },
        )
        .unwrap_or_else(|e| panic!("grammar failed to load under {parser:?}: {e}"));
        for (input, expected) in cases {
            let tree = lark
                .parse(input)
                .unwrap_or_else(|e| panic!("{parser:?} must parse {input:?}: {e}"))
                .as_tree()
                .unwrap()
                .clone();
            assert_eq!(
                shape(&tree.children),
                expected,
                "{parser:?}, input {input:?}"
            );
        }
    }
}

#[test]
fn test_oversized_negative_terminal_priority_saturates() {
    // `A.-99999999999999999999999` overflows i32; Lark (bignum priorities) accepts
    // it as an extremely low priority. We saturate to i32::MIN and still build/parse
    // (compliance ids 49/50). `ab` must lex as the higher-priority `AB`.
    let lark = build(
        "start: A B | AB\nA.-99999999999999999999999: \"a\"\nB: \"b\"\nAB: \"ab\"",
        false,
    );
    let tree = lark.parse("ab").expect("parse").as_tree().unwrap().clone();
    assert_eq!(shape(&tree.children), "ab"); // single AB token
    match &tree.children[0] {
        Child::Token(t) => assert_eq!(t.type_, "AB"),
        other => panic!("expected AB token, got {other:?}"),
    }
}
