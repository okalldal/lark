//! Bug-bounty findings, round 3 (h3) — failing oracle tests (XFAIL).
//!
//! Rounds 1 (`test_bounty_findings.rs`, RC series) and 2 (`test_bounty_findings_h2.rs`,
//! N series) harvested the missing-validation-gate layer, the lexer terminal-ordering
//! bugs, config legality, and char-vs-byte positions — almost all since fixed. Round 3
//! retargeted the *deeper* surfaces those rounds declared clean or never reached:
//! grammar-loader robustness (panics, template-parameter validation, terminal aliasing),
//! the Python-`re` dialect of **character classes / quantifiers / escapes** (not just
//! anchors), the Earley default-lexer resolution, `Tree.meta`, and a deterministic
//! Earley dynamic-lexer scan pathology.
//!
//! Each test asserts the **Python Lark 1.3.1** (oracle) behavior. This file is an XFAIL
//! catalog: every test below is `#[ignore]`d and fails today. Drop a test's `#[ignore]`
//! when its bug is fixed to turn it into a permanent regression guard. Run the still-open
//! XFAILs with:
//!
//!     cargo test --test test_bounty_findings_h3 -- --ignored
//!
//! The dynamic-lexer scaling gate (H11) additionally needs the deterministic work
//! counters, so run it with:
//!
//!     cargo test --features perf-counters --test test_bounty_findings_h3 -- --ignored
//!
//! The standalone expand1 lone-`None` find (H12) is an executable XFAIL too, but the
//! baked runtime is only reachable via the standalone module's internal harness, so it
//! lives in `src/standalone/mod.rs` (`standalone_expand1_lone_none_collapses_like_core`,
//! run with `cargo test --lib standalone_expand1_lone_none -- --ignored`).
//!
//! Baseline SHA: afa20a07f81d0599a9b6705aae881fc8c8223ccc. Catalog with repros,
//! severity, blast radius, and fix contracts: `docs/BOUNTY_FINDINGS_H3.md`.
//!
//! NONE of these reduce to a round-1/2 root cause (RC1–RC10, N1–N10, V1–V4) or the
//! open known-issue set (#272, #275, #281, #286, #288, #289, #299, #302, #304, #208).
//! The IDs below are the round-3 "H" series.

use lark_rs::{Child, Lark, LarkOptions, LexerType, ParseTree, ParserAlgorithm};

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
// Grammar-loader robustness & validation gaps.
// ─────────────────────────────────────────────────────────────────────────────

/// H1 (CRITICAL). A start symbol with no rule definition. Python:
/// `GrammarError: Using an undefined rule: NonTerminal('start')`. lark-rs **panics**
/// — `lower()` does `symbols.id(start).expect("start symbol interned")`
/// (`src/grammar/intern.rs:376`) with no prior defined-rule check, so any grammar
/// whose start (default or custom) is undefined aborts the process instead of
/// returning a clean error — a robustness/DoS hole on attacker- or user-supplied
/// grammars. Reproduces on lalr (basic+contextual) and earley.
#[test]
#[ignore = "XFAIL (bounty H1): undefined start symbol panics in lower() instead of GrammarError"]
fn h1_undefined_start_rejected_not_panicked() {
    let g = "foo: \"a\"\n".to_string();
    let o = opts(ParserAlgorithm::Lalr, LexerType::Contextual);
    // Must be a clean Err, never a panic.
    let result =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| Lark::new(&g, o).is_err()));
    assert!(
        matches!(result, Ok(true)),
        "H1: undefined start must be a clean GrammarError, not a panic (got {result:?})"
    );
}

/// H2a (HIGH). A template with a duplicate parameter name. Python:
/// `GrammarError: Duplicate Template Parameter x (in template foo)`. lark-rs has no
/// analogue of Python's `GrammarDefinition.validate()` template-parameter pass, so it
/// builds the malformed template silently.
#[test]
#[ignore = "XFAIL (bounty H2): duplicate template parameter not rejected"]
fn h2a_duplicate_template_param_rejected() {
    let g = "foo{x,x}: x\nstart: foo{\"a\",\"b\"}\n";
    assert_build_rejected(g, opts(ParserAlgorithm::Lalr, LexerType::Contextual), "H2a");
}

