//! Distilled pins for the wild-bank burndown gaps (2026-06): each test is the
//! minimal shape of a real-world grammar that lark-rs used to reject or
//! mis-lex while Python Lark accepted it. Expected trees come from Python
//! Lark 1.x run over these exact grammars (the oracle); the wild bank
//! (`tests/test_wild.rs`) covers the originals end-to-end, this file keeps
//! each root cause reproducible in isolation.
//!
//! 1. dotmotif — `//` / `#` comment lines *between* the `|` alternatives of a
//!    multi-line rule (the loader emitted two Newline tokens and dropped the
//!    continuation).
//! 2. vyper — a plain `(a|b)` group materialized a helper rule whose unit
//!    alternatives duplicated another rule's RHS, colliding as an
//!    unresolvable reduce/reduce where Python (which distributes groups into
//!    the parent) sees only a silently-resolved shift/reduce.
//! 3. matter_idl / pyquil — a `"keyword"i` literal lost its string-pattern
//!    classification, so it neither joined `unless` keyword retyping nor
//!    sorted like a string, and an overlapping identifier regex would win the
//!    tie and mis-lex the keyword.

use lark_rs::tree::{Child, ParseTree, Tree};
use lark_rs::{Lark, LarkOptions, LexerType, ParserAlgorithm};

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

/// Serialize a parse result exactly like the Python-side oracle script:
/// `(data child child …)` for trees, `TYPE:value` for tokens.
fn show(result: &ParseTree) -> String {
    fn tree(t: &Tree) -> String {
        let children: Vec<String> = t.children.iter().map(child).collect();
        format!("({} {})", t.data, children.join(" "))
    }
    fn child(c: &Child) -> String {
        match c {
            Child::Tree(t) => tree(t),
            Child::Token(tok) => format!("{}:{}", tok.type_, tok.value),
            Child::None => "None".to_string(),
        }
    }
    match result {
        ParseTree::Tree(t) => tree(t),
        ParseTree::Token(tok) => format!("{}:{}", tok.type_, tok.value),
    }
}

fn assert_parses(lark: &Lark, cases: &[(&str, &str)]) {
    for (input, expected) in cases {
        let got = lark
            .parse(input)
            .unwrap_or_else(|e| panic!("parse {input:?} failed: {e}"));
        assert_eq!(&show(&got), expected, "input {input:?}");
    }
}

// ─── 1. dotmotif: comments between alternatives ────────────────────────────

#[test]
fn comment_lines_between_alternatives_continue_the_rule() {
    // Python's COMMENT terminal starts with `\s*`, so a comment-only line is
    // swallowed together with the preceding newline and the `| …` line still
    // continues the rule.
    let g = r#"start: "a" -> a
     // comment line
     | "b" -> b
     # hash comment
     | "c" -> c
"#;
    let lark = build(g, LexerType::Contextual);
    assert_parses(&lark, &[("a", "(a )"), ("b", "(b )"), ("c", "(c )")]);
}

// ─── 2. vyper: plain-group distribution ─────────────────────────────────────

#[test]
fn inline_group_distributes_instead_of_colliding_with_unit_rules() {
    // Distilled from vyper's `subscript: (atom_expr | list) "[" expr "]"` next
    // to `?atom: … | list`: with a `__anon_group` helper rule, `list` reduces
    // to either the helper or `atom` on `[` — an unresolvable reduce/reduce.
    // Distributing the group (Python's lowering) turns it into a
    // shift-over-reduce that LALR resolves silently, and the trees match
    // Python's byte for byte.
    let g = r#"start: expr
?expr: subscript | atom
subscript: (atom | list) "[" expr "]"
?atom: NAME | list
list: "{" "}"
NAME: /[a-z]+/
"#;
    let lark = build(g, LexerType::Contextual);
    assert_parses(
        &lark,
        &[
            ("x", "(start NAME:x)"),
            ("{}", "(start (list ))"),
            ("x[y]", "(start (subscript NAME:x NAME:y))"),
            ("{}[x]", "(start (subscript (list ) NAME:x))"),
            ("{}[{}]", "(start (subscript (list ) (list )))"),
            (
                "x[y[z]]",
                "(start (subscript NAME:x (subscript NAME:y NAME:z)))",
            ),
        ],
    );
}

// ─── 3. matter_idl / pyquil: `"keyword"i` joins unless retyping ─────────────

