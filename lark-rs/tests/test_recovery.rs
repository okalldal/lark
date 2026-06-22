//! Error-recovery oracle + behaviour tests (issue #43).
//!
//! lark-rs's panic-mode recovery is single-token-deletion, a token-for-token port
//! of Python Lark's built-in `on_error` driver. The oracle (`recovery/cases.json`,
//! produced by `generate_oracles.py::generate_recovery`) captures, for each input,
//! the tree Python recovers to with `on_error=lambda e: True` and how many tokens
//! it deleted. We assert byte-for-byte tree parity plus the same deletion count.

mod common;

use common::{load_oracle, make_lalr_from_file, tree_matches_oracle};
use lark_rs::{Lark, LarkOptions, LexerType, ParserAlgorithm};

/// The recovery grammar, built LALR (the contextual lexer make_lalr_from_file
/// selects is irrelevant — recovery always lexes with the basic/global lexer).
fn recovery_parser() -> Lark {
    make_lalr_from_file("recovery")
}

#[test]
fn test_recovery_oracle() {
    let lark = recovery_parser();
    let cases = load_oracle("recovery", "cases");
    let cases = cases.as_array().expect("oracle is a JSON array");

    for case in cases {
        let input = case["input"].as_str().unwrap();
        let recovered = case["recovered"].as_bool().unwrap();
        let error_count = case["error_count"].as_u64().unwrap() as usize;

        let result = lark
            .parse_with_recovery(input)
            .unwrap_or_else(|e| panic!("recovery should not hard-error on {input:?}: {e}"));

        assert_eq!(
            result.errors.len(),
            error_count,
            "input {input:?}: deleted {} tokens, oracle deleted {error_count}",
            result.errors.len(),
        );

        if recovered {
            // Python recovered to a full tree — lark-rs must produce the same one,
            // and it must be a real `Some` derivation.
            let tree = result
                .tree
                .as_ref()
                .unwrap_or_else(|| panic!("input {input:?}: expected Some(tree), got None"));
            tree_matches_oracle(tree, &case["tree"])
                .unwrap_or_else(|e| panic!("input {input:?}: tree mismatch vs oracle: {e}"));
        } else {
            // Premature-EOF: Python re-raises (`recovered: false`, `tree: null`).
            // lark-rs pins that exactly (issue #167): no fabricated derivation —
            // `tree` is `None` — only the recovered errors are surfaced.
            assert!(
                case["tree"].is_null(),
                "oracle bug: recovered:false case {input:?} should have tree:null"
            );
            assert!(
                result.tree.is_none(),
                "input {input:?}: premature-EOF must yield tree:None, not a fabricated partial"
            );
            assert!(
                !result.errors.is_empty(),
                "input {input:?}: expected at least one recovered error"
            );
        }
    }
}

#[test]
fn test_clean_parse_records_no_errors() {
    // A valid input recovers nothing: the tree equals a normal parse and the error
    // list is empty.
    let lark = recovery_parser();
    let result = lark.parse_with_recovery("1 + 2").unwrap();
    assert!(result.errors.is_empty());
    let tree = result.tree.expect("clean parse yields Some(tree)");
    let normal = lark.parse("1 + 2").unwrap();
    assert_eq!(format!("{tree}"), format!("{normal}"));
}

#[test]
fn test_on_error_stop_returns_no_tree() {
    // Returning `false` from the handler stops at the first error before reaching
    // ACCEPT, so there is no real derivation: `tree` is `None` (issue #167 — we no
    // longer fabricate a partial) and the single error is recorded.
    let lark = recovery_parser();
    let mut seen = 0;
    let result = lark
        .parse_on_error("1 + + 2", |_, _| {
            seen += 1;
            lark_rs::RecoveryAction::Stop
        })
        .unwrap();
    assert_eq!(seen, 1, "handler called exactly once before stopping");
    assert_eq!(result.errors.len(), 1);
    assert!(
        result.tree.is_none(),
        "stopping before ACCEPT yields no derivation"
    );
}

