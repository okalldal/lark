//! The **non-negotiable adversarial gate** for lowering `python.STRING` into the DFA
//! (the marquee L2 NFA-state splice — `docs/LEXER_DFA_PLAN.md`, "leading boundary").
//!
//! `python.STRING`'s `(?!"")` opening guard is a *trailing-context* boundary: it makes
//! `""""` a lex error while `"" ""` is two empty strings — a distinction that lives only
//! at lex time. The splice lowers `(?!"")` (after the variable-width prefix + the opening
//! quote) by an empty/non-empty arm split with a trailing `(?!")` guard on the empty arm.
//! `fancy-regex` is the canary oracle: a mis-lowered splice (forgetting the guard) would
//! accept `""""`, so the `""""`-reject case is what makes the splice safe. This test is
//! hand-authored (not generated) and runs under the **default (`Dfa`) backend**, so it
//! gates the real shipped lexer, not a synthetic one.

mod common;

use lark_rs::lookaround::{lower::recognize_string_idiom, parse};
use lark_rs::{
    basic_lexer_conf, load_grammar, lower, lower_terminal_dotall, BasicLexer, Lexer, LexerBackend,
    Lowered, ParseError,
};

/// The bundled `python.STRING` pattern, verbatim (the `/i` flag lives on the terminal).
const STRING_RAW: &str =
    r#"([ubf]?r?|r[ubf])("(?!"").*?(?<!\\)(\\\\)*?"|'(?!'').*?(?<!\\)(\\\\)*?')"#;

/// Build a default-backend (`Dfa`) basic lexer for a grammar that imports `python.STRING`
/// (a single `TOK: STRING` terminal, whitespace ignored).
fn string_lexer() -> BasicLexer {
    let grammar = "start: STRING+\n%import python.STRING\n%ignore \" \"\n";
    let g = load_grammar(grammar, &["start".to_string()], false, false)
        .expect("grammar with %import python.STRING builds");
    let cg = lower(&g);
    let conf = basic_lexer_conf(&cg, 0).with_backend(LexerBackend::Dfa);
    BasicLexer::new(&conf).expect("Dfa BasicLexer builds")
}

/// The lex outcome reduced to the STRING token *values* on success, or the failing byte
/// position on a lex error.
fn lex(lexer: &BasicLexer, input: &str) -> Result<Vec<String>, usize> {
    match lexer.lex(input) {
        Ok(tokens) => Ok(tokens
            .into_iter()
            .filter(|t| t.type_ == "STRING")
            .map(|t| t.value)
            .collect()),
        Err(ParseError::UnexpectedCharacter { pos, .. }) => Err(pos),
        Err(_) => Err(usize::MAX),
    }
}

/// Deliverable 3 (a): the splice is **actually lowered**, not silently routed to
/// `fancy-regex`. The lowering returns four branches (a non-empty + an empty arm per
/// quote kind), so the DFA hosts STRING — the whole point of the milestone.
#[test]
fn string_actually_lowers_to_branches_under_dfa() {
    let lowered = lower_terminal_dotall("STRING", STRING_RAW, false)
        .expect("STRING must lower (not reject) now");
    match lowered {
        Lowered::Branches(branches) => {
            assert_eq!(
                branches.len(),
                4,
                "STRING lowers to 2 arms × {{non-empty, empty}} = 4 branches, got {branches:#?}"
            );
            // The empty arms carry the spliced trailing guard; the non-empty arms do not.
            let empties: Vec<_> = branches.iter().filter(|b| b.trailing.is_some()).collect();
            assert_eq!(empties.len(), 2, "exactly the two empty arms are guarded");
            for b in &empties {
                let g = b.trailing.as_ref().unwrap();
                assert!(g.neg, "the empty arm's trailing guard is negative `(?!q)`");
                assert!(
                    g.set == "\"" || g.set == "'",
                    "the guard forbids the delimiter, got {:?}",
                    g.set
                );
                assert!(
                    b.lookbehind.is_empty() && b.leading.is_none(),
                    "the (?<!\\\\) lookbehind is absorbed by the body normalization, not \
                     carried as a guard"
                );
            }
        }
        other => panic!("STRING must lower to Branches, got {other:?}"),
    }
}

