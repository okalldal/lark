//! Bug-bounty findings — failing oracle tests (XFAIL).
//!
//! Each test below encodes a confirmed divergence between Python Lark 1.3.1 (the
//! oracle) and lark-rs, found by the differential strike-team sweep driven through
//! `tools/diffcheck.py` + the `diffcheck` binary. Every test asserts the
//! **Python-oracle** behavior.
//!
//! This file began as an XFAIL catalog. As each finding is fixed, its `#[ignore]`
//! is dropped and the test becomes a permanent **live regression guard**; the
//! remaining known divergences stay `#[ignore]`d with an `XFAIL` reason (Rust has
//! no native xfail). So this file is a *mix* of live guards and still-open XFAILs —
//! consult each test's own attribute, not this header, for its current status. Run
//! only the still-open XFAILs with:
//!
//!     cargo test --test test_bounty_findings -- --ignored
//!
//! (each red == a reproduced, minimized, still-unfixed bug).
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

/// Assert that building `grammar` succeeds (Python accepts it at build).
fn assert_build_accepted(grammar: &str, parser: ParserAlgorithm, lexer: LexerType, why: &str) {
    let r = build(grammar, parser, lexer, false);
    assert!(
        r.is_ok(),
        "{why}: Python Lark accepts this grammar at build, but lark-rs rejected it: {:?}",
        r.err()
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
fn rc1_duplicate_rule_definition_rejected() {
    let g = "start: a\na: \"x\"\na: \"y\"\n";
    assert_build_rejected(g, ParserAlgorithm::Lalr, LexerType::Contextual, "RC1");
}

/// RC2 (HIGH). A terminal imported from a bundled library and then re-declared
/// (`%declare`) — or redefined locally — collides. Python:
/// `GrammarError: Terminal 'INT' defined more than once`. lark-rs keeps one
/// definition silently and builds.
#[test]
fn rc2_duplicate_terminal_import_then_declare_rejected() {
    let g = "%import common.INT\n%declare INT\nstart: INT\n";
    assert_build_rejected(g, ParserAlgorithm::Lalr, LexerType::Contextual, "RC2");
}

/// RC2b (HIGH). Same root cause via the import + local-redefinition surface.
#[test]
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
fn rc4a_alias_on_inlined_rule_rejected() {
    let g = "start: _w\n_w: A -> aliased\nA: \"a\"\n";
    assert_build_rejected(g, ParserAlgorithm::Lalr, LexerType::Contextual, "RC4a");
}

/// RC4b (HIGH). The `?` (expand1) modifier on an inlined (`_`-prefixed) rule.
/// Python: `GrammarError: Inlined rules (_rule) cannot use the ?rule modifier.`
/// lark-rs accepts.
#[test]
fn rc4b_qmark_on_inlined_rule_rejected() {
    let g = "?_w: A\nstart: _w\nA: \"a\"\n";
    assert_build_rejected(g, ParserAlgorithm::Lalr, LexerType::Contextual, "RC4b");
}

/// RC4c (HIGH). An alias inside a parenthesized sub-expression. Aliases are only
/// legal at the top level of an alternative; inside a group Python parses `foo` as
/// a rule reference: `GrammarError: Rule 'foo' used but not defined`. lark-rs
/// treats it as a local alias and builds a `foo` node.
#[test]
fn rc4c_alias_inside_group_rejected() {
    let g = "start: (A -> foo) B\nA: \"a\"\nB: \"b\"\n";
    assert_build_rejected(g, ParserAlgorithm::Lalr, LexerType::Contextual, "RC4c");
}

// ─────────────────────────────────────────────────────────────────────────────
// LALR table construction.
// ─────────────────────────────────────────────────────────────────────────────

/// RC7 (HIGH). Two star-arms differing only by parenthesization build distinct
/// but equivalent star-helper rules; Python's LALR analysis reports a
/// `Reduce/Reduce collision` and rejects at build. lark-rs used to build the table
/// and parse, masking real ambiguity.
///
/// FIXED (#272, Option A — amends ADR-0013): the load-bearing EBNF helper *sharing*
/// (`recurse_cache`) fuses `r0*` and `(r0)*` into one helper, so the conflict
/// detector (correct) never sees two rules to collide. Rather than un-share (which
/// regresses the LALR bank 512→482), the loader builds a Python-faithful **audit
/// shadow** — the same grammar with recurse helpers keyed on the inner *source-AST*
/// (Python Lark's `EBNF_to_BNF._add_recurse_rule`), so the two helpers split exactly
/// as Python mints them — and the LALR build runs the *real* conflict detector over
/// the shadow, surfacing the collision the sharing masks. LALR-only (Earley agrees).
/// The differential family (`+` variant, arm-order, nested, two-rule, tail-guarded,
/// and the legitimate-sharing accept cases) is pinned in `rc7_*` tests below.
#[test]
fn rc7_lalr_reduce_reduce_collision_rejected() {
    let g = "start: r0* | (r0)*\nr0: \"a\"\n";
    assert_build_rejected(g, ParserAlgorithm::Lalr, LexerType::Contextual, "RC7");
}

/// RC7 differential audit (#272): the reduce/reduce collision audit must match
/// Python Lark 1.3.1's accept/reject verdict *exactly* — reject only what Python
/// rejects, and never redden a legitimate sharing case Python accepts. Pinned
/// directly against the oracle's measured verdicts (Python Lark 1.3.1).
#[test]
fn rc7_reduce_reduce_differential_matches_oracle() {
    // (name, grammar, python_rejects?)
    let cases: &[(&str, &str, bool)] = &[
        // — Python REJECTS: distinct-inner-AST star/plus helpers that collide. —
        ("r0*|(r0)*", "start: r0* | (r0)*\nr0: \"a\"\n", true),
        ("r0+|(r0)+", "start: r0+ | (r0)+\nr0: \"a\"\n", true),
        (
            "arm-order (r0)*|r0*",
            "start: (r0)* | r0*\nr0: \"a\"\n",
            true,
        ),
        (
            "nested ((r0))*|r0*",
            "start: ((r0))* | r0*\nr0: \"a\"\n",
            true,
        ),
        (
            "tail (r0)* X | r0*",
            "start: (r0)* X | r0*\nr0: \"a\"\nX: \"x\"\n",
            true,
        ),
        (
            "two-rule x:a+/y:a+",
            "start: x | y\nx: a+\ny: a+\na: \"a\"\n",
            true,
        ),
        (
            "cross-rule p:r0* q:(r0)*",
            "start: p | q\np: r0*\nq: (r0)*\nr0: \"a\"\n",
            true,
        ),
        // Python shares the recurse core grammar-wide (its `rules_cache`), so two
        // rules each `WORD+` collide on the *shared* `__foo_plus_0`. lark-rs shares
        // too and its plain detector already catches this — no over-share audit
        // needed, but it must stay rejected.
        (
            "foo:WORD+/bar:WORD+",
            "start: foo | bar\nfoo: WORD+\nbar: WORD+\n%import common.WORD\n",
            true,
        ),
        (
            "a:(\",\"X)*/b:(\",\"X)*",
            "start: a | b\na: (\",\" X)*\nb: (\",\" X)*\nX: \"x\"\n",
            true,
        ),
        // keep_all (`!`) context: `A+` plain vs `(A)+` under `!` — distinct inner
        // AST, so Python splits and rejects. The shadow keys on `(ast_key,
        // keep_all)`; pins that the keep_all dimension does not perturb the verdict.
        ("!a: A+ | (A)+", "start: a\n!a: A+ | (A)+\nA: \"a\"\n", true),
        // Templates: two usages whose inner AST differs (`u{r0}` vs plain `r0`)
        // split exactly as Python's post-instantiation `rules_cache` would.
        (
            "template u{r0}*|r0*",
            "start: u{r0}* | r0*\nu{a}: a\nr0: \"x\"\n",
            true,
        ),
        (
            "two-template u{r0}*|v{r0}*",
            "start: u{r0}* | v{r0}*\nu{a}: a\nv{a}: a\nr0: \"x\"\n",
            true,
        ),
        // — Python ACCEPTS: the audit must NOT over-reject these. —
        // Same inner under two arms that genuinely differ (trailing `b`) — accept.
        (
            "same-rule A+ | A+ B",
            "start: A+ | A+ B\nA: \"a\"\nB: \"b\"\n",
            false,
        ),
        // Distinct inner symbols ⇒ distinct, non-colliding helpers.
        (
            "r0*|(s0)*",
            "start: r0* | (s0)*\nr0: \"a\"\ns0: \"b\"\n",
            false,
        ),
        // Distinct left-context (A / B) ⇒ the two split helpers never reach a
        // common state, so no reduce/reduce even though their bodies coincide.
        (
            "guarded A r0*|B (r0)*",
            "start: A r0* | B (r0)*\nr0: \"x\"\nA: \"a\"\nB: \"b\"\n",
            false,
        ),
        // Legitimate sharing Python accepts — the arms genuinely differ.
        ("a+ b | a+", "start: a+ b | a+\na: \"a\"\nb: \"b\"\n", false),
        ("a* b | a+", "start: a* b | a+\na: \"a\"\nb: \"b\"\n", false),
        // Identical inner AST shares one helper in *both* engines — accept.
        ("r0*|r0*", "start: r0* | r0*\nr0: \"a\"\n", false),
        ("single (\",\"X)*", "start: (\",\" X)*\nX: \"x\"\n", false),
    ];
    for (name, g, rejects) in cases {
        let r = build(g, ParserAlgorithm::Lalr, LexerType::Contextual, false);
        if *rejects {
            assert!(
                r.is_err(),
                "RC7 differential: Python rejects `{name}`, but lark-rs accepted it"
            );
        } else {
            assert!(
                r.is_ok(),
                "RC7 differential: Python accepts `{name}`, but lark-rs rejected it: {:?}",
                r.err()
            );
        }
    }
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

/// RC8 (HIGH, FIXED — #276). A zero-width regexp terminal (`A: /a*/`) under the
/// dynamic lexer. Python: `GrammarError: "Dynamic Earley doesn't allow zero-width
/// regexps"`. lark-rs used to build and parse under both `dynamic` and
/// `dynamic_complete` — missing the validation gate, more permissive than the
/// oracle. `DynamicMatcher::new` now rejects any terminal whose regexp can derive
/// the empty string, matching Python's `min_width == 0` rule on both dynamic lexers.
#[test]
fn rc8_zero_width_regexp_dynamic_rejected() {
    let g = "start: A\nA: /a*/\n";
    assert_build_rejected(g, ParserAlgorithm::Earley, LexerType::Dynamic, "RC8");
    assert_build_rejected(
        g,
        ParserAlgorithm::Earley,
        LexerType::DynamicComplete,
        "RC8 (dynamic_complete)",
    );
}

/// RC8 differential audit (#276). The zero-width gate matches Python Lark's
/// `min_width == 0` rule across the dynamic-lexer surface — not just `/a*/`. Each
/// pattern below was confirmed against Python Lark 1.3.1 (`get_regexp_width`): the
/// `reject` set has `min_width == 0` and Python raises "Dynamic Earley doesn't allow
/// zero-width regexps"; the `accept` set has `min_width >= 1` and Python builds. The
/// gate uses the assertion-aware min-width oracle so it agrees with Python on the
/// cases a plain `is_match("")` probe would miss — a zero-width *lookaround*
/// terminal (`/a*(?=b)/`, which routes to the lowered DFA path) and a bare word
/// boundary (`/\b/`, whose `min_width` is 0 in Python though it matches no empty
/// string). It must not over-reject: a terminal that can derive empty *and* a
/// non-empty string is rejected (Python rejects on min, not max width), but a
/// non-nullable lookaround terminal (`/a+(?=b)/`) still builds.
#[test]
fn rc8_zero_width_dynamic_differential_audit() {
    let reject: &[&str] = &[
        "A: /a*/",      // min 0
        "A: /a?/",      // min 0
        "A: /(ab)*/",   // min 0
        "A: /x*y*/",    // min 0
        "A: /a*(?=b)/", // min 0, lookaround → lowered branch
        "A: /(?=a)b*/", // min 0, lookaround → lowered branch
        r"A: /\b/",     // min 0, bare word boundary (is_match("") is false)
    ];
    let accept: &[&str] = &[
        "A: /a+/",        // min 1
        "A: /ab/",        // min 2
        "A: /a+(?=b)/",   // min 1, non-nullable lookaround → lowered branch
        r#"A: /[^"\\]/"#, // min 1
    ];
    for lexer in [LexerType::Dynamic, LexerType::DynamicComplete] {
        for body in reject {
            let g = format!("start: A\n{body}\n");
            assert_build_rejected(&g, ParserAlgorithm::Earley, lexer.clone(), body);
        }
        for body in accept {
            let g = format!("start: A\n{body}\n");
            assert_build_accepted(&g, ParserAlgorithm::Earley, lexer.clone(), body);
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tree shaping.
// ─────────────────────────────────────────────────────────────────────────────

/// RC9 (HIGH). `expand1` (`?rule`) fails to collapse a single placeholder-`None`
/// child. With `maybe_placeholders=true`, `?w: [A]` on empty input has exactly one
/// child — the `None` placeholder. Python collapses the single-child `?` rule,
/// yielding `start[None]`. lark-rs kept the `w` wrapper: `start[w[None]]`. With a
/// real single child both collapse correctly, isolating the bug to the lone-`None`
/// case. Backend-independent (LALR + Earley); now FIXED — the `?` collapse is purely
/// arity-1, never value-typed (a lone `None` collapses like any single child).
#[test]
fn rc9_expand1_collapses_lone_placeholder() {
    let g = "start: w\n?w: [A]\nA: \"a\"\n";
    for (parser, lexer) in [
        (ParserAlgorithm::Lalr, LexerType::Contextual),
        (ParserAlgorithm::Earley, LexerType::Dynamic),
    ] {
        let who = format!("{parser:?}");
        let lark = build(g, parser, lexer, true).expect("RC9: grammar should build");
        let tree = lark.parse("").expect("RC9: empty input parses");
        let ParseTree::Tree(t) = tree else {
            panic!("RC9 ({who}): expected a `start` tree");
        };
        assert_eq!(
            t.children.len(),
            1,
            "RC9 ({who}): start should have one child"
        );
        assert!(
            matches!(t.children[0], Child::None),
            "RC9 ({who}): expected start[None] (expand1 collapsed the ?w wrapper), got {:?}",
            t.children[0]
        );
    }
}

/// RC9 / V3 (HIGH, template variant). The same lone-`None` collapse through
/// parameterized-template instantiation. `?start: sep{i, ","}` / `?i: [A]` expands
/// each separated element through `?i`; on the empty branch each instantiation has a
/// lone `None` child that must collapse. Python yields `sep[None]` for `""` (the
/// `?start` collapses to its single `sep` child, whose lone element is the `None`),
/// `sep[a]` for `"a"`, and `sep[a, a, …]` for the separated forms — never a
/// surviving `i[None]`/`i[A]` wrapper. lark-rs previously left multiple un-collapsed
/// wrappers, one per element. Backend-independent (LALR + Earley).
#[test]
fn rc9_v3_expand1_collapses_lone_placeholder_via_template() {
    let g = "?start: sep{i, \",\"}\n?i: [A]\nA: \"a\"\nsep{x, s}: x (s x)*\n";
    // Expected children of the (collapsed-to-`sep`) root, per input. `""` is the
    // lone-`None` case; the rest exercise the real-single-child collapse per element.
    let cases: [(&str, &[Option<&str>]); 4] = [
        ("", &[None]),
        ("a", &[Some("a")]),
        ("a,a", &[Some("a"), Some("a")]),
        ("a,a,a", &[Some("a"), Some("a"), Some("a")]),
    ];
    for (parser, lexer) in [
        (ParserAlgorithm::Lalr, LexerType::Contextual),
        (ParserAlgorithm::Earley, LexerType::Dynamic),
    ] {
        let who = format!("{parser:?}");
        let lark = build(g, parser, lexer, true).expect("V3: grammar should build");
        for (input, expected) in cases.iter() {
            let tree = lark.parse(input).expect("V3: input parses");
            let ParseTree::Tree(t) = tree else {
                panic!("V3 ({who}, {input:?}): expected a `sep` tree");
            };
            assert_eq!(
                t.data, "sep",
                "V3 ({who}, {input:?}): ?start should collapse to its single sep child"
            );
            assert_eq!(
                t.children.len(),
                expected.len(),
                "V3 ({who}, {input:?}): child count, got {:?}",
                t.children
            );
            for (child, want) in t.children.iter().zip(expected.iter()) {
                match (child, want) {
                    (Child::None, None) => {}
                    (Child::Token(tok), Some(text)) => {
                        assert_eq!(&tok.value, text, "V3 ({who}, {input:?}): token text mismatch")
                    }
                    other => panic!(
                        "V3 ({who}, {input:?}): expected {want:?}, got {:?} (no i[] wrapper allowed)",
                        other.0
                    ),
                }
            }
        }
    }
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
/// FIXED (#280). `rc10_standalone_rejects_lookaround` is now a regression guard:
/// the standalone bake routes every terminal through the refusal seam
/// (`check_standalone_regex_hostable`), rejecting at generation time what the
/// pure-`regex` runtime cannot compile, instead of baking a panicking artifact.
#[test]
fn rc10_standalone_rejects_lookaround() {
    let opts = LarkOptions {
        parser: ParserAlgorithm::Lalr,
        lexer: LexerType::Basic,
        start: vec!["start".to_string()],
        ..Default::default()
    };
    // Both an inline negative-lookahead terminal and the bundled `python.STRING`
    // (lowered into the DFA in-process, but not hostable by the plain-`regex` runtime).
    for g in [
        "start: A\nA: /foo(?!bar)/\n",
        "start: STRING\n%import python.STRING\n",
    ] {
        // The core engine lowers the lookaround and builds fine...
        assert!(
            Lark::new(g, opts.clone()).is_ok(),
            "RC10: precondition — the core engine should build this lowered-lookaround \
             grammar ({g:?})"
        );
        // ...but the standalone bake path must REJECT it (the runtime can't host it),
        // rather than return Ok and bake an uncompilable regex.
        assert!(
            generate_standalone(g, &opts).is_err(),
            "RC10: standalone generation should reject the lookaround grammar {g:?}"
        );
    }
}

/// V1 (#280, extends RC10). A `\Z` anchor terminal: the plain-`regex` standalone
/// runtime cannot compile `\Z`, so baking it verbatim panics the generated parser.
/// The bake must reject at generation time. (`\Z` is mis-categorized as a lookaround
/// error by the core taxonomy — issue #275/N10 — but the standalone contract only
/// requires REJECTION, regardless of the precise category.)
#[test]
fn v1_standalone_rejects_z_anchor() {
    let g = "start: A\nA: /foo\\Z/\n";
    let opts = LarkOptions {
        parser: ParserAlgorithm::Lalr,
        lexer: LexerType::Basic,
        start: vec!["start".to_string()],
        ..Default::default()
    };
    assert!(
        generate_standalone(g, &opts).is_err(),
        "V1: standalone generation should reject a `\\Z` terminal the pure-`regex` \
         runtime cannot host, but it returned Ok and baked a panicking regex"
    );
}

/// V2 (#280, extends RC10). An oversized bounded repeat `[a-z]{200000}` exceeds the
/// `regex` crate's compiled-size limit, so the baked combined scanner panics at
/// `Regex::new`. The bake must reject at generation time. (The core *also*
/// mis-categorizes this as a lookaround error — related to the anchor-dialect fork
/// #275; the standalone contract here is only that generation REFUSES rather than
/// baking a panicking artifact, regardless of category — see the #275 follow-up.)
#[test]
fn v2_standalone_rejects_oversized_repeat() {
    let g = "start: A\nA: /[a-z]{200000}/\n";
    let opts = LarkOptions {
        parser: ParserAlgorithm::Lalr,
        lexer: LexerType::Basic,
        start: vec!["start".to_string()],
        ..Default::default()
    };
    assert!(
        generate_standalone(g, &opts).is_err(),
        "V2: standalone generation should reject an oversized bounded repeat the \
         pure-`regex` runtime cannot host, but it returned Ok and baked a panicking regex"
    );
}

/// Parity floor (#280): the refusal seam must reject *only* what the pure-`regex`
/// runtime cannot host — a normal standalone-able grammar must still bake. Guards
/// against the fix over-rejecting (which would silently break the json/arithmetic
/// fixtures and every standalone-eligible grammar).
#[test]
fn standalone_still_bakes_plain_grammar() {
    let g = "start: A B\nA: /[a-z]+/\nB: /[0-9]+/\n";
    let opts = LarkOptions {
        parser: ParserAlgorithm::Lalr,
        lexer: LexerType::Basic,
        start: vec!["start".to_string()],
        ..Default::default()
    };
    assert!(
        generate_standalone(g, &opts).is_ok(),
        "#280: a plain regex grammar with no lookaround/oversized terminals must still \
         bake — the refusal seam must reject only what the runtime cannot host"
    );
}