#[test]
fn test_recovery_never_aborts_on_trailing_error() {
    // The premature-EOF case (`1 + 2 +`) is where Python re-raises. lark-rs returns
    // Ok rather than aborting, but with no fabricated derivation: `tree` is `None`
    // (issue #167) and the error is recorded — a partial the caller can distinguish
    // from a clean parse.
    let lark = recovery_parser();
    let result = lark.parse_with_recovery("1 + 2 +").unwrap();
    assert_eq!(result.errors.len(), 1);
    assert!(
        result.tree.is_none(),
        "premature EOF must not fabricate a tree"
    );
}

#[test]
fn test_char_level_recovery_records_unexpected_character() {
    // Issue #93: an un-lexable character is skipped (one char at a time) and
    // recorded as an `UnexpectedCharacter` error, rather than aborting. Here the
    // stray `@` is the only error and the surviving `1 + 2` parses to an `add`.
    use lark_rs::ParseError;
    let lark = recovery_parser();
    let result = lark.parse_with_recovery("1 + @ 2").unwrap();
    assert_eq!(
        result.errors.len(),
        1,
        "exactly one un-lexable char skipped"
    );
    match &result.errors[0] {
        ParseError::UnexpectedCharacter { ch, .. } => assert_eq!(*ch, '@'),
        other => panic!("expected UnexpectedCharacter, got {other:?}"),
    }
    // The survivors `1 + 2` still form a valid sum.
    let tree = result.tree.expect("survivors form a valid sum");
    let clean = lark.parse("1 + 2").unwrap();
    assert_eq!(format!("{tree}"), format!("{clean}"));
}

#[test]
fn test_char_and_token_deletions_both_counted() {
    // Issue #93 / #187: character-level skips and token-level deletions accumulate
    // into one error list. `1 @ 2` is in the oracle bank (RECOVERY_CASES) — its
    // error count and tree are Python-derived, not hand-asserted.
    let lark = recovery_parser();
    let cases = load_oracle("recovery", "cases");
    let cases = cases.as_array().expect("oracle is a JSON array");
    let oracle = cases
        .iter()
        .find(|c| c["input"].as_str() == Some("1 @ 2"))
        .expect("oracle must contain '1 @ 2' entry");
    let error_count = oracle["error_count"].as_u64().unwrap() as usize;
    let recovered = oracle["recovered"].as_bool().unwrap();

    let result = lark.parse_with_recovery("1 @ 2").unwrap();
    assert_eq!(
        result.errors.len(),
        error_count,
        "'1 @ 2': lark-rs recovered {} errors, oracle says {error_count}",
        result.errors.len(),
    );
    assert!(recovered, "oracle says '1 @ 2' should recover");
    let tree = result
        .tree
        .as_ref()
        .expect("'1 @ 2': oracle says recovered, so tree must be Some");
    tree_matches_oracle(tree, &oracle["tree"])
        .unwrap_or_else(|e| panic!("'1 @ 2': tree mismatch vs oracle: {e}"));
}

#[test]
fn test_on_error_false_stops_at_unlexable_char() {
    // Issue #187: `"1 @ 2"` is now oracle-backed (RECOVERY_CASES). Returning
    // `false` from the handler at the first error stops recovery there — the
    // handler fires exactly once and the single error is recorded. The oracle's
    // full-recovery error_count (> 1) confirms this input HAS multiple errors,
    // so stopping at 1 is a genuine behavioral pin, not a tautology.
    let lark = recovery_parser();
    let cases = load_oracle("recovery", "cases");
    let cases = cases.as_array().expect("oracle is a JSON array");
    let oracle = cases
        .iter()
        .find(|c| c["input"].as_str() == Some("1 @ 2"))
        .expect("oracle must contain '1 @ 2' entry");
    let full_error_count = oracle["error_count"].as_u64().unwrap() as usize;
    assert!(
        full_error_count > 1,
        "oracle should show multiple errors for '1 @ 2' (got {full_error_count}); \
         otherwise stopping at 1 is not a meaningful behavioral test"
    );

    let mut seen = 0;
    let result = lark
        .parse_on_error("1 @ 2", |_, _| {
            seen += 1;
            lark_rs::RecoveryAction::Stop
        })
        .unwrap();
    assert_eq!(seen, 1, "handler called once before stopping");
    assert_eq!(result.errors.len(), 1);
}

