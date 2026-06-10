//! The **hand-authored adversarial gate** for lowering `lark.REGEXP` into the DFA —
//! the Stage-B **regex-literal idiom** (`docs/LEXER_DFA_PLAN.md`, "Stage B — the
//! delimited-token idiom family"; the sibling of `tests/test_string_splice.rs`).
//!
//! `lark.REGEXP` is `\/(?!\/)(\\\/|\\\\|[^\/])*?\/[imslux]*` — `/ body / flags` with an
//! internal `(?!\/)` that rejects the empty `//`. The lowering
//! (`src/lookaround/lower.rs::recognize_regexp_idiom`) absorbs the guard into a
//! non-empty-body bump (`*?` → `+?`): at the guard position the close (`/`) and every
//! body alternative (`\/`, `\\`, `[^/]`) start with disjoint chars, so "not followed by
//! `/`" is exactly "at least one body item". The canaries below pin the behaviors a
//! wrong lowering would break — the `//` reject, the lazy first-unescaped-slash close,
//! the escaped-slash/backslash pairing, the dangling-escape backtracking close, and the
//! greedy flags suffix — under the **default (`Dfa`) backend**, so they gate the real
//! shipped lexer with the real bundled grammar import (`%import lark.REGEXP`).

mod common;

use common::lowering::{regexp_idiom_reject_patterns, REGEXP_RAW};
use lark_rs::lookaround::{lower::recognize_regexp_idiom, parse};
use lark_rs::{
    basic_lexer_conf, load_grammar, lower, lower_terminal, route_terminal, BasicLexer, Lexer,
    LexerBackend, Lowered, LoweringRoute, ParseError,
};

