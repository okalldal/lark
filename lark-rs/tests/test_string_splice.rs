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

/// The bundled `"` / `'` delimiters (and escaped-punctuation delimiters like `\/`) are
/// still recognized — the literal-delimiter restriction must not over-decline and break
/// python.STRING.
#[test]
fn recognizer_still_accepts_literal_delimiters() {
    for p in [
        STRING_RAW,
        r#"(r?)("(?!"").*?(?<!\\)(\\\\)*?")"#,
        // an escaped-punctuation delimiter (`\/`) is a literal-escape → still accepted.
        r#"(\/(?!\/\/).*?(?<!\\)(\\\\)*?\/)"#,
    ] {
        let node = parse(p).unwrap_or_else(|e| panic!("parse {p:?} failed: {e:?}"));
        assert!(
            recognize_string_idiom(&node).is_some(),
            "recognizer must still accept the literal-delimiter idiom: {p:?}"
        );
    }
}

/// **Bundled-terminal lowering-status tripwire** (deliverable 6's payoff-check, made
/// executable). The honest scope on `master` is: `python.STRING` lowers into the DFA,
/// while `lark.REGEXP` and `python.LONG_STRING` do **not** lower — at runtime they route
/// to `fancy-regex`, so `fancy-regex` stays in the runtime and L4/L5 remain blocked
/// (`docs/LEXER_DFA_PLAN.md` / `docs/LEXER_DFA_STATUS.md`).
///
/// **What this pins, and what it deliberately does *not*.** It asserts only the runtime
/// observable: STRING lowers (`Ok(Lowered::Branches(_))`), and REGEXP/LONG_STRING do
/// **not** (`!matches!(…, Ok(Lowered::Branches(_)))`). It does **not** assert *why* they
/// fail to lower — that failure is treated as a **decline-to-fancy / transitional route**
/// (they still need `fancy-regex` today), *not* as proof of a permanent rejection. (Per
/// the plan's design-debt note, the current build path can even absorb a permanent
/// rejection into the same fancy fallback; distinguishing the two is L4 work, so this
/// tripwire stays agnostic and never matches on the error taxonomy.)
///
/// If a future change makes REGEXP or LONG_STRING lower, this test goes red on purpose.
/// **If this starts lowering, that is good news only if the PR also adds
/// equivalence/proof/canary coverage and updates the L4/L5 status** (drop `AnyRegex::Fancy`
/// from the runtime once *all* bundled lookaround terminals lower, bake the DFA bundle,
/// and update `docs/LEXER_DFA_PLAN.md` + `docs/LEXER_DFA_STATUS.md` + `CLAUDE.md`). It is
/// the same negative-pin discipline as
/// `test_lookaround.rs::string_lookaround_free_rewrite_is_not_equivalent`.
#[test]
fn bundled_lookaround_terminal_lowering_status() {
    // Verbatim from the bundled grammars (the `/i` / `/is` flags live on the terminal;
    // LONG_STRING is DOTALL so it is lowered with `dotall = true`).
    const REGEXP_RAW: &str = r#"\/(?!\/)(\\\/|\\\\|[^\/])*?\/[imslux]*"#;
    const LONG_STRING_RAW: &str =
        r#"([ubf]?r?|r[ubf])(""".*?(?<!\\)(\\\\)*?"""|'''.*?(?<!\\)(\\\\)*?''')"#;

    // STRING lowers (the milestone deliverable).
    assert!(
        matches!(
            lower_terminal_dotall("STRING", STRING_RAW, false),
            Ok(Lowered::Branches(_))
        ),
        "python.STRING must lower into the DFA"
    );

    // REGEXP and LONG_STRING do NOT lower — at runtime they route to fancy (a
    // transitional decline, not asserted to be a rejection; see the doc above). The
    // loose `!matches!(…, Ok(Branches))` check stays off the error taxonomy on purpose.
    for (name, raw, dotall) in [
        ("lark.REGEXP", REGEXP_RAW, false),
        ("python.LONG_STRING", LONG_STRING_RAW, true),
    ] {
        assert!(
            !matches!(
                lower_terminal_dotall(name, raw, dotall),
                Ok(Lowered::Branches(_))
            ),
            "{name} unexpectedly LOWERS now — fancy-regex was supposed to stay (L4 blocked). \
             If this starts lowering, that is good news ONLY if the PR also adds \
             equivalence/proof/canary coverage and updates the L4/L5 status (docs + this \
             tripwire). See docs/LEXER_DFA_STATUS.md."
        );
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
