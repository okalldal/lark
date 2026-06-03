//! Compliance milestone M8: EBNF repetition under shared recurse helpers, rule
//! priority on ambiguous alternations, and redundant nested nullable optionals.
//!
//! The unifying fix is that identical `x+`/`x*` occurrences share one recurse rule
//! (`P: x | P x`), exactly as Python Lark caches them. Sharing collapses the
//! duplicate `… -> x` reductions that were otherwise an unresolvable reduce/reduce,
//! making `a+ b | a+`, `a* b | a+`, and the rule-priority case `a.2 | b.1` (both
//! starting `"A"+`) all LALR-parseable. Separately, a `?` over an already-nullable
//! `?`/`*` helper is collapsed so `("A"?)?` does not stack two empty rules.
//!
//! Expected values come from Python Lark (the oracle); the compliance bank covers
//! these too (ids 77/78, 156/157, 160/161, 108/109), but this file pins them.

mod common;

use lark_rs::{Child, Lark, LarkOptions, LexerType, ParserAlgorithm};

fn build(grammar: &str) -> Lark {
    Lark::new(
        grammar,
        LarkOptions {
            parser: ParserAlgorithm::Lalr,
            lexer: LexerType::Contextual,
            start: vec!["start".to_string()],
            maybe_placeholders: true,
            ..Default::default()
        },
    )
    .unwrap_or_else(|e| panic!("Grammar failed to load: {e}"))
}

fn shape(c: &Child) -> String {
    match c {
        Child::Token(t) => format!("{}:{}", t.type_, t.value),
        Child::None => "_".into(),
        Child::Tree(t) => format!(
            "{}[{}]",
            t.data,
            t.children.iter().map(shape).collect::<Vec<_>>().join(",")
        ),
    }
}

fn parsed(lark: &Lark, input: &str) -> String {
    let tree = lark.parse(input).expect("parse").as_tree().unwrap().clone();
    shape(&Child::Tree(tree))
}

#[test]
fn test_plus_shared_between_branches() {
    // `"a"+ "b" | "a"+` — both `"a"+` share one recurse rule, so this is LALR.
    let lark = build("start: \"a\"+ \"b\"\n     | \"a\"+");
    assert_eq!(parsed(&lark, "aaaa"), "start[]");
    assert_eq!(parsed(&lark, "aaaab"), "start[]");
}

#[test]
fn test_star_and_plus_share_recurse() {
    // `"a"* "b" | "a"+` — the `*` and `+` share the same recurse rule.
    let lark = build("start: \"a\"* \"b\"\n     | \"a\"+");
    assert_eq!(parsed(&lark, "aaaa"), "start[]");
    assert_eq!(parsed(&lark, "aaaab"), "start[]");
    assert_eq!(parsed(&lark, "b"), "start[]");
}

#[test]
fn test_rule_priority_disambiguates_shared_plus() {
    // `a.2: "A"+` and `b.1: "A"+ "B"?` both start with the shared `"A"+`; the
    // reduce/reduce on end-of-input is resolved by rule priority (a > b).
    let lark = build("start: a | b\na.2: \"A\"+\nb.1: \"A\"+ \"B\"?");
    assert_eq!(parsed(&lark, "AAAA"), "start[a[]]");
    assert_eq!(parsed(&lark, "AAAB"), "start[b[]]");
}

#[test]
fn test_redundant_nested_optional_collapses() {
    // `("A"?)?` is just `"A"?` — the redundant outer `?` is collapsed instead of
    // building a second ambiguous empty rule.
    let lark = build("!start: (\"A\"?)?");
    assert_eq!(parsed(&lark, "A"), "start[A:A]");
    assert_eq!(parsed(&lark, ""), "start[]");
}

#[test]
fn test_repetition_trees_unaffected() {
    // Sharing must not change ordinary repetition trees: a kept `"a"+` still yields
    // one token per repeat, and a multi-symbol group repeats as a unit.
    let plus = build("!start: \"a\"+");
    assert_eq!(parsed(&plus, "aaa"), "start[A:a,A:a,A:a]");
    let group = build("!start: (\"a\" \"b\")+");
    assert_eq!(parsed(&group, "abab"), "start[A:a,B:b,A:a,B:b]");
}