/// The recognizer's own acceptance surface is gated, not just the classifier's: a
/// string-idiom-*shaped* terminal whose delimiter is **not a fixed single literal** —
/// `.` (any char), the anchors `\b` / `$`, the class escape `\d` — MUST be declined
/// (routed to `fancy-regex`), never lowered. Accepting one would be a false-accept (and
/// `\b` also breaks build-parity). This closes the recognizer's blind spot directly.
#[test]
fn recognizer_declines_non_literal_delimiters() {
    for p in common::lowering::string_idiom_reject_patterns() {
        // The structural recognizer must not match it…
        let node = parse(&p).unwrap_or_else(|e| panic!("parse {p:?} failed: {e:?}"));
        assert!(
            recognize_string_idiom(&node).is_none(),
            "recognizer wrongly accepted a non-literal-delimiter idiom: {p:?}"
        );
        // …and the lowering entry point must decline it (route to fancy), not lower it.
        assert!(
            !matches!(
                lower_terminal_dotall("ADV", &p, false),
                Ok(Lowered::Branches(_))
            ),
            "non-literal-delimiter idiom must NOT lower to branches: {p:?}"
        );
    }
}

/// The same gate for the **long arm's multi-character close**: a long-idiom-*shaped*
/// terminal whose multi-char delimiter run is **not a uniform run of one plain literal** —
/// a mixed run (`"'`), a run with a metacharacter (`".`), a class-escape run (`\d\d`), an
/// escaped-pair run (`\.\.`), or a bare metacharacter (`*`) — MUST be declined. Accepting
/// one would mis-emit the open/close bytes (a false-accept). This is the multi-char analog
/// of `recognizer_declines_non_literal_delimiters`.
#[test]
fn recognizer_declines_non_literal_multichar_delimiters() {
    for p in common::lowering::long_string_idiom_reject_patterns() {
        let node = parse(&p).unwrap_or_else(|e| panic!("parse {p:?} failed: {e:?}"));
        assert!(
            recognize_string_idiom(&node).is_none(),
            "recognizer wrongly accepted a non-literal multi-char-delimiter idiom: {p:?}"
        );
        assert!(
            !matches!(
                lower_terminal_dotall("ADV", &p, true),
                Ok(Lowered::Branches(_))
            ),
            "non-literal multi-char-delimiter idiom must NOT lower to branches: {p:?}"
        );
    }
}

/// The bundled `"` / `'` delimiters (and escaped-punctuation delimiters like `\/`) are
/// still recognized — the literal-delimiter restriction must not over-decline and break
/// python.STRING. The multi-char `"""` / `'''` runs (python.LONG_STRING) and a non-quote
/// multi-char run (`///`) are recognized too — the multi-char-close generalization must not
/// over-decline the bundled LONG_STRING.
#[test]
fn recognizer_still_accepts_literal_delimiters() {
    for p in [
        STRING_RAW,
        r#"(r?)("(?!"").*?(?<!\\)(\\\\)*?")"#,
        // an escaped-punctuation delimiter (`\/`) is a literal-escape → still accepted.
        r#"(\/(?!\/\/).*?(?<!\\)(\\\\)*?\/)"#,
        // the bundled LONG_STRING multi-char close, both arms.
        r#"([ubf]?r?|r[ubf])(""".*?(?<!\\)(\\\\)*?"""|'''.*?(?<!\\)(\\\\)*?''')"#,
        // a non-quote multi-char run (`///`) — the construction is delimiter-agnostic.
        r#"(///.*?(?<!\\)(\\\\)*?///)"#,
    ] {
        let node = parse(p).unwrap_or_else(|e| panic!("parse {p:?} failed: {e:?}"));
        assert!(
            recognize_string_idiom(&node).is_some(),
            "recognizer must still accept the literal-delimiter idiom: {p:?}"
        );
    }
}

