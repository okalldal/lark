//! Bug-bounty findings — failing oracle tests (XFAIL).
//!
//! Each test below encodes a confirmed divergence between Python Lark 1.3.1 (the
//! oracle) and lark-rs, found by the differential strike-team sweep driven through
//! `tools/diffcheck.py` + the `diffcheck` binary. Every test asserts the
//! **Python-oracle** behavior, so it currently FAILS against lark-rs — it is
//! therefore marked `#[ignore]` with an `XFAIL` reason (Rust has no native xfail)
//! so the suite stays green. Run them with:
//!
//!     cargo test --test test_bounty_findings -- --ignored
//!
//! to watch them go red (each red == a reproduced, minimized bug). When a bug is
//! fixed, drop its `#[ignore]` and the test becomes a permanent regression guard.
//!
//! Target SHA (frozen baseline the finds were minimized against):
//!   a005423  (branch claude/hackathon-baseline-bounty-08oolp)
//!
//! Full catalog (severity, root cause, blast radius): `docs/BOUNTY_FINDINGS.md`.
//! These reproduce known-good Python behavior; none overlap the ineligible
//! baseline issues (#176 seed-13, #210 seed-99, #258, #250, #228/#229, #253,
//! lexer same-span tie-breaks).

use lark_rs::{Ambiguity, Child, Lark, LarkOptions, LexerType, ParseTree, ParserAlgorithm};

/// Build a parser with the given knobs; returns the `Result` so a test can assert
/// either a build rejection (oracle rejects at construction) or a successful build.
fn build(
    grammar: &str,
    parser: ParserAlgorithm,
    lexer: LexerType,
    maybe_placeholders: bool,
) -> Result<Lark, lark_rs::LarkError> {
    Lark::new(
        grammar,
        LarkOptions {
            parser,
            lexer,
            ambiguity: Ambiguity::Resolve,
            start: vec!["start".to_string()],
            maybe_placeholders,
            ..Default::default()
        },
    )
}

