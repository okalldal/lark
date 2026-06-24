//! Root-level `?start: [A]` collapsing to a lone placeholder-`None` (#289).
//!
//! `?start: [A]` / `A: "a"` with `maybe_placeholders=true` on input `""` is the
//! one shape where the *start symbol itself* is a `?`-rule whose sole alternative
//! is an absent `[...]`. Python Lark (the oracle, 1.3.1) collapses the lone `None`
//! placeholder through `?start`'s expand1 to a **bare `None`** at the root on
//! every supported backend:
//!
//!   * `lalr`            → `None`
//!   * `earley` (basic)  → `None`
//!   * `earley` (dynamic)→ `None`
//!   * `cyk`             → rejects ("CYK doesn't support empty rules"); not a
//!                          `None` case, so excluded below.
//!
//! Before #289 lark-rs diverged: LALR returned `UnexpectedEOF` (rejected the
//! empty input at `accept()`), and Earley/dynamic returned an empty `start[]`
//! tree — neither is `None`. The fix lives in root assembly: a start rule whose
//! value collapses to a single `Child::None` yields `ParseTree::None`, the public
//! representation of Python's bare `None`. This is *not* a `tree_builder::shape()`
//! change — the lone-`None` expand1 collapse there (RC9) is correct; the bug was
//! only in how the three backends unwrap the augmented-start root value.

use lark_rs::{Lark, LarkOptions, LexerType, ParserAlgorithm};

fn build(parser: ParserAlgorithm, lexer: LexerType, grammar: &str) -> Lark {
    Lark::new(
        grammar,
        LarkOptions {
            parser,
            lexer,
            start: vec!["start".to_string()],
            maybe_placeholders: true,
            ..Default::default()
        },
    )
    .unwrap_or_else(|e| panic!("grammar failed to load: {e}"))
}

/// The three backends Python yields `None` on must all converge to `ParseTree::None`.
#[test]
fn root_optional_start_empty_input_is_bare_none() {
    let grammar = "?start: [A]\nA: \"a\"";
    let configs = [
        (ParserAlgorithm::Lalr, LexerType::Contextual, "lalr"),
        (ParserAlgorithm::Earley, LexerType::Basic, "earley/basic"),
        (
            ParserAlgorithm::Earley,
            LexerType::Dynamic,
            "earley/dynamic",
        ),
    ];
    for (parser, lexer, label) in configs {
        let lark = build(parser.clone(), lexer.clone(), grammar);
        let result = lark.parse("").unwrap_or_else(|e| {
            panic!("{label}: `?start: [A]` on \"\" must parse (Python → None): {e}")
        });
        assert!(
            result.is_none(),
            "{label}: expected bare None (Python's result), got {result:?}"
        );
    }
}

/// `maybe_placeholders=false` negative control (#382, architect ask on omnibus #354).
/// With `maybe_placeholders=false` the absent `[A]` does *not* inject a `None`
/// placeholder, so `?start: [A]` on `""` must NOT collapse to `ParseTree::None`.
/// Oracle (Python Lark 1.3.1, all three backends) returns an **empty `start` tree**
/// `Tree('start', [])`, not `None`:
///
///   * `lalr`             → Tree('start', [])
///   * `earley` (basic)   → Tree('start', [])
///   * `earley` (dynamic) → Tree('start', [])
///
/// This pins that the #289 bare-`None` root mapping is gated on the placeholder
/// `None` actually being produced (mp=true) and never fires for mp=false — guarding
/// against an accidental `ParseTree::None` when no placeholder exists.
#[test]
fn root_optional_start_empty_input_mp_false_is_empty_tree_not_none() {
    let grammar = "?start: [A]\nA: \"a\"";
    let configs = [
        (ParserAlgorithm::Lalr, LexerType::Contextual, "lalr"),
        (ParserAlgorithm::Earley, LexerType::Basic, "earley/basic"),
        (
            ParserAlgorithm::Earley,
            LexerType::Dynamic,
            "earley/dynamic",
        ),
    ];
    for (parser, lexer, label) in configs {
        // maybe_placeholders=false (the default differs from the other tests here).
        let lark = Lark::new(
            grammar,
            LarkOptions {
                parser: parser.clone(),
                lexer: lexer.clone(),
                start: vec!["start".to_string()],
                maybe_placeholders: false,
                ..Default::default()
            },
        )
        .unwrap_or_else(|e| panic!("{label}: grammar failed to load: {e}"));
        let result = lark.parse("").unwrap_or_else(|e| {
            panic!(
                "{label}: `?start: [A]` on \"\" (mp=false) must parse (Python → empty tree): {e}"
            )
        });
        assert!(
            !result.is_none(),
            "{label}: mp=false must NOT yield ParseTree::None (Python → Tree('start', [])), got {result:?}"
        );
        let tree = result.as_tree().unwrap_or_else(|| {
            panic!("{label}: mp=false expected a Tree('start', []), got {result:?}")
        });
        assert_eq!(tree.data, "start", "{label}: expected root data `start`");
        assert!(
            tree.children.is_empty(),
            "{label}: expected empty children (Python → Tree('start', [])), got {:?}",
            tree.children
        );
    }
}

/// Present-branch sibling: `?start: [A]` on `"a"` collapses to the bare `A` token
/// (expand1 with a single real child), on every backend — the non-empty arm.
#[test]
fn root_optional_start_present_collapses_to_token() {
    let grammar = "?start: [A]\nA: \"a\"";
    for (parser, lexer) in [
        (ParserAlgorithm::Lalr, LexerType::Contextual),
        (ParserAlgorithm::Earley, LexerType::Basic),
        (ParserAlgorithm::Earley, LexerType::Dynamic),
    ] {
        let lark = build(parser.clone(), lexer.clone(), grammar);
        let tok = lark
            .parse("a")
            .unwrap_or_else(|e| panic!("{parser:?}/{lexer:?}: `a` must parse: {e}"))
            .as_token()
            .cloned()
            .unwrap_or_else(|| panic!("{parser:?}/{lexer:?}: expected bare A token"));
        assert_eq!(tok.value, "a", "{parser:?}/{lexer:?}");
        assert_eq!(tok.type_, "A", "{parser:?}/{lexer:?}");
    }
}

/// Negative control: a non-collapsing `?start: A` (no optional) must keep
/// working — `"a"` yields the bare `A` token (expand1), `""` is rejected. This
/// guards that the fix touches *only* the lone-`None` collapse, not the normal
/// `?start` single-token path.
#[test]
fn non_optional_root_start_unchanged() {
    let grammar = "?start: A\nA: \"a\"";
    for (parser, lexer) in [
        (ParserAlgorithm::Lalr, LexerType::Contextual),
        (ParserAlgorithm::Earley, LexerType::Basic),
        (ParserAlgorithm::Earley, LexerType::Dynamic),
    ] {
        let lark = build(parser.clone(), lexer.clone(), grammar);
        // Present input collapses to the bare token, never None.
        let result = lark
            .parse("a")
            .unwrap_or_else(|e| panic!("{parser:?}/{lexer:?}: `a` must parse: {e}"));
        assert!(
            !result.is_none(),
            "{parser:?}/{lexer:?}: `?start: A` on `a` must NOT be None, got {result:?}"
        );
        assert_eq!(
            result.as_token().map(|t| t.value.as_str()),
            Some("a"),
            "{parser:?}/{lexer:?}: expected bare A token"
        );
        // Empty input is genuinely rejected (no nullable arm).
        assert!(
            lark.parse("").is_err(),
            "{parser:?}/{lexer:?}: `?start: A` on \"\" must be rejected"
        );
    }
}