/// matter_idl's `member_attribute: "optional"i` before a field: with `attr*`
/// both the keyword and `NAME` are legal in the same lexer state, both match
/// the same span, and `NAME` (ranked first) must *retype* to the keyword —
/// case-insensitively — exactly like a case-sensitive keyword would.
#[test]
fn anonymous_ci_keyword_retypes_an_overlapping_identifier() {
    let g = r#"start: attr* NAME NAME ";"
attr: "optional"i
NAME: /[a-zA-Z_][a-zA-Z0-9_]*/
%ignore / +/
"#;
    let cases: &[(&str, &str)] = &[
        ("optional foo bar;", "(start (attr ) NAME:foo NAME:bar)"),
        ("OPTIONAL foo bar;", "(start (attr ) NAME:foo NAME:bar)"),
        ("OpTiOnAl foo bar;", "(start (attr ) NAME:foo NAME:bar)"),
        // Not the keyword: lexes as plain NAMEs.
        ("foo bar;", "(start NAME:foo NAME:bar)"),
        ("optionalx bar;", "(start NAME:optionalx NAME:bar)"),
    ];
    for lexer in [LexerType::Contextual, LexerType::Basic] {
        let lark = build(g, lexer.clone());
        assert_parses(&lark, cases);
    }
}

/// The embed rule (Python: `strtok.pattern.flags <= retok.pattern.flags`): a
/// `"kw"i` under a case-*sensitive* identifier regex must stay in the scanner
/// alternation — here `NAME` is lowercase-only, so `OPTIONAL` is matchable
/// only by the keyword's own `(?i:…)` pattern. Embedding it would be a lex
/// error.
#[test]
fn ci_keyword_is_not_embedded_under_a_case_sensitive_regex() {
    let g = r#"start: attr* NAME NAME ";"
attr: "optional"i
NAME: /[a-z]+/
%ignore / +/
"#;
    let cases: &[(&str, &str)] = &[
        ("OPTIONAL foo bar;", "(start (attr ) NAME:foo NAME:bar)"),
        ("optional foo bar;", "(start (attr ) NAME:foo NAME:bar)"),
    ];
    for lexer in [LexerType::Contextual, LexerType::Basic] {
        let lark = build(g, lexer.clone());
        assert_parses(&lark, cases);
    }
}

/// pyquil's `!function: "SIN"i | "SQRT"i …` vs `IDENTIFIER` in `1/sqrt(2)`:
/// the keyword tie must resolve to the function terminal (named via the
/// literal hint, like Python) so the `(` continues an `apply`, not a
/// dead-end name reference.
#[test]
fn ci_function_keywords_win_over_identifier_in_expressions() {
    let g = r#"start: expr
expr: NUMBER "/" fun "(" NUMBER ")" -> apply
    | NUMBER "/" NAME -> name_ref
!fun: "SQRT"i | "SIN"i
NAME: /[a-zA-Z_][a-zA-Z0-9_]*/
NUMBER: /[0-9]+/
%ignore / +/
"#;
    let lark = build(g, LexerType::Contextual);
    assert_parses(
        &lark,
        &[
            (
                "1/sqrt(2)",
                "(start (apply NUMBER:1 (fun SQRT:sqrt) NUMBER:2))",
            ),
            (
                "1/SQRT(2)",
                "(start (apply NUMBER:1 (fun SQRT:SQRT) NUMBER:2))",
            ),
            (
                "1/sin(3)",
                "(start (apply NUMBER:1 (fun SIN:sin) NUMBER:3))",
            ),
            ("1/other", "(start (name_ref NUMBER:1 NAME:other))"),
        ],
    );
}

/// A *named* case-insensitive keyword terminal behaves identically (it is a
/// `PatternStr` with the flag attached, like Python), and its token survives
/// in the tree under its own name.
#[test]
fn named_ci_keyword_terminal_retypes_too() {
    let g = r#"start: attr* NAME ";"
attr: OPT
OPT: "optional"i
NAME: /[a-zA-Z_][a-zA-Z0-9_]*/
%ignore / +/
"#;
    let lark = build(g, LexerType::Contextual);
    assert_parses(
        &lark,
        &[
            ("optional foo;", "(start (attr OPT:optional) NAME:foo)"),
            ("OPTIONAL foo;", "(start (attr OPT:OPTIONAL) NAME:foo)"),
            ("foo;", "(start NAME:foo)"),
        ],
    );
}
