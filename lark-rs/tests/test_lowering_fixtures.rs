//! L2 lowering harness — **the seam/edge fixtures** (`docs/LEXER_DFA_PLAN.md`,
//! "Seam/edge checklist the generators must hit").
//!
//! Each fixture pins one integration seam the lowering must get right, as a concrete
//! grammar + input so the shape sessions have a *target*: trailing guard at EOF;
//! zero-width; maximal-munch competition (`OP` vs `RULE`); `unless` over a lowered
//! terminal; `%ignore` + contextual narrowing; newline/DOTALL bodies; UTF-8 byte
//! boundaries (the DFA is byte-level, terminals are char-level); `g_regex_flags`;
//! and `PatternID` leftmost-first priority surviving the union.
//!
//! What is **active now** (every fixture, every build): both scanner backends *build*
//! the grammar (build parity), and the fixture's headline lookaround terminal
//! classifies into the shape it targets. What is **pending** (`#[ignore]`'d until the
//! relevant shape lands): the lowered token stream equals the `fancy-regex` reference
//! — i.e. the Dfa backend (once it *lowers* instead of routing to `fancy-regex`)
//! lexes byte-identically to the Regex backend. That comparison is the per-fixture
//! restriction of the master differential, and flips on automatically when lowering
//! replaces the `fancy` routing.

mod common;

use lark_rs::grammar::terminal::flags;
use lark_rs::{
    basic_lexer_conf, classify, load_grammar, lower, BasicLexer, Lexer, LexerBackend, ParseError,
    ShapeClass,
};