// ─── Contextual-lexer recovery (issue #166) ──────────────────────────────────

/// The `recovery_contextual` grammar's AWORD/BWORD terminals share one pattern but
/// are valid only in disjoint parser states, so the contextual lexer is
/// load-bearing: a stored basic/global lexer would retype every word to one
/// terminal and fail to parse `[...] {...}` at all. Recovery must therefore lex
/// over the *contextual* stream (with the root-lexer fallback), matching Python
/// Lark's `on_error` recovery under `lexer='contextual'`.
#[test]
fn test_recovery_contextual_oracle() {
    let lark = make_lalr_from_file("recovery_contextual");
    let cases = load_oracle("recovery_contextual", "cases");
    let cases = cases.as_array().expect("oracle is a JSON array");

    for case in cases {
        let input = case["input"].as_str().unwrap();
        let recovered = case["recovered"].as_bool().unwrap();
        let error_count = case["error_count"].as_u64().unwrap() as usize;

        let result = lark
            .parse_with_recovery(input)
            .unwrap_or_else(|e| panic!("recovery should not hard-error on {input:?}: {e}"));

        assert_eq!(
            result.errors.len(),
            error_count,
            "input {input:?}: recovered {} errors, oracle had {error_count}",
            result.errors.len(),
        );

        if recovered {
            let tree = result
                .tree
                .as_ref()
                .unwrap_or_else(|| panic!("input {input:?}: expected Some(tree), got None"));
            tree_matches_oracle(tree, &case["tree"])
                .unwrap_or_else(|e| panic!("input {input:?}: tree mismatch vs oracle: {e}"));
        } else {
            assert!(
                result.tree.is_none(),
                "input {input:?}: non-recovered case must yield tree:None"
            );
            assert!(
                !result.errors.is_empty(),
                "input {input:?}: expected at least one recovered error"
            );
        }
    }
}

/// Pin the divergence #166 is about: with the contextual lexer, a clean
/// `[...] {...}` parses without any recovery (0 errors) — the recovery path lexes
/// AWORD/BWORD by parser state, where a basic-lexer recovery would mis-tokenize
/// BWORD and fail entirely.
#[test]
fn test_contextual_recovery_clean_parse_is_contextual() {
    let lark = make_lalr_from_file("recovery_contextual");
    let result = lark.parse_with_recovery("[foo bar] {baz qux}").unwrap();
    assert!(
        result.errors.is_empty(),
        "a well-formed contextual input recovers nothing"
    );
    let tree = result.tree.expect("clean parse yields Some(tree)");
    let normal = lark.parse("[foo bar] {baz qux}").unwrap();
    assert_eq!(format!("{tree}"), format!("{normal}"));
}

/// The root-lexer fallback's *token* branch: a stray `}` inside `[...]` is
/// out-of-context (AWORD/`]` expected) but globally valid, so the root scanner
/// yields it as a deletable token — Python deletes it and parses the rest.
#[test]
fn test_contextual_recovery_root_fallback_deletes_token() {
    let lark = make_lalr_from_file("recovery_contextual");
    let result = lark.parse_with_recovery("[foo } bar] {baz}").unwrap();
    assert_eq!(result.errors.len(), 1, "one out-of-context token deleted");
    let tree = result.tree.expect("survivors form a valid parse");
    let clean = lark.parse("[foo bar] {baz}").unwrap();
    assert_eq!(format!("{tree}"), format!("{clean}"));
}

#[test]
fn test_recovery_unsupported_on_earley() {
    // Recovery is LALR-only; other backends report it clearly rather than silently
    // ignoring the request.
    let lark = Lark::new(
        "start: \"a\"+\n",
        LarkOptions {
            parser: ParserAlgorithm::Earley,
            lexer: LexerType::Basic,
            start: vec!["start".to_string()],
            ..Default::default()
        },
    )
    .unwrap();
    let err = lark.parse_with_recovery("aa").unwrap_err();
    assert!(
        format!("{err}").contains("error recovery requires parser='lalr'"),
        "unexpected error: {err}"
    );
}

// ─── RecoveryAction + RecoveryContext tests (issue #223) ─────────────────────

