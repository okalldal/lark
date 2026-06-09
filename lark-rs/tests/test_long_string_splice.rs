//! The **non-negotiable adversarial gate** for lowering `python.LONG_STRING` into the DFA
//! (the multi-character-close idiom — `docs/LEXER_DFA_PLAN.md`, "How the lowering works").
//!
//! `python.LONG_STRING`'s arm `<delim>.*?(?<!\\)(\\\\)*?<delim>` has a **multi-character**
//! close (`"""` / `'''`) and **no** opening guard. Because a lone `"` is legal *inside* a
//! `"""…"""` body, the lowered body cannot exclude the delimiter the way `python.STRING`'s
//! single-character close lets it — so the body stays **lazy** and the first `"""` closes
//! the string. The adversarial case is therefore lazy-vs-greedy: `"""a""""""b"""` must be
//! **two** long strings (each closing at its first `"""`), not one greedy match to the
//! final `"""`. `fancy-regex` is the oracle this is verified against (the scanner
//! differential); this hand-authored test runs under the **default (`Dfa`) backend**, so it
//! gates the real shipped lexer, not a synthetic one.

mod common;

use lark_rs::{
    basic_lexer_conf, load_grammar, lower, lower_terminal_dotall, BasicLexer, Lexer, LexerBackend,
    Lowered, ParseError,
};

/// The bundled `python.LONG_STRING` pattern, verbatim (the `/is` flags live on the terminal).
const LONG_STRING_RAW: &str =
    r#"([ubf]?r?|r[ubf])(""".*?(?<!\\)(\\\\)*?"""|'''.*?(?<!\\)(\\\\)*?''')"#;

/// Build a default-backend (`Dfa`) basic lexer for a grammar importing `python.LONG_STRING`
/// (a single `TOK: LONG_STRING` terminal). No `%ignore`, so adjacent long strings stay
/// adjacent — the lazy-close canary depends on it.
fn long_string_lexer() -> BasicLexer {
    let grammar = "start: LONG_STRING+\n%import python.LONG_STRING\n";
    let g = load_grammar(grammar, &["start".to_string()], false, false)
        .expect("grammar with %import python.LONG_STRING builds");
    let cg = lower(&g);
    let conf = basic_lexer_conf(&cg, 0).with_backend(LexerBackend::Dfa);
    BasicLexer::new(&conf).expect("Dfa BasicLexer builds")
}

/// The lex outcome reduced to the LONG_STRING token *values* on success, or the failing
/// byte position on a lex error.
fn lex(lexer: &BasicLexer, input: &str) -> Result<Vec<String>, usize> {
    match lexer.lex(input) {
        Ok(tokens) => Ok(tokens
            .into_iter()
            .filter(|t| t.type_ == "LONG_STRING")
            .map(|t| t.value)
            .collect()),
        Err(ParseError::UnexpectedCharacter { pos, .. }) => Err(pos),
        Err(_) => Err(usize::MAX),
    }
}

/// The splice is **actually lowered**, not silently routed to `fancy-regex`. The lowering
/// returns two unguarded branches (one per quote kind), so the DFA hosts LONG_STRING — the
/// whole point of the milestone. The `(?<!\\)` lookbehind is absorbed by the body
/// normalization, so no branch carries a guard.
#[test]
fn long_string_actually_lowers_to_branches_under_dfa() {
    let lowered = lower_terminal_dotall("LONG_STRING", LONG_STRING_RAW, true)
        .expect("LONG_STRING must lower (not decline) now");
    match lowered {
        Lowered::Branches(branches) => {
            assert_eq!(
                branches.len(),
                2,
                "LONG_STRING lowers to one unguarded branch per quote-kind arm, got {branches:#?}"
            );
            for b in &branches {
                assert!(
                    b.leading.is_none() && b.trailing.is_none() && b.lookbehind.is_empty(),
                    "the multi-character-close arm lowers to an unguarded branch (the \
                     `(?<!\\\\)` is absorbed into the body class), got {b:?}"
                );
            }
        }
        other => panic!("LONG_STRING must lower to Branches, got {other:?}"),
    }
}

/// A run of `n` copies of the quote char `q` — used to build the quote-heavy canary
/// inputs unambiguously (raw-string quote counting is too error-prone here).
fn q(c: char, n: usize) -> String {
    std::iter::repeat(c).take(n).collect()
}