/// Build a basic lexer for a grammar that imports the real bundled `lark.REGEXP`
/// (whitespace ignored), under the **default backend** — which this asserts is `Dfa`,
/// so every canary below exercises the lowered engine, not a fancy side-probe.
fn regexp_lexer() -> BasicLexer {
    assert_eq!(
        LexerBackend::default(),
        LexerBackend::Dfa,
        "the canaries must run on the default Dfa backend"
    );
    let grammar = "start: REGEXP+\n%import lark.REGEXP\n%ignore \" \"\n";
    let g = load_grammar(grammar, &["start".to_string()], false, false)
        .expect("grammar with %import lark.REGEXP builds");
    let cg = lower(&g);
    let conf = basic_lexer_conf(&cg, 0); // default backend = Dfa (asserted above)
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

/// The idiom is **actually lowered**, not routed to `fancy-regex`: one unguarded
/// lookaround-free branch (the guard is absorbed into the `+?` bump, not carried as a
/// guard table) — the whole point of the milestone.
#[test]
fn regexp_actually_lowers_to_one_unguarded_branch_under_dfa() {
    match route_terminal("REGEXP", REGEXP_RAW) {
        LoweringRoute::Lowered(branches) => {
            assert_eq!(branches.len(), 1, "got {branches:#?}");
            let b = &branches[0];
            assert_eq!(b.regex, r"\/(\\\/|\\\\|[^\/])+?\/[imslux]*");
            assert!(
                b.leading.is_none() && b.trailing.is_none() && b.lookbehind.is_empty(),
                "the (?!\\/) is absorbed by the non-empty-body bump, not carried as a guard"
            );
        }
        other => panic!("lark.REGEXP must lower, got {other:?}"),
    }
    assert!(matches!(
        lower_terminal("REGEXP", REGEXP_RAW),
        Ok(Lowered::Branches(_))
    ));
}

/// The recognizer's acceptance surface is exact: every near-miss (wrong delimiter,
/// changed guard, mutated body, greedy quantifier, missing close, different flags)
/// must NOT be recognized and must NOT lower to branches — each would need its own
/// proof, so reject-when-unsure routes it to the generic path.
#[test]
fn recognizer_rejects_near_miss_shapes() {
    for p in regexp_idiom_reject_patterns() {
        let node = parse(&p).unwrap_or_else(|e| panic!("parse {p:?} failed: {e:?}"));
        assert!(
            recognize_regexp_idiom(&node).is_none(),
            "recognizer wrongly accepted a near-miss idiom: {p:?}"
        );
        assert!(
            !matches!(lower_terminal("ADV", &p), Ok(Lowered::Branches(_))),
            "near-miss idiom must NOT lower to branches: {p:?}"
        );
    }
}

/// **The empty-body canary.** `//` is a LEX ERROR — the `(?!\/)`'s entire job. A
/// lowering that dropped the guard without the `+?` bump would accept it as an empty
/// regex literal.
#[test]
fn empty_regexp_is_a_lex_error() {
    let lexer = regexp_lexer();
    assert_eq!(lex(&lexer, "//"), Err(0), "// must be a lex error");
    assert_eq!(lex(&lexer, "///"), Err(0), "/// must be a lex error");
    assert_eq!(lex(&lexer, "//i"), Err(0), "//i must be a lex error");
    // A bare `/` (no close) is likewise no REGEXP.
    assert_eq!(lex(&lexer, "/"), Err(0));
    assert_eq!(
        lex(&lexer, "/a"),
        Err(0),
        "unterminated /a must be a lex error"
    );
}

/// The plain shapes lex as single REGEXP tokens: simple bodies, escaped slashes,
/// escaped backslashes, and flags.
#[test]
fn regexp_literals_lex_as_single_tokens() {
    let lexer = regexp_lexer();
    for s in [
        "/a/",      // minimal body
        "/abc/",    // longer body
        "/a/i",     // one flag
        "/abc/im",  // multi-flag suffix
        r"/\//",    // escaped slash body — the \/ does not close
        r"/\\/",    // escaped backslash body — the \\ lets the next / close
        r"/a\/b/",  // escaped slash mid-body
        r"/\\\\/",  // two escaped backslashes
        r"/[a-z]/", // class-looking content (just chars to REGEXP)
    ] {
        assert_eq!(
            lex(&lexer, s),
            Ok(vec![s.to_string()]),
            "{s:?} must lex as one REGEXP token"
        );
    }
}

/// **The lazy-close canary.** The body is lazy: the token closes at the *first*
/// unescaped slash, never swallowing a following `/`. `/a//` is `/a/` + a dangling `/`
/// (a lex error at byte 3 — not one over-long token), and `/a//b/` is exactly two
/// REGEXP tokens.
#[test]
fn second_slash_is_not_swallowed() {
    let lexer = regexp_lexer();
    assert_eq!(
        lex(&lexer, "/a//"),
        Err(3),
        "/a// must lex /a/ then fail on the dangling slash at byte 3"
    );
    assert_eq!(
        lex(&lexer, "/a//b/"),
        Ok(vec!["/a/".to_string(), "/b/".to_string()]),
        "/a//b/ must be exactly two adjacent REGEXP tokens"
    );
    assert_eq!(
        lex(&lexer, "/a/ /b/"),
        Ok(vec!["/a/".to_string(), "/b/".to_string()]),
    );
}

/// **The flags-suffix canary.** `[imslux]*` is greedy but stops exactly at the first
/// non-flag char: `/a/iX` is the token `/a/i` followed by a lex error on `X` (matching
/// what fancy-regex says — the flags never swallow `X`, and the token never gives the
/// `i` back).
#[test]
fn flags_stop_exactly_at_the_first_non_flag() {
    let lexer = regexp_lexer();
    assert_eq!(
        lex(&lexer, "/a/iX"),
        Err(4),
        "/a/iX must lex /a/i then fail on X at byte 4"
    );
    // All six flag letters, in one run.
    assert_eq!(lex(&lexer, "/a/imslux"), Ok(vec!["/a/imslux".to_string()]),);
    // A flag run interrupted by a new literal: two tokens.
    assert_eq!(
        lex(&lexer, "/a/i /b/m"),
        Ok(vec!["/a/i".to_string(), "/b/m".to_string()]),
    );
}

/// **The dangling-escape backtracking canary.** On an *unterminated* literal whose body
/// holds an escaped slash, the backtracking oracle re-reads the `\` as a plain body char
/// and closes at the `/` it had escaped: `/a\/b` matches `/a\/` (then `b` fails to lex),
/// and `/a\/i` is one token — the close at the re-read slash plus the `i` *flag*. The
/// lowered branch must reproduce this exactly (it is the regression a "first unescaped
/// slash" approximation of the lazy close would get wrong).
#[test]
fn dangling_escaped_slash_closes_by_backtracking() {
    let lexer = regexp_lexer();
    assert_eq!(
        lex(&lexer, r"/a\/b"),
        Err(4),
        r"/a\/b must lex /a\/ then fail on b at byte 4"
    );
    assert_eq!(
        lex(&lexer, r"/a\/i"),
        Ok(vec![r"/a\/i".to_string()]),
        r"/a\/i must be ONE token: the fallback close at the escaped slash + the i flag"
    );
    // With a real close later, no fallback: the escaped slash stays escaped.
    assert_eq!(lex(&lexer, r"/a\/b/"), Ok(vec![r"/a\/b/".to_string()]));
}