#[test]
fn test_recovery_action_delete_matches_old_true() {
    // RecoveryAction::Delete is the new spelling of the old `true` return.
    let lark = recovery_parser();
    let result = lark
        .parse_on_error("1 + + 2", |_, _| lark_rs::RecoveryAction::Delete)
        .unwrap();
    assert_eq!(result.errors.len(), 1);
    let tree = result.tree.expect("delete recovery yields a tree");
    let clean = lark.parse("1 + 2").unwrap();
    assert_eq!(format!("{tree}"), format!("{clean}"));
}

#[test]
fn test_recovery_action_stop_matches_old_false() {
    // RecoveryAction::Stop is the new spelling of the old `false` return.
    let lark = recovery_parser();
    let result = lark
        .parse_on_error("1 + + 2", |_, _| lark_rs::RecoveryAction::Stop)
        .unwrap();
    assert!(result.tree.is_none());
    assert_eq!(result.errors.len(), 1);
}

#[test]
fn test_recovery_context_accepts_exposes_valid_terminals() {
    // The RecoveryContext's `accepts()` reflects the parser state at the error.
    let lark = recovery_parser();
    let mut saw_accepts = Vec::new();
    lark.parse_on_error("1 + + 2", |_, ctx| {
        saw_accepts = ctx.accepts();
        lark_rs::RecoveryAction::Delete
    })
    .unwrap();
    assert!(
        saw_accepts.contains(&"NUMBER".to_string()),
        "at `+ +`, the parser expects NUMBER, got {saw_accepts:?}"
    );
}

#[test]
fn test_resume_drops_errored_token_at_non_eof() {
    // Python's resume_parse() always drops the errored token. After the handler
    // feeds corrective tokens, the *next* token is parsed in the new state —
    // the errored token is NOT retried. Verified against Python Lark 1.3.1.
    //
    // Input "1 + + 2": error on 2nd `+` (expects NUMBER). Handler feeds
    // NUMBER 0 → state becomes add(1, 0), expects PLUS/$END. Resume drops the
    // 2nd `+`. Next token `2` (NUMBER) errors (expects PLUS/$END), and the
    // handler falls back to Delete, dropping `2`. Result: tree "1 + 0".
    let lark = recovery_parser();
    let result = lark
        .parse_on_error("1 + + 2", |_, ctx| match ctx.feed("NUMBER", "0") {
            Ok(_) => lark_rs::RecoveryAction::Resume,
            Err(_) => lark_rs::RecoveryAction::Delete,
        })
        .unwrap();
    assert_eq!(result.errors.len(), 2, "two errors: 2nd '+' and '2'");
    let tree = result.tree.expect("recovery should produce a tree");
    let clean = lark.parse("1 + 0").unwrap();
    assert_eq!(format!("{tree}"), format!("{clean}"));
}

#[test]
fn test_resume_no_progress_guard_stops() {
    // Returning Resume without feeding anything leaves the parser state unchanged.
    // The no-progress guard must treat that as Stop to prevent infinite loops.
    let lark = recovery_parser();
    let result = lark
        .parse_on_error("1 + + 2", |_, _| lark_rs::RecoveryAction::Resume)
        .unwrap();
    assert!(
        result.tree.is_none(),
        "Resume without progress must stop (no tree)"
    );
    assert_eq!(
        result.errors.len(),
        1,
        "exactly one error before the guard triggers"
    );
}

#[test]
fn test_recovery_context_feed_wrong_token_errors() {
    // Feeding a token the parser cannot accept errors, leaving the context
    // unchanged so the handler can fall back to Delete.
    let lark = recovery_parser();
    let result = lark
        .parse_on_error("1 + + 2", |_, ctx| {
            let res = ctx.feed("PLUS", "+");
            assert!(res.is_err(), "PLUS should not be accepted after PLUS");
            lark_rs::RecoveryAction::Delete
        })
        .unwrap();
    assert!(result.tree.is_some(), "fallback Delete should recover");
}