/// H2b (HIGH). A template parameter whose name shadows a defined rule. Python:
/// `GrammarError: Template Parameter conflicts with rule x (in template foo)`.
/// lark-rs builds it **and mis-parses**: input `"a"` yields `start(foo())` (the
/// literal arg substitutes for the param, shadowing rule `x`) where Python rejects the
/// grammar outright. Same missing-`validate()` root cause as H2a, second surface.
#[test]
#[ignore = "XFAIL (bounty H2): template parameter shadowing a rule not rejected"]
fn h2b_template_param_shadows_rule_rejected() {
    let g = "x: \"z\"\nfoo{x}: x\nstart: foo{\"a\"}\n";
    assert_build_rejected(g, opts(ParserAlgorithm::Lalr, LexerType::Contextual), "H2b");
}

/// H3 (HIGH). An alias (`->`) inside a *terminal* definition. Python:
/// `GrammarError: Aliasing not allowed in terminals (You used -> in the wrong place)`.
/// lark-rs accepts it and silently drops the alias (`"a"` parses to
/// `start(Token(A,"a"))`). Distinct from RC4a/b/c (aliases on *rules* / inside groups);
/// this is the terminal-definition surface with its own Python check.
#[test]
#[ignore = "XFAIL (bounty H3): alias inside a terminal definition not rejected"]
fn h3_alias_in_terminal_rejected() {
    let g = "A: \"a\" -> foo\nstart: A\n";
    assert_build_rejected(g, opts(ParserAlgorithm::Lalr, LexerType::Contextual), "H3");
}

// ─────────────────────────────────────────────────────────────────────────────
// Earley default-lexer resolution.
// ─────────────────────────────────────────────────────────────────────────────

