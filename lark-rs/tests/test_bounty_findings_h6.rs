//! Bug-bounty findings, round 6 (h6) — failing oracle tests (XFAIL).
//!
//! Rounds 1–5 (`test_bounty_findings.rs` RC, `_h2.rs` N, `_h3.rs` H, `_h4.rs` H4-*,
//! `_h5.rs` H5-*) harvested the validation-gate layer, the lexer terminal-ordering
//! bugs, four+ waves of Python-`re` regex-dialect divergences, config legality,
//! char-vs-byte positions, error/`ParseError` parity, import-closure mangling,
//! tree-shaping lone-`None`, the standalone bake, and the bindings surface. Round 6
//! retargeted the corners those rounds either declared clean or never reached:
//!
//!   * H6-1 — terminal **value-length tiebreak** measures the *normalized* pattern
//!     (`\<\<\<` stored as `<<<`), not Python's *raw source* length, flipping which of
//!     two equal-priority/equal-width terminals wins a span (distinct from N2/#268's
//!     flag-wrapper strip and RC5/#268's `max_width`).
//!   * H6-2 — the `{,m}` quantifier (Python's `{0,m}`) is **rejected and
//!     mis-categorized** as an OutOfScope lookaround/backtracking refusal, where Python
//!     `re`/Lark accept it (opposite polarity to the H6–H9/#375 dialect *narrowings*).
//!   * H6-3 — two **nullable alternatives differing only by an alias** (`p: "a"? -> al1
//!     | "b"? -> al2`) produce a spurious LALR reduce/reduce rejection; Python's LALR
//!     and lark-rs's own Earley accept it (opposite direction to RC7/#272's
//!     *under*-reporting audit).
//!   * H6-4 — a **bare nested optional under repetition** (`[[A]]* C`) mints a recurse
//!     helper with twin byte-identical empty arms → a spurious LALR reduce/reduce; the
//!     non-nested forms `[A]* C` / `([A])* C` build fine, and Earley accepts `[[A]]*`.
//!   * H6-5 — `Tree.meta` (with `propagate_positions`) spans only the **post-filter**
//!     children, so a rule wrapped by filtered punctuation (`"(" A ")"`) reports a span
//!     that omits the parens; Python derives meta from the *pre-filter* children
//!     (distinct from N8's byte-vs-char `*_pos` and H10/#337's positionless-empty flag).
//!   * H6-6 — a string literal whose source is byte-identical to a named **regex**
//!     terminal (`"ab"` beside `AB: /ab/`) is wrongly **unified** onto that terminal, so
//!     the literal is typed `AB` and kept instead of being a distinct, filtered
//!     `__ANON_*` (distinct from H4-9/#347's Str-vs-Str *alternation-arm* dedup; this is
//!     the Re-vs-Str *interning* merge in `patterns_equivalent`).
//!   * H6-7 — `(X|X) (X|X) … (X|X)` (k duplicate-arm inline groups) makes
//!     `compile_expansion` materialize a `2^k` cartesian product before its single
//!     end-of-function dedup; Python's `SimplifyRule_Visitor` dedups each group's arms
//!     first and builds in linear time to one rule (distinct from N9's `~n..m` size and
//!     #252's `~n` repeat path, where Python *also* blows up).
//!   * H6-8 — rule/terminal names with **no alphabetic char** (`_`, `__`, `_9`) are
//!     accepted; Python's grammar lexer requires `[a-z]`/`[A-Z]` (distinct from
//!     H5-2/#361's `__foo`, which *has* a letter).
//!
//! Each test asserts the **Python Lark 1.3.1** (oracle) behavior. This file is an XFAIL
//! catalog: every test below is `#[ignore]`d and fails today. Drop a test's `#[ignore]`
//! when its bug is fixed to turn it into a permanent regression guard. Run the still-open
//! XFAILs with:
//!
//!     cargo test --test test_bounty_findings_h6 -- --ignored
//!
//! Baseline SHA: b4ab6cd578b1bd334f7fddc79781202fc66bba4a. Catalog with repros, severity,
//! blast radius, fix contracts, the provisional bindings findings (C-API
//! `maybe_placeholders` default; error-hierarchy collapse) and the un-minimized Earley
//! token-filter lead, plus the dedup against rounds 1–5: `docs/BOUNTY_FINDINGS_H6.md`.
//!
//! NONE of these reduce to a round-1..5 root cause (RC1–RC10, N1–N10, V1–V4, H1–H12,
//! P1–P2, H4-1…H4-12, H5-1…H5-9) or the open known-issue set. Adjacencies are noted at
//! each test.