/// Assert that building `grammar` is rejected (Python raises a `GrammarError`).
fn assert_build_rejected(grammar: &str, parser: ParserAlgorithm, lexer: LexerType, why: &str) {
    let r = build(grammar, parser, lexer, false);
    assert!(
        r.is_err(),
        "{why}: Python Lark rejects this grammar at build, but lark-rs accepted it"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Grammar-loader: missing validation gates (lark-rs is more permissive than the
// oracle — unfalsifiable permissiveness, ADR-0017 corollary → a bug).
// ─────────────────────────────────────────────────────────────────────────────

/// RC1 (CRITICAL). A rule defined twice with distinct bodies. Python:
/// `GrammarError: Rule 'a' defined more than once`. lark-rs silently MERGES the
/// two bodies into alternatives and accepts both. Default path; all five backends.
#[test]
#[ignore = "XFAIL (bounty RC1): duplicate rule definition not rejected"]
fn rc1_duplicate_rule_definition_rejected() {
    let g = "start: a\na: \"x\"\na: \"y\"\n";
    assert_build_rejected(g, ParserAlgorithm::Lalr, LexerType::Contextual, "RC1");
}

/// RC2 (HIGH). A terminal imported from a bundled library and then re-declared
/// (`%declare`) — or redefined locally — collides. Python:
/// `GrammarError: Terminal 'INT' defined more than once`. lark-rs keeps one
/// definition silently and builds.
#[test]
#[ignore = "XFAIL (bounty RC2): duplicate terminal definition (import + %declare) not rejected"]
fn rc2_duplicate_terminal_import_then_declare_rejected() {
    let g = "%import common.INT\n%declare INT\nstart: INT\n";
    assert_build_rejected(g, ParserAlgorithm::Lalr, LexerType::Contextual, "RC2");
}

/// RC2b (HIGH). Same root cause via the import + local-redefinition surface.
#[test]
#[ignore = "XFAIL (bounty RC2b): duplicate terminal definition (import + local) not rejected"]
fn rc2b_duplicate_terminal_import_then_local_rejected() {
    let g = "%import common.INT\nINT: \"x\"\nstart: INT\n";
    assert_build_rejected(g, ParserAlgorithm::Lalr, LexerType::Contextual, "RC2b");
}

/// RC3 (HIGH). Two sibling optional-bracket terminals collide into a duplicate
/// production. Python: `GrammarError: Rules defined twice ... (colliding expansion
/// of optionals)`. lark-rs accepts. Default `maybe_placeholders=false` path —
/// structurally distinct from the ineligible #258 (`([A])?`/`[A]~0..1` under
/// maybe_placeholders=true, where both engines agree by rejecting).
#[test]
#[ignore = "XFAIL (bounty RC3): colliding optional expansion not rejected (mp=false)"]
fn rc3_colliding_optional_expansion_rejected() {
    let g = "start: [A] [A] \"c\"\nA: \"a\"\n";
    assert_build_rejected(g, ParserAlgorithm::Lalr, LexerType::Contextual, "RC3");
}

/// RC4a (HIGH). An alias (`-> name`) on an inlined (`_`-prefixed) rule. Python:
/// `GrammarError: Rule _w is marked for expansion ... isn't allowed to have
/// aliases`. lark-rs accepts and emits the aliased node.
#[test]
#[ignore = "XFAIL (bounty RC4a): alias on an inlined _rule not rejected"]
fn rc4a_alias_on_inlined_rule_rejected() {
    let g = "start: _w\n_w: A -> aliased\nA: \"a\"\n";
    assert_build_rejected(g, ParserAlgorithm::Lalr, LexerType::Contextual, "RC4a");
}

/// RC4b (HIGH). The `?` (expand1) modifier on an inlined (`_`-prefixed) rule.
/// Python: `GrammarError: Inlined rules (_rule) cannot use the ?rule modifier.`
/// lark-rs accepts.
#[test]
#[ignore = "XFAIL (bounty RC4b): ?modifier on an inlined _rule not rejected"]
fn rc4b_qmark_on_inlined_rule_rejected() {
    let g = "?_w: A\nstart: _w\nA: \"a\"\n";
    assert_build_rejected(g, ParserAlgorithm::Lalr, LexerType::Contextual, "RC4b");
}

/// RC4c (HIGH). An alias inside a parenthesized sub-expression. Aliases are only
/// legal at the top level of an alternative; inside a group Python parses `foo` as
/// a rule reference: `GrammarError: Rule 'foo' used but not defined`. lark-rs
/// treats it as a local alias and builds a `foo` node.
#[test]
#[ignore = "XFAIL (bounty RC4c): alias inside a parenthesized group not rejected"]
fn rc4c_alias_inside_group_rejected() {
    let g = "start: (A -> foo) B\nA: \"a\"\nB: \"b\"\n";
    assert_build_rejected(g, ParserAlgorithm::Lalr, LexerType::Contextual, "RC4c");
}

// ─────────────────────────────────────────────────────────────────────────────
// LALR table construction.
// ─────────────────────────────────────────────────────────────────────────────

/// RC7 (HIGH). Two star-arms differing only by parenthesization build distinct
/// but equivalent star-helper rules; Python's LALR analysis reports a
/// `Reduce/Reduce collision` and rejects at build. lark-rs builds the table and
/// parses, masking real ambiguity. LALR-only (Earley agrees → conflict detector,
/// not the loader).
#[test]
#[ignore = "XFAIL (bounty RC7): undetected LALR reduce/reduce collision"]
fn rc7_lalr_reduce_reduce_collision_rejected() {
    let g = "start: r0* | (r0)*\nr0: \"a\"\n";
    assert_build_rejected(g, ParserAlgorithm::Lalr, LexerType::Contextual, "RC7");
}

// ─────────────────────────────────────────────────────────────────────────────
// Lexer.
// ─────────────────────────────────────────────────────────────────────────────

/// RC5 (CRITICAL). Terminal selection ignores `max_width`. Python orders
/// terminals by `(-priority, -max_width, -len(pattern), name)` (lark/lexer.py:583)
/// so an *unbounded* terminal (`A: /a+/`, max_width = ∞) is tried before a
/// *bounded* one with a longer regex source (`B: /aa?/`, max_width = 2). lark-rs
/// orders by `(-priority, -pattern_len, name)` only — it tries `B` first, commits
/// to its greedy 2-char match `"aa"`, and the leftover `"a"` rejects. Python takes
/// the maximal `A="aaa"`. Same root cause underlies the `%ignore`-steals-a-char
/// and longest-vs-higher-rank variants (see catalog). Not the documented
/// equal-span tie-break — the spans differ (3 vs 2).
#[test]
#[ignore = "XFAIL (bounty RC5): terminal ordering ignores max_width"]
fn rc5_terminal_ordering_uses_max_width() {
    let g = "start: A | B\nA: /a+/\nB: /aa?/\n";
    let lark = build(g, ParserAlgorithm::Lalr, LexerType::Contextual, false)
        .expect("RC5: grammar should build");
    let tree = lark
        .parse("aaa")
        .expect("RC5: Python accepts 'aaa' as A=\"aaa\"");
    let ParseTree::Tree(t) = tree else {
        panic!("RC5: expected a `start` tree");
    };
    assert_eq!(t.children.len(), 1, "RC5: expected a single A token");
    match &t.children[0] {
        Child::Token(tok) => assert_eq!(
            tok.value, "aaa",
            "RC5: expected the maximal match A=\"aaa\", got {:?}",
            tok.value
        ),
        other => panic!("RC5: expected an A token, got {other:?}"),
    }
}

/// RC6 (MEDIUM). A Python `re` word-boundary `\b` (or `\B`) in a terminal. Python
/// tokenizes `/x\b/` on `"x"` fine. lark-rs fails at *build* with a raw,
/// uncategorized `regex-automata` backend error ("cannot build DFAs for regexes
/// with Unicode word boundaries") instead of either supporting it or surfacing the
/// documented `GrammarError::LookaroundScope` refusal. Distinct from the
/// documented `\<`/`\>` dialect normalization.
#[test]
#[ignore = "XFAIL (bounty RC6): \\b word boundary leaks an uncategorized backend error"]
fn rc6_word_boundary_supported_like_python() {
    let g = r"start: A
A: /x\b/
";
    let lark = build(g, ParserAlgorithm::Lalr, LexerType::Contextual, false)
        .expect("RC6: Python builds and tokenizes /x\\b/");
    let tree = lark.parse("x").expect("RC6: Python accepts \"x\"");
    let ParseTree::Tree(t) = tree else {
        panic!("RC6: expected a `start` tree");
    };
    assert_eq!(t.children.len(), 1, "RC6: expected a single A token");
}

// ─────────────────────────────────────────────────────────────────────────────
// Earley / dynamic lexer.
// ─────────────────────────────────────────────────────────────────────────────

/// RC8 (HIGH). A zero-width regexp terminal (`A: /a*/`) under the dynamic lexer.
/// Python: `GrammarError: "Dynamic Earley doesn't allow zero-width regexps"`.
/// lark-rs builds and parses under both `dynamic` and `dynamic_complete` — missing
/// the validation gate, more permissive than the oracle.
#[test]
#[ignore = "XFAIL (bounty RC8): zero-width regexp under dynamic lexer not rejected"]
fn rc8_zero_width_regexp_dynamic_rejected() {
    let g = "start: A\nA: /a*/\n";
    assert_build_rejected(g, ParserAlgorithm::Earley, LexerType::Dynamic, "RC8");
}

// ─────────────────────────────────────────────────────────────────────────────
// Tree shaping.
// ─────────────────────────────────────────────────────────────────────────────

/// RC9 (HIGH). `expand1` (`?rule`) fails to collapse a single placeholder-`None`
/// child. With `maybe_placeholders=true`, `?w: [A]` on empty input has exactly one
/// child — the `None` placeholder. Python collapses the single-child `?` rule,
/// yielding `start[None]`. lark-rs keeps the `w` wrapper: `start[w[None]]`. With a
/// real single child both collapse correctly, isolating the bug to the lone-`None`
/// case. Backend-independent (LALR + Earley).
#[test]
#[ignore = "XFAIL (bounty RC9): expand1 keeps wrapper around a lone placeholder-None"]
fn rc9_expand1_collapses_lone_placeholder() {
    let g = "start: w\n?w: [A]\nA: \"a\"\n";
    let lark = build(g, ParserAlgorithm::Lalr, LexerType::Contextual, true)
        .expect("RC9: grammar should build");
    let tree = lark.parse("").expect("RC9: empty input parses");
    let ParseTree::Tree(t) = tree else {
        panic!("RC9: expected a `start` tree");
    };
    assert_eq!(t.children.len(), 1, "RC9: start should have one child");
    assert!(
        matches!(t.children[0], Child::None),
        "RC9: expected start[None] (expand1 collapsed the ?w wrapper), got {:?}",
        t.children[0]
    );
}
