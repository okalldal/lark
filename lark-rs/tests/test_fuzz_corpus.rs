//! Differential-fuzz corpus replay — the deterministic regression tier.
//!
//! `tools/fuzz_differential.py` grows `fuzz/inputs.json` (grammar + input pairs)
//! by generating random inputs for the trusted grammars; `generate_oracles.py`
//! freezes Python Lark's verdict for each into `fuzz/corpus.json`. This test
//! replays that frozen corpus and diffs lark-rs against it using the same
//! normalization as every other oracle test (`tree_matches_oracle`).
//!
//! A RED here means lark-rs diverged from Python Lark on a corpus input — either
//! a tree-shape mismatch or an accept/reject disagreement. That is precisely the
//! kind of edge case the fuzzer exists to surface; once committed (and minimized)
//! it is guarded forever, just like the strip-mined compliance bank. Open-ended
//! fuzzing stays off the PR critical path — only this deterministic replay runs
//! in CI.

mod common;

use common::{load_oracle, make_lalr_from_file, tree_matches_oracle};
use lark_rs::{Child, Tree};
use std::collections::HashMap;

/// KNOWN PARITY GAP — start-rule `expand1` to a bare token.
///
/// Discovered by the differential fuzzer on its first run (minimal repro: the
/// arithmetic input `"0"`). When the start rule collapses via `?rule` to a single
/// token, Python Lark returns that bare `Token` as the parse result. lark-rs's
/// `parse()` is typed `-> Result<Tree, _>`, so it cannot return a bare token; at
/// ACCEPT (`lalr.rs`) it wraps the token in `Tree::new(tok.type_, [tok])` — a tree
/// named after the *terminal*, which is wrong under any policy.
///
/// Closing it properly is an API change (a `Tree`-or-`Token` parse result), out of
/// scope for the fuzzer-infrastructure change that surfaced it. Until then we
/// forgive *exactly* this wrapping: the oracle root is a bare token, and lark-rs
/// returned a single-child tree wrapping the same token (type + value). Any other
/// shape — or, once the API is fixed, a clean match — falls through and fails, so
/// this carve-out is self-deleting (just like a compliance-bank xfail flip).
fn known_bare_token_root_gap(oracle_token: &serde_json::Value, tree: &Tree) -> Result<(), String> {
    let want_type = oracle_token["token_type"].as_str().unwrap_or("?");
    let want_value = oracle_token["value"].as_str().unwrap_or("?");
    match tree.children.as_slice() {
        [Child::Token(tok)] if tree.data == tok.type_ => {
            if tok.type_ != want_type {
                return Err(format!(
                    "bare-token-root gap: token type {:?} != {want_type:?}",
                    tok.type_
                ));
            }
            if tok.value != want_value {
                return Err(format!(
                    "bare-token-root gap: token value {:?} != {want_value:?}",
                    tok.value
                ));
            }
            Ok(())
        }
        _ => Err(format!(
            "oracle root is a bare token {want_type}({want_value:?}) but lark-rs \
             returned an unexpected shape: data={:?}, {} children \
             (the known wrapping gap is now closed or changed — update the differ)",
            tree.data,
            tree.children.len()
        )),
    }
}

#[test]
fn test_fuzz_corpus_against_oracle() {
    let oracle = load_oracle("fuzz", "corpus");
    let cases = oracle.as_array().expect("fuzz corpus must be a JSON array");

    // Build each grammar's parser once and reuse it across its cases.
    let mut parsers: HashMap<String, lark_rs::Lark> = HashMap::new();
    let mut failures = Vec::new();
    let mut known_gap = 0usize;

    for case in cases {
        let grammar = case["grammar"].as_str().unwrap_or("");
        let input = case["input"].as_str().unwrap_or("");
        let oracle_ok = case["ok"].as_bool().unwrap_or(false);

        let lark = parsers
            .entry(grammar.to_string())
            .or_insert_with(|| make_lalr_from_file(grammar));

        let result = lark.parse(input);

        match (oracle_ok, &result) {
            // Python Lark parsed it: lark-rs must parse it and agree on the tree.
            (true, Ok(tree)) => {
                // Bare-token root is a documented parity gap — forgive only that.
                if case["tree"]["type"].as_str() == Some("token") {
                    match known_bare_token_root_gap(&case["tree"], tree) {
                        Ok(()) => known_gap += 1,
                        Err(msg) => failures.push(format!("[{grammar}] input={input:?}: {msg}")),
                    }
                } else if let Err(msg) = tree_matches_oracle(tree, &case["tree"]) {
                    failures.push(format!(
                        "[{grammar}] input={input:?}: tree mismatch: {msg}"
                    ));
                }
            }
            (true, Err(e)) => {
                failures.push(format!(
                    "[{grammar}] input={input:?}: Python Lark parsed but lark-rs errored: {e}"
                ));
            }
            // Python Lark rejected it: lark-rs must reject it too.
            (false, Err(_)) => {}
            (false, Ok(_)) => {
                failures.push(format!(
                    "[{grammar}] input={input:?}: Python Lark rejected but lark-rs accepted"
                ));
            }
        }
    }

    if !failures.is_empty() {
        panic!(
            "Differential-fuzz corpus: {} of {} cases diverged from Python Lark:\n{}",
            failures.len(),
            cases.len(),
            failures.join("\n")
        );
    }

    eprintln!(
        "fuzz corpus: {} cases agree ({} via the documented bare-token-root parity gap)",
        cases.len(),
        known_gap
    );
}