use lark_rs::{Child, Lark, LarkOptions, LexerType, ParseTree, ParserAlgorithm};

fn opts(parser: ParserAlgorithm, lexer: LexerType) -> LarkOptions {
    LarkOptions {
        parser,
        lexer,
        start: vec!["start".to_string()],
        ..Default::default()
    }
}

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

/// Count tokens (pre-order) in the tree — for asserting filtering behaviour.
fn token_count(t: &ParseTree) -> usize {
    fn walk(c: &Child) -> usize {
        match c {
            Child::Token(_) => 1,
            Child::Tree(tr) => tr.children.iter().map(walk).sum(),
            Child::None => 0,
        }
    }
    match t {
        ParseTree::Token(_) => 1,
        ParseTree::Tree(tr) => tr.children.iter().map(walk).sum(),
        ParseTree::None => 0,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Lexer: terminal ranking / dialect.
// ─────────────────────────────────────────────────────────────────────────────

/// H6-1 (MEDIUM, lexer ranking). Python's terminal sort key is
/// `(-priority, -max_width, -len(pattern.value), name)`, where `pattern.value` is the
/// **verbatim source**. lark-rs's `Pattern::raw_value_len` (`grammar/terminal.rs`)
/// measures the *normalized* stored pattern: `PatternRe::new` runs
/// `normalize_python_escapes`, which rewrites `\<\<\<` → `<<<` (len 6 → 3) before
/// storage and discards the raw source. So for two equal-priority, equal-`max_width`
/// terminals `A: /\<\<\</` (Python value-len 6) and `B: /<<<|q/` (value-len 5), Python
/// ranks `A` first and emits token `A` on `"<<<"`, while lark-rs sees `A`=3 < `B`=5,
/// ranks `B` first, and emits `B`. Distinct from N2/#268 (the *flag-wrapper* leak,
/// fixed by `strip_whole_pattern_flag_wrapper`) and RC5/#268 (`max_width`, the 2nd key);
/// this is the 3rd key (`raw_value_len`) and a different lost-length source (body-escape
/// normalization). Fix (#399): `PatternRe` retains the pre-normalization `raw` source and
/// `raw_value_len` measures that, so `raw_value_len() == len(pattern.value)`.
#[test] // FIXED (#399): the value-length tiebreak measures the verbatim source, not the
        // normalized pattern.
fn h6_1_value_length_tiebreak_uses_raw_source() {
    let g = "start: A | B\nA: /\\<\\<\\</\nB: /<<<|q/\n";
    let lark = Lark::new(g, opts(ParserAlgorithm::Lalr, LexerType::Basic)).expect("H6-1: builds");
    let tree = lark.parse("<<<").expect("H6-1: parses");
    assert_eq!(
        first_token_type(&tree).as_deref(),
        Some("A"),
        "H6-1: Python's value-length tiebreak (source len 6 > 5) selects terminal A; \
         lark-rs measured the normalized pattern (len 3) and selected B"
    );
}

/// H6-1, second trigger (the `(?#…)` comment-strip length-loss source). The issue calls
/// for the comment-strip case to reproduce/pass identically to the `\<\<\<` body-escape
/// case: `normalize_python_escapes` drops a `(?#…)` comment span before storage, so
/// `ZZ: /ab(?#cccc)/` (verbatim source len 10) normalizes to `ab` (len 2) with the same
/// `max_width` 2 as `B: /ab/` (len 2). Python ranks by `(-priority, -max_width,
/// -len(pattern.value), name)`: equal priority and width, ZZ's *longer raw value* wins
/// the 3rd key *before* the name sort, so Python emits `ZZ` on `"ab"`. A `raw_value_len`
/// that measured the normalized pattern would tie both at 2 and fall through to the name
/// sort (`B` < `ZZ` → wrong `B`). Names chosen so the name tiebreak disagrees with the
/// value-length tiebreak, isolating the 3rd key.
#[test] // FIXED (#399): comment-strip body normalization no longer changes a terminal's rank.
fn h6_1_value_length_tiebreak_uses_raw_source_comment_strip() {
    let g = "start: ZZ | B\nZZ: /ab(?#cccc)/\nB: /ab/\n";
    let lark = Lark::new(g, opts(ParserAlgorithm::Lalr, LexerType::Basic)).expect("H6-1c: builds");
    let tree = lark.parse("ab").expect("H6-1c: parses");
    assert_eq!(
        first_token_type(&tree).as_deref(),
        Some("ZZ"),
        "H6-1c: ZZ's verbatim source (len 10, comment stripped to `ab`) outranks B (len 2) on \
         the value-length tiebreak before the name sort; a normalized-length measure would \
         tie both at 2 and wrongly pick B by name"
    );
}

/// H6-2 (MEDIUM, regex dialect). The `{,m}` quantifier is Python `re`'s shorthand for
/// `{0,m}` (`re.match(r'a{,3}b','aaab')` matches), and Python Lark builds
/// `A: /a{,3}b/`. The Rust `regex` crate requires a decimal lower bound, so `Regex::new`
/// fails; `PatternRe::new` then hands the pattern to the lookaround analyzer, which
/// can't parse it, and routes it to `GrammarError::LookaroundScope` / `OutOfScope`
/// "backtracking-only syntax" — two faults: it rejects a Python-accepted regex, and the
/// refusal category is wrong (it is a plain dialect-normalization gap, not lookaround).
/// Note `base_quantifier_len` already *recognizes* `{,n}` as a well-formed quantifier;
/// only the `{,n}` → `{0,n}` normalization is missing. Opposite polarity to the
/// H6–H9/#375 dialect *narrowings* (which reject to match Python's rejection). Expected
/// fix: normalize `{,n}` → `{0,n}` in `normalize_python_escapes` (class/escape-aware).
#[test]
#[ignore = "XFAIL (bounty H6-2): {,m} quantifier rejected and mis-categorized as OutOfScope lookaround"]
fn h6_2_empty_lower_bound_quantifier_accepted() {
    let g = "start: A\nA: /a{,3}b/\n";
    let lark = Lark::new(g, opts(ParserAlgorithm::Lalr, LexerType::Contextual));
    let lark =
        lark.expect("H6-2: Python accepts /a{,3}b/ (== /a{0,3}b/); lark-rs rejected the build");
    let tree = lark.parse("aaab").expect("H6-2: parses 'aaab'");
    assert_eq!(
        first_token_type(&tree).as_deref(),
        Some("A"),
        "H6-2: /a{{,3}}b/ must match 'aaab' as token A, matching Python"
    );
}

/// H6-6 (MEDIUM-HIGH, terminal unification). A string literal `"ab"` whose source is
/// byte-identical to a named **regex** terminal `AB: /ab/` is wrongly unified onto `AB`
/// during anon-terminal interning: `patterns_equivalent` (`grammar/loader/terminals.rs`)
/// compares `a.as_regex_str() == b.as_regex_str() && flags match`, which collapses
/// `PatternStr("ab")` and `PatternRe(/ab/)` because both project to the source `ab`.
/// Python's `Pattern.__eq__` requires `type(self) == type(other)`, and `term_reverse`
/// is consulted only for `PatternStr`, so a literal never unifies with a regex
/// terminal — Python mints a distinct anonymous `__ANON_*` terminal (filtered from the
/// tree). lark-rs keeps the literal typed `AB` and *unfiltered*, an extra child under
/// default options. Distinct from H4-9/#347 (Str-vs-Str *alternation-arm* dedup via
/// `sym_key`); this is the Re-vs-Str *interning* merge. Expected fix: gate
/// `patterns_equivalent` on matching `Pattern` kind (never `Str` ≡ `Re`).
#[test]
#[ignore = "XFAIL (bounty H6-6): string literal unified onto a same-source regex terminal, kept instead of filtered"]
fn h6_6_string_literal_not_unified_with_regex_terminal() {
    let g = "start: AB | \"ab\"\nAB: /ab/\n";
    let lark =
        Lark::new(g, opts(ParserAlgorithm::Lalr, LexerType::Contextual)).expect("H6-6: builds");
    let tree = lark.parse("ab").expect("H6-6: parses");
    // Python: the literal "ab" is a distinct PatternStr (__ANON_0), filtered → no tokens.
    // lark-rs: the literal is unified onto AB and kept → one token.
    assert_eq!(
        token_count(&tree),
        0,
        "H6-6: Python keeps the literal as a distinct filtered __ANON terminal (0 tokens in the tree); \
         lark-rs unified it onto the regex terminal AB and kept it"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// LALR table construction: spurious reduce/reduce on nullable arms.
// ─────────────────────────────────────────────────────────────────────────────

/// H6-3 (MEDIUM-HIGH, lalr-table). Two nullable alternatives of the same origin that
/// differ only by an **alias** (`p: "a"? -> al1 | "b"? -> al2`) survive lowering as two
/// distinct `Rule`s with distinct `tree_name` (`grammar/loader/ebnf.rs`,
/// `grammar/intern.rs`); the reduce/reduce detector (`parsers/lalr.rs`) then treats the
/// two `p -> ε` reductions on `$END` as an unresolvable collision. Without aliases the
/// arms dedup and the grammar builds. Python's LALR resolves same-rule ties by
/// priority/order (first arm wins) and treats the alias as pure tree-naming metadata
/// outside the R/R comparison; lark-rs's own Earley also accepts (proving the grammar is
/// legal). Opposite direction to RC7/#272 (recurse-helper over-share, which *under*-
/// reports). Expected fix: in the R/R resolution, reduce (not error) candidates that
/// share `origin`+`expansion` and differ only by alias, picking the lowest `rule.order`.
#[test]
#[ignore = "XFAIL (bounty H6-3): aliased nullable alternatives produce a spurious LALR reduce/reduce rejection"]
fn h6_3_aliased_nullable_alternatives_build() {
    let g = "p: \"a\"? -> al1 | \"b\"? -> al2\nstart: p\n";
    let lark = Lark::new(g, opts(ParserAlgorithm::Lalr, LexerType::Contextual));
    assert!(
        lark.is_ok(),
        "H6-3: Python's LALR (and lark-rs's own Earley) accept aliased nullable alternatives; \
         lark-rs's LALR reported a spurious reduce/reduce collision"
    );
}

/// H6-4 (MEDIUM, ebnf-loader). A bare nested optional under repetition (`[[A]]* C`)
/// reaches `inner_alternatives` (`grammar/loader/ebnf.rs`) as an `Expr::Maybe` whose sole
/// content is another `Maybe`; it is wrapped in a single `__anon_group_*` helper whose
/// rule then carries **two byte-identical empty productions** (inner-absent and
/// outer-absent), never collapsed the way a lone `([A])?` is. Those twin ε-arms surface
/// as a self reduce/reduce collision (`__anon_group_0 -> / __anon_group_0 ->`). The
/// non-nested forms `[A]* C`, `([A])* C`, and `[[A] B]* C` all build, and Earley accepts
/// `[[A]]*` — isolating the bare-double-bracket-under-repetition path. Python's
/// `EBNF_to_BNF`/`SimplifyRule_Visitor` collapses the twin empties and accepts.
/// Expected fix: collapse the helper's duplicate empty arms (or distribute the nested
/// maybe's arms) so a single ε base arm is emitted.
#[test]
#[ignore = "XFAIL (bounty H6-4): nested bare optional under repetition [[A]]* spuriously rejected (twin empty arms)"]
fn h6_4_nested_optional_under_repetition_builds() {
    let g = "start: [[A]]* C\nA: \"a\"\nC: \"c\"\n";
    let lark = Lark::new(g, opts(ParserAlgorithm::Lalr, LexerType::Contextual));
    let lark = lark.expect(
        "H6-4: Python accepts [[A]]* C; lark-rs minted a recurse helper with twin empty arms \
         and reported a spurious LALR reduce/reduce",
    );
    // Python parses 'c' to start[C].
    assert_eq!(
        first_token_type(&lark.parse("c").expect("H6-4: parses 'c'")).as_deref(),
        Some("C"),
        "H6-4: [[A]]* C on 'c' must yield token C, matching Python"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Tree metadata: propagate_positions span.
// ─────────────────────────────────────────────────────────────────────────────

/// H6-5 (MEDIUM, core / tree-meta). With `propagate_positions`, Python wraps the node
/// builder as `child_filter → PropagatePositions`, deriving a tree's `meta` from the
/// **unfiltered** children (`_pp_get_meta`), so filtered punctuation contributes to the
/// span. lark-rs computes meta in `Meta::from_children` (`tree.rs`) over the
/// **already-filtered** children that `apply_rule_options` produced, so a rule wrapped by
/// filtered literals (`start: "(" A ")"`) reports a span that omits the parens
/// (start_pos/end_pos `2..6` instead of Python's `0..8`). The token positions are
/// correct; only the tree-meta span diverges (and the diffcheck harness strips meta, so
/// this surface was never exercised). Distinct from N8 (byte-vs-char `*_pos`, fixed) and
/// H10/#337 (positionless-empty `meta.empty` flag). Expected fix: compute meta from the
/// production's pre-filter child span (a filtered token contributes its own start/end).
#[test]
#[ignore = "XFAIL (bounty H6-5): Tree.meta span excludes filtered tokens under propagate_positions"]
fn h6_5_meta_span_includes_filtered_tokens() {
    let mut o = opts(ParserAlgorithm::Lalr, LexerType::Contextual);
    o.propagate_positions = true;
    let g = "start: \"(\" A \")\"\nA: /caf./\n%import common.WS\n%ignore WS\n";
    let lark = Lark::new(g, o).expect("H6-5: builds");
    let ParseTree::Tree(t) = lark.parse("( cafX )").expect("H6-5: parses") else {
        panic!("H6-5: expected a tree");
    };
    // Python: start meta spans the whole "( cafX )" including the filtered parens: 0..8.
    assert_eq!(
        (t.meta.start_pos, t.meta.end_pos),
        (Some(0), Some(8)),
        "H6-5: start meta must span the filtered '(' and ')' (0..8), matching Python; \
         lark-rs computed it from post-filter children (2..6)"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Grammar name-token lexer: name shape validation.
// ─────────────────────────────────────────────────────────────────────────────

/// H6-8 (LOW, grammar-loader). Python's grammar lexer regexes are
/// `RULE = _?[a-z][_a-z0-9]*` and `TERMINAL = _?[A-Z][_A-Z0-9]*` — at most one leading
/// underscore and **at least one** alphabetic char. lark-rs's `lex_rule`/`lex_terminal`
/// (`grammar/loader/tokenizer.rs`) consume any run of name characters with no name-shape
/// validation, so a name with no letter (`_`, `__`, `_9`) is accepted where Python
/// rejects it at grammar-lex time. Distinct from H5-2/#361 (`__foo` — a name that *has*
/// a letter but a disallowed `__` prefix); this is the no-letter-at-all class, a
/// different validation predicate. Per ADR-0017 (being more permissive than the oracle
/// is unfalsifiable), expected fix: reject-like-Python.
#[test]
#[ignore = "XFAIL (bounty H6-8): rule/terminal names with no alphabetic char accepted; Python rejects"]
fn h6_8_letterless_names_rejected() {
    for g in [
        "_: \"a\"\nstart: _\n",
        "__: \"a\"\nstart: __\n",
        "_9: \"a\"\nstart: _9\n",
    ] {
        assert!(
            Lark::new(g, opts(ParserAlgorithm::Lalr, LexerType::Contextual)).is_err(),
            "H6-8: Python rejects a name with no [a-z]/[A-Z] at grammar-lex; lark-rs accepted it. grammar={g:?}"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Deterministic resource bounds (grammar build).
// ─────────────────────────────────────────────────────────────────────────────

/// H6-7 (MEDIUM, perf / grammar build). `compile_expansion`'s per-position loop
/// (`grammar/loader/ebnf.rs`) folds each group into `acc` with the **non-deduping**
/// `concat_alts`, deduping only once at the end. A chain of `k` inline groups with `m`
/// duplicate arms (`(X|X) (X|X) … (X|X)`) materializes `m^k` intermediate
/// alternatives before collapsing to a single rule — a deterministic `2^k`-vs-`O(1)`
/// blowup (measured: k=12 → 12 ms, k=14 → 65 ms, k=16 → 325 ms, k=18 → 1569 ms; ~2× per
/// +1 k, final surface rules = 1). Python's `SimplifyRule_Visitor` dedups each group's
/// arms *before* the product and builds the identical grammar in flat linear time.
/// Distinct from N9 (`~n..m` O(n²) *size*) and #252 (the `~n` repeat path, which uses
/// the existing `concat_alts_dedup` and where Python *also* blows up). Expected fix:
/// use `concat_alts_dedup` (already in the file) at the per-position fold + add a
/// sub-exponential build-scaling gate. The fix exists in-file; it is just not wired into
/// the general `compile_expansion` loop.
///
/// The XFAIL gate: building `(X|X)^k` at `k=20` (~6 s on the non-deduping path today,
/// instant once fixed) must finish within a generous budget. The build runs on a worker
/// thread with a timeout so the ignored test fails fast today rather than hanging.
#[test]
#[ignore = "XFAIL (bounty H6-7): O(2^k) grammar-build blowup on duplicate-arm inline-group cross-products"]
fn h6_7_duplicate_group_cross_product_build_blowup() {
    use std::sync::mpsc;
    use std::time::Duration;

    let k = 20usize;
    let body = vec!["(X|X)"; k].join(" ");
    let grammar = format!("start: {body}\nX: \"x\"\n");

    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let built = Lark::new(&grammar, opts(ParserAlgorithm::Lalr, LexerType::Contextual)).is_ok();
        let _ = tx.send(built);
    });

    match rx.recv_timeout(Duration::from_secs(3)) {
        Ok(true) => { /* fixed: the deduped fold builds the trivial grammar instantly */ }
        Ok(false) => panic!("H6-7: (X|X)^{k} unexpectedly rejected"),
        Err(_) => panic!(
            "H6-7: (X|X)^{k} build did not finish in 3 s — the non-deduping `concat_alts` \
             materialized a 2^{k} cartesian product; Python builds the identical grammar \
             (1 rule) in linear time"
        ),
    }
}
