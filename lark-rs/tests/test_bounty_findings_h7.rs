//! Bug-bounty findings, round 7 (h7) — failing oracle tests (XFAIL).
//!
//! Rounds 1–6 (`test_bounty_findings.rs` RC, `_h2.rs` N, `_h3.rs` H, `_h4.rs` H4-*,
//! `_h5.rs` H5-*, `_h6.rs` H6-*) harvested the validation-gate layer, the lexer
//! terminal-ordering bugs, six waves of Python-`re` regex-dialect divergences, config
//! legality, char-vs-byte positions, error/`ParseError` parity, import-closure
//! mangling, tree-shaping lone-`None`, the standalone bake, and the bindings surface.
//! Round 7 retargeted the corners those rounds either declared clean or never reached:
//!
//!   * H7-1 — `%ignore` of a `%declare`d (pattern-less) terminal is accepted; Python
//!     rejects at build with `LexError: Ignore terminals are not defined`. The terminal
//!     *is* defined (so lark-rs's existing UndefinedTerminal gate passes) but has no
//!     pattern, which is exactly the case Python's distinct lexer-build `LexError`
//!     catches. Distinct from RC1/RC2/#299 (duplicate *definition*) and the
//!     `%ignore <rule>` / bare-undefined-name gates, which lark-rs already handles.
//!   * H7-2 — a literal newline inside a `/regex/` literal (H7-2a) or a `"string"`
//!     literal terminal (H7-2b) is accepted as pattern/value text; Python's grammar
//!     loader rejects both at build (`_literal_to_pattern`: "You can only use newlines
//!     in regular expressions with the `x` flag" / a STRING token that cannot span a
//!     newline). One root cause — the loader's literal tokenizers (`lex_regexp`,
//!     `lex_string` in `grammar/loader/tokenizer.rs`) omit the no-embedded-newline gate
//!     — surfaced on two literal kinds. New surface, not a regex-engine dialect screen.
//!   * H7-3 — a Python `re` *conditional* group reference `(?(1)yes|no)` is rejected at
//!     **build** and mis-categorized as a generic InvalidRegex/"backtracking-only
//!     syntax" lookaround refusal, where Python Lark *builds* the terminal and matches
//!     with it. Two faults: it rejects a Python-accepted construct, and the refusal
//!     category is wrong. Distinct from the backref `\1` row (a conditional is not a
//!     backreference) and from #275/#400/#332 (those reject in *both* engines).
//!
//! Each test asserts the **Python Lark 1.3.1** (oracle) behavior. This file is an XFAIL
//! catalog: a test is `#[ignore]`d while its bug is open and fails. Drop a test's
//! `#[ignore]` when its bug is fixed to turn it into a permanent regression guard.
//! H7-1 and H7-2a/H7-2b are **fixed** (#414, the loader reject-gates) and are now active
//! guards; H7-3 remains an open XFAIL. Run the still-open XFAILs with:
//!
//!     cargo test --test test_bounty_findings_h7 -- --ignored
//!
//! Baseline SHA: 9acb50bb203bcf4b5949f3a19bfdc4bfe3f0b2d5. Catalog with repros, severity,
//! blast radius, fix contracts, the standalone `None`-root variant (V-H7-1, pinned in
//! `src/standalone/mod.rs`), the PyO3 binding findings (eq/hash invariant + surface
//! gaps, A-level but binding-surface — documented, not encoded here), and the dedup
//! against rounds 1–6: `docs/BOUNTY_FINDINGS_H7.md`.
//!
//! NONE of these reduce to a round-1..6 root cause (RC1–RC10, N1–N10, V1–V4, H1–H12,
//! P1–P2, H4-1…H4-12, H5-1…H5-9, H6-1…H6-8) or the open known-issue set. Adjacencies
//! are noted at each test.

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

// ─────────────────────────────────────────────────────────────────────────────
// Grammar-loader validation gates: too-permissive acceptance.
// ─────────────────────────────────────────────────────────────────────────────

