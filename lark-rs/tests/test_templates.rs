//! Compliance milestone M4: parameterized-template instantiation tree-shape.
//!
//! A template instance must (1) form a tree node labeled with the *base* template
//! name (not the mangled instance name), (2) be transparent iff the base name is
//! `_`-prefixed, (3) inherit the template's own rule options (`!` keep-all, `?`
//! expand1, priority), and (4) honor an alias arm. Higher-order templates — a
//! parameter itself applied as a template — instantiate the bound template.
//!
//! Expected values come from Python Lark (the oracle); the compliance bank covers
//! these too (ids 2–9, 245/246), but this file pins the behavior readably.

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

/// Compact tree shape: `T:v` for tokens, `_` for None, `name[..]` for subtrees.
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

fn parsed_shape(lark: &Lark, input: &str) -> String {
    let tree = lark.parse(input).expect("parse").as_tree().unwrap().clone();
    shape(&Child::Tree(tree))
}

#[test]
fn test_named_template_keeps_base_label() {
    // `sep{NUMBER, ","}` forms a `sep` node (base name), not the instance name; the
    // delimiter `","` is filtered, the items are kept.
    let lark = build(
        "start: \"[\" sep{NUMBER, \",\"} \"]\"\nsep{item, delim}: item (delim item)*\n NUMBER: /\\d+/\n%ignore \" \"",
    );
    assert_eq!(parsed_shape(&lark, "[1, 2, 3]"), "start[sep[NUMBER:1,NUMBER:2,NUMBER:3]]");
    assert_eq!(parsed_shape(&lark, "[1]"), "start[sep[NUMBER:1]]");
}

#[test]
fn test_transparent_template_inlines_and_keeps_all() {
    // `_expr` is transparent (inlined) and `!` keeps all tokens, so both `A` and the
    // substituted `"B"` survive, spliced directly into `start`.
    let lark = build("start: _expr{\"B\"}\n!_expr{t}: \"A\" t");
    assert_eq!(parsed_shape(&lark, "AB"), "start[A:A,B:B]");
}

#[test]
fn test_named_template_inherits_keep_all() {
    // Same body but named `expr` (not `_`): forms an `expr` node and still keeps its
    // tokens — proving the instance inherits the template's `!` option.
    let lark = build("start: expr{\"B\"}\n!expr{t}: \"A\" t");
    assert_eq!(parsed_shape(&lark, "AB"), "start[expr[A:A,B:B]]");
}

#[test]
fn test_template_alias_arm() {
    // The aliased alternative `-> b` labels its node `b`; the other arm keeps the
    // base name `expr`. All tokens here are filtered string literals, so both are
    // empty nodes.
    let lark = build("start: expr{\"C\"}\nexpr{t}: \"A\" t\n     | \"B\" t -> b");
    assert_eq!(parsed_shape(&lark, "AC"), "start[expr[]]");
    assert_eq!(parsed_shape(&lark, "BC"), "start[b[]]");
}

#[test]
fn test_higher_order_template() {
    // `a{b}` binds parameter `t` to template `b`, then applies it as `t{"a"}` =
    // `b{"a"}`. The instance must resolve the bound template name, not error on `t`.
    let lark = build("start: a{b}\na{t}: t{\"a\"}\nb{x}: x");
    assert_eq!(parsed_shape(&lark, "a"), "start[a[b[]]]");
}