/// H4 (HIGH). `lexer="auto"` (the *default*) with `parser="earley"` must resolve to
/// the **dynamic** (parse-directed) lexer when there is no postlex, exactly as Python
/// Lark does (`lark/lark.py`: auto→dynamic for earley, basic only with a postlex).
/// lark-rs's `build_earley` (`src/parsers/mod.rs`) has a catch-all arm that routes
/// `LexerType::Auto` to the **basic** lexer, so the common `Lark(g, parser="earley")`
/// idiom silently uses a different lexer than Python — changing accept/reject (here)
/// and tree shape. The whole Earley suite masks it by forcing an explicit lexer.
#[test]
#[ignore = "XFAIL (bounty H4): earley+auto routes to the basic lexer instead of dynamic"]
fn h4_earley_auto_lexer_is_dynamic() {
    let g = "start: \"print\" NAME\nNAME: /[a-z]+/\n%ignore \" \"\n";
    let lark = Lark::new(g, opts(ParserAlgorithm::Earley, LexerType::Auto)).expect("H4: builds");
    // The dynamic lexer is parse-directed: "print" then NAME="x". The basic lexer
    // maximal-munches "printx" as one NAME and the parse fails.
    assert!(
        lark.parse("printx").is_ok(),
        "H4: earley+auto must accept \"printx\" via the dynamic lexer (Python does); \
         lark-rs routed to the basic lexer and rejected it"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Python-`re` dialect — character classes, quantifiers, escapes (beyond anchors).
// The anchor dialect (\b \B \Z) is the parked #275; these are distinct constructs.
// ─────────────────────────────────────────────────────────────────────────────

/// H5a (HIGH). POSIX character-class syntax `[[:alpha:]]`. Python `re` has **no** POSIX
/// classes: it reads `[[:alpha:]]` as the class `[[:alph a]` (members `[ : a l p h`)
/// followed by a **literal** `]`, so `/[[:alpha:]]/` matches the 2-char string `"a]"`.
/// The Rust `regex` crate **does** support POSIX classes, so lark-rs matches a single
/// alphabetic char and rejects the trailing `]`. `normalize_python_escapes`
/// (`src/grammar/terminal.rs`) rewrites only `\<`/`\>`, never the char-class dialect.
/// Fix contract is a fork (match Python's literal reading vs reject with a categorized
/// error) — see catalog; this asserts the ADR-0017 oracle default (match Python).
#[test]
#[ignore = "XFAIL (bounty H5): POSIX class [[:alpha:]] uses Rust-regex semantics, not Python re"]
fn h5a_posix_class_python_re_dialect() {
    let g = "start: A\nA: /[[:alpha:]]/\n";
    let lark =
        Lark::new(g, opts(ParserAlgorithm::Lalr, LexerType::Contextual)).expect("H5a: builds");
    assert!(
        lark.parse("a]").is_ok(),
        "H5a: Python `re` matches the 2-char token \"a]\" ([[:alph a] then literal ]); \
         lark-rs used the Rust POSIX class and rejected it"
    );
}

/// H5b (HIGH). Character-class set operations `[a-c&&b]` (intersection), `--`, `~~`.
/// Python `re` reads `&&` as **literal** chars, so `[a-c&&b]` is the class
/// `{a,b,c,&}` and matches `"a"`. The Rust `regex` crate reads `&&` as set
/// intersection (→ `{b}`), so lark-rs **rejects** `"a"` that Python accepts. Same
/// char-class-dialect root cause as H5a, second surface.
#[test]
#[ignore = "XFAIL (bounty H5): char-class set-op [a-c&&b] uses Rust-regex intersection, not Python literal"]
fn h5b_class_setop_python_re_dialect() {
    let g = "start: A\nA: /[a-c&&b]/\n";
    let lark =
        Lark::new(g, opts(ParserAlgorithm::Lalr, LexerType::Contextual)).expect("H5b: builds");
    assert!(
        lark.parse("a").is_ok(),
        "H5b: Python `re` treats && as literal so [a-c&&b] matches \"a\"; \
         lark-rs used set-intersection and rejected it"
    );
}

/// H6 (HIGH). A possessive quantifier `a++` (also `*+`, `?+`). Python `re` treats it as
/// possessive — `re.match("a++a","aaa")` is `None` (no give-back), so the parse
/// rejects. The Rust `regex` crate parses `a++` as nested repetition `(a+)+` (greedy,
/// backtracking-allowed) and **matches** `"aaa"`, so lark-rs silently accepts. The
/// documented contract (`docs/LOOKAROUND_SCOPE.md`) is a *categorized refusal* of
/// backtracking-only syntax; the bug is that possessives slip past the refusal seam
/// (the regex crate accepts the syntax with a different meaning) and silently
/// mis-match. Acceptable fixes: refuse (categorized) or match Python — never silently
/// accept the greedy reading.
#[test]
#[ignore = "XFAIL (bounty H6): possessive quantifier a++ silently reinterpreted as greedy (a+)+"]
fn h6_possessive_not_silently_greedy() {
    let g = "start: A\nA: /a++a/\n";
    match Lark::new(g, opts(ParserAlgorithm::Lalr, LexerType::Contextual)) {
        // A categorized build refusal is acceptable (documented non-goal).
        Err(_) => {}
        Ok(l) => assert!(
            l.parse("aaa").is_err(),
            "H6: possessive a++a must not match \"aaa\" — Python rejects it; \
             lark-rs matched it as greedy (a+)+a"
        ),
    }
}

/// H7 (MEDIUM). A stacked quantifier `/a{2}{3}/` (a repeat applied directly to a repeat).
/// Python `re` (via `sre_parse`) raises "multiple repeat", so Lark build-errors
/// (`Cannot compile token A`). The Rust `regex` crate accepts stacked quantifiers, so
/// lark-rs builds and lexes the terminal. Per ADR-0017, being more permissive than the
/// oracle is a bug. Distinct dialect axis from H5 (quantifier shape, not char class).
#[test]
#[ignore = "XFAIL (bounty H7): stacked quantifier /a{2}{3}/ accepted; Python rejects 'multiple repeat'"]
fn h7_stacked_quantifier_rejected() {
    let g = "start: A\nA: /a{2}{3}/\n";
    assert_build_rejected(g, opts(ParserAlgorithm::Lalr, LexerType::Contextual), "H7");
}

/// H8 (MEDIUM). An inline comment group `(?#c)`. Python `re` drops it, so `/a(?#c)b/`
/// matches `"ab"`. lark-rs **build-errors with a raw, uncategorized regex-crate leak**
/// (`Invalid regex pattern '…': regex parse error: … unrecognized flag`) — the
/// RC6/N4-class symptom (a raw engine error escaping the refusal taxonomy) on a fresh
/// construct. The minimum fix is to stop leaking a raw error; the oracle default is to
/// support it (strip the comment) and match Python. Distinct from RC6 (`\b`).
#[test]
#[ignore = "XFAIL (bounty H8): (?#comment) leaks a raw uncategorized regex error; Python accepts it"]
fn h8_inline_comment_group_supported() {
    let g = "start: A\nA: /a(?#c)b/\n";
    let lark = Lark::new(g, opts(ParserAlgorithm::Lalr, LexerType::Contextual));
    assert!(
        lark.is_ok(),
        "H8: (?#...) is a Python `re` comment (Python accepts /a(?#c)b/); \
         lark-rs leaked a raw regex-parse error at build"
    );
    assert!(
        lark.unwrap().parse("ab").is_ok(),
        "H8: /a(?#c)b/ must match \"ab\" like Python"
    );
}

/// H9a (MEDIUM). An octal escape `\101`. Python `re` reads it as the octal char
/// `0o101 == 'A'`, so `/\101/` matches `"A"`. lark-rs **rejects it at build AND
/// mis-categorizes** it as `LookaroundScope` "backtracking-only syntax (…
/// backreference …)" — the Rust regex crate reads `\1` as a backreference, and the
/// `route_fancy_only_terminal` catch-all over-claims the category. `\101`/`\0` are
/// plain octal literals, neither lookaround nor backref. Distinct from N10/#275 (those
/// are the `\b`/`\B`/`\Z` *anchor* policy fork; these are Python-accepted regular
/// escapes that should simply be translated and matched).
#[test]
#[ignore = "XFAIL (bounty H9): octal escape \\101 rejected + mis-categorized as backtracking lookaround"]
fn h9a_octal_escape_supported() {
    let g = "start: A\nA: /\\101/\n";
    let lark = Lark::new(g, opts(ParserAlgorithm::Lalr, LexerType::Contextual));
    assert!(
        lark.is_ok(),
        "H9a: \\101 is a Python octal escape ('A'); lark-rs rejected it (mis-categorized as lookaround)"
    );
    assert!(
        lark.unwrap().parse("A").is_ok(),
        "H9a: /\\101/ must match \"A\" like Python"
    );
}

/// H9b (MEDIUM). Backspace inside a character class `[\b]`. Python `re` reads `\b`
/// *inside a class* as the backspace char `\x08` (only outside a class is it a word
/// boundary), so `/[\b]/` matches `"\x08"`. The Rust regex crate rejects `\b` in a
/// class, and lark-rs mis-categorizes it as `LookaroundScope` "backtracking-only".
/// Same route over-claim as H9a; `[\b]` is a regular, Python-accepted construct.
#[test]
#[ignore = "XFAIL (bounty H9): [\\b] (backspace-in-class) rejected + mis-categorized as lookaround"]
fn h9b_backspace_in_class_supported() {
    let g = "start: A\nA: /[\\b]/\n";
    let lark = Lark::new(g, opts(ParserAlgorithm::Lalr, LexerType::Contextual));
    assert!(
        lark.is_ok(),
        "H9b: [\\b] is backspace inside a class in Python `re`; lark-rs rejected it (mis-categorized)"
    );
    assert!(
        lark.unwrap().parse("\u{0008}").is_ok(),
        "H9b: /[\\b]/ must match the backspace char like Python"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Tree.meta parity.
// ─────────────────────────────────────────────────────────────────────────────

/// H10 (MEDIUM). `Tree.meta.empty` for a node whose children are all positionless. In
/// Python, `Meta.empty` defaults `True` and `PropagatePositions` clears it only when a
/// position-bearing first/last child is found (skipping empty subtrees) — so for
/// `start: empty` / `empty:` on `""`, both `start.meta.empty` and the inner subtree's
/// are `True`. lark-rs's `Meta::from_children` (`src/tree.rs`) sets `empty=true` **only
/// when `children.is_empty()`**, so the `start` node (one positionless child) reports
/// `empty=false`. The position *spans* are correct (the bug is isolated to the flag);
/// #307 fixed `Token` char-vs-byte positions but never touched this `Meta` field.
#[test]
#[ignore = "XFAIL (bounty H10): Tree.meta.empty is false for a node with only positionless children"]
fn h10_meta_empty_for_positionless_children() {
    let mut o = opts(ParserAlgorithm::Lalr, LexerType::Contextual);
    o.propagate_positions = true;
    let lark = Lark::new("start: empty\nempty:\n", o).expect("H10: builds");
    let ParseTree::Tree(t) = lark.parse("").expect("H10: parses") else {
        panic!("H10: expected a tree");
    };
    assert!(
        t.meta.empty,
        "H10: start has only a positionless child, so meta.empty must be true (Python)"
    );
    if let Some(Child::Tree(inner)) = t.children.first() {
        assert!(
            inner.meta.empty,
            "H10: the empty subtree's meta.empty must be true"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Deterministic resource bounds.
// ─────────────────────────────────────────────────────────────────────────────

/// H11 (HIGH, perf). The Earley **dynamic** lexer (`DynamicMatcher`,
/// `src/lexer/dynamic.rs`) matches a plain terminal with `Regex::find_at`, which is an
/// **unanchored forward search** (it scans forward then checks the match starts at
/// `pos`). A sparse terminal in the per-position scan set forward-scans O(remaining
/// input) at every position, so total lexing is **O(n²)**. Python's dynamic matcher
/// uses `re.Pattern.match(text, index)`, which is **anchored at `index`** — O(n) total.
/// The `\G` start-of-search anchor that fixed the combined `Scanner` (#104) was never
/// ported to `DynamicMatcher`, and `test_lexer_scaling.rs` gates only basic/contextual.
/// Measured deterministically via the `lexer_scan_steps` work counter (no wall-clock):
/// scan/byte grows 67 → 259 → 1027 → 4099 across n=64→4096 (linear in n ⇒ O(n²) total).
#[cfg(feature = "perf-counters")]
#[test]
#[ignore = "XFAIL (bounty H11): Earley dynamic lexer forward-scans (O(n^2)); Python's anchored match is O(n)"]
fn h11_dynamic_lexer_scan_is_flat_per_byte() {
    use lark_rs::perf;
    let g = "start: (WORD | STR)+\nWORD: /[a-z]+/\nSTR: /\\#[^\\#]*\\#/\n%ignore \" \"\n";
    let lark =
        Lark::new(g, opts(ParserAlgorithm::Earley, LexerType::Dynamic)).expect("H11: builds");

    let words = |n: usize| -> String {
        let mut s = vec!["a"; n].join(" ");
        s.push_str(" #z#");
        s
    };
    let mut per_byte: Vec<(usize, f64)> = Vec::new();
    for &n in &[64usize, 256, 1024, 4096] {
        let input = words(n);
        perf::reset();
        lark.parse(&input).expect("H11: parses");
        let scan = perf::lexer_scan_steps();
        per_byte.push((input.len(), scan as f64 / input.len() as f64));
    }
    let first = per_byte.first().unwrap().1;
    let last = per_byte.last().unwrap().1;
    assert!(
        last <= first * 1.6,
        "H11: dynamic-lexer scan is not flat per byte — grew {first:.1} → {last:.1} scan/byte \
         (rows {per_byte:?}); DynamicMatcher::match_end_at forward-scans (O(n^2)) where \
         Python's anchored re.match is O(n)"
    );
}
