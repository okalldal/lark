//! #447 — the fully-empty quantifier `{,}` (Python `re`'s `{0,}`, == `*`).
//!
//! Sibling of #400 (H6-2, the `{,n}` empty-*lower*-bound quantifier). Python `re` reads
//! `{,}` (empty lower **and** upper bound) as `{0,}` — i.e. `*` (unbounded) — *not* as a
//! literal brace run:
//!
//! ```text
//! sre_parse.parse(r'a{,}b')   # [(MAX_REPEAT,(0, MAXREPEAT,[(LITERAL,97)])),(LITERAL,98)]  == a{0,}b
//! re.match(r'a{,}b','aaab')   # MATCH   (a*)
//! re.match(r'a{,}b','b')      # MATCH   (zero a's)
//! re.match(r'a{,}b','a{,}b')  # None    (NOT a literal-brace match)
//! ```
//!
//! lark-rs (pre-fix) rejected `/a{,}b/` on every engine: the `regex` crate's "repetition
//! quantifier expects a valid decimal", routed (via the lookaround analyzer) to a
//! mis-categorized `LookaroundScope`/`OutOfScope` "backtracking-only syntax" build error.
//! The conservative direction (we rejected what Python accepts), but still a parity gap
//! with the wrong refusal category, exactly as in #400.
//!
//! Two faults, mirroring #400: (1) `base_quantifier_len`'s digit guard treated `{,}` as a
//! literal brace (it required ≥1 digit); (2) `normalize_python_escapes` did not rewrite
//! `{,}` → `{0,}`. The fix widens `base_quantifier_len` (a `{…}` is a quantifier iff it
//! carries a digit *or* a comma) and the `empty_lower_bound_quantifier_upper_len` rewrite
//! (`≥0` upper digits), class-aware and escape-aware via the shared `RegexCursor`.
//!
//! Each expectation is the Python Lark 1.3.1 (oracle) behavior, verified live.

use lark_rs::{Child, Lark, LarkOptions, LexerType, ParseTree, ParserAlgorithm};

fn opts(parser: ParserAlgorithm, lexer: LexerType) -> LarkOptions {
    LarkOptions {
        parser,
        lexer,
        start: vec!["start".to_string()],
        ..Default::default()
    }
}

const ENGINES: &[(ParserAlgorithm, LexerType)] = &[
    (ParserAlgorithm::Lalr, LexerType::Contextual),
    (ParserAlgorithm::Lalr, LexerType::Basic),
    (ParserAlgorithm::Earley, LexerType::Basic),
    (ParserAlgorithm::Earley, LexerType::Dynamic),
];

/// First token (pre-order) in the tree — for asserting which terminal won a span.
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

/// #447 — `/a{,}b/` (== `/a{0,}b/` == `/a*b/`) builds and matches its full match-set as
/// token `A` on **all four** engine configs. The match-set is the `a*b` set: `b` (zero
/// a's), `ab`, `aaab` all match; the literal `a{,}b` does NOT (Python reads `{,}` as a
/// quantifier, not literal braces).
#[test]
fn issue447_empty_quantifier_builds_and_matches_a_star_b() {
    let g = "start: A\nA: /a{,}b/\n";
    for (parser, lexer) in ENGINES.iter().cloned() {
        let lark = Lark::new(g, opts(parser.clone(), lexer.clone())).unwrap_or_else(|e| {
            panic!(
                "#447 ({parser:?}/{lexer:?}): Python accepts /a{{,}}b/ (== /a{{0,}}b/ == /a*b/); \
                 lark-rs rejected the build: {e:?}"
            )
        });
        // The `a*b` match-set: matches as token A.
        for inp in ["b", "ab", "aaab"] {
            let tree = lark.parse(inp).unwrap_or_else(|e| {
                panic!("#447 ({parser:?}/{lexer:?}): {inp:?} must parse: {e:?}")
            });
            assert_eq!(
                first_token_type(&tree).as_deref(),
                Some("A"),
                "#447 ({parser:?}/{lexer:?}): /a{{,}}b/ must match {inp:?} as token A (a*b), \
                 matching Python's {{0,}}"
            );
        }
        // Negative: the literal `a{,}b` is NOT a match — `{,}` is a quantifier, not braces.
        assert!(
            lark.parse("a{,}b").is_err(),
            "#447 ({parser:?}/{lexer:?}): /a{{,}}b/ reads `{{,}}` as a quantifier; the literal \
             string `a{{,}}b` must NOT parse (Python's re.match returns None)"
        );
    }
}