/// **Bundled-terminal lowering-status tripwire** (deliverable 6's payoff-check, made
/// executable). The honest scope after the LONG_STRING milestone is: `python.STRING`
/// **and** `python.LONG_STRING` lower into the DFA, while `lark.REGEXP` (an internal
/// `(?!\/)`) is the **sole remaining decline** — it routes to `fancy-regex`, so
/// `fancy-regex` stays in the runtime and L4/L5 remain blocked on REGEXP alone.
///
/// This pins that scope as a fact. If a future change makes REGEXP lower, this test goes
/// red on purpose — forcing the author to (a) confirm the new lowering is proven correct
/// and (b) re-run the payoff-check: with *all* bundled lookaround terminals lowered, L4
/// (drop `AnyRegex::Fancy` from the runtime) and L5 (bake) become unblocked, and
/// `docs/LEXER_DFA_PLAN.md` + `CLAUDE.md` must be updated. It is the same negative-pin
/// discipline as `test_lookaround.rs::string_lookaround_free_rewrite_is_not_equivalent`.
#[test]
fn bundled_lookaround_terminal_lowering_status() {
    // Verbatim from the bundled grammars (the `/i` / `/is` flags live on the terminal;
    // LONG_STRING is DOTALL so it is lowered with `dotall = true`).
    const REGEXP_RAW: &str = r#"\/(?!\/)(\\\/|\\\\|[^\/])*?\/[imslux]*"#;
    const LONG_STRING_RAW: &str =
        r#"([ubf]?r?|r[ubf])(""".*?(?<!\\)(\\\\)*?"""|'''.*?(?<!\\)(\\\\)*?''')"#;

    // STRING and LONG_STRING lower (the milestone deliverables).
    for (name, raw, dotall) in [
        ("python.STRING", STRING_RAW, false),
        ("python.LONG_STRING", LONG_STRING_RAW, true),
    ] {
        assert!(
            matches!(
                lower_terminal_dotall(name, raw, dotall),
                Ok(Lowered::Branches(_))
            ),
            "{name} must lower into the DFA"
        );
    }

    // REGEXP is the sole remaining decline (routes to fancy). A returned `Branches` here
    // means the scope changed — see the doc above: prove it and re-run the L4/L5 payoff.
    assert!(
        !matches!(
            lower_terminal_dotall("lark.REGEXP", REGEXP_RAW, false),
            Ok(Lowered::Branches(_))
        ),
        "lark.REGEXP unexpectedly LOWERS now — fancy-regex was supposed to stay (L4 blocked). \
         If this lowering is intentional and proven, update the payoff-check + docs and \
         revise this tripwire."
    );
}

/// LONG_STRING lowers to exactly **two unguarded branches** (one per quote-run arm), each
/// a lazy escaped body between a multi-character `"""` / `'''` close. Unlike STRING's
/// opening-guard splice (4 branches, the empty arms guarded), LONG_STRING carries **no**
/// guard — there is no `(?!"")`, and the `(?<!\\)` close lookbehind is absorbed by the
/// `\\.` escape pairing — so no branch carries a leading/trailing/lookbehind guard.
#[test]
fn long_string_lowers_to_two_unguarded_branches() {
    const LONG_STRING_RAW: &str =
        r#"([ubf]?r?|r[ubf])(""".*?(?<!\\)(\\\\)*?"""|'''.*?(?<!\\)(\\\\)*?''')"#;
    let lowered = lower_terminal_dotall("LONG_STRING", LONG_STRING_RAW, true)
        .expect("LONG_STRING must lower (not decline) now");
    match lowered {
        Lowered::Branches(branches) => {
            assert_eq!(
                branches.len(),
                2,
                "LONG_STRING lowers to one unguarded branch per quote-run arm, got {branches:#?}"
            );
            for b in &branches {
                assert!(
                    b.leading.is_none() && b.trailing.is_none() && b.lookbehind.is_empty(),
                    "a long-string arm is unguarded (the `(?<!\\\\)` is absorbed by `\\\\.`): \
                     {b:?}"
                );
                // The lazy body is the lowering's load-bearing piece: a multi-char close
                // with a `*?` (lazy) body, the proven E2a rewrite.
                assert!(
                    b.regex.contains("*?"),
                    "the long-string body must stay lazy (`*?`): {}",
                    b.regex
                );
            }
        }
        other => panic!("LONG_STRING must lower to Branches, got {other:?}"),
    }
}

