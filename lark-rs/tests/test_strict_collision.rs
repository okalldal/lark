//! Strict-mode regex-collision detection and the lexer build-time validation it
//! sits next to (issue #35).
//!
//! Under `strict=True`, Python Lark (via `interegular`) rejects two same-priority
//! *regex* terminals whose languages share a string. lark-rs reproduces this with
//! a product-construction emptiness test over each terminal's DFA. These tests pin
//! the contract *and* every divergence risk that was identified during review —
//! each expected outcome below was confirmed against **Python Lark 1.3.1 +
//! interegular 0.3.3** (the relevant cross-checks are noted inline as
//! `oracle: …`).

use lark_rs::{Lark, LarkOptions, LexerType, ParserAlgorithm};

/// Build a grammar and reduce the result to either `BUILD_OK` or the full error
/// message (collision messages are multi-line, so we keep the whole text).
fn build(g: &str, strict: bool, lexer: LexerType, parser: ParserAlgorithm) -> String {
    let opts = LarkOptions {
        parser,
        lexer,
        strict,
        ..Default::default()
    };
    match Lark::new(g, opts) {
        Ok(_) => "BUILD_OK".to_string(),
        Err(e) => e.to_string(),
    }
}

fn lalr(g: &str, strict: bool, lexer: LexerType) -> String {
    build(g, strict, lexer, ParserAlgorithm::Lalr)
}

fn is_collision(tag: &str) -> bool {
    tag.contains("Collision between Terminals")
}
fn is_zero_width(tag: &str) -> bool {
    tag.contains("zero-width terminals")
}

// ── The canonical overlap: `/e?rez/` and `/erez?/` both match "erez". ──────────
// This is the compliance-bank case (construct:57 / :58).

const COLLIDING: &str = "start: A | B\nA: /e?rez/\nB: /erez?/\n";

#[test]
fn overlapping_regex_terminals_collide_in_strict_mode() {
    // oracle: LexError under both lalr lexers.
    for lexer in [LexerType::Contextual, LexerType::Basic] {
        let tag = lalr(COLLIDING, true, lexer.clone());
        assert!(
            is_collision(&tag) && tag.contains('A') && tag.contains('B'),
            "{lexer:?}: expected a collision naming A and B, got: {tag}"
        );
    }
}

#[test]
fn collision_reports_the_shared_witness_string() {
    // The BFS yields the *shortest* common string; for this pair that is "erez".
    // (Risk #7: the witness is byte-derived, but for unicode-mode patterns it is
    //  always valid UTF-8.)
    let tag = lalr(COLLIDING, true, LexerType::Basic);
    assert!(
        tag.contains("\"erez\""),
        "expected witness \"erez\" in the message, got: {tag}"
    );
}

#[test]
fn overlap_is_only_rejected_in_strict_mode() {
    // oracle: builds fine without strict (Python only warns there).
    assert_eq!(lalr(COLLIDING, false, LexerType::Basic), "BUILD_OK");
}

// ── `pattern.type` parity: only regex terminals are compared (Risk #3). ────────
// lark-rs compiles every named terminal to a regex, so a `string_type` flag
// recovers Python's PatternStr/PatternRE split. Each case's `oracle:` outcome was
// confirmed against Python — a mismatch here is a real parity regression.

#[test]
fn string_keyword_is_not_checked_against_a_regex() {
    // oracle: BUILD_OK — `A: "if"` is a PatternStr, excluded; `unless` retyping
    // (not a collision) disambiguates it from B at lex time.
    let g = "start: A B\nA: \"if\"\nB: /[a-z]+/\n";
    assert_eq!(lalr(g, true, LexerType::Basic), "BUILD_OK");
}

#[test]
fn case_insensitive_string_is_still_a_string_terminal() {
    // oracle: BUILD_OK — `"if"i` is a PatternStr with a flag, still excluded.
    let g = "start: A B\nA: \"if\"i\nB: /[a-z]+/\n";
    assert_eq!(lalr(g, true, LexerType::Basic), "BUILD_OK");
}

#[test]
fn terminal_referencing_only_a_string_is_a_string_terminal() {
    // oracle: BUILD_OK — `A: B` with `B: "if"` resolves to a PatternStr, so A is
    // excluded too (the reference is followed when classifying).
    let g = "start: A B\nA: B\nB: \"if\"\nC: /[a-z]+/\nx: C\n";
    assert_eq!(lalr(g, true, LexerType::Basic), "BUILD_OK");
}

#[test]
fn concatenation_of_strings_is_a_regex_terminal() {
    // oracle: Collision — `A: "a" "b"` is a *joined* PatternRE, so it IS compared,
    // and "ab" is in B's language.
    let g = "start: A B\nA: \"a\" \"b\"\nB: /[a-z]+/\n";
    assert!(is_collision(&lalr(g, true, LexerType::Basic)));
}

