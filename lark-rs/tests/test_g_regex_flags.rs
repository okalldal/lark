//! `g_regex_flags` parity: a global regex flag (e.g. `re.IGNORECASE`) applies to
//! *every* terminal pattern — string literals included — so the whole grammar
//! lexes case-insensitively without mutating any individual terminal.
//!
//! Oracle: Python Lark's `test_g_regex_flags` builds
//! `start: "a" /b+/ C; C: "C" | D; D: "D" E; E: "e"` with `g_regex_flags=re.I`
//! and parses "ABBc" and "abdE". See COMPLIANCE_PARITY.md (the former "M6b").

use lark_rs::grammar::terminal::flags;
use lark_rs::{Child, Lark, LarkOptions, LexerType, ParseTree, ParserAlgorithm};

fn opts(global_flags: u32) -> LarkOptions {
    LarkOptions {
        parser: ParserAlgorithm::Lalr,
        lexer: LexerType::Contextual,
        start: vec!["start".to_string()],
        g_regex_flags: global_flags,
        ..Default::default()
    }
}

const G: &str = "start: \"a\" /b+/ C\nC: \"C\" | D\nD: \"D\" E\nE: \"e\"\n";

/// The children of a parsed `start` tree as `(token_type, value)` pairs.
fn child_tokens(t: &ParseTree) -> Vec<(String, String)> {
    let ParseTree::Tree(tree) = t else {
        panic!("expected a tree root, got a bare token");
    };
    tree.children
        .iter()
        .map(|c| match c {
            Child::Token(tok) => (tok.type_.clone(), tok.value.clone()),
            Child::Tree(sub) => ("TREE".to_string(), sub.data.clone()),
            Child::None => ("NONE".to_string(), String::new()),
        })
        .collect()
}

#[test]
fn ignorecase_makes_every_terminal_case_insensitive() {
    let p = Lark::new(G, opts(flags::IGNORECASE)).expect("grammar builds with g_regex_flags");

    // /b+/ matches the uppercase "BB"; C ("C" | "D" "e") matches lowercase "c".
    let t = p.parse("ABBc").expect("ABBc parses case-insensitively");
    assert_eq!(
        child_tokens(&t),
        vec![
            ("__ANON_0".to_string(), "BB".to_string()),
            ("C".to_string(), "c".to_string()),
        ]
    );

    // "a" matches "a", /b+/ matches "b", and D "e" ("D" then E:"e") matches "dE".
    let t = p.parse("abdE").expect("abdE parses case-insensitively");
    assert_eq!(
        child_tokens(&t),
        vec![
            ("__ANON_0".to_string(), "b".to_string()),
            ("C".to_string(), "dE".to_string()),
        ]
    );
}

#[test]
fn without_global_flag_matching_is_case_sensitive() {
    // Default (no global flag): "ABBc" must NOT parse — the literal "a" expects a
    // lowercase 'a'. This guards that the flag is genuinely opt-in.
    let p = Lark::new(G, opts(0)).expect("grammar builds without flags");
    assert!(
        p.parse("ABBc").is_err(),
        "without g_regex_flags the case-sensitive grammar must reject 'ABBc'"
    );
}