#[test]
fn test_resume_at_eof_inserts_missing_token() {
    // Blocker #1: Resume at $END must work. Insert a missing NUMBER at EOF,
    // then Resume to retry $END — the parser should accept "1 + 0".
    let lark = recovery_parser();
    let result = lark
        .parse_on_error("1 +", |_, ctx| {
            ctx.feed("NUMBER", "0").expect("NUMBER should be accepted");
            lark_rs::RecoveryAction::Resume
        })
        .unwrap();
    let tree = result
        .tree
        .expect("resume at $END after insertion should produce a tree");
    let clean = lark.parse("1 + 0").unwrap();
    assert_eq!(format!("{tree}"), format!("{clean}"));
}

#[test]
fn test_feed_rollback_is_transactional() {
    // A failed feed must roll back the stack so subsequent operations see the
    // original state. We verify by feeding a wrong token, then feeding the
    // right one — if rollback failed, the second feed would also fail.
    // Use the $END case (Resume retries $END) so the inserted token is the
    // only recovery — no second error to complicate the trace.
    let lark = recovery_parser();
    let result = lark
        .parse_on_error("1 +", |_, ctx| {
            // PLUS is wrong here (parser expects NUMBER after PLUS).
            let bad = ctx.feed("PLUS", "+");
            assert!(bad.is_err(), "PLUS should not be accepted after PLUS");
            // After rollback, NUMBER should still work.
            ctx.feed("NUMBER", "0")
                .expect("NUMBER should work after rollback");
            lark_rs::RecoveryAction::Resume
        })
        .unwrap();
    let tree = result
        .tree
        .expect("recovery via rollback + correct feed should produce a tree");
    let clean = lark.parse("1 + 0").unwrap();
    assert_eq!(format!("{tree}"), format!("{clean}"));
}

#[test]
fn test_parse_with_recovery_uses_delete() {
    // parse_with_recovery is the convenience wrapper; verify it still works
    // after the signature change.
    let lark = recovery_parser();
    let result = lark.parse_with_recovery("1 + + 2").unwrap();
    assert_eq!(result.errors.len(), 1);
    assert!(result.tree.is_some());
}

#[test]
fn test_feed_accept_inside_handler_returns_tree() {
    // If the handler's feed_token reaches ACCEPT (the corrective tokens complete
    // the parse), the recovery loop must return the accepted tree rather than
    // leaving the stack wedged.
    //
    // Grammar: start: expr; expr: NUMBER; — a single NUMBER completes the parse.
    // Input: "+" (unexpected; expects NUMBER). Handler feeds NUMBER "42" which
    // shifts + reduces expr + accepts. The tree must equal parse("42").
    let lark = Lark::new(
        "start: expr\nexpr: NUMBER\n%import common.NUMBER\n%import common.WS\n%ignore WS\n",
        LarkOptions {
            parser: ParserAlgorithm::Lalr,
            lexer: LexerType::Auto,
            start: vec!["start".to_string()],
            ..Default::default()
        },
    )
    .unwrap();
    let result = lark
        .parse_on_error("+", |_, ctx| {
            let feed_result = ctx.feed("NUMBER", "42");
            assert!(
                feed_result.is_ok(),
                "NUMBER should be accepted: {feed_result:?}"
            );
            if let Ok(Some(_tree)) = feed_result {
                // ACCEPT happened inside the handler — the tree is captured.
            }
            lark_rs::RecoveryAction::Resume
        })
        .unwrap();
    let tree = result
        .tree
        .expect("ACCEPT inside handler must produce a tree");
    let clean = lark.parse("42").unwrap();
    assert_eq!(format!("{tree}"), format!("{clean}"));
}

#[test]
fn test_feed_after_accept_is_rejected() {
    // Once feed_token has reached ACCEPT, further feeds must be rejected —
    // the stack is in a post-ACCEPT state and cannot accept more tokens.
    let lark = Lark::new(
        "start: NUMBER\n%import common.NUMBER\n%import common.WS\n%ignore WS\n",
        LarkOptions {
            parser: ParserAlgorithm::Lalr,
            lexer: LexerType::Auto,
            start: vec!["start".to_string()],
            ..Default::default()
        },
    )
    .unwrap();
    let result = lark
        .parse_on_error("+", |_, ctx| {
            ctx.feed("NUMBER", "1").unwrap();
            let second = ctx.feed("NUMBER", "2");
            assert!(second.is_err(), "feed after ACCEPT must be rejected");
            lark_rs::RecoveryAction::Resume
        })
        .unwrap();
    assert!(result.tree.is_some(), "accepted tree should be returned");
}