#[test]
fn char_range_is_a_regex_terminal() {
    // oracle: Collision — `A: "a".."z"` is a PatternRE (a char class), compared
    // against the overlapping `/[a-z]/`.
    let g = "start: A B\nA: \"a\"..\"z\"\nB: /[a-z]/\n";
    assert!(is_collision(&lalr(g, true, LexerType::Basic)));
}

#[test]
fn overlap_at_different_priorities_is_not_a_collision() {
    // oracle: BUILD_OK — Python groups by priority and never compares across
    // groups, even though the languages are identical to COLLIDING.
    let g = "start: A | B\nA.2: /e?rez/\nB.1: /erez?/\n";
    assert_eq!(lalr(g, true, LexerType::Basic), "BUILD_OK");
}

#[test]
fn disjoint_regex_terminals_do_not_collide() {
    // `[0-9]+` and `[a-z]+` share no string — must not be over-rejected.
    let g = "start: A | B\nA: /[0-9]+/\nB: /[a-z]+/\n";
    assert_eq!(lalr(g, true, LexerType::Basic), "BUILD_OK");
}

// ── Risk #1: the contextual lexer scopes the check per parser state. ───────────
// X and Y overlap (both match "foo") at equal priority but live in different rules,
// so the contextual lexer never offers them in the same state.

const STATE_SEPARATED: &str =
    "start: a | b\na: P X\nb: Q Y\nP: \"p\"\nQ: \"q\"\nX: /foo/\nY: /foo|bar/\n";

#[test]
fn contextual_lexer_does_not_flag_terminals_from_different_states() {
    // oracle: contextual → BUILD_OK (each state's BasicLexer sees only its own
    // terminals); basic → Collision (one scanner sees all terminals). lark-rs must
    // match *both* — a global check here would over-reject the contextual case.
    assert_eq!(
        lalr(STATE_SEPARATED, true, LexerType::Contextual),
        "BUILD_OK",
        "contextual lexer must not compare terminals that never share a state"
    );
    assert!(
        is_collision(&lalr(STATE_SEPARATED, true, LexerType::Basic)),
        "basic lexer compiles all terminals together, so it must flag X/Y"
    );
}

// ── Risk #2: zero-width terminals are rejected, like Python (unconditionally). ──

#[test]
fn nullable_terminal_is_rejected_even_without_strict() {
    // oracle: LexError "Lexer does not allow zero-width terminals" — raised in the
    // BasicLexer sanitization, NOT gated on strict, and before the collision check.
    for strict in [true, false] {
        let tag = lalr("start: A\nA: /a*/\n", strict, LexerType::Basic);
        assert!(
            is_zero_width(&tag),
            "strict={strict}: expected a zero-width rejection, got: {tag}"
        );
    }
}

#[test]
fn zero_width_check_precedes_the_collision_check() {
    // Two nullable overlapping terminals: Python reports the zero-width error
    // first, not a collision on the empty string. lark-rs must do the same so the
    // diagnostic is correct rather than a confusing `Both match the string ""`.
    let tag = lalr("start: A B\nA: /a*/\nB: /a*/\n", true, LexerType::Basic);
    assert!(is_zero_width(&tag) && !is_collision(&tag), "got: {tag}");
}

// ── Risk #6: the check is scoped to the basic-lexer model on Earley too. ───────

#[test]
fn earley_basic_lexer_runs_the_check_but_dynamic_does_not() {
    use ParserAlgorithm::Earley;
    // oracle: earley+basic → LexError; earley+dynamic → BUILD_OK (the dynamic lexer
    // has no BasicLexer, so Python runs neither the zero-width nor the collision
    // check there).
    assert!(is_collision(&build(
        COLLIDING,
        true,
        LexerType::Basic,
        Earley
    )));
    assert_eq!(
        build(COLLIDING, true, LexerType::Dynamic, Earley),
        "BUILD_OK",
        "the dynamic Earley lexer must not run the collision check"
    );
}

// ── Risk #4/#5: bounded build cost; a real overlap is still found quickly. ─────

#[test]
fn large_bounded_overlap_still_terminates_and_reports() {
    // A wide, long bounded repetition: the product BFS finds the one-character
    // witness immediately (it never materializes the whole DFA), so a genuine
    // overlap is reported in milliseconds rather than hanging.
    let g = "start: A | B\nA: /[a-z]{1,300}/\nB: /[a-z]{1,300}x?/\n";
    let tag = lalr(g, true, LexerType::Basic);
    assert!(
        is_collision(&tag) && tag.contains("\"a\""),
        "expected a fast collision with witness \"a\", got: {tag}"
    );
}
