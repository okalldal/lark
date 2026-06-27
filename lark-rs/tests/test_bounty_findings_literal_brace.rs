//! Issue #462 — literal `{x}` brace-run (non-quantifier `{...}`) rejected +
//! mis-categorized as `LookaroundScope`; Python `re`/Lark accept it.
//!
//! A regex `{...}` that is **not** a well-formed quantifier — `{x}`, `a{x}b`, `{}`,
//! `{ 2}`, `{a,b}`, `{,x}`, an unterminated `a{`, … — is a **literal brace run** in
//! Python `re` (`re.compile(r'a{x}b')` matches the literal text `a{x}b`). lark-rs
//! rejected it at build, and — because the Rust `regex` crate rejects a brace
//! expression with a non-numeric body, and that failure routed through the lookaround
//! seam — the rejection was *mis-categorized* as `GrammarError::LookaroundScope`
//! ("backtracking-only syntax"), which it is not (no lookaround/backtracking is
//! involved). Found incidentally while fixing #364 (H5-5/H5-6).
//!
//! Fix (oracle-faithful "support & match", ADR-0017): `normalize_python_escapes`
//! (`src/grammar/terminal.rs`) escapes a literal brace (`{` → `\{`) when the `{` is
//! out-of-class, unescaped, and **not** a well-formed `base_quantifier_len`
//! quantifier (so a real `{2}`/`{2,3}`/`{2,}` is untouched, and the empty-lower-bound
//! `{,n}`/`{,}` was already rewritten to `{0,n}`/`{0,}` by #400/#447). The terminal
//! then builds and matches the literal text exactly like Python.
//!
//! This is the sibling of H6-2 (#400, the `{,n}` empty-lower-bound rewrite): same
//! `normalize_python_escapes` seam, same opposite-polarity contract (support a
//! Python-accepted form, not narrow to a Python rejection). Distinct from #461
//! (`\N{NAME}`, which needs a Unicode-name→codepoint table — left rejecting here, it
//! is the named-character escape, not a plain brace run) and from the inverted-bound
//! `{3,2}` (a genuine reject both engines keep).

use lark_rs::{Child, Lark, LarkOptions, LexerType, ParseTree, ParserAlgorithm};

fn opts(parser: ParserAlgorithm, lexer: LexerType) -> LarkOptions {
    LarkOptions {
        parser,
        lexer,
        start: vec!["start".to_string()],
        ..Default::default()
    }
}

/// First token (pre-order) type in the tree — for asserting which terminal won a span.
fn first_token_type(t: &ParseTree) -> Option<String> {
    fn walk(c: &Child) -> Option<String> {
        match c {
            Child::Token(tok) => Some(tok.type_.clone()),
            Child::Tree(tr) => tr.children.iter().find_map(walk),
            Child::None => None,
        }
    }
    match t {
        ParseTree::Token(tok) => Some(tok.type_.clone()),
        ParseTree::Tree(tr) => tr.children.iter().find_map(walk),
        ParseTree::None => None,
    }
}

/// The text of the first token (pre-order) in the tree — for asserting the *literal*
/// span a brace-run terminal matched, not just its type.
fn first_token_text(t: &ParseTree) -> Option<String> {
    fn walk(c: &Child) -> Option<String> {
        match c {
            Child::Token(tok) => Some(tok.value.clone()),
            Child::Tree(tr) => tr.children.iter().find_map(walk),
            Child::None => None,
        }
    }
    match t {
        ParseTree::Token(tok) => Some(tok.value.clone()),
        ParseTree::Tree(tr) => tr.children.iter().find_map(walk),
        ParseTree::None => None,
    }
}

/// All four engine/lexer configs — the brace normalization is engine-independent (it
/// runs in the shared `PatternRe::new` front-end), so every config must build & match.
const CONFIGS: &[(ParserAlgorithm, LexerType)] = &[
    (ParserAlgorithm::Lalr, LexerType::Contextual),
    (ParserAlgorithm::Lalr, LexerType::Basic),
    (ParserAlgorithm::Earley, LexerType::Basic),
    (ParserAlgorithm::Earley, LexerType::Dynamic),
];