/// **The canary.** The lazy multi-character close: `"""a""""""b"""` is **two** long strings
/// (`"""a"""` then `"""b"""`), each closing at its *first* `"""` — not one greedy match to
/// the final `"""`. A greedy (or delimiter-excluding) body normalization would get this
/// wrong, matching the whole run as a single token.
#[test]
fn lazy_close_splits_adjacent_long_strings() {
    let lexer = long_string_lexer();

    // `"""a"""` immediately followed by `"""b"""` (six quotes between `a` and `b`).
    let two = format!("{three}a{three}{three}b{three}", three = q('"', 3));
    let arm = format!("{three}a{three}", three = q('"', 3));
    let armb = format!("{three}b{three}", three = q('"', 3));
    assert_eq!(
        lex(&lexer, &two),
        Ok(vec![arm, armb]),
        "adjacent triple-quote strings must close at the first \"\"\" each (lazy), not greedily"
    );
    // The single-quote variant, same shape.
    let two_sq = format!("{t}a{t}{t}b{t}", t = q('\'', 3));
    assert_eq!(
        lex(&lexer, &two_sq),
        Ok(vec![
            format!("{t}a{t}", t = q('\'', 3)),
            format!("{t}b{t}", t = q('\'', 3))
        ]),
    );
    // A lone in-body delimiter char (one/two quotes that do not form a full close) stays
    // part of the body — `"""a"b""c"""` is a single long string containing `a"b""c`.
    let embedded = format!(r#"{t}a"b""c{t}"#, t = q('"', 3));
    assert_eq!(lex(&lexer, &embedded), Ok(vec![embedded.clone()]));
}

/// Empty and minimal long strings, plus the open-without-close lex errors.
#[test]
fn empty_and_unterminated_long_strings() {
    let lexer = long_string_lexer();

    // Six quotes is one empty long string (open `"""` + empty body + close `"""`).
    assert_eq!(lex(&lexer, &q('"', 6)), Ok(vec![q('"', 6)]));
    assert_eq!(lex(&lexer, &q('\'', 6)), Ok(vec![q('\'', 6)]));

    // Three/four/five quotes cannot close a triple-quote string → a lex error at byte 0.
    for n in [3usize, 4, 5] {
        assert_eq!(
            lex(&lexer, &q('"', n)),
            Err(0),
            "{n} quotes must be a lex error (no triple-quote close)"
        );
    }
}

/// The `(?<!\\)` escape interaction, absorbed into the normalized body. An escaped quote
/// inside does not close the string; an escaped backslash before the close does let it
/// close; and (because the terminal is DOTALL) a body may span raw newlines.
#[test]
fn escape_and_dotall_interactions() {
    let lexer = long_string_lexer();

    // `"""` + `\"` + `"""` — an escaped quote then the real close: one long string (the
    // `\"` does not contribute to the closing run).
    let esc_q = format!(r#"{t}\"{t}"#, t = q('"', 3));
    assert_eq!(lex(&lexer, &esc_q), Ok(vec![esc_q.clone()]));
    // `"""` + `\\` + `"""` — an escaped backslash then the close: one long string.
    let esc_bs = format!(r#"{t}\\{t}"#, t = q('"', 3));
    assert_eq!(lex(&lexer, &esc_bs), Ok(vec![esc_bs.clone()]));
    // DOTALL: a raw newline inside the body is allowed (unlike the short STRING).
    let multiline = format!("{t}a\nb{t}", t = q('"', 3));
    assert_eq!(lex(&lexer, &multiline), Ok(vec![multiline.clone()]));
}

/// The `r`/`b`/`u`/`f` and combined `rb`/`br` prefixes lex correctly (the variable-width
/// prefix the idiom composes with), case-insensitively (the terminal's `/i`).
#[test]
fn long_string_prefixes_lex() {
    let lexer = long_string_lexer();
    let t = q('"', 3);
    let s = q('\'', 3);
    for input in [
        format!("r{t}raw{t}"),
        format!("b{s}bytes{s}"),
        format!("rb{t}x{t}"),
        format!("BR{t}X{t}"), // case-insensitive prefix
        format!("f{t}{t}"),   // prefixed empty long string (f + 6 quotes)
    ] {
        assert_eq!(
            lex(&lexer, &input),
            Ok(vec![input.clone()]),
            "{input:?} must lex as one LONG_STRING token"
        );
    }
}