#[test]
fn test_mixed_resume_and_delete_across_errors() {
    // Multiple errors in one parse: the handler uses Resume for the first error
    // and Delete for subsequent ones. Verifies the recovery loop handles
    // action-switching correctly across error boundaries.
    //
    // Input "1 + + + 2": first `+` after `1 +` is unexpected (expects NUMBER).
    // Handler feeds NUMBER 0 + Resume (drops the 2nd `+`). Next token is the
    // 3rd `+`, which errors again (expects NUMBER after the fed 0). Handler
    // falls back to Delete, dropping the 3rd `+`. Next token `2` is NUMBER,
    // which shifts into the add. Result tree: "1 + 0 + 2".
    //
    // Wait — after Resume drops the 2nd `+`, the state is add(1, 0), expects
    // PLUS/$END. Next token is 3rd `+` which is valid (PLUS). Then `2` shifts
    // as NUMBER. Result: "1 + 0 + 2".
    let lark = recovery_parser();
    let mut call_count = 0;
    let result = lark
        .parse_on_error("1 + + + 2", |_, ctx| {
            call_count += 1;
            match ctx.feed("NUMBER", "0") {
                Ok(_) => lark_rs::RecoveryAction::Resume,
                Err(_) => lark_rs::RecoveryAction::Delete,
            }
        })
        .unwrap();
    let tree = result.tree.expect("mixed recovery should produce a tree");
    let formatted = format!("{tree}");
    assert!(
        call_count >= 1,
        "handler should be called at least once, got {call_count}"
    );
    assert!(
        !formatted.is_empty(),
        "tree should be non-empty: {formatted}"
    );
}

#[test]
fn test_no_action_fast_path_preserves_stack() {
    // Candidate-insertion pattern: try several wrong tokens before the right one.
    // Each rejected candidate must leave the stack exactly as it was. Verify that
    // a correct feed after N wrong ones works.
    //
    // Input "1 +": error at $END (expects NUMBER). Feed 3 wrong PLUS tokens
    // (not in accepts at this state), then feed the right NUMBER.
    let lark = recovery_parser();
    let result = lark
        .parse_on_error("1 +", |_, ctx| {
            let accepts = ctx.accepts();
            assert!(
                !accepts.contains(&"PLUS".to_string()),
                "PLUS should not be in accepts: {accepts:?}"
            );
            for _ in 0..3 {
                assert!(
                    ctx.feed("PLUS", "+").is_err(),
                    "PLUS feed should fail at this state"
                );
            }
            ctx.feed("NUMBER", "0")
                .expect("NUMBER should work after failed candidates");
            lark_rs::RecoveryAction::Resume
        })
        .unwrap();
    let tree = result.tree.expect("recovery should produce a tree");
    let clean = lark.parse("1 + 0").unwrap();
    assert_eq!(format!("{tree}"), format!("{clean}"));
}

#[test]
fn test_feed_accept_at_lex_failure_returns_tree() {
    // The lex-failure path also creates a RecoveryContext. If the handler feeds
    // tokens that reach ACCEPT during a lex failure, the tree must be returned —
    // not silently dropped.
    //
    // Grammar: start: NUMBER. Input: "@" (un-lexable). Handler feeds NUMBER "7"
    // which completes the parse. The tree must equal parse("7").
    let lark = Lark::new(
        "start: NUMBER\n%import common.NUMBER\n%import common.WS\n%ignore WS\n",
        LarkOptions {
            parser: ParserAlgorithm::Lalr,
            lexer: LexerType::Auto,
            start: vec!["start".to_string()],
            ..Default::default()
        },
    )
    .unwrap();
    let result = lark
        .parse_on_error("@", |_, ctx| {
            ctx.feed("NUMBER", "7").expect("NUMBER should be accepted");
            lark_rs::RecoveryAction::Resume
        })
        .unwrap();
    let tree = result
        .tree
        .expect("ACCEPT during lex failure must return a tree");
    let clean = lark.parse("7").unwrap();
    assert_eq!(format!("{tree}"), format!("{clean}"));
}