/// #462 core: a literal `{...}` brace-run builds and matches the literal text exactly
/// like Python, on every engine/lexer config. The four issue-named repros plus the
/// fully-empty `{}` and an unterminated `a{`.
#[test]
fn issue462_literal_brace_run_builds_and_matches() {
    // (terminal body, input == the literal text Python matches)
    let cases = [
        ("{x}", "{x}"),
        ("a{x}b", "a{x}b"),
        ("N{x}", "N{x}"),
        ("{}", "{}"),
        ("a{}b", "a{}b"),
        ("a{", "a{"),
    ];
    for (body, input) in cases {
        for (parser, lexer) in CONFIGS.iter().cloned() {
            let g = format!("start: A\nA: /{body}/\n");
            let lark = Lark::new(&g, opts(parser.clone(), lexer.clone())).unwrap_or_else(|e| {
                panic!(
                    "#462 ({parser:?}/{lexer:?}): Python accepts /{body}/ as a literal brace run; \
                     lark-rs rejected the build: {e:?}"
                )
            });
            let tree = lark.parse(input).unwrap_or_else(|e| {
                panic!("#462 ({parser:?}/{lexer:?}): /{body}/ must parse {input:?}: {e:?}")
            });
            assert_eq!(
                first_token_type(&tree).as_deref(),
                Some("A"),
                "#462 ({parser:?}/{lexer:?}): /{body}/ must lex {input:?} as token A"
            );
            assert_eq!(
                first_token_text(&tree).as_deref(),
                Some(input),
                "#462 ({parser:?}/{lexer:?}): /{body}/ must match the *literal* text {input:?}"
            );
        }
    }
}

