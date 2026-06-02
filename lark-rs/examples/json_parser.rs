//! JSON parser example — mirrors Lark's canonical JSON example.
//!
//! This demonstrates the Rust Lark API using the same grammar format
//! as Python Lark, parsing JSON with LALR + contextual lexer.

use lark_rs::{Lark, LarkOptions, ParserAlgorithm, LexerType};

const JSON_GRAMMAR: &str = r#"
    ?start: value

    ?value: object
          | array
          | string
          | SIGNED_NUMBER  -> number
          | "true"         -> true
          | "false"        -> false
          | "null"         -> null

    array  : "[" [value ("," value)*] "]"
    object : "{" [pair ("," pair)*] "}"
    pair   : string ":" value

    string : ESCAPED_STRING

    %import common.ESCAPED_STRING
    %import common.SIGNED_NUMBER
    %import common.WS
    %ignore WS
"#;

fn main() {
    let options = LarkOptions {
        start: vec!["start".to_string()],
        parser: ParserAlgorithm::Lalr,
        lexer: LexerType::Contextual,
        ..LarkOptions::default()
    };

    let parser = match Lark::new(JSON_GRAMMAR, options) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Grammar error: {}", e);
            std::process::exit(1);
        }
    };

    let inputs = [
        r#"{"key": "value", "num": 42, "arr": [1, 2, 3]}"#,
        r#"[true, false, null]"#,
        r#""hello world""#,
    ];

    for input in &inputs {
        match parser.parse(input) {
            Ok(tree) => println!("Parse OK: {}", tree),
            Err(e) => eprintln!("Parse error on {:?}: {}", input, e),
        }
    }
}
