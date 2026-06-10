//! Hand-authored adversarial canaries for lowering `lark.REGEXP` into the DFA — the
//! Stage-B **regex-literal delimited-token idiom** (`docs/LEXER_DFA_PLAN.md`, Stage B).
//!
//! `lark.REGEXP` is `\/(?!\/)(\\\/|\\\\|[^\/])*?\/[imslux]*` — a slash-delimited `/ body /
//! flags` token whose internal `(?!\/)` rejects the empty `//` and whose lazy escaped-slash
//! body closes at the first unescaped slash. The lowering strips the `(?!\/)` to a non-empty
//! `+` and proves the lazy close equals a greedy `+` (the body cannot consume an unescaped
//! delimiter). These canaries pin the exact seams a wrong lowering would miss — and run
//! under the **default (`Dfa`) backend**, so they gate the real shipped lexer.

mod common;

use lark_rs::lookaround::{lower::recognize_regexp_idiom, parse};
use lark_rs::{basic_lexer_conf, load_grammar, lower, BasicLexer, Lexer, LexerBackend, ParseError};

/// Build a **default-backend** basic lexer for a grammar that imports `lark.REGEXP`
/// (`start: REGEXP+`, whitespace ignored). No explicit `.with_backend`, so this exercises
/// the real default (`Dfa`) path.
fn regexp_lexer() -> BasicLexer {
    let grammar = "start: REGEXP+\n%import lark.REGEXP\n%ignore \" \"\n";
    let g = load_grammar(grammar, &["start".to_string()], false, false)
        .expect("grammar with %import lark.REGEXP builds");
    let cg = lower(&g);
    let conf = basic_lexer_conf(&cg, 0);
    BasicLexer::new(&conf).expect("default-backend BasicLexer builds")
}

/// The lex outcome reduced to the REGEXP token *values* on success, or the failing byte
/// position on a lex error.
fn lex(lexer: &BasicLexer, input: &str) -> Result<Vec<String>, usize> {
    match lexer.lex(input) {
        Ok(tokens) => Ok(tokens
            .into_iter()
            .filter(|t| t.type_ == "REGEXP")
            .map(|t| t.value)
            .collect()),
        Err(ParseError::UnexpectedCharacter { pos, .. }) => Err(pos),
        Err(_) => Err(usize::MAX),
    }
}

/// The anchored match value at offset 0 (the REGEXP token the lexer would take first), or
/// `None` if nothing matches there.
fn first(lexer: &BasicLexer, input: &str) -> Option<String> {
    lexer.match_at(input, 0).map(|(_, v)| v.to_string())
}

/// The default lexer backend is the `Dfa` engine (the lowering's host). The lowered REGEXP
/// rides this backend, so the canaries below test the real shipped lexer, not a synthetic
/// `Dfa`-forced one.
#[test]
fn default_backend_is_dfa() {
    assert_eq!(LexerBackend::default(), LexerBackend::Dfa);
}

/// The empty regex `//` is a lex error / yields **no** REGEXP token: the `(?!\/)` (lowered
/// to a non-empty `+`) forbids the empty body, so nothing opens at byte 0.
#[test]
fn empty_double_slash_is_a_lex_error() {
    let lexer = regexp_lexer();
    assert_eq!(lex(&lexer, "//"), Err(0), "// must be a lex error");
    assert_eq!(first(&lexer, "//"), None, "no REGEXP opens at //");
    // A lone `/` and a never-closed `/a` are likewise no-match (need the close slash).
    assert_eq!(first(&lexer, "/"), None);
    assert_eq!(first(&lexer, "/a"), None);
}

/// `/a/` lexes as exactly one REGEXP token.
#[test]
fn single_regex_literal() {
    let lexer = regexp_lexer();
    assert_eq!(lex(&lexer, "/a/"), Ok(vec!["/a/".to_string()]));
    assert_eq!(first(&lexer, "/a/"), Some("/a/".to_string()));
}

/// `/\//` (an escaped slash inside the body) lexes as one REGEXP — the `\/` does **not**
/// close the literal; the final `/` does.
#[test]
fn escaped_slash_inside() {
    let lexer = regexp_lexer();
    assert_eq!(lex(&lexer, r"/\//"), Ok(vec![r"/\//".to_string()]));
}

/// `/\\/` (an escaped backslash then the close) lexes as one REGEXP.
#[test]
fn escaped_backslash_inside() {
    let lexer = regexp_lexer();
    assert_eq!(lex(&lexer, r"/\\/"), Ok(vec![r"/\\/".to_string()]));
}

/// `/a/i` lexes as one REGEXP **including** the trailing flag; `/abc/im` too. The flags
/// suffix `[imslux]*` is part of the matched token.
#[test]
fn flags_are_included() {
    let lexer = regexp_lexer();
    assert_eq!(lex(&lexer, "/a/i"), Ok(vec!["/a/i".to_string()]));
    assert_eq!(lex(&lexer, "/abc/im"), Ok(vec!["/abc/im".to_string()]));
    // A non-flag letter after the close is not swallowed: `/a/iX` is `/a/i` then `X`.
    assert_eq!(first(&lexer, "/a/iX"), Some("/a/i".to_string()));
}

/// `/a//` does **not** swallow the second slash into the first token — the close is the
/// first unescaped slash, so the first REGEXP is `/a/` and the trailing `/` is left over
/// (here it then fails to lex as a lone slash, the error landing at byte 3).
#[test]
fn does_not_swallow_second_slash() {
    let lexer = regexp_lexer();
    assert_eq!(
        first(&lexer, "/a//"),
        Some("/a/".to_string()),
        "the first token must be exactly /a/, not /a//"
    );
    // Lexing the whole input fails at the leftover lone `/` (byte 3), confirming the first
    // token consumed exactly `/a/`.
    assert_eq!(lex(&lexer, "/a//"), Err(3));
}

/// **The recognizer's reject surface.** Each adversarial near-miss (wrong delimiter, a body
/// with a nested assertion, an unrelated lazy `.*?` body, a missing `(?!\/)`, a missing
/// close, a different flags suffix, a two-slash guard, an extra body arm) must be **declined**
/// by `recognize_regexp_idiom` — accepting one would risk lowering a shape whose match-end is
/// not the proven idiom (a false-accept).
#[test]
fn recognizer_declines_near_misses() {
    for p in common::lowering::regexp_idiom_reject_patterns() {
        let node = parse(p).unwrap_or_else(|e| panic!("parse {p:?} failed: {e:?}"));
        assert!(
            recognize_regexp_idiom(&node).is_none(),
            "recognizer wrongly accepted a non-idiom near-miss: {p:?}"
        );
    }
}

/// The bundled `lark.REGEXP` shape **is** recognized (and its non-capturing-body twin) — the
/// narrowing must not over-decline and break the real terminal.
#[test]
fn recognizer_accepts_the_bundled_shape() {
    for p in [
        r#"\/(?!\/)(\\\/|\\\\|[^\/])*?\/[imslux]*"#,
        r#"\/(?!\/)(?:\\\/|\\\\|[^\/])*?\/[imslux]*"#,
    ] {
        let node = parse(p).unwrap_or_else(|e| panic!("parse {p:?} failed: {e:?}"));
        assert!(
            recognize_regexp_idiom(&node).is_some(),
            "recognizer must accept the bundled regex-literal idiom: {p:?}"
        );
    }
}
