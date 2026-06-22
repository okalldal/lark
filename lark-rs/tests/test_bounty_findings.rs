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
//! Accounting (see `docs/BOUNTY_FINDINGS.md` for the full catalog):
//!   * 10 fresh, harness-confirmed root causes: RC1, RC2, RC4a, RC4b, RC4c, RC5,
//!     RC6, RC7, RC8, RC9 (RC2b is a second *surface* of RC2, not a new cause).
//!   * RC10 — fresh, confirmed at the standalone-generation boundary (its own test).
//!   * RC3 — a KNOWN issue (#252, fixed by the merged PR #259 on the sprint
//!     branch); it still reproduces on this target SHA only because that fix has
//!     not reached `master` yet. Kept as a guard, NOT counted as a fresh find.
//! That is 11 fresh root causes + 1 known-issue guard, across 13 tests.
//!
//! None of the fresh finds overlap the ineligible baseline set (#176 seed-13,
//! #210 seed-99, #258, #250, #228/#229, #253, the equal-span lexer tie-break).

use lark_rs::{
    generate_standalone, Ambiguity, Child, Lark, LarkOptions, LexerType, ParseTree, ParserAlgorithm,
};

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

/// RC1 (HIGH). A rule defined twice with distinct bodies. Python:
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

/// RC3 (KNOWN — not a fresh find). Two sibling optional-bracket terminals collide
/// into a duplicate production. Python: `GrammarError: Rules defined twice ...
/// (colliding expansion of optionals)`. lark-rs accepts. This is the
/// `maybe_placeholders=false` colliding-optional parity gap of **#252**, already
/// fixed by the merged **PR #259** (which oracle-checks `[A] [A]` explicitly, test
/// `test_literal_optional_pair_collides`). It still reproduces on this target SHA
/// only because #259 landed on the sprint branch, not `master`. Kept as a guard;
/// it will pass once #255 lands. Counted as a known-issue duplicate, not a fresh
/// find. (Distinct from #258, which is the mp=true case where both engines agree.)
#[test]
#[ignore = "XFAIL (bounty RC3): KNOWN #252/#259 colliding-optional parity gap (guard only)"]
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

/// RC5 (CRITICAL). Terminal ordering uses the wrong width for regex terminals.
/// Both engines sort terminals by `(-priority, -max_width, -len(pattern), name)`
/// (Python: lark/lexer.py:583; lark-rs: src/lexer/plan.rs:312). The bug is in
/// lark-rs's *width inference*: `Pattern::max_width()` returns `None` for **every**
/// regex (`grammar/terminal.rs:23` — `Pattern::Re(_) => None`), and `plan.rs` maps
/// `None → usize::MAX`. So a *finite* regex like `B: /aa?/` (true max_width = 2) is
/// treated as unbounded, ties with the genuinely-unbounded `A: /a+/`, and the
/// `-len(pattern)` tiebreak then wrongly puts `B` (longer source) first. lark-rs
/// commits to `B`'s greedy `"aa"`, leaving `"a"` to reject; Python computes the
/// finite width, keeps `A` (∞) ahead of `B` (2), and takes the maximal `A="aaa"`.
/// Fix point: compute finite max-width for bounded regexes — NOT the sort key,
/// which is already correct. Same root cause underlies the `%ignore`-steals-a-char
/// and longest-vs-higher-rank variants (see catalog). Not the documented
/// equal-span tie-break — the spans differ (3 vs 2).
#[test] // FIXED (#268): finite regex max-width inference + raw-pattern-length tiebreak.
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

// ─────────────────────────────────────────────────────────────────────────────
// Distribution (standalone generation).
// ─────────────────────────────────────────────────────────────────────────────

/// RC10 (MEDIUM). The standalone generator (and `include_lark!`) silently bakes a
/// lookaround terminal into the pure-`regex` runtime, which cannot compile it. The
/// documented contract (STATUS.md / `lark_proc/src/lib.rs`) is that lookaround
/// grammars are *"rejected at compile time with a clear error"* / *"not
/// standalone-able"*. Instead `generate_standalone()` returns `Ok` with raw
/// `(?!…)`/`(?<…)` baked into `scan_groups`; the generated runtime then panics at
/// `Regex::new(...).expect("baked scanner regex is valid")` on first parse.
///
/// This test asserts the contract at the *generation boundary* (no need to compile
/// the emitted parser): generation should be rejected. The core in-process engine
/// builds the same grammar correctly (it lowers the lookaround into the DFA), so
/// the gap is specific to the standalone bake path. Empirically reproduces both
/// for an inline negative-lookahead terminal and for `%import python.STRING`.
#[test]
#[ignore = "XFAIL (bounty RC10): standalone bakes lookaround instead of rejecting it"]
fn rc10_standalone_rejects_lookaround() {
    let g = "start: A\nA: /foo(?!bar)/\n";
    let opts = LarkOptions {
        parser: ParserAlgorithm::Lalr,
        lexer: LexerType::Basic,
        start: vec!["start".to_string()],
        ..Default::default()
    };
    // The core engine lowers the lookaround and builds fine...
    assert!(
        Lark::new(g, opts.clone()).is_ok(),
        "RC10: precondition — the core engine should build this lowered-lookaround grammar"
    );
    // ...but the standalone bake path must REJECT it (the runtime can't host it).
    let r = generate_standalone(g, &opts);
    assert!(
        r.is_err(),
        "RC10: standalone generation should reject a lookaround grammar, but it \
         returned Ok and baked an uncompilable regex (the generated parser panics \
         at Regex::new on first parse)"
    );
}
