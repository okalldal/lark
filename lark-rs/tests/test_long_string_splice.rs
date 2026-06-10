//! The **hand-authored adversarial gate** for lowering `python.LONG_STRING` into the
//! DFA — the third Stage-B **delimited-token idiom** (`docs/LEXER_DFA_PLAN.md`,
//! "Stage B — the delimited-token idiom family"; the sibling of
//! `tests/test_string_splice.rs` and `tests/test_regexp_splice.rs`).
//!
//! `python.LONG_STRING` is `([ubf]?r?|r[ubf])(""".*?(?<!\\)(\\\\)*?"""|'''…''')` with
//! `/is` flags — `<prefix> <qqq> body <qqq>` whose `(?<!\\)(\\\\)*?` escape-parity close
//! sits after the variable-width `.*?`. The lowering
//! (`src/lookaround/lower.rs::recognize_long_string_idiom`) absorbs it into lazy
//! escape-pair body items (`(?:[^\\]|\\.)*?` under DOTALL): a backslash can only be
//! consumed as the start of a pair, so item boundaries fall exactly at even
//! backslash-parity positions — the original close condition — and the kept lazy `*?`
//! picks the first such `<qqq>`. The canaries below pin the behaviors a wrong lowering
//! would break — the empty `""""""`, the lazy first-valid-triple close on quote runs,
//! lone quotes inside the body, the escape parity (`"""\"""` rejected), the DOTALL
//! newline body, and the prefixes — under the **default (`Dfa`) backend**, so they gate
//! the real shipped lexer with the real bundled grammar import
//! (`%import python.LONG_STRING`, which carries the real `/is` flags).

mod common;

use common::lowering::{corpus, long_string_idiom_reject_patterns, LONG_STRING_RAW};
use lark_rs::lookaround::{lower::recognize_long_string_idiom, parse};
use lark_rs::{
    basic_lexer_conf, load_grammar, lower, lower_terminal_dotall, route_terminal_dotall,
    BasicLexer, Lexer, LexerBackend, Lowered, LoweringRoute, ParseError,
};

/// Build a basic lexer for a grammar that imports the real bundled
/// `python.LONG_STRING` (whitespace ignored), under the given backend. The default
/// backend is asserted to be `Dfa`, so the canaries exercise the lowered engine, not a
/// fancy side-probe.
fn long_string_lexer(backend: LexerBackend) -> BasicLexer {
    assert_eq!(
        LexerBackend::default(),
        LexerBackend::Dfa,
        "the canaries must run on the default Dfa backend"
    );
    let grammar = "start: LONG_STRING+\n%import python.LONG_STRING\n%ignore \" \"\n";
    let g = load_grammar(grammar, &["start".to_string()], false, false)
        .expect("grammar with %import python.LONG_STRING builds");
    let cg = lower(&g);
    let conf = basic_lexer_conf(&cg, 0).with_backend(backend);
    BasicLexer::new(&conf).expect("BasicLexer builds")
}

/// The lex outcome reduced to the token *values* of `ty` on success, or the failing
/// byte position on a lex error.
fn lex_ty(lexer: &BasicLexer, ty: &str, input: &str) -> Result<Vec<String>, usize> {
    match lexer.lex(input) {
        Ok(tokens) => Ok(tokens
            .into_iter()
            .filter(|t| t.type_ == ty)
            .map(|t| t.value)
            .collect()),
        Err(ParseError::UnexpectedCharacter { pos, .. }) => Err(pos),
        Err(_) => Err(usize::MAX),
    }
}

fn lex(lexer: &BasicLexer, input: &str) -> Result<Vec<String>, usize> {
    lex_ty(lexer, "LONG_STRING", input)
}

/// The idiom is **actually lowered**, not routed to `fancy-regex`: two unguarded
/// lookaround-free branches (one per quote arm; the escape-parity lookbehind is
/// absorbed into the `\\.` pairing, not carried as a guard) — the whole point of the
/// milestone.
#[test]
fn long_string_actually_lowers_to_two_unguarded_branches_under_dfa() {
    match route_terminal_dotall("LONG_STRING", LONG_STRING_RAW, true) {
        LoweringRoute::Lowered(branches) => {
            assert_eq!(branches.len(), 2, "got {branches:#?}");
            assert_eq!(
                branches[0].regex,
                r#"([ubf]?r?|r[ubf])"""(?:[^\\]|\\.)*?""""#
            );
            assert_eq!(
                branches[1].regex,
                r#"([ubf]?r?|r[ubf])'''(?:[^\\]|\\.)*?'''"#
            );
            for b in &branches {
                assert!(
                    b.leading.is_none() && b.trailing.is_none() && b.lookbehind.is_empty(),
                    "the (?<!\\\\)(\\\\\\\\)*? is absorbed by escape pairing, not a guard"
                );
            }
        }
        other => panic!("python.LONG_STRING must lower, got {other:?}"),
    }
    assert!(matches!(
        lower_terminal_dotall("LONG_STRING", LONG_STRING_RAW, true),
        Ok(Lowered::Branches(_))
    ));
}

