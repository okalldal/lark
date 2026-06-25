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
    generate_standalone, Ambiguity, Child, GrammarError, Lark, LarkError, LarkOptions, LexerType,
    ParseTree, ParserAlgorithm,
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

/// Assert a build rejected specifically as Python Lark's duplicate-definition error
/// — `GrammarError::Other` whose message is `"<Type> '<name>' defined more than
/// once"`. Tighter than a bare `is_err()` (cf. `assert_reduce_reduce_conflict`): it
/// fails if the grammar rejected for an *unrelated* reason (a reduce/reduce conflict,
/// a broken import, a nullable-`$END` collision), so a #428 rejection cannot silently
/// regress to a false pass that rejects for the wrong cause.
fn assert_duplicate_definition_rejected(
    grammar: &str,
    parser: ParserAlgorithm,
    lexer: LexerType,
    name: &str,
    why: &str,
) {
    let expected = format!("Rule '{name}' defined more than once");
    match build(grammar, parser, lexer, false) {
        Err(LarkError::Grammar(GrammarError::Other { msg })) => assert!(
            msg.contains(&expected),
            "{why}: rejected as GrammarError::Other, but the message is not the \
             duplicate-definition error (expected to contain {expected:?}):\n{msg}"
        ),
        Err(e) => panic!("{why}: expected the duplicate-definition GrammarError, got: {e:?}"),
        Ok(_) => panic!("{why}: expected a duplicate-definition rejection, but build succeeded"),
    }
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

/// Assert a build result rejected specifically as the **reduce/reduce collision**
/// the RC7 audit targets — the `GrammarError::Conflict` variant whose report names a
/// `Reduce/Reduce collision`. Tighter than a bare `is_err()`: it fails if the grammar
/// rejected for an *unrelated* reason (a duplicate definition, a broken import, a
/// nullable-`$END` collision), which would let a falsely-passing build slip through
/// the differential. Mirrors the `Conflict`-variant assertion in
/// `test_lalr_core.rs::test_conflict_detection_matches_oracle`.
fn assert_reduce_reduce_conflict<T>(r: &Result<T, LarkError>, why: &str) {
    match r {
        Err(LarkError::Grammar(GrammarError::Conflict { report })) => assert!(
            report.contains("Reduce/Reduce collision"),
            "{why}: rejected as GrammarError::Conflict, but the report is not a \
             reduce/reduce collision:\n{report}"
        ),
        Err(e) => panic!("{why}: expected a reduce/reduce GrammarError::Conflict, got: {e:?}"),
        Ok(_) => {
            panic!("{why}: expected a reduce/reduce GrammarError::Conflict, but build succeeded")
        }
    }
}

/// Build a LALR parser whose `%import .module (...)` directives resolve against an
/// in-memory `name -> text` map (the WASM no-filesystem loader path, `import_sources`).
/// `files["main.lark"]` is the entry grammar; the rest are sibling imports. This is
/// how the RC7 `%import` differential constructs multi-file grammars without writing
/// into the shared source tree (mirrors `test_imports.rs::make_lalr_in_memory`).
fn build_with_imports(files: &[(&str, &str)]) -> Result<Lark, lark_rs::LarkError> {
    use std::collections::HashMap;
    use std::sync::Arc;
    let mut sources = HashMap::new();
    let mut main = "";
    for (name, text) in files {
        if *name == "main.lark" {
            main = text;
        }
        sources.insert((*name).to_string(), (*text).to_string());
    }
    Lark::new(
        main,
        LarkOptions {
            parser: ParserAlgorithm::Lalr,
            lexer: LexerType::Contextual,
            ambiguity: Ambiguity::Resolve,
            start: vec!["start".to_string()],
            import_sources: Some(Arc::new(sources)),
            ..Default::default()
        },
    )
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

/// RC2c (#299, spun out of #270). Two *different* imported terminals aliased to the
/// same final name. Python: `Terminal 'X' defined more than once`; lark-rs used to
/// keep one silently (`copy_requested`/`import_terminal` skip when the final name is
/// already defined) and build. The fix dedups by import *source/definition*, not by
/// final name, so two distinct sources at one alias collide while an idempotent
/// re-import of one definition (RC2c-neg, below) still dedups.
#[test]
fn rc2c_duplicate_import_alias_collision_rejected() {
    let g = "%import common.INT -> X\n%import common.WS -> X\nstart: X\n";
    assert_build_rejected(g, ParserAlgorithm::Lalr, LexerType::Contextual, "RC2c");
}

/// RC2c-neg-a (#299, NEGATIVE CONTROL). A legitimate re-import of the *same* terminal
/// under the *same* alias is idempotent — Python accepts it. The dedup must key on
/// the import definition, not reject every duplicate final name.
#[test]
fn rc2c_neg_same_import_twice_accepted() {
    let g = "%import common.INT -> X\n%import common.INT -> X\nstart: X\n";
    assert_build_accepted(
        g,
        ParserAlgorithm::Lalr,
        LexerType::Contextual,
        "RC2c-neg-a",
    );
}

/// RC2c-neg-b (#299, NEGATIVE CONTROL). The same idempotence via the un-aliased
/// re-import surface (`%import common.INT` twice) — Python accepts.
#[test]
fn rc2c_neg_same_import_noalias_twice_accepted() {
    let g = "%import common.INT\n%import common.INT\nstart: INT\n";
    assert_build_accepted(
        g,
        ParserAlgorithm::Lalr,
        LexerType::Contextual,
        "RC2c-neg-b",
    );
}

/// RC2c-388 (#388, FIXED — architect ask on omnibus #354). The risky edge of the
/// RC2c source/alias dedup: the **same** original terminal imported **twice under
/// two *different* aliases**, then only the *shadowed* (earlier) alias used. Python's
/// per-module `import_aliases.update` keeps only the *last* alias binding (`X` is
/// never defined) and rejects at build: `Rule 'X' used but not defined (in rule
/// start)` (verified against Python Lark 1.3.1). lark-rs used to import *both* `X`
/// and `Y` and over-accept `start: X` — a *more-permissive* divergence (ADR-0017
/// corollary: unfalsifiable permissiveness ⇒ a bug). Filed as **#388**.
///
/// Fixed by **last-alias-wins**: the loader drops every non-last alias for a given
/// `(module, original)` source so it is never defined (`alias_survives` /
/// `import_alias_map`), and the #299 collision pre-pass only considers surviving
/// aliases. Now lark-rs rejects `start: X` like Python. (No longer `#[ignore]`d.)
#[test]
fn rc2c_388_same_source_two_aliases_unused_alias_rejected() {
    // common.INT imported as both X and Y; start uses only X. Python: last alias
    // (Y) wins, X is undefined → "Rule 'X' used but not defined".
    let g = "%import common.INT -> X\n%import common.INT -> Y\nstart: X\n";
    assert_build_rejected(g, ParserAlgorithm::Lalr, LexerType::Contextual, "RC2c-388");
}

/// RC2c-388-last (#388, last-alias-wins ACCEPT). The mirror of the case above: the
/// **surviving** (last) alias `Y` *is* defined and usable, so `start: Y` builds.
/// Python Lark 1.3.1 accepts (only the last binding of `(common, INT)` survives).
#[test]
fn rc2c_388_same_source_two_aliases_last_alias_accepted() {
    let g = "%import common.INT -> X\n%import common.INT -> Y\nstart: Y\n";
    assert_build_accepted(
        g,
        ParserAlgorithm::Lalr,
        LexerType::Contextual,
        "RC2c-388-last",
    );
}

/// RC2c-388-both (#388, last-alias-wins REJECT-on-dropped). Using *both* aliases in
/// one rule still rejects: `X` was dropped (only `Y` survives), so `start: X | Y`
/// references an undefined `X`. Python Lark 1.3.1 rejects `Rule 'X' used but not
/// defined (in rule start)`.
#[test]
fn rc2c_388_same_source_two_aliases_both_used_rejected() {
    let g = "%import common.INT -> X\n%import common.INT -> Y\nstart: X | Y\n";
    assert_build_rejected(
        g,
        ParserAlgorithm::Lalr,
        LexerType::Contextual,
        "RC2c-388-both",
    );
}

/// RC2c-388-rule (#388, bundled rule-closure variant — architect ask). Last-alias-wins
/// must also hold where the imported symbol is a *rule* whose dependency closure is
/// copied (not a `common` terminal). `%import python.name -> a` then `-> b` keeps
/// only the last alias `b`: Python Lark 1.3.1 rejects `start: a` (`Rule 'a' used but
/// not defined`) and accepts `start: b`. Exercises the closure-copy path
/// (`import_rule_closure`), not just the `common` terminal-table fast path.
#[test]
fn rc2c_388_bundled_rule_two_aliases_dropped_alias_rejected() {
    let g = "%import python.name -> a\n%import python.name -> b\nstart: a\n";
    assert_build_rejected(
        g,
        ParserAlgorithm::Lalr,
        LexerType::Contextual,
        "RC2c-388-rule-a",
    );
}

/// RC2c-388-rule-b (#388, bundled rule-closure variant — surviving alias ACCEPT).
/// The mirror: the surviving rule alias `b` is defined, so `start: b` builds. Python
/// Lark 1.3.1 accepts.
#[test]
fn rc2c_388_bundled_rule_two_aliases_last_alias_accepted() {
    let g = "%import python.name -> a\n%import python.name -> b\nstart: b\n";
    assert_build_accepted(
        g,
        ParserAlgorithm::Lalr,
        LexerType::Contextual,
        "RC2c-388-rule-b",
    );
}

/// RC2d (#299, spun out of #270). `%extend` of an abstract (`%declare`d,
/// pattern-less) terminal. After `%declare FOO`, FOO lives in `self.terminals`, not
/// `raw_terms`; the Extend arm passed the pre-existence gate, found no `RawTerm` to
/// splice onto, and silently dropped the body. Python:
/// `Can't extend terminal FOO - it is abstract.` lark-rs used to build.
#[test]
fn rc2d_extend_abstract_declared_terminal_rejected() {
    let g = "%declare FOO\n%extend FOO: \"x\"\nstart: FOO\n";
    assert_build_rejected(g, ParserAlgorithm::Lalr, LexerType::Contextual, "RC2d");
}

/// RC2d-neg (#299, NEGATIVE CONTROL). A normal `%extend` of a *concrete* terminal
/// (one with a pattern) must still work — Python accepts.
#[test]
fn rc2d_neg_extend_concrete_terminal_accepted() {
    let g = "BAR: \"a\"\n%extend BAR: \"b\"\nstart: BAR\n";
    assert_build_accepted(g, ParserAlgorithm::Lalr, LexerType::Contextual, "RC2d-neg");
}

/// RC3 (KNOWN — not a fresh find). Two sibling optional-bracket terminals collide
/// into a duplicate production. Python: `GrammarError: Rules defined twice ...
/// (colliding expansion of optionals)`. lark-rs accepts. This is the
/// `maybe_placeholders=false` colliding-optional parity gap of **#252**, already
/// fixed by the merged **PR #259** (which oracle-checks `[A] [A]` explicitly, test
/// `test_literal_optional_pair_collides`). Counted as a known-issue duplicate, not
/// a fresh find. (Distinct from #258, which is the mp=true case where both engines
/// agree.) Live guard since #385 (RC XFAIL burndown #282): the #252/#259
/// sibling-optional collision fix is on the baseline, so lark-rs now correctly
/// rejects this grammar and the test runs by default.
#[test]
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
    // Assert the *kind*, not just `is_err()`: a build that failed for an unrelated
    // reason (duplicate definition, broken import) must not pass this guard. The audit
    // shadow surfaces the masked reduce/reduce, exactly Python's `Reduce/Reduce collision`.
    let r = build(g, ParserAlgorithm::Lalr, LexerType::Contextual, false);
    assert_reduce_reduce_conflict(&r, "RC7");
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
            // Every reject cell in this direct family was verified against the real
            // build to reject specifically as a reduce/reduce `Conflict` (grounded
            // 2026-06-23) — none reject via an unrelated mechanism — so we assert the
            // *kind*, not just `is_err()`. A future cell that rejects by a different
            // (still-Python-matching) mechanism must NOT be added here; pin it with a
            // bare `is_err()` and a comment naming its mechanism instead (see the
            // import family's `straddle` note for the precedent).
            assert_reduce_reduce_conflict(&r, &format!("RC7 differential `{name}`"));
        } else {
            assert!(
                r.is_ok(),
                "RC7 differential: Python accepts `{name}`, but lark-rs rejected it: {:?}",
                r.err()
            );
        }
    }
}

