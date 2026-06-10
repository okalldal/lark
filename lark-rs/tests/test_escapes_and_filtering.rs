//! Lexer/terminal-filtering parity fixes (compliance milestones M1–M3, M5):
//!
//! - M1: `\xHH`, `\uHHHH`, `\UHHHHHHHH` escapes decode to the right scalar value
//!   (in plain string terminals and in char-range bounds).
//! - M2: an anonymous `/regex/` literal in a rule body produces a *kept* token,
//!   while an anonymous `"string"` literal is filtered out.
//! - M3: the case-insensitive flag (`"a"i`, `/a/i`) actually matches other cases.
//! - M5: the grammar-wide `keep_all_tokens` option keeps tokens and drives
//!   `maybe_placeholders`, not only the per-rule `!` modifier.
//!
//! Expected values come from Python Lark (the oracle); the compliance bank covers
//! these too, but this file pins the behavior in a readable, position-aware form.

mod common;

use common::make_lalr;
use lark_rs::{Child, Lark, LarkOptions, LexerType, ParserAlgorithm};

fn tok_value<'a>(c: &'a [Child], i: usize) -> (&'a str, &'a str) {
    match &c[i] {
        Child::Token(t) => (t.type_.as_str(), t.value.as_str()),
        other => panic!("child {i} is not a token: {other:?}"),
    }
}

#[test]
fn test_hex_and_unicode_escapes_decode() {
    // A: \x01 (2 hex), B: /\x02/ (regex-crate escape), C: \xAB then literal "CD".
    let lark = make_lalr("start: A B C\nA: \"\\x01\"\nB: /\\x02/\nC: \"\\xABCD\"");
    let tree = lark
        .parse("\u{01}\u{02}\u{AB}CD")
        .expect("parse")
        .as_tree()
        .unwrap()
        .clone();
    assert_eq!(tok_value(&tree.children, 0), ("A", "\u{01}"));
    assert_eq!(tok_value(&tree.children, 1), ("B", "\u{02}"));
    assert_eq!(tok_value(&tree.children, 2), ("C", "\u{AB}CD"));
}

#[test]
fn test_astral_unicode_escape() {
    // \U with an astral codepoint must decode to a single char (not raise / split).
    let lark = make_lalr("start: A\nA: \"\\U0001F600\"");
    let tree = lark
        .parse("\u{1F600}")
        .expect("parse")
        .as_tree()
        .unwrap()
        .clone();
    assert_eq!(tok_value(&tree.children, 0), ("A", "\u{1F600}"));
}

#[test]
fn test_char_range_with_escaped_bounds_builds_and_matches() {
    // Build failures 202–207: escaped range bounds were not decoded, so the range
    // regex was malformed and the grammar failed to build.
    let lark = make_lalr("start: A+\nA: \"\\x01\"..\"\\x03\"");
    let tree = lark
        .parse("\u{01}\u{02}\u{03}")
        .expect("parse")
        .as_tree()
        .unwrap()
        .clone();
    assert_eq!(tree.children.len(), 3);
}

#[test]
fn test_anonymous_regex_literal_is_kept() {
    // An inline `/regex/` produces a kept `__ANON_n` token...
    let lark = make_lalr("start: /\\w/");
    let tree = lark.parse("a").expect("parse").as_tree().unwrap().clone();
    assert_eq!(
        tree.children.len(),
        1,
        "anonymous regex literal must be kept"
    );
    assert_eq!(tok_value(&tree.children, 0).1, "a");
}

#[test]
fn test_anonymous_string_literal_is_filtered() {
    // ...but an inline `"string"` literal is filtered out (keyword-like punctuation).
    let lark = make_lalr("start: \"a\" B\nB: \"b\"");
    let tree = lark.parse("ab").expect("parse").as_tree().unwrap().clone();
    assert_eq!(
        tree.children.len(),
        1,
        "anonymous string literal must be filtered"
    );
    assert_eq!(tok_value(&tree.children, 0), ("B", "b"));
}