/// H7-1 (LOW, grammar-loader). `%ignore` of a `%declare`d terminal is accepted by
/// lark-rs. A `%declare`d terminal is pushed into `self.terminals` as a pattern-less
/// `TerminalDef::declared(...)` (`grammar/loader/compiler.rs::declare_terminals`); the
/// `IgnoreEntry::Named` check only verifies the name appears in `self.terminals`, so a
/// pattern-less declared terminal passes. Python builds the lexer and raises
/// `LexError: Ignore terminals are not defined: {'Z'}` (`lark/lexer.py`: a declared
/// terminal carries no pattern and is absent from the lexer's terminal list, so the
/// ignore-set difference is non-empty). A `%declare`d terminal *used in a rule* builds
/// fine in both — the rejection is specific to `%ignore`-ing it. Per ADR-0017 (more
/// permissive than the oracle is unfalsifiable), expected fix: reject `IgnoreEntry::Named`
/// when the resolved terminal is pattern-less (`declared`). Distinct from RC1/RC2/#299
/// (duplicate *definition*) and the `%ignore <rule>` gate lark-rs already has.
#[test]
fn h7_1_ignore_of_declared_terminal_rejected() {
    let g = "%declare Z\nstart: \"a\"\n%ignore Z\n";
    assert!(
        Lark::new(g, opts(ParserAlgorithm::Lalr, LexerType::Basic)).is_err(),
        "H7-1: Python rejects %ignore of a %declare'd (pattern-less) terminal at build \
         (LexError: Ignore terminals are not defined); lark-rs accepted it"
    );
}

/// H7-2a (LOW, grammar-loader / tokenizer). A literal newline inside a `/regex/` literal
/// is accepted by lark-rs's `lex_regexp` (`grammar/loader/tokenizer.rs`), which scans to
/// the closing `/` with no newline guard, so the embedded `\n` becomes pattern text and
/// matches a literal newline. Python's `_literal_to_pattern` (`lark/load_grammar.py`)
/// rejects it: `GrammarError: You can only use newlines in regular expressions with the
/// `x` (verbose) flag`. New surface (the loader's regex-literal newline rule), not a
/// regex-engine dialect screen in `terminal.rs`. Expected fix: reject-like-Python (a
/// newline in a `/…/` literal without the `x` flag).
#[test]
fn h7_2a_newline_in_regex_literal_rejected() {
    let g = "start: A\nA: /a\nb/\n";
    assert!(
        Lark::new(g, opts(ParserAlgorithm::Lalr, LexerType::Contextual)).is_err(),
        "H7-2a: Python rejects a literal newline inside a /regex/ literal without the `x` \
         flag; lark-rs accepted it and matched a literal newline"
    );
}

/// H7-2b (LOW, grammar-loader / tokenizer). Sibling surface of H7-2a (same root cause):
/// a literal newline inside a `"string"` literal terminal is accepted by lark-rs's
/// `lex_string` (`grammar/loader/tokenizer.rs`), which scans to the closing `"` with no
/// newline guard. Python's grammar tokenizer's STRING terminal cannot span a newline —
/// `GrammarError: Unexpected input` at the newline (and `_literal_to_pattern` would also
/// raise "You cannot put newlines in string literals"). Folded under H7-2 as the
/// string-literal surface of the missing no-embedded-newline gate.
#[test]
fn h7_2b_newline_in_string_literal_rejected() {
    let g = "start: A\nA: \"a\nb\"\n";
    assert!(
        Lark::new(g, opts(ParserAlgorithm::Lalr, LexerType::Contextual)).is_err(),
        "H7-2b: Python rejects a literal newline inside a \"string\" literal terminal; \
         lark-rs accepted it as string value text"
    );
}

// ── H7-2 fix-boundary pins (#414) ────────────────────────────────────────────
// The newline gates must reject a real `\n` *byte* even when a backslash precedes
// it (Python's grammar tokenizer's `\\.` escape cannot match `\n`), must still
// *accept* the `\n` escape *sequence* (backslash+`n`), and must accept a real
// newline in a `/…/` literal once the `x` (verbose) flag is set — all verified
// against Python Lark 1.3.1.