/// The recognizer's acceptance surface is exact: every near-miss (two-quote delimiter,
/// mismatched open/close, missing/wrong/positive lookbehind, greedy quantifiers, a
/// non-quote tripled delimiter) must NOT be recognized and must NOT lower to branches —
/// each would need its own proof, so reject-when-unsure routes it to the generic path.
#[test]
fn recognizer_rejects_near_miss_shapes() {
    for p in long_string_idiom_reject_patterns() {
        let node = parse(&p).unwrap_or_else(|e| panic!("parse {p:?} failed: {e:?}"));
        assert!(
            recognize_long_string_idiom(&node).is_none(),
            "recognizer wrongly accepted a near-miss idiom: {p:?}"
        );
        assert!(
            !matches!(
                lower_terminal_dotall("ADV", &p, true),
                Ok(Lowered::Branches(_))
            ),
            "near-miss idiom must NOT lower to branches: {p:?}"
        );
    }
}

/// **The empty/quote-run canaries.** `""""""` is ONE empty long string; the lazy close
/// takes the *first* valid triple, so a 7-quote run is the empty token + a dangling
/// quote (a lex error), never one over-long token.
#[test]
fn empty_long_string_and_quote_runs() {
    let lexer = long_string_lexer(LexerBackend::Dfa);
    assert_eq!(
        lex(&lexer, "\"\"\"\"\"\""),
        Ok(vec!["\"\"\"\"\"\"".to_string()]),
        "\"\"\"\"\"\" must be one empty LONG_STRING"
    );
    assert_eq!(
        lex(&lexer, "''''''"),
        Ok(vec!["''''''".to_string()]),
        "'''''' must be one empty LONG_STRING"
    );
    // 7 quotes: lazy close at 6, the dangling quote fails to open anything.
    assert_eq!(
        lex(&lexer, "\"\"\"\"\"\"\""),
        Err(6),
        "a 7-quote run is the empty token then a lex error at byte 6 (lazy close)"
    );
    // Unterminated bodies never match.
    assert_eq!(lex(&lexer, "\"\"\"abc"), Err(0));
    assert_eq!(lex(&lexer, "\"\"\"abc\"\""), Err(0));
    assert_eq!(lex(&lexer, "\"\"\""), Err(0));
}

/// Lone quotes **inside** the body do not close — unlike the STRING splice, the
/// delimiter quote is not excluded from the body class; only a full unescaped triple
/// closes (laziness picks the first one).
#[test]
fn lone_quotes_inside_the_body_do_not_close() {
    let lexer = long_string_lexer(LexerBackend::Dfa);
    for s in [
        "\"\"\"a\"b\"\"\"",   // single quote inside
        "\"\"\"a\"\"b\"\"\"", // double quote inside
        "\"\"\"a'b''c\"\"\"", // other-kind quotes inside
        "'''a\"b\"\"c'''",    // dq runs inside an sq long string
    ] {
        assert_eq!(
            lex(&lexer, s),
            Ok(vec![s.to_string()]),
            "{s:?} must lex as one LONG_STRING token"
        );
    }
}

/// **The DOTALL canary.** The real terminal is `/is`: a newline lives inside the body
/// (the very thing that distinguishes LONG_STRING from STRING). A lowering that lost
/// the dotall threading (emitting the `[^\\\n]` non-dotall class) would reject this.
#[test]
fn newline_lives_inside_the_body() {
    let lexer = long_string_lexer(LexerBackend::Dfa);
    assert_eq!(
        lex(&lexer, "\"\"\"a\nb\"\"\""),
        Ok(vec!["\"\"\"a\nb\"\"\"".to_string()]),
        "a docstring spans lines"
    );
    assert_eq!(
        lex(&lexer, "'''\n\n'''"),
        Ok(vec!["'''\n\n'''".to_string()])
    );
}