/// **The canary.** `""""` (and `''''`) is a LEX ERROR; `"" ""` (and `'' ''`) is exactly
/// two empty STRING tokens. This is the `(?!"")` trailing-context boundary, and the case
/// a forgotten guard would get wrong (it would accept `""""` as one empty string).
#[test]
fn four_quotes_is_a_lex_error_two_empties_are_two_tokens() {
    let lexer = string_lexer();

    // The over-long quote-run: no STRING opens inside it → lex error at byte 0.
    assert_eq!(
        lex(&lexer, r#""""""#),
        Err(0),
        "\"\"\"\" must be a lex error"
    );
    assert_eq!(lex(&lexer, "''''"), Err(0), "'''' must be a lex error");
    // 3-quotes-then-content is likewise an error (STRING refuses to open in the run).
    assert_eq!(
        lex(&lexer, r#""""a""#),
        Err(0),
        "\"\"\"a\" must be a lex error"
    );

    // Two separated empty strings lex as two empty STRING tokens.
    assert_eq!(
        lex(&lexer, r#""" """#),
        Ok(vec!["\"\"".to_string(), "\"\"".to_string()]),
        "\"\" \"\" must be two empty STRING tokens"
    );
    assert_eq!(
        lex(&lexer, "'' ''"),
        Ok(vec!["''".to_string(), "''".to_string()]),
        "'' '' must be two empty STRING tokens"
    );
    // A single empty string is one token.
    assert_eq!(lex(&lexer, r#""""#), Ok(vec!["\"\"".to_string()]));
    // An empty string immediately followed by a non-quote is one empty token + more.
    assert_eq!(
        lex(&lexer, r#""" "a""#),
        Ok(vec!["\"\"".to_string(), "\"a\"".to_string()]),
    );
}

/// The `r`/`b`/`u`/`f` and combined `rb`/`br` prefixes lex correctly (the variable-width
/// prefix the splice composes with), case-insensitively (the terminal's `/i`).
#[test]
fn string_prefixes_lex() {
    let lexer = string_lexer();
    for s in [
        r#"r"raw""#,
        r#"b"bytes""#,
        r#"f"f""#,
        r#"u"u""#,
        r#"rb"x""#,
        r#"br"x""#,
        r#"BR"X""#, // case-insensitive prefix
        r#"R'y'"#,
        r#"rb"""#, // prefixed empty string
    ] {
        assert_eq!(
            lex(&lexer, s),
            Ok(vec![s.to_string()]),
            "{s:?} must lex as one STRING token"
        );
    }
}

/// The `(?<!\\)` escape interaction (absorbed into the normalized body): an escaped quote
/// `\"` does **not** close the string; an escaped backslash `\\` does let the next quote
/// close it; and a raw newline never appears in a (non-DOTALL) short string.
#[test]
fn escape_interactions() {
    let lexer = string_lexer();

    // `"\""` — escaped quote inside: the whole 4-char token is one STRING (the `\"` does
    // not close; the final `"` does).
    assert_eq!(lex(&lexer, r#""\"""#), Ok(vec![r#""\"""#.to_string()]));
    // `"\\"` — escaped backslash then the real closing quote: a 4-char STRING.
    assert_eq!(lex(&lexer, r#""\\""#), Ok(vec![r#""\\""#.to_string()]));
    // `"\\\\"` — two escaped backslashes then the close.
    assert_eq!(lex(&lexer, r#""\\\\""#), Ok(vec![r#""\\\\""#.to_string()]));

    // A raw newline is not allowed in a short string → lex error at byte 0 (the body
    // excludes `\n` since STRING is not DOTALL, so no STRING opens there).
    assert_eq!(lex(&lexer, "\"a\nb\""), Err(0));
    // A backslash before a raw newline still does not rescue it (no line continuation in
    // the lexer's view of a short string).
    assert_eq!(lex(&lexer, "\"a\\\nb\""), Err(0));
}

// ─── python.LONG_STRING under the default Dfa backend ───────────────────────────────

/// Build a default-backend (`Dfa`) basic lexer for a grammar that imports both
/// `python.STRING` and `python.LONG_STRING` (the way python.lark uses them together), so
/// the maximal-munch competition between the short and long quote-runs is exercised on the
/// real shipped pair — not a synthetic single terminal.
fn long_string_lexer() -> BasicLexer {
    let grammar = "start: TOK+\nTOK: LONG_STRING | STRING\n\
                   %import python.STRING\n%import python.LONG_STRING\n%ignore \" \"\n";
    let g = load_grammar(grammar, &["start".to_string()], false, false)
        .expect("grammar with %import python.LONG_STRING builds");
    let cg = lower(&g);
    let conf = basic_lexer_conf(&cg, 0).with_backend(LexerBackend::Dfa);
    BasicLexer::new(&conf).expect("Dfa BasicLexer builds")
}

/// The lex outcome reduced to every real token value on success (so STRING vs LONG_STRING
/// segmentation is visible; the `$END` sentinel is filtered out), or the failing byte
/// position on a lex error.
fn lex_all(lexer: &BasicLexer, input: &str) -> Result<Vec<String>, usize> {
    match lexer.lex(input) {
        Ok(tokens) => Ok(tokens
            .into_iter()
            .filter(|t| t.type_ != "$END")
            .map(|t| t.value)
            .collect()),
        Err(ParseError::UnexpectedCharacter { pos, .. }) => Err(pos),
        Err(_) => Err(usize::MAX),
    }
}

/// **The LONG_STRING canary.** A `"""…"""` body spans content, newlines (DOTALL), embedded
/// single/double quotes, and escaped delimiters, and the multi-char close means a lone `"`
/// or `""` inside the body does **not** end the string — only the full `"""` run does. This
/// gates the marquee new piece of the milestone: a multi-character close delimiter lowered
/// into the DFA, run under the real shipped lexer.
#[test]
fn long_string_close_delimiter_under_dfa() {
    let lexer = long_string_lexer();

    // A basic triple-quoted string.
    assert_eq!(
        lex_all(&lexer, r#""""hello""""#),
        Ok(vec![r#""""hello""""#.to_string()])
    );
    // DOTALL: the body spans a newline.
    assert_eq!(
        lex_all(&lexer, "\"\"\"a\nb\"\"\""),
        Ok(vec!["\"\"\"a\nb\"\"\"".to_string()])
    );
    // A lone `"` and a `""` inside the body do NOT close it — only `"""` does. So the whole
    // `"""a"b""c"""` is one LONG_STRING, not several.
    assert_eq!(
        lex_all(&lexer, r#""""a"b""c""""#),
        Ok(vec![r#""""a"b""c""""#.to_string()])
    );
    // The triple-quoted empty string.
    assert_eq!(
        lex_all(&lexer, r#""""""""#),
        Ok(vec![r#""""""""#.to_string()])
    );
    // Single-quote variant + a prefix.
    assert_eq!(
        lex_all(
            &lexer,
            r#"r'''raw
multiline'''"#
        ),
        Ok(vec!["r'''raw\nmultiline'''".to_string()])
    );

    // Escape interaction: an escaped delimiter char `\"` inside does not contribute to a
    // closing run; the body continues to the real `"""` close.
    assert_eq!(
        lex_all(&lexer, r#""""x\"y""""#),
        Ok(vec![r#""""x\"y""""#.to_string()])
    );
    // An escaped backslash `\\` before the close: the `\\.` pairing consumes it, so the
    // following `"""` closes the string.
    assert_eq!(
        lex_all(&lexer, r#""""a\\""""#),
        Ok(vec![r#""""a\\""""#.to_string()])
    );
    // An embedded apostrophe inside a `'''`-delimited string is body content (a single
    // `'` does not close a `'''` string).
    assert_eq!(
        lex_all(&lexer, r#"'''it's a test'''"#),
        Ok(vec!["'''it's a test'''".to_string()])
    );
}

/// The maximal-munch competition between STRING (short, `"`) and LONG_STRING (long, `"""`):
/// where both could open, Lark's longest-match must pick the LONG_STRING — exactly the
/// disambiguation the original `python.STRING` `(?!"")` opening guard exists to enforce.
/// This is the cross-terminal seam the splice + the multi-char close must get right
/// together.
#[test]
fn short_and_long_string_maximal_munch() {
    let lexer = long_string_lexer();

    // `""""""` (six quotes) is the empty LONG_STRING `""""""`, NOT three empty STRINGs:
    // LONG_STRING is longer, so maximal munch takes it.
    assert_eq!(
        lex_all(&lexer, r#""""""""#),
        Ok(vec![r#""""""""#.to_string()]),
        "six quotes is one empty LONG_STRING"
    );
    // `"" ""` (two separated empties) stays two short STRINGs (a space breaks the run).
    assert_eq!(
        lex_all(&lexer, r#""" """#),
        Ok(vec!["\"\"".to_string(), "\"\"".to_string()]),
        "separated empties are two short STRINGs"
    );
    // `"""abc"""def` — one LONG_STRING then a `def` name region is not part of this lexer;
    // truncate to just the long string + a trailing short string to keep it lexable.
    assert_eq!(
        lex_all(&lexer, r#""""abc""""x""#),
        Ok(vec![r#""""abc""""#.to_string(), r#""x""#.to_string()]),
        "the long string closes at the first `\"\"\"`, then a short string follows"
    );
}
