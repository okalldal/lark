//! Construct-error parity (compliance milestone M7): grammars Python Lark rejects
//! at construction must fail to build in lark-rs too, rather than silently produce
//! a parser that diverges later.

mod common;

use lark_rs::{Lark, LarkOptions, LexerType, ParserAlgorithm};

fn try_build(grammar: &str) -> Result<Lark, lark_rs::LarkError> {
    Lark::new(
        grammar,
        LarkOptions {
            parser: ParserAlgorithm::Lalr,
            lexer: LexerType::Contextual,
            start: vec!["start".to_string()],
            ..Default::default()
        },
    )
}

#[test]
fn test_empty_repetition_range_is_rejected() {
    // `~3..2` has min > max, so it matches nothing; Lark raises at construction.
    assert!(try_build("!start: \"A\"~3..2").is_err());
    // A well-formed range still builds.
    assert!(try_build("!start: \"A\"~2..3").is_ok());
}

#[test]
fn test_unresolvable_import_is_rejected() {
    // Only the bundled `common` library is available; importing from a non-existent
    // module must error (Lark raises when the module is not found) instead of
    // silently dropping the symbol.
    assert!(try_build("start: NUMBER WORD\n%import bad_test.NUMBER").is_err());
    // A `common` import still resolves.
    assert!(try_build("start: WORD\n%import common.WORD").is_ok());
}

#[test]
fn test_deeply_nested_terminal_fails_gracefully_without_aborting() {
    // #455: a terminal regex with thousands of nested groups used to drive the
    // lookaround front-end's unbounded recursion (`lookaround::parse` /
    // `width_range`, reached via `Pattern::max_width` during terminal ordering on
    // *every* lexer build) to a stack overflow that **aborted the whole process**.
    // With the front-end's nesting cap it must instead fail to build with a normal,
    // recoverable error — reaching the assertion at all is the evidence the build did
    // not abort.
    let depth = 50_000;
    let grammar = format!(
        "start: TOK\nTOK: /{}a{}/\n",
        "(".repeat(depth),
        ")".repeat(depth)
    );
    assert!(
        try_build(&grammar).is_err(),
        "a pathologically deep terminal must error, not abort the process"
    );

    // A deep-but-reasonable terminal still builds (the cap only bites the pathological
    // case, well past anything a real grammar reaches).
    let ok_depth = 20;
    let ok_grammar = format!(
        "start: TOK\nTOK: /{}a{}/\n",
        "(".repeat(ok_depth),
        ")".repeat(ok_depth)
    );
    assert!(
        try_build(&ok_grammar).is_ok(),
        "a reasonably-nested terminal must still build"
    );
}