/// RC7 across `%import` (#272 follow-up). The reduce/reduce over-share audit must
/// propagate through import resolution: an over-share that lives in (or straddles)
/// an imported grammar is rejected exactly as Python rejects it, and a legitimately-
/// sharing import is NOT over-rejected. Every cell is grounded directly on Python
/// Lark 1.3.1 (`Lark.open`, parser="lalr") over the same multi-file grammar fed
/// here through the in-memory `import_sources` loader path.
///
/// Pre-fix, the audit shadow was attached to the *parent* grammar only, while import
/// resolution compiled imported files through the normal (non-Python-keyed) loader
/// and copied their rule closure out without carrying the audit — so an imported
/// (or import-straddling) over-share built and parsed where Python rejects it. The
/// fix makes the shadow's own import resolution Python-keyed and carries any imported
/// `lalr_audit` rule closure into the parent shadow.
#[test]
fn rc7_reduce_reduce_differential_matches_oracle_via_import() {
    // (name, files, python_rejects?)
    let cases: &[(&str, &[(&str, &str)], bool)] = &[
        // — Python REJECTS: an RC7 over-share reached through %import. —
        // (a) the whole over-share lives in the imported file.
        (
            "imported bad: r0*|(r0)*",
            &[
                ("main.lark", "%import .bad (bad)\nstart: bad\n"),
                ("bad.lark", "bad: r0* | (r0)*\nr0: \"a\"\n"),
            ],
            true,
        ),
        // (a+) the `+` variant, imported.
        (
            "imported bad: r0+|(r0)+",
            &[
                ("main.lark", "%import .bad (bad)\nstart: bad\n"),
                ("bad.lark", "bad: r0+ | (r0)+\nr0: \"a\"\n"),
            ],
            true,
        ),
        // (b) the over-share straddles the import boundary: the shared inner rule
        //     `rr` is imported, and the two distinct-AST helpers (`x: rr*`, `y: (rr)*`)
        //     are local — so the helpers split across files.
        (
            "straddle: imported rr, local rr*|(rr)*",
            &[
                (
                    "main.lark",
                    "%import .frag (rr)\nstart: x | y\nx: rr*\ny: (rr)*\n",
                ),
                ("frag.lark", "rr: \"a\"\n"),
            ],
            true,
        ),
        // (c) the parent has its OWN over-share *plus* an unrelated import.
        (
            "parent overshare + unrelated import",
            &[
                (
                    "main.lark",
                    "%import .frag (thing)\nstart: bad | use\nbad: r0* | (r0)*\nr0: \"a\"\nuse: thing\n",
                ),
                ("frag.lark", "thing: \"t\"\n"),
            ],
            true,
        ),
        // (d) nested imports: main imports mid, mid re-imports the RC7 pattern.
        (
            "nested main->mid->bad",
            &[
                ("main.lark", "%import .mid (bad)\nstart: bad\n"),
                ("mid.lark", "%import .bad (bad)\n"),
                ("bad.lark", "bad: r0* | (r0)*\nr0: \"a\"\n"),
            ],
            true,
        ),
        // — Python ACCEPTS: the audit must NOT over-reject a legitimate import. —
        // (acc1) a single recurse helper — nothing to collide.
        (
            "imported p: r0* (single helper)",
            &[
                ("main.lark", "%import .frag (p)\nstart: p\n"),
                ("frag.lark", "p: r0*\nr0: \"a\"\n"),
            ],
            false,
        ),
        // (acc2) identical inner AST shares one helper in both engines.
        (
            "imported bad: r0*|r0* (shared)",
            &[
                ("main.lark", "%import .frag (bad)\nstart: bad\n"),
                ("frag.lark", "bad: r0* | r0*\nr0: \"a\"\n"),
            ],
            false,
        ),
        // (acc3) the guarded distinct-context case: the two split helpers sit behind
        //        distinct terminals and never reach a common state — Python accepts.
        (
            "imported guarded A r0*|B (r0)*",
            &[
                ("main.lark", "%import .frag (bad)\nstart: bad\n"),
                (
                    "frag.lark",
                    "bad: A r0* | B (r0)*\nr0: \"x\"\nA: \"a\"\nB: \"b\"\n",
                ),
            ],
            false,
        ),
        // (acc4) two distinct, guarded imported rules — non-colliding.
        (
            "import two guarded rules",
            &[
                ("main.lark", "%import .frag (p, q)\nstart: p | q\n"),
                (
                    "frag.lark",
                    "p: A r0*\nq: B s0*\nr0: \"a\"\ns0: \"b\"\nA: \"x\"\nB: \"y\"\n",
                ),
            ],
            false,
        ),
        // (acc5) legitimate sharing Python accepts, imported.
        (
            "imported a+ b | a+",
            &[
                ("main.lark", "%import .frag (bad)\nstart: bad\n"),
                ("frag.lark", "bad: a+ b | a+\na: \"a\"\nb: \"b\"\n"),
            ],
            false,
        ),
    ];
    for (name, files, rejects) in cases {
        let r = build_with_imports(files);
        if *rejects {
            // Each import reject cell was verified against the real build to reject as a
            // reduce/reduce `Conflict` (grounded 2026-06-23). NB the `straddle` cell
            // (imported `rr`, local `x: rr*` / `y: (rr)*`) also rejects as a genuine
            // reduce/reduce — the two split helpers collide on `x ->` / `y ->` at
            // `$END` (state 0) — NOT a "Rules defined twice"/duplicate-definition
            // reject, so the reduce/reduce assertion is the faithful one for every cell
            // here. (If a future import cell rejects by a different but still
            // Python-matching mechanism — e.g. a nullable-`$END` collision — pin it
            // with `is_err()` + a comment naming that mechanism, do not force it here.)
            assert_reduce_reduce_conflict(&r, &format!("RC7 import differential `{name}`"));
        } else {
            assert!(
                r.is_ok(),
                "RC7 import differential: Python accepts `{name}`, but lark-rs rejected it: {:?}",
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

// ─────────────────────────────────────────────────────────────────────────────
// #372 — `%import` overlapping interior closures duplicate a shared origin.
// ─────────────────────────────────────────────────────────────────────────────

/// #372. Two rules independently imported from the *same* bundled module whose
/// dependency closures overlap (`decorators: decorator+` shares the interior
/// `python__name`/`python__dotted_name` closure with `decorator`). Before the fix,
/// `import_rule_closure` copied the shared interior origin once *per* requested
/// rule with no cross-call dedup — the duplicated `python__name` origin became a
/// spurious reduce/reduce the build rejected in the sibling-before-owner /
/// cross-directive orders (the owner-first order already built after #343, because
/// the interior `decorator` was left unmangled and hit the requested-name guard).
///
/// Python Lark 1.3.1 builds the repro in **every** order and yields a tree-identical
/// parse (verified with `maybe_placeholders=False`). The fix dedups the interior
/// rule-copy loop (mirroring the terminal-copy guard), so lark-rs must now build and
/// parse identically in all three orders. The expected tree below is the
/// Python-oracle shape (in lark-rs's `Display` rendering) for input `@foo\n@bar\n`.
#[test]
fn rc_import_overlapping_interior_closure_builds_all_orders() {
    // Three directives that all import the same overlapping closures from `python`.
    let grammars = [
        // sibling-before-owner (the regressing order before the fix)
        "start: decorators\n%import python (decorator, decorators)\n%ignore \" \"\n",
        // owner-first (already built after #343 — kept as a negative control)
        "start: decorators\n%import python (decorators, decorator)\n%ignore \" \"\n",
        // cross-directive (same root cause across two separate %import lines)
        "start: decorators\n%import python (decorator)\n%import python (decorators)\n%ignore \" \"\n",
    ];
    // Python-oracle tree (maybe_placeholders=False) for `@foo\n@bar\n`, rendered in
    // lark-rs's Display form. Identical across all three orders.
    let expected = "Tree(start, [Tree(decorators, [\
        Tree(decorator, [Tree(python__dotted_name, [Tree(python__name, [Token(python__NAME, \"foo\")])])]), \
        Tree(decorator, [Tree(python__dotted_name, [Tree(python__name, [Token(python__NAME, \"bar\")])])])])])";

    for g in grammars {
        let lark = Lark::new(
            g,
            LarkOptions {
                parser: ParserAlgorithm::Lalr,
                lexer: LexerType::Contextual,
                ambiguity: Ambiguity::Resolve,
                start: vec!["start".to_string()],
                maybe_placeholders: false,
                ..Default::default()
            },
        )
        .unwrap_or_else(|e| {
            panic!("#372: Python builds this grammar in every order; lark-rs rejected:\n{g}\n -> {e:?}")
        });
        let tree = lark
            .parse("@foo\n@bar\n")
            .unwrap_or_else(|e| panic!("#372: parse failed for grammar:\n{g}\n -> {e:?}"));
        assert_eq!(
            tree.to_string(),
            expected,
            "#372: tree mismatch vs Python oracle for grammar:\n{g}"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// #428 — a user rule colliding with a mangled interior import origin.
// ─────────────────────────────────────────────────────────────────────────────

/// #428 (RC1-class, import surface). A user-authored rule whose name equals the
/// **mangled interior origin** of an imported closure (`python__name`, the
/// prefix-mangled `name` that `%import python (decorator)` pulls in transitively)
/// collides. Python Lark 1.3.1 raises `GrammarError: Rule 'python__name' defined
/// more than once` in **both** definition orders (rule-before-import and
/// import-before-rule — verified against the oracle). lark-rs used to silently
/// MERGE the user rule with the import-copied origin and build (the
/// over-permissiveness ADR-0017's corollary forbids).
///
/// This is distinct from the *requested*-name collision (a user `decorator` beside
/// `%import python (decorator)`), which the import-final-name seeding of the
/// single-definition ledger (#270) already rejects: here the collision is on an
/// *interior* origin that no `%import` directive names, so it never reaches that
/// ledger. It is also distinct from #372's import-vs-import interior dedup
/// (negative control below), which must keep building.
#[test]
fn rc1_user_rule_vs_mangled_import_origin_rejected() {
    // The exact repro from the issue: the user rule precedes the import.
    let rule_before_import =
        "start: decorator python__name\npython__name: \"z\"\n%import python (decorator)\n%ignore \" \"\n";
    assert_duplicate_definition_rejected(
        rule_before_import,
        ParserAlgorithm::Lalr,
        LexerType::Contextual,
        "python__name",
        "#428 rule-before-import",
    );

    // The mirror order: the import copies the interior origin first, then the user
    // rule is staged. Python rejects this order too, with the same message.
    let import_before_rule =
        "start: decorator python__name\n%import python (decorator)\npython__name: \"z\"\n%ignore \" \"\n";
    assert_duplicate_definition_rejected(
        import_before_rule,
        ParserAlgorithm::Lalr,
        LexerType::Contextual,
        "python__name",
        "#428 import-before-rule",
    );
}

/// #428 NEGATIVE CONTROL — must not regress #372. Two rules independently imported
/// from the same bundled module whose dependency closures overlap (`decorator` and
/// `decorators` share the interior `python__name`): the shared interior origin is
/// copied **once** (the `imported_origins` dedup), so this is *not* a user-vs-import
/// collision and Python builds it. The #428 rejection keys on *import-copied vs.
/// claimed* origins precisely so it fires for the collision above without dropping a
/// legitimate import here.
#[test]
fn rc1_import_vs_import_interior_origin_still_builds() {
    let g = "start: decorators\n%import python (decorator, decorators)\n%ignore \" \"\n";
    assert_build_accepted(
        g,
        ParserAlgorithm::Lalr,
        LexerType::Contextual,
        "#428 negative control (import-vs-import interior dedup, #372)",
    );
}

/// #428 — a *surviving* import alias that lands on another import's mangled interior
/// origin is a genuine collision and is rejected. `mod.lark` defines `outer: inner`
/// (interior `inner` mangles to `mod__inner` under `%import .mod (outer)`); the alias
/// `%import .mod.thing -> mod__inner` registers a *second* definition of `mod__inner`.
/// Python Lark 1.3.1: `GrammarError: Rule 'mod__inner' defined more than once`. The
/// #428 discriminator includes surviving import final names, so this rejects exactly
/// like a user rule of the same name.
#[test]
fn rc1_surviving_alias_vs_import_origin_rejected() {
    let files = [
        ("mod.lark", "outer: inner\ninner: \"i\"\nthing: \"t\"\n"),
        (
            "main.lark",
            "start: outer mod__inner\n%import .mod.thing -> mod__inner\n%import .mod (outer)\n%ignore \" \"\n",
        ),
    ];
    match build_with_imports(&files) {
        Err(LarkError::Grammar(GrammarError::Other { msg })) => assert!(
            msg.contains("Rule 'mod__inner' defined more than once"),
            "#428 surviving-alias collision: rejected, but not as the duplicate-definition \
             error (expected `Rule 'mod__inner' defined more than once`):\n{msg}"
        ),
        Err(e) => panic!(
            "#428 surviving-alias collision: expected the duplicate-definition GrammarError, got: {e:?}"
        ),
        Ok(_) => panic!(
            "#428 surviving-alias collision: Python rejects this; lark-rs accepted it"
        ),
    }
}

/// #428 NEGATIVE CONTROL (#388, last-alias-wins). A *dropped* import alias whose name
/// happens to have the `<module>__interior` mangle shape must NOT false-reject. Here
/// `%import .mod.thing -> mod__inner` is shadowed by a later `%import .mod.thing ->
/// other` (last-alias-wins, #388), so `mod__inner` is *never defined* — and the
/// interior `inner → mod__inner` from `%import .mod (outer)` therefore does not
/// collide. Python Lark 1.3.1 BUILDS and parses this; the #428 check must key on the
/// *surviving* final names only (`claimed_rule_names`), not every reserved name, or it
/// regresses a grammar the oracle accepts.
#[test]
fn rc1_dropped_alias_of_mangled_shape_still_builds() {
    let files = [
        ("mod.lark", "outer: inner\ninner: \"i\"\nthing: \"t\"\n"),
        (
            "main.lark",
            "start: outer other\n%import .mod.thing -> mod__inner\n%import .mod.thing -> other\n%import .mod (outer)\n%ignore \" \"\n",
        ),
    ];
    let lark = build_with_imports(&files).unwrap_or_else(|e| {
        panic!("#428 dropped-alias: Python builds this; lark-rs rejected: {e:?}")
    });
    // Tree-identical to the Python oracle (maybe_placeholders default) for input `i t`.
    let tree = lark
        .parse("i t")
        .unwrap_or_else(|e| panic!("#428 dropped-alias: parse failed: {e:?}"));
    assert_eq!(
        tree.to_string(),
        "Tree(start, [Tree(outer, [Tree(mod__inner, [])]), Tree(other, [])])",
        "#428 dropped-alias: tree mismatch vs Python oracle"
    );
}
