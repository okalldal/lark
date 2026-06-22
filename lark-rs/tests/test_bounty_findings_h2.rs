//! Bug-bounty findings, round 2 (Phase 2) — failing oracle tests (XFAIL).
//!
//! Round 1 (`test_bounty_findings.rs`, PR #263) harvested the "front door too
//! permissive" layer: missing build-time validation gates plus the RC5 lexer
//! width bug. Round 2 went after the subtler classes the user called out — valid
//! grammar → wrong token/parse, config/backend validation drift, distribution and
//! binding divergence, regex-dialect taxonomy, and deterministic resource growth.
//!
//! Each test asserts the **Python Lark 1.3.1** (oracle) behavior, so it fails
//! against lark-rs today and is marked `#[ignore]` (XFAIL — Rust has no native
//! one). Run them with:
//!
//!     cargo test --test test_bounty_findings_h2 -- --ignored
//!
//! Every find here was independently re-verified through `tools/diffcheck.py`
//! (`compare()`), the `diffcheck` binary, or a direct `lark_rs` API check.
//!
//! Target SHA: the round-1 branch tip (PR #263 stacked on `master` @ a005423).
//! Catalog with repros/severity/blast-radius: `docs/BOUNTY_FINDINGS_H2.md`.
//!
//! NONE of these reduce to a round-1 root cause (RC1–RC10) or the ineligible
//! baseline issues. The IDs below are the round-2 "N" series.

use lark_rs::{Ambiguity, Lark, LarkOptions, LexerType, ParseTree, ParserAlgorithm};

fn opts(parser: ParserAlgorithm, lexer: LexerType) -> LarkOptions {
    LarkOptions {
        parser,
        lexer,
        start: vec!["start".to_string()],
        ..Default::default()
    }
}

