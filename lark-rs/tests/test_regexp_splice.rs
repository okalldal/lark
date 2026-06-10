//! The **hand-authored adversarial gate** for lowering `lark.REGEXP` into the DFA — the
//! Stage-B regex-literal delimited-token idiom (`docs/LEXER_DFA_PLAN.md`, "Stage B";
//! `src/lookaround/lower.rs::recognize_regexp_idiom`). The sibling of
//! `test_regexp_splice`'s namesake `test_string_splice.rs`.
//!
//! `lark.REGEXP`'s `(?!\/)` after the opening slash is the empty-body guard: it makes
//! `//` a lex error while `/a/` is one token — and the lazy close must not let `/a//`
//! swallow the trailing slash. The lowering is the proven Type-A rewrite
//! `\/(\\\/|\\\\|[^\/])+\/[imslux]*` (one unguarded branch); these canaries pin its
//! end-to-end behavior on a real grammar that **imports the bundled terminal**
//! (`%import lark.REGEXP`) under the **default lexer backend** — so they gate the real
//! shipped lexer, with `fancy-regex` nowhere on this terminal's path.

use lark_rs::lookaround::{lower::recognize_regexp_idiom, parse};
use lark_rs::{
    basic_lexer_conf, load_grammar, lower, route_terminal, BasicLexer, Lexer, LexerBackend,
    LoweringRoute, ParseError,
};

/// The bundled `lark.REGEXP` pattern, verbatim (`src/grammars/lark.lark`).
const REGEXP_RAW: &str = r#"\/(?!\/)(\\\/|\\\\|[^\/])*?\/[imslux]*"#;

/// Build a basic lexer for a grammar that imports the bundled `lark.REGEXP` (a single
/// terminal, whitespace ignored), under the **default** backend — which must be `Dfa`,
/// so these canaries gate the lowered engine, not an explicitly-selected one.
fn regexp_lexer() -> BasicLexer {
    assert!(
        matches!(LexerBackend::default(), LexerBackend::Dfa),
        "the default lexer backend must be Dfa for these canaries to gate the lowering"
    );
    let grammar = "start: REGEXP+\n%import lark.REGEXP\n%ignore \" \"\n";
    let g = load_grammar(grammar, &["start".to_string()], false, false)
        .expect("grammar with %import lark.REGEXP builds");
    let cg = lower(&g);
    let conf = basic_lexer_conf(&cg, 0); // default backend (Dfa)
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

/// The bundled REGEXP is **actually lowered** (one unguarded lookaround-free branch),
/// not routed to `fancy-regex` — the whole point of the Stage-B idiom.
#[test]
fn regexp_actually_lowers_to_one_unguarded_branch() {
    match route_terminal("REGEXP", REGEXP_RAW) {
        LoweringRoute::Lowered(branches) => {
            assert_eq!(branches.len(), 1);
            let b = &branches[0];
            assert_eq!(b.regex, r#"\/(\\\/|\\\\|[^\/])+\/[imslux]*"#);
            assert!(b.leading.is_none() && b.trailing.is_none() && b.lookbehind.is_empty());
        }
        other => panic!("lark.REGEXP must route to Lowered, got {other:?}"),
    }
}

/// **The canaries.** The `(?!\/)` empty-body boundary and the lazy-close behavior, end
/// to end on the default backend: `//` never lexes as a REGEXP; `/a/` (and the escape
/// and flags forms) lex as exactly one token; `/a//` does **not** swallow the second
/// slash into the first token.
#[test]
fn empty_body_rejected_and_real_literals_lex_as_one_token() {
    let lexer = regexp_lexer();

    // The empty `//`: no REGEXP opens there → lex error at byte 0. (This is what the
    // dropped `(?!\/)` guard enforced; the non-empty body `+` re-enforces it.)
    assert_eq!(lex(&lexer, "//"), Err(0), "// must be a lex error");
    // A bare unterminated `/` (and an unterminated body) likewise never lex.
    assert_eq!(lex(&lexer, "/"), Err(0), "/ must be a lex error");
    assert_eq!(lex(&lexer, "/a"), Err(0), "/a must be a lex error");

    // One-token literals: plain body, escaped slash, escaped backslash, flags suffix.
    for s in ["/a/", r"/\//", r"/\\/", "/a/i", "/abc/im", r"/a\/b/x"] {
        assert_eq!(
            lex(&lexer, s),
            Ok(vec![s.to_string()]),
            "{s:?} must lex as one REGEXP token"
        );
    }

    // The lazy-close pin: `/a//` is `/a/` followed by a stray `/` that fails to lex at
    // byte 3 — the first token must NOT swallow the second slash (the body cannot cross
    // an unescaped `/`, and `/` is not a flag).
    assert_eq!(
        lex(&lexer, "/a//"),
        Err(3),
        "/a// must lex /a/ then fail on the stray slash, not swallow it"
    );

    // Two separated regexps are two tokens (the boundary composes with %ignore).
    assert_eq!(
        lex(&lexer, "/a/ /b/i"),
        Ok(vec!["/a/".to_string(), "/b/i".to_string()]),
    );
}

/// The recognizer's reject surface at the route level (the test-tree complement of the
/// unit tests in `src/lookaround/lower.rs`): each near-miss must not be recognized and
/// must keep its pre-idiom route — never lowered.
#[test]
fn near_miss_idioms_are_not_recognized_and_do_not_lower() {
    let near_misses = [
        // Wrong delimiter.
        r#"\"(?!\")(\\\"|\\\\|[^\"])*?\"[imslux]*"#,
        // Missing the `(?!\/)` guard (also a plain regex-crate pattern: stays Plain).
        r#"\/(\\\/|\\\\|[^\/])*?\/[imslux]*"#,
        // Body with a nested assertion.
        r#"\/(?!\/)(\\\/|\\\\|(?=x)[^\/])*?\/[imslux]*"#,
        // Body replaced by an unrelated lazy `.*?`.
        r#"\/(?!\/).*?\/[imslux]*"#,
        // Missing the close slash.
        r#"\/(?!\/)(\\\/|\\\\|[^\/])*?[imslux]*"#,
        // Out-of-surface flags suffixes: a range class, a slash, a `+` quantifier.
        r#"\/(?!\/)(\\\/|\\\\|[^\/])*?\/[a-z]*"#,
        r#"\/(?!\/)(\\\/|\\\\|[^\/])*?\/[im\/]*"#,
        r#"\/(?!\/)(\\\/|\\\\|[^\/])*?\/[imslux]+"#,
    ];
    for p in near_misses {
        let node = parse(p).unwrap_or_else(|e| panic!("parse {p:?} failed: {e:?}"));
        assert!(
            recognize_regexp_idiom(&node).is_none(),
            "recognizer wrongly accepted a near-miss idiom: {p:?}"
        );
        // The guardless variant is genuinely Plain (no lookaround at all); every other
        // near-miss keeps its internal `(?!…)` and must NOT lower.
        let route = route_terminal("ADV", p);
        assert!(
            !matches!(route, LoweringRoute::Lowered(_)),
            "near-miss idiom must not lower: {p:?} → {route:?}"
        );
    }
}