/// The category is the load-bearing second defect: the build *was* refused as
/// `LookaroundScope` (backtracking-only). After the fix the literal-brace forms build
/// (no error at all). This pins that we no longer surface ANY error for them — and, as
/// a guard against a regression that re-rejects-but-recategorizes, that the build is
/// `Ok`, the strongest statement (support, not merely re-bucketing).
#[test]
fn issue462_literal_brace_no_longer_lookaround_error() {
    for body in ["{x}", "a{x}b", "N{x}", "{}", "a{"] {
        let g = format!("start: A\nA: /{body}/\n");
        let built = Lark::new(&g, opts(ParserAlgorithm::Lalr, LexerType::Basic));
        assert!(
            built.is_ok(),
            "#462: /{body}/ must build (was mis-categorized as LookaroundScope); err={:?}",
            built.err()
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Targeted differential audit vs Python `re` (lark 1.3.1, verified live).
// Regex categorization is a space the compliance/wild banks under-sample, so these
// pin the adversarial literal-brace cases the issue names — bare `{x}`, lone `{`,
// `a{b}c`, escaped `\{`, and (critically) literal braces *mixed with real
// quantifiers* — against the exact Python match/no-match behavior.
// ─────────────────────────────────────────────────────────────────────────────

/// Literal-brace forms with a non-quantifier body that still carries a digit/comma
/// (so they superficially resemble a quantifier but Python reads them as literals):
/// `{ 2}`, `{2 }`, `{a,b}`, `{,x}`, `{2,x}`, `{1.5}`. Each must build and match its
/// literal text — proving the fix keys on `base_quantifier_len` (the shared quantifier
/// oracle), not a coarse "has a digit ⇒ quantifier" heuristic.
#[test]
fn issue462_audit_digit_carrying_literal_braces() {
    let cases = [
        ("{ 2}", "{ 2}"),
        ("{2 }", "{2 }"),
        ("{a,b}", "{a,b}"),
        ("{,x}", "{,x}"),
        ("{2,x}", "{2,x}"),
        ("{1.5}", "{1.5}"),
    ];
    for (body, input) in cases {
        for (parser, lexer) in CONFIGS.iter().cloned() {
            let g = format!("start: A\nA: /{body}/\n");
            let lark = Lark::new(&g, opts(parser.clone(), lexer.clone())).unwrap_or_else(|e| {
                panic!("#462 audit ({parser:?}/{lexer:?}): /{body}/ builds: {e:?}")
            });
            let tree = lark.parse(input).unwrap_or_else(|e| {
                panic!("#462 audit ({parser:?}/{lexer:?}): /{body}/ parses {input:?}: {e:?}")
            });
            assert_eq!(
                first_token_text(&tree).as_deref(),
                Some(input),
                "#462 audit ({parser:?}/{lexer:?}): /{body}/ must match literal {input:?}"
            );
        }
    }
}

/// **Mixed literal-brace + real quantifier** — the decisive differential. A real
/// quantifier `{2}` must keep its repetition meaning while a sibling literal `{x}` is
/// escaped, exactly as Python reads it:
///   * `a{x}c{2}d` matches `a{x}ccd` (literal `{x}`, then `c` repeated twice), NOT
///     `a{x}cd`.
///   * `a{2}{x}` matches `aa{x}` (`a` twice, literal `{x}`), NOT `a{x}`.
/// If the fix wrongly escaped *every* `{`, the `{2}` would become literal and these
/// would mis-match; if it escaped *none*, the build would still fail. Pins both halves.
#[test]
fn issue462_audit_mixed_literal_and_real_quantifier() {
    // (body, input_that_matches, input_that_must_NOT_match)
    let cases = [
        ("a{x}c{2}d", "a{x}ccd", "a{x}cd"),
        ("a{2}{x}", "aa{x}", "a{x}"),
        ("{x}a{2}", "{x}aa", "{x}a"),
    ];
    for (body, good, bad) in cases {
        for (parser, lexer) in CONFIGS.iter().cloned() {
            let g = format!("start: A\nA: /{body}/\n");
            let lark = Lark::new(&g, opts(parser.clone(), lexer.clone())).unwrap_or_else(|e| {
                panic!("#462 mixed ({parser:?}/{lexer:?}): /{body}/ builds: {e:?}")
            });
            assert_eq!(
                first_token_text(&lark.parse(good).expect("matches good")).as_deref(),
                Some(good),
                "#462 mixed ({parser:?}/{lexer:?}): /{body}/ must match {good:?} (real `{{2}}` kept, literal `{{x}}` escaped)"
            );
            assert!(
                lark.parse(bad).is_err(),
                "#462 mixed ({parser:?}/{lexer:?}): /{body}/ must NOT match {bad:?} — the real `{{2}}` \
                 quantifier still requires its repetition (Python rejects {bad:?} too)"
            );
        }
    }
}

/// `{x}` is a *literal* `{`, `x`, `}` — exactly three chars — so it must NOT match
/// `{xx}` (Python: `re.match('{x}', '{xx}')` is None). Pins that the escape does not
/// silently turn the body into a class/quantifier that would over-match.
#[test]
fn issue462_audit_literal_brace_is_exact() {
    let g = "start: A\nA: /{x}/\n";
    for (parser, lexer) in CONFIGS.iter().cloned() {
        let lark = Lark::new(g, opts(parser.clone(), lexer.clone()))
            .unwrap_or_else(|e| panic!("#462 exact ({parser:?}/{lexer:?}): builds: {e:?}"));
        assert_eq!(
            first_token_text(&lark.parse("{x}").expect("matches '{x}'")).as_deref(),
            Some("{x}"),
            "#462 exact ({parser:?}/{lexer:?}): /{{x}}/ matches the literal '{{x}}'"
        );
        assert!(
            lark.parse("{xx}").is_err(),
            "#462 exact ({parser:?}/{lexer:?}): /{{x}}/ is a 3-char literal; it must NOT match '{{xx}}'"
        );
    }
}

/// Escaped `\{` — the author already escaped the brace — must keep working unchanged
/// (the cursor's escape step consumes the `\{` pair before the literal-brace branch, so
/// it is never double-escaped). `/a\{x}b/` matches the literal `a{x}b`, same as the
/// unescaped `/a{x}b/`.
#[test]
fn issue462_audit_pre_escaped_brace_unchanged() {
    let g = "start: A\nA: /a\\{x}b/\n";
    for (parser, lexer) in CONFIGS.iter().cloned() {
        let lark = Lark::new(g, opts(parser.clone(), lexer.clone()))
            .unwrap_or_else(|e| panic!("#462 escaped ({parser:?}/{lexer:?}): builds: {e:?}"));
        assert_eq!(
            first_token_text(&lark.parse("a{x}b").expect("matches 'a{x}b'")).as_deref(),
            Some("a{x}b"),
            "#462 escaped ({parser:?}/{lexer:?}): /a\\{{x}}b/ matches literal 'a{{x}}b'"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Negative controls — the fix must NOT widen acceptance past Python.
// ─────────────────────────────────────────────────────────────────────────────

/// Real quantifiers are untouched: `/a{2}b/` is `a` `aa`-repeat… er, `a` twice — it
/// matches `aab`, not `ab` (so it never became a literal `{2}`). The `{2,3}` and `{2,}`
/// forms likewise keep their repetition meaning. (`base_quantifier_len` recognizes
/// these, so the literal-brace branch never fires on them.)
#[test]
fn issue462_control_real_quantifiers_untouched() {
    let cases = [
        ("a{2}b", "aab", "ab"),
        ("a{2,3}b", "aaab", "ab"),
        ("a{2,}b", "aaaab", "ab"),
    ];
    for (body, good, bad) in cases {
        for (parser, lexer) in CONFIGS.iter().cloned() {
            let g = format!("start: A\nA: /{body}/\n");
            let lark = Lark::new(&g, opts(parser.clone(), lexer.clone())).unwrap_or_else(|e| {
                panic!("#462 control ({parser:?}/{lexer:?}): /{body}/ builds: {e:?}")
            });
            assert!(
                lark.parse(good).is_ok(),
                "#462 control ({parser:?}/{lexer:?}): /{body}/ must still match {good:?} (real quantifier)"
            );
            assert!(
                lark.parse(bad).is_err(),
                "#462 control ({parser:?}/{lexer:?}): /{body}/ must NOT match {bad:?} — it is a real \
                 quantifier, not a literal brace run"
            );
        }
    }
}

/// `{2}{x}` is "nothing to repeat" in Python (`re.compile('{2}{x}')` raises) — the
/// leading `{2}` has no preceding expression. The literal-brace fix must NOT make this
/// build: the `{2}` stays a (dangling) quantifier, so `find_nothing_to_repeat` still
/// rejects it. Pins parity with Python's rejection (ADR-0017: never out-permit).
#[test]
fn issue462_control_leading_quantifier_still_nothing_to_repeat() {
    for body in ["{2}{x}", "{2}", "{2,3}"] {
        let g = format!("start: A\nA: /{body}/\n");
        assert!(
            Lark::new(&g, opts(ParserAlgorithm::Lalr, LexerType::Basic)).is_err(),
            "#462 control: /{body}/ has a leading quantifier with nothing to repeat — Python rejects \
             it; the literal-brace fix must not make it build"
        );
    }
}

/// `\N{NAME}` is the **named-character escape** (#461), not a plain brace run: Python
/// *accepts* it (→ a codepoint) but full support needs a 138k-entry Unicode-name table
/// the regex crate does not ship, so lark-rs still rejects it — and must NOT be tricked
/// into "supporting" it by the literal-brace branch (the `\N` is an escape pair the
/// cursor consumes before the `{`; the `{NAME}` after it is *not* a quantifier, but the
/// dedicated `reject_named_unicode_escape` screen rejects `\N{` first). Pins that #462
/// and #461 stay distinct.
#[test]
fn issue462_control_named_unicode_escape_still_rejected() {
    // `\N{BULLET}` — a real named codepoint Python accepts; lark-rs rejects (table, #461).
    let g = "start: A\nA: /\\N{BULLET}/\n";
    assert!(
        Lark::new(g, opts(ParserAlgorithm::Lalr, LexerType::Basic)).is_err(),
        "#462 control: `\\N{{NAME}}` is the #461 named-character escape, not a literal brace run — \
         it must stay rejected (needs a Unicode-name table), not be silently 'supported' by the \
         #462 brace fix"
    );
}