/// **The escape-parity canaries.** An escaped quote eats the first closing quote
/// (`"""a\""""` is one 9-char token; `"""a\"""` has no close at all); a close preceded
/// by an odd backslash run is rejected; an even run closes.
#[test]
fn escape_parity_decides_the_close() {
    let lexer = long_string_lexer(LexerBackend::Dfa);
    // `"""a\""""` — the `\"` consumes the first quote of the run; the next three close.
    assert_eq!(
        lex(&lexer, r#""""a\"""""#),
        Ok(vec![r#""""a\"""""#.to_string()])
    );
    // `"""a\"""` — the `\"` leaves only `""`: no close anywhere.
    assert_eq!(lex(&lexer, r#""""a\""""#), Err(0));
    // Odd backslash run before the triple: rejected.
    assert_eq!(lex(&lexer, r#""""\""""#), Err(0));
    // Even runs close.
    assert_eq!(
        lex(&lexer, r#""""\\""""#),
        Ok(vec![r#""""\\""""#.to_string()])
    );
    assert_eq!(
        lex(&lexer, r#""""\\\\""""#),
        Ok(vec![r#""""\\\\""""#.to_string()])
    );
}

/// The `r`/`b`/`u`/`f` and combined prefixes lex correctly, case-insensitively (the
/// terminal's `/i`).
#[test]
fn long_string_prefixes_lex() {
    let lexer = long_string_lexer(LexerBackend::Dfa);
    for s in [
        "r\"\"\"x\"\"\"",
        "rb'''y'''",
        "br\"\"\"z\"\"\"",
        "B\"\"\"x\"\"\"", // case-insensitive prefix
        "F'''x'''",
        "r\"\"\"\"\"\"", // prefixed empty long string
    ] {
        assert_eq!(
            lex(&lexer, s),
            Ok(vec![s.to_string()]),
            "{s:?} must lex as one LONG_STRING token"
        );
    }
}

/// Adjacent tokens split correctly — the lazy close never bleeds into a neighbour.
#[test]
fn adjacent_long_strings_are_separate_tokens() {
    let lexer = long_string_lexer(LexerBackend::Dfa);
    assert_eq!(
        lex(&lexer, "\"\"\"\"\"\"''''''"),
        Ok(vec!["\"\"\"\"\"\"".to_string(), "''''''".to_string()])
    );
    assert_eq!(
        lex(&lexer, "\"\"\"a\"\"\" \"\"\"b\"\"\""),
        Ok(vec![
            "\"\"\"a\"\"\"".to_string(),
            "\"\"\"b\"\"\"".to_string()
        ])
    );
}

/// **The STRING-vs-LONG_STRING interplay**, with both bundled terminals imported (both
/// lowered now): a triple-quoted literal is ONE LONG_STRING — python.STRING's `(?!"")`
/// opening guard refuses to open inside the quote run — and a short string stays a
/// STRING.
#[test]
fn triple_quote_prefers_long_string_over_string() {
    let grammar = "start: (STRING | LONG_STRING)+\n\
                   %import python.STRING\n\
                   %import python.LONG_STRING\n\
                   %ignore \" \"\n";
    let g = load_grammar(grammar, &["start".to_string()], false, false)
        .expect("grammar importing both bundled string terminals builds");
    let cg = lower(&g);
    let conf = basic_lexer_conf(&cg, 0).with_backend(LexerBackend::Dfa);
    let lexer = BasicLexer::new(&conf).expect("Dfa BasicLexer builds");

    assert_eq!(
        lex_ty(&lexer, "LONG_STRING", "\"\"\"doc\"\"\""),
        Ok(vec!["\"\"\"doc\"\"\"".to_string()]),
        "a docstring is one LONG_STRING"
    );
    assert_eq!(
        lex_ty(&lexer, "STRING", "\"\"\"doc\"\"\""),
        Ok(vec![]),
        "…and zero STRINGs"
    );
    assert_eq!(
        lex_ty(&lexer, "STRING", "\"d\""),
        Ok(vec!["\"d\"".to_string()]),
        "a short string is one STRING"
    );
    assert_eq!(
        lex_ty(&lexer, "LONG_STRING", "\"\"\"\"\"\""),
        Ok(vec!["\"\"\"\"\"\"".to_string()]),
        "six quotes are one empty LONG_STRING (STRING's (?!\"\") guard refuses)"
    );
}

/// **The exhaustive dotall backend differential** — the per-grammar restriction of the
/// master differential, under the terminal's real `/is` flags (the gap the unflagged
/// generative harness leaves): every string over `" \ a \n` up to length 8, lexed under
/// `LexerBackend::Regex` (LONG_STRING on `fancy-regex`) and `LexerBackend::Dfa`
/// (lowered), must produce identical outcomes. The signal is two-sided: the few clean
/// lexes must agree on the token stream, and the many failing inputs must agree on the
/// **failure byte** — which is where a divergent match end would surface (e.g. an
/// 8-quote run errs at byte 6 only if both engines lazily close the empty token at 6).
#[test]
fn dotall_backend_differential_over_quote_backslash_newline_corpus() {
    let reference = long_string_lexer(LexerBackend::Regex);
    let dfa = long_string_lexer(LexerBackend::Dfa);
    let mut compared = 0usize;
    let mut matched = 0usize;
    for input in corpus(&['"', '\\', 'a', '\n'], 8) {
        let a = lex(&reference, &input);
        let b = lex(&dfa, &input);
        assert_eq!(a, b, "backend divergence on {input:?}");
        compared += 1;
        matched += matches!(&a, Ok(v) if !v.is_empty()) as usize;
    }
    assert!(
        compared > 80_000,
        "the corpus must be exhaustive ({compared})"
    );
    // Whole-input lexing is strict (LONG_STRING+ only), so clean lexes are rare —
    // but the newline-bearing `"""\n"""`-style tokens must be among them, or the
    // dotall path was not exercised.
    assert!(
        matched >= 10,
        "the corpus must actually exercise LONG_STRING matches ({matched})"
    );
}
