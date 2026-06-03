//! Differential-fuzz corpus replay — the deterministic regression tier.
//!
//! `fuzz/inputs.json` is a small, curated set of *minimized finds* — inputs the
//! differential fuzzer found to expose a lark-rs ↔ Python-Lark divergence, not
//! random samples (a divergence on a tight input is one a human can read).
//! `generate_oracles.py` freezes Python Lark's verdict for each into
//! `fuzz/corpus.json`; this test replays that frozen corpus and diffs lark-rs
//! against it using the same normalization as every other oracle test
//! (`tree_matches_oracle`), which compares both tree roots and bare-token roots.
//!
//! A RED here means lark-rs regressed on a find — a tree-shape mismatch or an
//! accept/reject disagreement — and it is guarded forever, just like the
//! strip-mined compliance bank.
//!
//! Open-ended *discovery* stays off the PR critical path: `fuzz_differential.py`
//! runs explicitly or on a nightly schedule (`lark-rs-fuzz.yml`), generates a
//! large batch with fresh entropy, and points `LARK_FUZZ_INPUTS` at it so this
//! same replay diffs lark-rs against the batch. A nightly RED is a new find —
//! minimize it (`--minimize`), then keep it (`--record`).

mod common;

use common::{load_oracle, make_lalr_from_file, tree_matches_oracle};
use std::collections::HashMap;

#[test]
fn test_fuzz_corpus_against_oracle() {
    let oracle = load_oracle("fuzz", "corpus");
    let cases = oracle.as_array().expect("fuzz corpus must be a JSON array");

    // Build each grammar's parser once and reuse it across its cases.
    let mut parsers: HashMap<String, lark_rs::Lark> = HashMap::new();
    let mut failures = Vec::new();

    for case in cases {
        let grammar = case["grammar"].as_str().unwrap_or("");
        let input = case["input"].as_str().unwrap_or("");
        let oracle_ok = case["ok"].as_bool().unwrap_or(false);

        let lark = parsers
            .entry(grammar.to_string())
            .or_insert_with(|| make_lalr_from_file(grammar));

        let result = lark.parse(input);

        match (oracle_ok, &result) {
            // Python Lark parsed it: lark-rs must parse it and agree on the result
            // — whether the root is a tree or a bare token (expand1 collapse).
            (true, Ok(parse_tree)) => {
                if let Err(msg) = tree_matches_oracle(parse_tree, &case["tree"]) {
                    failures.push(format!("[{grammar}] input={input:?}: mismatch: {msg}"));
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

    eprintln!("fuzz corpus: {} cases agree with Python Lark", cases.len());
}
