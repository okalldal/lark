//! `strict=True` parity: grammars with an unresolvable shift/reduce conflict must
//! be *rejected at construction time* instead of silently resolved as a shift.
//!
//! Oracle: Python Lark `Lark(grammar, parser="lalr", strict=True)` raises a
//! `GrammarError` for `start: a "."` / `a: "."+`, but builds it fine in the
//! default (non-strict) mode. See COMPLIANCE_PARITY.md (M8b).

use lark_rs::{Lark, LarkOptions, LexerType, ParserAlgorithm};

fn opts(strict: bool) -> LarkOptions {
    LarkOptions {
        parser: ParserAlgorithm::Lalr,
        lexer: LexerType::Contextual,
        start: vec!["start".to_string()],
        strict,
        ..Default::default()
    }
}

/// The canonical shift/reduce conflict: after `a: "."+` has matched one `.`, the
/// next `.` can either extend the repetition (shift) or reduce `a` and let the
/// trailing `"."` in `start` consume it (reduce).
const SHIFT_REDUCE: &str = "start: a \".\"\na: \".\"+\n";

#[test]
fn shift_reduce_conflict_builds_in_default_mode() {
    // Non-strict: Lark (and lark-rs) resolve as shift, no error.
    assert!(
        Lark::new(SHIFT_REDUCE, opts(false)).is_ok(),
        "default mode must silently resolve the S/R conflict as a shift"
    );
}

#[test]
fn shift_reduce_conflict_is_fatal_in_strict_mode() {
    let err = Lark::new(SHIFT_REDUCE, opts(true));
    assert!(
        err.is_err(),
        "strict mode must reject the shift/reduce conflict at construction time"
    );
    let msg = format!("{}", err.err().unwrap());
    assert!(
        msg.contains("Shift/Reduce") || msg.to_lowercase().contains("conflict"),
        "strict-mode error should mention the shift/reduce conflict, got: {msg}"
    );
}

#[test]
fn conflict_free_grammar_builds_under_strict() {
    // A plainly LALR(1) grammar must still build with strict=True — strict only
    // rejects genuine conflicts, it does not over-reject.
    let g = "start: \"a\" \"b\"\n";
    assert!(
        Lark::new(g, opts(true)).is_ok(),
        "strict mode must not reject a conflict-free grammar"
    );
}
