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