/// One seam fixture: a grammar + input, plus an optional `(pattern, shape)` target
/// for the headline lookaround terminal so the classifier is pinned to the seam.
struct SeamFixture {
    name: &'static str,
    seam: &'static str,
    grammar: &'static str,
    g_regex_flags: u32,
    input: &'static str,
    /// `(terminal pattern, expected shape)` — `None` for seams whose terminal shape
    /// is exercised elsewhere (e.g. the priority/munch competitions).
    classify_target: Option<(&'static str, ShapeClass)>,
}

fn fixtures() -> Vec<SeamFixture> {
    vec![
        SeamFixture {
            name: "eof_trailing_guard",
            seam: "a trailing guard must hold at end-of-input (no next byte to peek)",
            grammar: "start: NUM\nNUM: /[0-9]+(?![0-9])/\n",
            g_regex_flags: 0,
            input: "123",
            classify_target: Some((r"[0-9]+(?![0-9])", ShapeClass::TrailingBoundary)),
        },
        SeamFixture {
            name: "zero_width_lookahead",
            seam: "a zero-width positive lookahead at the leading edge",
            grammar: "start: A\nA: /(?=x)x/\n",
            g_regex_flags: 0,
            input: "x",
            classify_target: Some((r"(?=x)x", ShapeClass::LeadingBoundary)),
        },
        SeamFixture {
            name: "op_vs_rule_munch",
            seam: "maximal-munch competition: the short guarded OP vs the longer RULE",
            grammar: "start: t+\n!t: OP | RULE\nOP: /[?](?![a-z])/\nRULE: /[?][a-z]+/\n",
            g_regex_flags: 0,
            input: "?a",
            classify_target: Some((r"[?](?![a-z])", ShapeClass::TrailingBoundary)),
        },
        SeamFixture {
            name: "unless_over_lowered",
            seam: "`unless` keyword retyping over a lowered (lookaround) terminal",
            grammar: "start: t+\n!t: NAME | IF\nIF: \"if\"\nNAME: /[a-z]+(?![0-9])/\n",
            g_regex_flags: 0,
            input: "if",
            classify_target: Some((r"[a-z]+(?![0-9])", ShapeClass::TrailingBoundary)),
        },
        SeamFixture {
            name: "ignore_and_boundary",
            seam: "`%ignore` whitespace alongside a trailing-boundary terminal",
            grammar: "start: A B\nA: /[a-z]+(?=:)/\nB: \":\"\n%ignore \" \"\n",
            g_regex_flags: 0,
            input: "ab:",
            classify_target: Some((r"[a-z]+(?=:)", ShapeClass::TrailingBoundary)),
        },
        SeamFixture {
            name: "newline_dotall_body",
            seam: "a DOTALL body crossing newlines with an even-backslash lookbehind close",
            grammar: "start: LONG\nLONG: /\"\"\".*?(?<!\\\\)(\\\\\\\\)*?\"\"\"/s\n",
            g_regex_flags: 0,
            input: "\"\"\"a\nb\"\"\"",
            classify_target: Some((
                r#""""\.*?(?<!\\)(\\\\)*?""""#,
                ShapeClass::BoundedLookbehind,
            )),
        },
        SeamFixture {
            name: "utf8_byte_boundary",
            seam: "a guard adjacent to a multi-byte char (DFA is byte-level, terminal char-level)",
            grammar: "start: T\nT: /é(?![a-z])/\n",
            g_regex_flags: 0,
            input: "é",
            classify_target: Some((r"é(?![a-z])", ShapeClass::TrailingBoundary)),
        },
        SeamFixture {
            name: "g_regex_flags_ignorecase",
            seam: "g_regex_flags=IGNORECASE applied over a leading-boundary assertion",
            grammar: "start: KW\nKW: /(?!if)[a-z]+/\n",
            g_regex_flags: flags::IGNORECASE,
            input: "Foo",
            classify_target: Some((r"(?!if)[a-z]+", ShapeClass::LeadingBoundary)),
        },
        SeamFixture {
            name: "patternid_priority",
            seam: "leftmost-first PatternID priority must survive the union of lowered terminals",
            grammar: "start: t+\n!t: A | B\nA.2: /x(?![0-9])/\nB.1: /xx/\n",
            g_regex_flags: 0,
            input: "x",
            classify_target: Some((r"x(?![0-9])", ShapeClass::TrailingBoundary)),
        },
    ]
}

/// Build a `BasicLexer` for `fixture` under `backend`, or `None` if it cannot be
/// built (caught, so a fixture bug surfaces as a failed assertion, not a panic).
fn build(fixture: &SeamFixture, backend: LexerBackend) -> Option<BasicLexer> {
    let grammar = load_grammar(fixture.grammar, &["start".to_string()], false, false).ok()?;
    let cg = lower(&grammar);
    let conf = basic_lexer_conf(&cg, fixture.g_regex_flags).with_backend(backend);
    BasicLexer::new(&conf).ok()
}

/// Active: every fixture builds under *both* backends (build parity is part of the
/// contract — the engine swap must not change whether a lexer builds), and its
/// headline terminal classifies into the shape the seam targets.
#[test]
fn seam_fixtures_build_under_both_backends_and_classify() {
    for f in fixtures() {
        let regex = build(&f, LexerBackend::Regex);
        let dfa = build(&f, LexerBackend::Dfa);
        assert!(
            regex.is_some(),
            "fixture `{}` ({}) failed to build under the Regex backend",
            f.name,
            f.seam
        );
        assert!(
            dfa.is_some(),
            "fixture `{}` ({}) failed to build under the Dfa backend — build parity broken",
            f.name,
            f.seam
        );

        if let Some((pattern, shape)) = f.classify_target {
            let c = classify(pattern)
                .unwrap_or_else(|e| panic!("fixture `{}` classify {pattern:?}: {e}", f.name));
            assert!(
                c.assertions
                    .iter()
                    .any(|a| a.verdict() == lark_rs::Verdict::Supported(shape)),
                "fixture `{}`: terminal {pattern:?} does not classify as {shape:?}",
                f.name
            );
        }
    }
}

/// The lex outcome the per-fixture differential compares: the token (type, value)
/// stream on success, or the failing byte position.
fn outcome(lexer: &BasicLexer, input: &str) -> Result<Vec<(String, String)>, usize> {
    match lexer.lex(input) {
        Ok(tokens) => Ok(tokens
            .into_iter()
            .map(|t| (t.type_.clone(), t.value.clone()))
            .collect()),
        Err(ParseError::UnexpectedCharacter { pos, .. }) => Err(pos),
        Err(_) => Err(usize::MAX),
    }
}

/// The Dfa backend must lex each seam fixture byte-identically to the Regex/`fancy`
/// reference — the per-fixture restriction of the master differential. With M1/M2/M3
/// landed the Dfa side genuinely **lowers** every fixture whose terminal is in shape;
/// a terminal the lowering *declines* (the `newline_dotall_body` fixture's
/// variable-offset lookbehind behind a flag wrapper) routes to `fancy-regex` on both
/// sides, so they still agree. Either way the contract is the same: swapping the engine
/// changes nothing.
#[test]
fn seam_fixtures_lowered_lex_equals_fancy() {
    for f in fixtures() {
        let regex = build(&f, LexerBackend::Regex)
            .unwrap_or_else(|| panic!("fixture `{}` Regex build failed", f.name));
        let dfa = build(&f, LexerBackend::Dfa)
            .unwrap_or_else(|| panic!("fixture `{}` Dfa build failed", f.name));
        assert_eq!(
            outcome(&regex, f.input),
            outcome(&dfa, f.input),
            "fixture `{}` ({}): lowered Dfa lex diverged from the fancy reference",
            f.name,
            f.seam
        );
    }
}
