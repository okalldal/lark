//! Indenter / postlex error-recovery oracle (issue #94, sub-target 1).
//!
//! Extends single-token-deletion recovery to the LALR + Indenter (postlex) path.
//! Python Lark wires `lexer → PostLexConnector(postlex) → parser`, so its
//! `on_error`/`resume_parse` recovery operates on the *post-indenter* token stream:
//! the Indenter injects INDENT/DEDENT over the clean lex, and token-deletion
//! recovery happens downstream of that injection — a deleted token never reaches
//! the Indenter, so its bracket/indent bookkeeping cannot desync. lark-rs mirrors
//! that ordering exactly (lex with char-skip recovery → `Indenter::process` over
//! the survivors → the recovering LALR loop over the indented stream), so the
//! recovered trees and deletion/skip counts match Python byte-for-byte.
//!
//! The oracle is `indenter_recovery/cases.json`
//! (`generate_oracles.py::generate_indenter_recovery`, `lexer='basic'`,
//! `postlex=Indenter`, `on_error=lambda e: True`).

mod common;

use common::{load_oracle, tree_matches_oracle};
use lark_rs::{Indenter, Lark, LarkOptions, LexerType, ParserAlgorithm};

/// Build an LALR + Indenter parser for a grammar file with the paren token lists
/// the oracle group recorded, on a given lexer.
fn make_indenter(name: &str, lexer: LexerType, open: Vec<String>, close: Vec<String>) -> Lark {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/grammars")
        .join(format!("{name}.lark"));
    let grammar_text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("Cannot read {}: {e}", path.display()));
    Lark::new(
        &grammar_text,
        LarkOptions {
            parser: ParserAlgorithm::Lalr,
            lexer,
            start: vec!["start".to_string()],
            postlex: Some(Indenter {
                nl_type: "_NL".to_string(),
                open_paren_types: open,
                close_paren_types: close,
                indent_type: "_INDENT".to_string(),
                dedent_type: "_DEDENT".to_string(),
                tab_len: 8,
            }),
            ..Default::default()
        },
    )
    .unwrap_or_else(|e| panic!("Indenter grammar failed to load: {e}"))
}

fn strs(v: &serde_json::Value) -> Vec<String> {
    v.as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .map(|s| s.as_str().unwrap().to_string())
        .collect()
}

/// Replay one oracle group's cases against a built `lark` (a specific lexer).
fn replay_group(name: &str, lark: &Lark, lexer_label: &str, cases: &[serde_json::Value]) {
    for case in cases {
        let input = case["input"].as_str().unwrap();
        let recovered = case["recovered"].as_bool().unwrap();
        let error_count = case["error_count"].as_u64().unwrap() as usize;
        let error_kind = case["error_kind"].as_str().unwrap();
        let ctx = format!("[{name}/{lexer_label}] input {input:?}");

        // An Indenter error (DedentError) is raised *through the postlex* — Python
        // re-raises it rather than returning a recovered tree. lark-rs surfaces it as
        // a hard `LarkError` (the accumulated errors are discarded with the Err, so
        // the count is not observable on this path — we only pin that it *is* an
        // Err and names the dedent), distinct from the `Ok(tree: None)`
        // premature-$END convention. `error_count` is not checked on this path.
        if error_kind == "postlex" {
            assert!(
                !recovered,
                "{ctx}: oracle bug — postlex-error case should be recovered:false"
            );
            let err = lark
                .parse_with_recovery(input)
                .expect_err(&format!("{ctx}: an Indenter error must surface as Err"));
            assert!(
                format!("{err}").to_lowercase().contains("dedent"),
                "{ctx}: expected a dedent error, got: {err}"
            );
            continue;
        }

        let result = lark
            .parse_with_recovery(input)
            .unwrap_or_else(|e| panic!("{ctx}: recovery should not hard-error: {e}"));

        assert_eq!(
            result.errors.len(),
            error_count,
            "{ctx}: recovered {} errors, oracle recovered {error_count}",
            result.errors.len(),
        );

        if recovered {
            // Python recovered to a full tree — lark-rs must produce the same one,
            // and it must be a real `Some` derivation.
            let tree = result
                .tree
                .as_ref()
                .unwrap_or_else(|| panic!("{ctx}: expected Some(tree), got None"));
            tree_matches_oracle(tree, &case["tree"])
                .unwrap_or_else(|e| panic!("{ctx}: tree mismatch vs oracle: {e}"));
        } else {
            // Python re-raised a premature `$END`. lark-rs pins `tree: None` — no
            // fabricated derivation (issue #167) — with the errors recorded.
            assert!(
                case["tree"].is_null(),
                "{ctx}: oracle bug — recovered:false case should have tree:null"
            );
            assert!(
                result.tree.is_none(),
                "{ctx}: re-raise case must yield tree:None, not a partial"
            );
        }
    }
}