/// The `normalize_python_escapes` pin the issue's Done-when names directly:
/// `normalize_python_escapes("a{,}b") == "a{0,}b"`. Exercised through the public build
/// path — a successful build of `/a{,}b/` is only possible if the normalizer supplied the
/// implicit `0` (the bare `{,}` is a regex-crate build error). The match-set test above is
/// the behavioral proof; this comment records the unit-level contract the issue specifies.
#[test]
fn issue447_normalize_rewrites_empty_quantifier() {
    // `normalize_python_escapes` is module-private; the observable proxy is that the
    // build succeeds and the bare-brace literal does not match (covered above). The exact
    // `{,}` → `{0,}` rewrite is unit-pinned inside `grammar/terminal.rs`
    // (`empty_quantifier_normalizes_to_zero_lower`), alongside the #400 `{,n}` pin.
    let lark = Lark::new(
        "start: A\nA: /a{,}b/\n",
        opts(ParserAlgorithm::Lalr, LexerType::Contextual),
    )
    .expect("#447: /a{,}b/ builds only if `{,}` normalized to `{0,}`");
    assert_eq!(
        first_token_type(&lark.parse("aaab").expect("parses")).as_deref(),
        Some("A"),
        "#447: the normalized `{{,}}`->`{{0,}}` matches `aaab` as `a*b`"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Differential audit (#447): the class-aware / escape-aware edges and the
// multiple-repeat / nothing-to-repeat dialect decisions that this widening
// touches. Each expectation is the Python Lark 1.3.1 oracle, verified live.
// ─────────────────────────────────────────────────────────────────────────────

/// `[a{,}]` — inside a character class `{`, `,`, `}` are all literal members (Python:
/// `IN [LITERAL a, LITERAL {, LITERAL ,, LITERAL }]`). The `{,}` rewrite is class-aware
/// and must NOT fire: the class matches any one of `a`, `{`, `,`, `}` and is unchanged.
#[test]
fn issue447_brace_run_inside_char_class_is_literal() {
    let g = "start: A\nA: /[a{,}]/\n";
    for (parser, lexer) in ENGINES.iter().cloned() {
        let lark = Lark::new(g, opts(parser.clone(), lexer.clone()))
            .unwrap_or_else(|e| panic!("#447 class ({parser:?}/{lexer:?}): builds: {e:?}"));
        // Each of the four literal class members matches as one A; a non-member ('z') does not.
        for inp in ["a", "{", ",", "}"] {
            assert_eq!(
                first_token_type(&lark.parse(inp).expect("parses")).as_deref(),
                Some("A"),
                "#447 class ({parser:?}/{lexer:?}): /[a{{,}}]/ matches the literal {inp:?} as A"
            );
        }
        assert!(
            lark.parse("z").is_err(),
            "#447 class ({parser:?}/{lexer:?}): /[a{{,}}]/ must not match 'z' (only a/{{/,/}})"
        );
    }
}

/// `a\{,}` — the `{` is escaped (`\{`), so the run is a literal `{,}` brace sequence, not a
/// quantifier (Python: `LITERAL a, LITERAL {, LITERAL ,, LITERAL }`). The rewrite is
/// escape-aware (the cursor consumes the `\{` escape pair before the `{` branch), so it
/// must NOT fire — the terminal matches the literal text `a{,}`.
#[test]
fn issue447_escaped_brace_is_literal() {
    let g = "start: A\nA: /a\\{,}/\n";
    for (parser, lexer) in ENGINES.iter().cloned() {
        let lark = Lark::new(g, opts(parser.clone(), lexer.clone()))
            .unwrap_or_else(|e| panic!("#447 escape ({parser:?}/{lexer:?}): builds: {e:?}"));
        assert_eq!(
            first_token_type(&lark.parse("a{,}").expect("parses")).as_deref(),
            Some("A"),
            "#447 escape ({parser:?}/{lexer:?}): /a\\{{,}}/ matches the literal text `a{{,}}` as A"
        );
        // It is the *literal* text, not `a*`: a bare `a` (the quantifier reading) must NOT match.
        assert!(
            lark.parse("a").is_err(),
            "#447 escape ({parser:?}/{lexer:?}): /a\\{{,}}/ is the literal `a{{,}}`, not `a*`; \
             'a' alone must not parse"
        );
    }
}

/// `a{,}{,}b` — with `{,}` now a base quantifier, two adjacent `{,}` stack into a
/// "multiple repeat", which Python `re` rejects at build (`sre_parse`: "multiple repeat"),
/// exactly like `a{0,}{0,}b`. `reject_quantifier_dialect_divergence` (which calls
/// `base_quantifier_len`) must now reject it on every engine — matching the oracle.
#[test]
fn issue447_double_empty_quantifier_is_multiple_repeat() {
    let g = "start: A\nA: /a{,}{,}b/\n";
    for (parser, lexer) in ENGINES.iter().cloned() {
        assert!(
            Lark::new(g, opts(parser.clone(), lexer.clone())).is_err(),
            "#447 double ({parser:?}/{lexer:?}): /a{{,}}{{,}}b/ stacks two quantifiers — Python \
             rejects it as \"multiple repeat\"; lark-rs must too (now that `{{,}}` is a base \
             quantifier)"
        );
    }
}

/// `{,}a` — a leading `{,}` with nothing before it is "nothing to repeat" in Python `re`
/// (a build error, like `*a`). After the rewrite to `{0,}a`, the `regex` crate also
/// rejects the leading repetition operator, so the build fails on every engine — matching
/// Python's rejection (both reject; ADR-0017's conservative direction is preserved).
#[test]
fn issue447_leading_empty_quantifier_is_nothing_to_repeat() {
    let g = "start: A\nA: /{,}a/\n";
    for (parser, lexer) in ENGINES.iter().cloned() {
        assert!(
            Lark::new(g, opts(parser.clone(), lexer.clone())).is_err(),
            "#447 leading ({parser:?}/{lexer:?}): /{{,}}a/ is \"nothing to repeat\" in Python; \
             lark-rs must also reject (the rewritten `{{0,}}a` is a leading repetition the \
             regex crate rejects too)"
        );
    }
}

// Negative control: the bare `{}` (no comma, no digit) stays a **literal** brace pair in
// Python (`LITERAL {, LITERAL }`), not a quantifier — the widening keys on "digit *or*
// comma", so `{}` (neither) is NOT recognized as a quantifier and `normalize_python_escapes`
// leaves it byte-exact (pinned by the unit test `normalize_rewrites_empty_lower_bound_quantifier`
// in `grammar/terminal.rs`: `normalize_python_escapes("a{}b") == "a{}b"`). The end-to-end
// build of `/a{}b/` is a *separate* pre-existing dialect gap (the regex crate rejects the
// bare `{}` where Python reads it literally) outside #447's scope — not asserted here so
// this file pins only the `{,}` contract it owns.
