//! Public `parse_into` seam (#232, C7): the value-parametric semantic-output API.
//!
//! Two gates here:
//!   1. **Relative oracle through the public path** — a *user-defined* tree-rebuilding
//!      `OutputBuilder` (`TreeRebuild`) must reproduce, byte-for-byte, the tree
//!      `parse()` returns, across grammars and with `propagate_positions` both off and
//!      on. This proves the public seam drives every shaping decision (filtering,
//!      transparent/anon splicing, `expand1`, `maybe_placeholders`, position meta)
//!      identically to the built-in tree backend.
//!   2. **A genuinely non-tree `Value`** flows through the same shaping (a token-value
//!      collector), and the unsupported configurations (Earley) return a typed error.

use lark_rs::{
    Child, Lark, LarkOptions, LexerType, Meta, OutputBuilder, OutputContext, ParseTree,
    ParserAlgorithm, Token, Tree,
};

// ─── A user-defined tree-rebuilding builder over the public seam ────────────────

struct TreeRebuild;

impl<'i> OutputBuilder<'i> for TreeRebuild {
    type Value = Child;

    fn token(&mut self, token: Token, _input: &'i str, _ctx: &OutputContext) -> Child {
        Child::Token(token)
    }

    fn reduce(
        &mut self,
        rule: usize,
        children: &mut Vec<Child>,
        meta: &Meta,
        ctx: &OutputContext,
    ) -> Child {
        Child::Tree(Tree {
            data: ctx.callback_name(rule).to_string(),
            children: std::mem::take(children),
            meta: meta.clone(),
        })
    }

    fn placeholder(&mut self, _ctx: &OutputContext) -> Child {
        Child::None
    }
}

fn child_to_parse_tree(c: Child) -> ParseTree {
    match c {
        Child::Tree(t) => ParseTree::Tree(t),
        Child::Token(t) => ParseTree::Token(t),
        Child::None => ParseTree::None,
    }
}

fn lark(grammar: &str, lexer: LexerType, propagate: bool) -> Lark {
    Lark::new(
        grammar,
        LarkOptions {
            parser: ParserAlgorithm::Lalr,
            lexer,
            start: vec!["start".to_string()],
            propagate_positions: propagate,
            ..Default::default()
        },
    )
    .expect("grammar builds")
}

const ARITH: &str = r#"
    start: sum
    ?sum: product | sum "+" product | sum "-" product
    ?product: atom | product "*" atom | product "/" atom
    ?atom: NUMBER | "(" sum ")"
    NUMBER: /[0-9]+/
    %ignore " "
"#;

const JSON: &str = r#"
    start: value
    ?value: object | array | STRING | NUMBER | "true" | "false" | "null"
    object: "{" [pair ("," pair)*] "}"
    pair: STRING ":" value
    array: "[" [value ("," value)*] "]"
    STRING: /"[^"]*"/
    NUMBER: /-?[0-9]+/
    %ignore /[ \t\n]+/
"#;

fn assert_parse_into_matches_parse(grammar: &str, inputs: &[&str]) {
    for lexer in [LexerType::Basic, LexerType::Contextual] {
        for propagate in [false, true] {
            let l = lark(grammar, lexer.clone(), propagate);
            for &input in inputs {
                let via_parse = l.parse(input).expect("parse ok");
                let via_into = l
                    .parse_into(input, &mut TreeRebuild)
                    .map(child_to_parse_tree)
                    .expect("parse_into ok");
                assert_eq!(
                    format!("{via_parse:?}"),
                    format!("{via_into:?}"),
                    "parse_into diverged from parse (lexer={lexer:?}, propagate={propagate}) on {input:?}"
                );
            }
        }
    }
}

#[test]
fn parse_into_tree_rebuild_matches_parse_arith() {
    assert_parse_into_matches_parse(ARITH, &["1", "1+2*3", "(1+2)*3-4", "10 / 2 + 3"]);
}

#[test]
fn parse_into_tree_rebuild_matches_parse_json() {
    assert_parse_into_matches_parse(
        JSON,
        &[
            r#"{"a": 1}"#,
            r#"[1, 2, 3]"#,
            r#"{"x": [true, null], "y": {"z": "s"}}"#,
            r#"[]"#,
        ],
    );
}

// ─── A genuinely non-tree Value: collect token values left-to-right ─────────────

struct TokenText(Vec<String>);

impl<'i> OutputBuilder<'i> for TokenText {
    type Value = ();

    fn token(&mut self, token: Token, _input: &'i str, _ctx: &OutputContext) -> Self::Value {
        self.0.push(token.value);
    }

    fn reduce(
        &mut self,
        _rule: usize,
        _children: &mut Vec<Self::Value>,
        _meta: &Meta,
        _ctx: &OutputContext,
    ) -> Self::Value {
    }
}

#[test]
fn parse_into_non_tree_value_sees_shaped_tokens() {
    // Punctuation (`+`, `*`, parens) is filtered by shaping, so only kept terminals
    // (the NUMBERs) reach `token()` as retained values — but `token()` runs for every
    // shifted terminal, so it observes them all; the collector records shift order.
    let l = lark(ARITH, LexerType::Contextual, false);
    let mut b = TokenText(Vec::new());
    l.parse_into("1+2*3", &mut b).expect("parse_into ok");
    // Every shifted terminal materialized a value (numbers and operators alike).
    assert_eq!(b.0, vec!["1", "+", "2", "*", "3"]);
}

// ─── Unsupported configuration: Earley refuses parse_into (ADR-0029 fork 4) ──────

#[test]
fn parse_into_rejects_earley() {
    let l = Lark::new(
        ARITH,
        LarkOptions {
            parser: ParserAlgorithm::Earley,
            lexer: LexerType::Basic,
            start: vec!["start".to_string()],
            ..Default::default()
        },
    )
    .expect("grammar builds");
    let mut b = TreeRebuild;
    let err = l.parse_into("1+2", &mut b).unwrap_err();
    assert!(
        format!("{err}").contains("parser='lalr'"),
        "expected a typed LALR-only refusal, got: {err}"
    );
}
