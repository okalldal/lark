//! Integration test for `include_lark!`: a real `.lark` grammar is validated at
//! compile time and the generated parser parses JSON at test time.

use lark_proc::include_lark;
use lark_rs::{Child, ParseTree};

// Compile-time: reads `grammars/json.lark` (relative to this crate's
// CARGO_MANIFEST_DIR), validates it through the lark-rs loader, and generates a
// `JsonParser` struct. A broken grammar here would fail `cargo build`.
include_lark!("grammars/json.lark");

// The same grammar with an explicit struct name.
include_lark!("grammars/json.lark", Json);

#[test]
fn generated_struct_parses_a_json_object() {
    let parser = JsonParser::new();
    let tree = parser
        .parse(r#"{"key": [1, 2, true, null], "s": "hi"}"#)
        .expect("valid JSON should parse");

    // `?start: value` collapses to the `object` rule for an object literal.
    match tree {
        ParseTree::Tree(t) => assert_eq!(t.data, "object"),
        ParseTree::Token(_) => panic!("expected an `object` tree, got a bare token"),
    }
}

#[test]
fn generated_struct_parses_a_scalar() {
    let parser = JsonParser::new();
    // A bare number collapses past `?start`/`?value` to the `number`-aliased token.
    let tree = parser.parse("42").expect("a bare number should parse");
    match tree {
        ParseTree::Tree(t) => {
            // number is an alias -> Tree("number", [SIGNED_NUMBER token])
            assert_eq!(t.data, "number");
            assert!(matches!(t.children.first(), Some(Child::Token(_))));
        }
        ParseTree::Token(_) => panic!("expected a `number` tree"),
    }
}

#[test]
fn explicit_name_struct_also_parses() {
    let tree = Json::default()
        .parse("[true, false, null]")
        .expect("a JSON array should parse");
    assert!(tree.is_tree());
}

#[test]
fn invalid_input_is_a_parse_error_not_a_panic() {
    let parser = JsonParser::new();
    let err = parser.parse("{ this is not json }").unwrap_err();
    // We get a structured ParseError, surfaced at runtime (the *grammar* was fine).
    let msg = format!("{err}");
    assert!(!msg.is_empty());
}

#[test]
fn grammar_constant_is_embedded() {
    assert!(JsonParser::GRAMMAR.contains("ESCAPED_STRING"));
}