#[test]
fn test_indenter_recovery_oracle() {
    let groups = load_oracle("indenter_recovery", "cases");
    let groups = groups.as_array().expect("oracle is a JSON array of groups");

    for group in groups {
        let name = group["name"].as_str().unwrap();
        let open = strs(&group["open_paren_types"]);
        let close = strs(&group["close_paren_types"]);
        let cases = group["cases"].as_array().unwrap();

        // `indent_context` is contextual-load-bearing (NAME/VALUE overlap, split only
        // by parser state), so it builds and recovers under the contextual lexer only
        // — the `LalrContextualPostlex` streaming-indenter recovery path. The
        // `indent`/`indent_paren` groups build under both lexers, so we exercise both
        // recovery paths against the same oracle: `LalrContextualPostlex` (contextual)
        // and `LalrPostlex` (basic). Both must match Python byte-for-byte.
        let lexers: &[(LexerType, &str)] = if name == "indent_context" {
            &[(LexerType::Contextual, "contextual")]
        } else {
            &[
                (LexerType::Contextual, "contextual"),
                (LexerType::Basic, "basic"),
            ]
        };
        for (lexer, label) in lexers {
            let lark = make_indenter(name, lexer.clone(), open.clone(), close.clone());
            replay_group(name, &lark, label, cases);
        }
    }
}

/// A token-deletion recovery over an Indenter grammar: the stray `NAME` on one
/// line is deleted, and the surviving `a` parses to a `simple` — identical to a
/// clean `a\n`. This is the pin that recovery now works *with* a postlex hook
/// (the configuration #94 lifted the restriction on).
#[test]
fn test_indenter_recovery_deletes_stray_token() {
    let lark = make_indenter("indent", LexerType::Contextual, vec![], vec![]);
    let result = lark.parse_with_recovery("a a\n").unwrap();
    assert_eq!(result.errors.len(), 1, "one stray NAME deleted");
    let tree = result.tree.expect("survivors form a valid program");
    let clean = lark.parse("a\n").unwrap();
    assert_eq!(format!("{tree}"), format!("{clean}"));
}

/// An Indenter `DedentError` (a dedent to an unknown column) is raised by the
/// postlex hook itself, *before* the parser ever sees a token error — so recovery
/// never begins and the error surfaces as a hard `LarkError`, exactly as Python
/// re-raises the `DedentError` through the postlex generator without invoking
/// `on_error`.
#[test]
fn test_indenter_dedent_error_is_not_recovered() {
    let lark = make_indenter("indent", LexerType::Contextual, vec![], vec![]);
    let err = lark
        .parse_with_recovery("if x:\n    a\n   b\n")
        .expect_err("an Indenter dedent error must surface as Err, not a partial");
    assert!(
        format!("{err}").to_lowercase().contains("dedent"),
        "expected a dedent error, got: {err}"
    );
}

/// Recovery over a *contextual-load-bearing* Indenter grammar (`indent_context`):
/// `VALUE` and `NAME` share a regex and are split only by parser state, so a basic
/// lexer mis-tokenizes them. Recovery must run over the contextual stream (the
/// `LalrContextualPostlex` streaming-indenter path) — here the stray `VALUE` after
/// `x = y` is deleted and the survivors form the same `assign` tree a clean
/// `x = y\n` parses to. This is the case the basic-lexer recovery path cannot serve.
#[test]
fn test_indenter_recovery_contextual_load_bearing() {
    let lark = make_indenter("indent_context", LexerType::Contextual, vec![], vec![]);
    let result = lark.parse_with_recovery("x = y z\n").unwrap();
    assert_eq!(result.errors.len(), 1, "one stray VALUE deleted");
    let tree = result.tree.expect("survivors form a valid assign");
    let clean = lark.parse("x = y\n").unwrap();
    assert_eq!(format!("{tree}"), format!("{clean}"));
}