#[test]
fn test_case_insensitive_terminal_matches_other_case() {
    // `"a"i` must match `A`; the IGNORECASE flag was being dropped by the scanner.
    let lark = make_lalr("!start: \"a\"i+");
    let tree = lark.parse("aA").expect("parse").as_tree().unwrap().clone();
    assert_eq!(tree.children.len(), 2);
    assert_eq!(tok_value(&tree.children, 0).1, "a");
    assert_eq!(tok_value(&tree.children, 1).1, "A");
}

#[test]
fn test_global_keep_all_tokens_and_placeholders() {
    // The grammar-wide keep_all_tokens option (not just `!`) keeps tokens and, with
    // maybe_placeholders, emits one `None` per absent `[...]`.
    let lark = Lark::new(
        "start: [\"a\"] [\"b\"] [\"c\"]",
        LarkOptions {
            parser: ParserAlgorithm::Lalr,
            lexer: LexerType::Contextual,
            start: vec!["start".to_string()],
            keep_all_tokens: true,
            maybe_placeholders: true,
            ..Default::default()
        },
    )
    .expect("build");
    let tree = lark.parse("").expect("parse").as_tree().unwrap().clone();
    assert_eq!(tree.children.len(), 3, "one None per absent optional");
    assert!(tree.children.iter().all(|c| matches!(c, Child::None)));
}

#[test]
fn test_python_dialect_angle_escapes_are_literals() {
    // Python `re` treats `\<` / `\>` as the literal chars; the regex crate reserves
    // them as word-boundary escapes (zero-width — `\<\>` would match *nothing* where
    // Python matches "<>", and inside a class they are a compile error). The
    // wild-bank dotmotif OPERATOR `(?:[\!=\>\<][=]?)|(?:\<\>)` is the real-world
    // shape; the loader normalizes the two divergent escapes to bare literals.
    // The wild arm order: the class arm wins leftmost-first at `<`, so `<>` splits
    // into two OPs — exactly what Python `re` does on the same pattern (the un-fixed
    // word-boundary reading would instead make `<` and `>` *unmatchable*).
    let lark = make_lalr("start: OP+\nOP: /(?:[\\!=\\>\\<][=]?)|(?:\\<\\>)/\n%ignore / /");
    let tree = lark
        .parse("<> >= !")
        .expect("parse")
        .as_tree()
        .unwrap()
        .clone();
    assert_eq!(tree.children.len(), 4);
    assert_eq!(tok_value(&tree.children, 0), ("OP", "<"));
    assert_eq!(tok_value(&tree.children, 1), ("OP", ">"));
    assert_eq!(tok_value(&tree.children, 2), ("OP", ">="));
    assert_eq!(tok_value(&tree.children, 3), ("OP", "!"));

    // Reordered so the `\<\>` arm is reachable: it must match the literal "<>" as
    // one token (Python parity), not act as a pair of word-boundary assertions.
    let lark = make_lalr("start: OP+\nOP: /(?:\\<\\>)|(?:[\\!=\\>\\<][=]?)/\n%ignore / /");
    let tree = lark
        .parse("<> >=")
        .expect("parse")
        .as_tree()
        .unwrap()
        .clone();
    assert_eq!(tree.children.len(), 2);
    assert_eq!(tok_value(&tree.children, 0), ("OP", "<>"));
    assert_eq!(tok_value(&tree.children, 1), ("OP", ">="));
}

#[test]
fn test_comment_lines_between_rule_alternatives() {
    // Python Lark's grammar-of-grammars lets a full-line comment sit *between* the
    // `|` alternatives of a multi-line rule (lark.lark's COMMENT swallows its
    // leading newline run, so the rule is not terminated) — the wild-bank dotmotif
    // grammar's shape. Both `//` and `#` comment forms, including consecutive
    // comment lines and a comment terminating the last alternative.
    let lark = make_lalr(
        "start: A\n     // a comment between alternatives\n     | B\n     # hash form\n     \
         // two in a row\n     | C\n     // trailing comment after the last alternative\n\
         A: \"a\"\nB: \"b\"\nC: \"c\"",
    );
    for (input, ty) in [("a", "A"), ("b", "B"), ("c", "C")] {
        let tree = lark.parse(input).expect("parse").as_tree().unwrap().clone();
        assert_eq!(tok_value(&tree.children, 0), (ty, input));
    }
}