/// A backslash immediately before a real newline does **not** escape it: Python
/// rejects `/a\<LF>b/` and `"a\<LF>b"` exactly like the bare-newline forms.
#[test]
fn h7_2_backslash_before_newline_still_rejected() {
    let regex_g = "start: A\nA: /a\\\nb/\n";
    assert!(
        Lark::new(regex_g, opts(ParserAlgorithm::Lalr, LexerType::Contextual)).is_err(),
        "a backslash before a real newline in a /regex/ literal is still rejected by Python"
    );
    let string_g = "start: A\nA: \"a\\\nb\"\n";
    assert!(
        Lark::new(string_g, opts(ParserAlgorithm::Lalr, LexerType::Contextual)).is_err(),
        "a backslash before a real newline in a \"string\" literal is still rejected by Python"
    );
}

/// The `\n` *escape sequence* (backslash+`n`, not a real newline byte) stays
/// accepted in both literal kinds — Python decodes it to U+000A and builds fine.
#[test]
fn h7_2_newline_escape_sequence_still_accepted() {
    let regex_g = "start: A\nA: /a\\nb/\n";
    assert!(
        Lark::new(regex_g, opts(ParserAlgorithm::Lalr, LexerType::Contextual)).is_ok(),
        "the \\n escape sequence in a /regex/ literal must still build (Python accepts it)"
    );
    let string_g = "start: A\nA: \"a\\nb\"\n";
    assert!(
        Lark::new(string_g, opts(ParserAlgorithm::Lalr, LexerType::Contextual)).is_ok(),
        "the \\n escape sequence in a \"string\" literal must still build (Python accepts it)"
    );
}

/// A real newline inside a `/…/` literal is accepted once the `x` (verbose) flag
/// is present — Python builds it (verbose mode ignores unescaped whitespace).
#[test]
fn h7_2a_newline_in_regex_literal_accepted_with_verbose_flag() {
    let g = "start: A\nA: /a\nb/x\n";
    assert!(
        Lark::new(g, opts(ParserAlgorithm::Lalr, LexerType::Contextual)).is_ok(),
        "a newline in a /regex/ literal with the `x` flag must build, matching Python"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Python-re dialect: a build-accepted construct lark-rs build-rejects.
// ─────────────────────────────────────────────────────────────────────────────

/// H7-3 (LOW-MEDIUM, regex dialect / refusal taxonomy). A Python `re` conditional group
/// reference `(?(id)yes|no)` is rejected at **build** by lark-rs: `Regex::new` fails
/// ("unrecognized flag"), the lookaround analyzer can't parse it either, so it lands in
/// the generic InvalidRegex / "backtracking-only syntax" lookaround refusal
/// (`grammar/terminal.rs::PatternRe::new`). Python Lark *builds* the terminal `A: /(a)(?(1)b|c)/`
/// and parses `"ac"` → `start(Token A "ac")` (a conditional is a regular-ish alternation
/// the linear engine could in principle host). Two faults: it rejects a Python-accepted
/// regex, and the refusal category is wrong (a conditional is not a backreference and not
/// "backtracking-only"). Distinct from the backref `\1` refusal and from #275/#400/#332
/// (those reject in *both* engines). Expected fix: support and match Python (lower the
/// group-existence conditional), or at minimum re-categorize as a dialect gap. Per
/// ADR-0017's two-axis routing, the contract here is "support and match Python."
#[test]
#[ignore = "XFAIL (bounty H7-3): conditional regex (?(1)yes|no) build-rejected and mis-categorized; Python builds and matches"]
fn h7_3_conditional_group_reference_build_accepted() {
    let g = "start: A\nA: /(a)(?(1)b|c)/\n";
    let lark = Lark::new(g, opts(ParserAlgorithm::Lalr, LexerType::Contextual))
        .expect("H7-3: Python builds /(a)(?(1)b|c)/; lark-rs rejected the build");
    let tree = lark.parse("ac").expect("H7-3: parses 'ac'");
    assert_eq!(
        first_token_type(&tree).as_deref(),
        Some("A"),
        "H7-3: /(a)(?(1)b|c)/ must match 'ac' as token A, matching Python"
    );
}
