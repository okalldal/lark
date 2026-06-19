//! Integration test for `include_lark!`: a real `.lark` grammar is baked at
//! compile time (through the same `generate_standalone` emitter the CLI uses, #85)
//! and the generated parser parses JSON at test time.
//!
//! The parser is the self-contained standalone runtime, so its `ParseTree`/`Tree`/
//! `Child` live in the per-struct baked module (`__lark_baked_<StructName>`), not in
//! lark-rs — that is the unified emitter's surface.

use lark_proc::include_lark;

// Compile-time: reads `grammars/json.lark` (relative to this crate's
// CARGO_MANIFEST_DIR), bakes it through the lark-rs standalone emitter, and
// generates a `JsonParser` struct + its baked `__lark_baked_JsonParser` module. A
// broken grammar here would fail `cargo build`.
include_lark!("grammars/json.lark");

// The same grammar with an explicit struct name.
include_lark!("grammars/json.lark", Json);

// Collision pin (#85 review): a struct name that differs from `Json` only in case
// must bake into a *distinct* module. Deriving the module from the verbatim struct
// name keeps these unique; lowercasing them both to `__lark_baked_json` would be a
// duplicate-module compile error.
include_lark!("grammars/json.lark", JSON);

// The baked tree types for the `JsonParser` struct (the standalone runtime's own
// types, re-exported by the per-struct module). The module name mirrors the struct
// name verbatim.
use __lark_baked_JsonParser::{Child, ParseTree};

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
    // The explicit-name struct bakes into its own `__lark_baked_Json` module.
    assert!(matches!(tree, __lark_baked_Json::ParseTree::Tree(_)));
}

#[test]
fn parse_with_start_uses_the_named_start() {
    // The standalone runtime honors an explicit start symbol (json.lark's only
    // start is `start`, so this exercises the delegation path, not a new start).
    let tree = JsonParser::new()
        .parse_with_start("[1, 2]", "start")
        .expect("array parses from the explicit start");
    assert!(matches!(tree, ParseTree::Tree(_)));
}

#[test]
fn invalid_input_is_a_parse_error_not_a_panic() {
    let parser = JsonParser::new();
    let err = parser.parse("{ this is not json }").unwrap_err();
    // We get a structured error string, surfaced at runtime (the *grammar* was fine).
    assert!(!err.is_empty());
}

#[test]
fn grammar_constant_is_embedded() {
    assert!(JsonParser::GRAMMAR.contains("ESCAPED_STRING"));
}

/// Unification pin (#85): the macro bakes through the *same* `generate_standalone`
/// emitter the `generate-parser` CLI uses, so the inline-baked parser must agree
/// tree-for-tree with the lark-rs basic-lexer engine (the standalone backend's
/// oracle) on the embedded grammar — exactly the guarantee `tests/test_standalone.rs`
/// pins for the CLI front-end. One emitter, two front-ends, identical baked data.
#[test]
fn baked_macro_parser_matches_lark_rs_oracle() {
    use lark_rs::{Lark, LarkOptions, LexerType, ParserAlgorithm};

    let oracle = Lark::new(
        JsonParser::GRAMMAR,
        LarkOptions {
            parser: ParserAlgorithm::Lalr,
            lexer: LexerType::Basic,
            ..Default::default()
        },
    )
    .expect("oracle grammar builds");

    let parser = JsonParser::new();
    let inputs = [
        r#"42"#,
        r#""hi""#,
        r#"true"#,
        r#"[1, 2, 3]"#,
        r#"{"a": 1, "b": [true, false, null]}"#,
        r#"[{"x": [1, 2]}, {"y": "z"}]"#,
    ];
    for input in inputs {
        let got = parser.parse(input).expect("baked parser parses");
        let want = oracle.parse(input).expect("oracle parses").to_string();
        // The standalone runtime's `Display` matches lark-rs's `Tree`/`Token` Display.
        assert_eq!(got.to_string(), want, "tree mismatch on {input:?}");
    }
}