fn assert_build_rejected(grammar: &str, o: LarkOptions, why: &str) {
    assert!(
        Lark::new(grammar, o).is_err(),
        "{why}: Python Lark rejects this at build, but lark-rs accepted it"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// N1 — `%override` / `%extend` directive modifiers are silently dropped
// (grammar/loader/parser.rs: "consume modifier; treat same as normal for now").
// The directive never reaches the compiler, so bodies are merged like a plain
// duplicate. Distinct from RC1/RC2 (those are plain duplicate-definition merges
// with no directive). The merge case is a VALID-grammar parse divergence.
// ─────────────────────────────────────────────────────────────────────────────

/// N1a (CRITICAL). `%override start: B` should *replace* `start`, so only `"b"`
/// parses. lark-rs merges to `start: A | B` and wrongly accepts `"a"`.
/// Fixed in #269: directives now reach the compiler, which replaces the body.
#[test]
fn n1a_override_replaces_not_merges() {
    let g = "start: A\n%override start: B\nA: \"a\"\nB: \"b\"\n";
    let lark = Lark::new(g, opts(ParserAlgorithm::Lalr, LexerType::Contextual))
        .expect("N1a: grammar builds");
    // After override, the grammar is `start: B`, so "a" must be rejected (Python does).
    assert!(
        lark.parse("a").is_err(),
        "N1a: %override should have replaced `start` with B; \"a\" must not parse"
    );
}

/// N1b (HIGH). `%override` of a rule that does not exist. Python:
/// `GrammarError: Cannot override a nonexisting rule`. Fixed in #269.
#[test]
fn n1b_override_nonexistent_rejected() {
    let g = "%override foo: A\nstart: A\nA: \"a\"\n";
    assert_build_rejected(g, opts(ParserAlgorithm::Lalr, LexerType::Contextual), "N1b");
}

/// N1c (HIGH). `%extend` of a rule that does not exist. Python:
/// `GrammarError: Can't extend rule foo as it wasn't defined before`. Fixed in #269.
#[test]
fn n1c_extend_nonexistent_rejected() {
    let g = "%extend foo: A\nstart: A\nA: \"a\"\n";
    assert_build_rejected(g, opts(ParserAlgorithm::Lalr, LexerType::Contextual), "N1c");
}

/// N1 differential pin (#269 audit). `%extend` / `%override` of a *parameterized
/// rule* (template) must edit the template, not compile as a flat rule — Python
/// instantiates `foo{C}` from the overridden / extended body. (Pre-fix the
/// directive misrouted to `compile_rule`, which then rejected the template
/// parameter as an "undefined rule".)
#[test]
fn n1_template_override_and_extend() {
    let o = opts(ParserAlgorithm::Lalr, LexerType::Contextual);
    // override: `foo{C}` is `"b" C`, so "ac" is rejected, "bc" parses.
    let ov = Lark::new(
        "foo{x}: \"a\" x\n%override foo{x}: \"b\" x\nstart: foo{C}\nC: \"c\"\n",
        o.clone(),
    )
    .expect("template override builds");
    assert!(
        ov.parse("ac").is_err(),
        "override replaced the template body"
    );
    assert!(ov.parse("bc").is_ok(), "override body parses");
    // extend: both `"a" C` and `"b" C` arms are kept.
    let ex = Lark::new(
        "foo{x}: \"a\" x\n%extend foo{x}: \"b\" x\nstart: foo{C}\nC: \"c\"\n",
        o,
    )
    .expect("template extend builds");
    assert!(ex.parse("ac").is_ok(), "extend keeps the original arm");
    assert!(ex.parse("bc").is_ok(), "extend adds the new arm");
}

/// N1 differential pin (#269 audit), XFAIL — tracked as #286. `%extend` of an
/// *imported* terminal should add the new alternative (Python: `"z"` parses),
/// but lark-rs drops it because the imported terminal is already resolved by the
/// time the directive is staged. Same-grammar terminal extend and imported
/// terminal *override* both work; only imported-terminal extend diverges.
#[test]
#[ignore = "XFAIL (#286): %extend of an imported terminal drops the new alternative"]
fn n1_extend_imported_terminal_keeps_both() {
    let g = "%import common.INT\nstart: INT\n%extend INT: \"z\"\n";
    let lark =
        Lark::new(g, opts(ParserAlgorithm::Lalr, LexerType::Contextual)).expect("grammar builds");
    assert!(lark.parse("123").is_ok(), "the imported INT still parses");
    assert!(
        lark.parse("z").is_ok(),
        "the extended `\"z\"` alternative should parse (Python accepts it)"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Lexer / regex dialect.
// ─────────────────────────────────────────────────────────────────────────────

/// N2 (HIGH). A case-insensitive (flagged) regex terminal is mis-ranked. Python
/// sorts on `len(pattern.value)` (the *raw* source, flags kept separate); lark-rs
/// bakes the flag into the regex string (`(?i:aa)`) and the tiebreak in
/// `lexer/plan.rs` compares that **wrapped** length. So `B: /aa/i` (raw len 2,
/// wrapped len 7) outranks the equal `A: /aa/` and the name-asc tiebreak is
/// subverted: Python emits `A`, lark-rs emits `B`. Distinct from RC5 (max_width):
/// both widths tie; the bug is the flag-wrapper length leaking into the tiebreak.
#[test] // FIXED (#268): raw-pattern-length tiebreak strips the baked flag wrapper.
fn n2_flagged_terminal_ranking() {
    let g = "start: A | B\nA: /aa/\nB: /aa/i\n";
    let lark = Lark::new(g, opts(ParserAlgorithm::Lalr, LexerType::Contextual))
        .expect("N2: grammar builds");
    let ParseTree::Tree(t) = lark.parse("aa").expect("N2: \"aa\" parses") else {
        panic!("N2: expected a tree");
    };
    match &t.children[0] {
        lark_rs::Child::Token(tok) => assert_eq!(
            tok.type_, "A",
            "N2: equal width+priority → name-asc picks A; lark-rs picked {}",
            tok.type_
        ),
        other => panic!("N2: expected a token, got {other:?}"),
    }
}

/// N3 (HIGH). A *global* inline flag group `(?i)` (also `(?m)`, `(?s)`, `(?x)`).
/// Python rejects every terminal with a global inline flag because Lark wraps the
/// pattern source, demoting the flag off position 0:
/// `error: global flags not at the start of the expression`. lark-rs strips the
/// wrapper into a flag bitset and accepts + applies it. (Scoped `(?i:…)` is fine on
/// both — only the global form diverges.) A new more-permissive validation family.
#[test] // FIXED (#274): a global (bodiless) inline flag group is rejected at build,
        // while scoped `(?i:…)` stays accepted — `PatternRe::new` parity gate.
fn n3_global_inline_flag_rejected() {
    let g = "start: A\nA: /(?i)abc/\n";
    assert_build_rejected(g, opts(ParserAlgorithm::Lalr, LexerType::Contextual), "N3");
}

/// N4 (MEDIUM). A named backreference `(?P=name)` is mis-categorized. General
/// backreferences are a documented OUT-OF-SCOPE non-goal (LOOKAROUND_SCOPE.md), so
/// the *correct* behavior is a categorized `GrammarError::LookaroundScope` refusal
/// — exactly what `\1`/`\k`/`\g` produce ("not supported (by design) … a
/// backreference …"). But the classifier's `has_backref` misses the `(?P=name)`
/// spelling, so it slips past classification and lark-rs instead leaks a raw,
/// uncategorized regex error (`Invalid regex pattern … regex parse error`). The
/// XFAIL asserts the *categorized* refusal (matching `\1`), NOT support — this is
/// not promotion to a supported feature. Distinct from RC6 (`\b`, different
/// construct).
#[test] // FIXED (#274): the front-end keeps `(?P=name)` verbatim, so it routes through
        // the categorized backref refusal (`BacktrackingOnlySyntax`) like `\1`/`\k`.
fn n4_named_backref_categorized() {
    let g = "start: A\nA: /(?P<x>a)(?P=x)/\n";
    let err = Lark::new(g, opts(ParserAlgorithm::Lalr, LexerType::Contextual))
        .err()
        .map(|e| e.to_string())
        .unwrap_or_default();
    // A numeric backref `\1` yields the categorized refusal; `(?P=name)` must too.
    assert!(
        err.contains("not supported (by design)"),
        "N4: (?P=name) should give the categorized backref refusal (like \\1), \
         but lark-rs leaked: {err:?}"
    );
}

/// N10 (MEDIUM). The end-of-string anchor `\Z` is a plain Python `re` anchor that
/// Python accepts and tokenizes. lark-rs rejects it at build AND mis-categorizes
/// it as `LookaroundScope` "backtracking-only" — `\Z` is neither lookaround nor
/// backtracking. (Same mis-categorization hits oversized bounded repeats; see
/// catalog.) Distinct from RC6 (uncategorized leak) — this is a *wrong* category.
#[test]
#[ignore = "XFAIL (bounty N10): \\Z anchor rejected + mis-categorized as lookaround"]
fn n10_end_anchor_supported_like_python() {
    let g = r"start: A
A: /x\Z/
";
    let lark = Lark::new(g, opts(ParserAlgorithm::Lalr, LexerType::Contextual))
        .expect("N10: Python builds and tokenizes /x\\Z/");
    assert!(lark.parse("x").is_ok(), "N10: Python accepts \"x\"");
}

// ─────────────────────────────────────────────────────────────────────────────
// Configuration / backend validation drift.
// ─────────────────────────────────────────────────────────────────────────────

/// N5 (HIGH). Parser/lexer pairing legality is not enforced. Python raises
/// `ConfigurationError` for every illegal pair (e.g. `lalr`+`dynamic`,
/// `cyk`+`contextual`, `earley`+`contextual`); lark-rs silently substitutes a
/// working lexer and parses. The only pairing gate in the tree is the
/// postlex+dynamic refusal. Distinct from RC8 (zero-width *content* on dynamic).
#[test]
#[ignore = "XFAIL (bounty N5): illegal parser/lexer pairing not rejected"]
fn n5_illegal_parser_lexer_pairing_rejected() {
    let g = "start: \"a\"\n";
    assert_build_rejected(
        g,
        opts(ParserAlgorithm::Lalr, LexerType::Dynamic),
        "N5 lalr+dynamic",
    );
    assert_build_rejected(
        g,
        opts(ParserAlgorithm::Cyk, LexerType::Contextual),
        "N5 cyk+contextual",
    );
}

/// N6 (HIGH). `ambiguity=` is only valid for Earley/CYK; Python raises
/// `ConfigurationError: 'lalr' doesn't support disambiguation`. lark-rs's
/// `build_lalr` never reads `options.ambiguity`, so it silently accepts and builds.
#[test]
#[ignore = "XFAIL (bounty N6): ambiguity= on parser=lalr not rejected"]
fn n6_ambiguity_on_lalr_rejected() {
    let g = "start: \"a\"\n";
    let mut o = opts(ParserAlgorithm::Lalr, LexerType::Contextual);
    o.ambiguity = Ambiguity::Explicit;
    assert_build_rejected(g, o, "N6");
}

// ─────────────────────────────────────────────────────────────────────────────
// Core correctness surfaced through the bindings.
// ─────────────────────────────────────────────────────────────────────────────

/// N8 (MEDIUM). `start_pos`/`end_pos` are byte offsets in lark-rs but char indices
/// in Python Lark. On `"héllo"` (the `é` is 2 UTF-8 bytes) lark-rs reports
/// `end_pos=6`, Python `5`. `column`/`end_column` are char-based in both and match;
/// only `*_pos` diverge. Core-rooted (`LexCursor` advances by byte length), copied
/// verbatim into the PyO3/WASM/C bindings under a Python-compatible API.
#[test]
#[ignore = "XFAIL (bounty N8): start_pos/end_pos are byte offsets, not char indices"]
fn n8_positions_are_char_indices() {
    let g = "start: A\nA: /h.llo/\n";
    let mut o = opts(ParserAlgorithm::Lalr, LexerType::Contextual);
    o.propagate_positions = true;
    let lark = Lark::new(g, o).expect("N8: grammar builds");
    let ParseTree::Tree(t) = lark.parse("héllo").expect("N8: parses") else {
        panic!("N8: expected a tree");
    };
    let lark_rs::Child::Token(tok) = &t.children[0] else {
        panic!("N8: expected a token");
    };
    assert_eq!(
        tok.end_pos, 5,
        "N8: Python reports char index end_pos=5 for \"héllo\"; lark-rs gave {} (bytes)",
        tok.end_pos
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Deterministic resource growth.
// ─────────────────────────────────────────────────────────────────────────────

/// N9 (MEDIUM). `x~mn..mx` above Python's `REPEAT_BREAK_THRESHOLD` (50) lowers to
/// O(n²) grammar size in lark-rs — `compile_repeat`'s range branch emits one rule
/// per count `k` with `k` copies, Σk = n(n+1)/2 — where Python's `_generate_repeats`
/// factors it into O(log n) shared sub-rules. Measured deterministically (no
/// wall-clock) via the total RHS-symbol count of the lowered grammar: for a 4×
/// bound it grows ≈16× (quadratic). A factored lowering would be near-flat.
/// Both engines build correct parsers — the divergence is purely build/size cost.
#[test]
#[ignore = "XFAIL (bounty N9): x~n..m above threshold is O(n^2) grammar size, not O(log n)"]
fn n9_bounded_repeat_grammar_size_subquadratic() {
    let total_rhs = |n: usize| -> usize {
        let g = lark_rs::load_grammar(
            &format!("start: \"x\"~0..{n}\n"),
            &["start".to_string()],
            false,
            false,
        )
        .expect("N9: grammar loads");
        lark_rs::lower(&g)
            .rules
            .iter()
            .map(|r| r.expansion.len())
            .sum()
    };
    let (r100, r400) = (total_rhs(100), total_rhs(400));
    // A 4× larger bound should NOT quadruple-squared the grammar. Python's factored
    // lowering keeps this ratio ~flat; lark-rs's per-count expansion makes it ≈16×.
    assert!(
        r400 < r100 * 6,
        "N9: total RHS symbols grew {:.1}× for a 4× bound (≈quadratic): {r100}→{r400}",
        r400 as f64 / r100 as f64
    );
}
