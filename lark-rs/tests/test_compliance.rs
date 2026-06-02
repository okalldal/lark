//! Compliance bank: replays grammars/inputs strip-mined from Python Lark's own
//! test suite (tools/extract_lark_compliance.py) and checks lark-rs against the
//! captured oracle trees and conflict outcomes.
//!
//! Because the bank exercises features lark-rs has not implemented yet, the test
//! is gated by an XFAIL allow-list (compliance/xfail.json): every currently
//! failing case is listed there. The build fails only on *regressions* — a case
//! that newly fails and is not in the allow-list. Set the environment variable
//! `LARK_COMPLIANCE_WRITE_XFAIL=1` to regenerate the allow-list after an
//! intentional change, then review the diff before committing.

mod common;

use common::tree_matches_oracle;
use lark_rs::{Lark, LarkOptions, LexerType, ParserAlgorithm};
use serde_json::Value;
use std::collections::BTreeSet;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/oracles/compliance")
}

fn load_json(name: &str) -> Option<Value> {
    let path = fixtures_dir().join(name);
    let text = std::fs::read_to_string(&path).ok()?;
    Some(serde_json::from_str(&text).expect("valid JSON"))
}

fn record_options(rec: &Value) -> LarkOptions {
    let start = match &rec["start"] {
        Value::String(s) => vec![s.clone()],
        Value::Array(a) => a.iter().filter_map(|v| v.as_str().map(String::from)).collect(),
        _ => vec!["start".to_string()],
    };
    let lexer = match rec["lexer"].as_str() {
        Some("basic") => LexerType::Basic,
        _ => LexerType::Contextual,
    };
    LarkOptions {
        start,
        parser: ParserAlgorithm::Lalr,
        lexer,
        maybe_placeholders: rec["maybe_placeholders"].as_bool().unwrap_or(true),
        keep_all_tokens: rec["keep_all_tokens"].as_bool().unwrap_or(false),
        ..Default::default()
    }
}

/// Build a parser, treating both errors and panics as "did not build".
fn try_build(grammar: &str, opts: LarkOptions) -> Option<Lark> {
    match catch_unwind(AssertUnwindSafe(|| Lark::new(grammar, opts))) {
        Ok(Ok(lark)) => Some(lark),
        _ => None,
    }
}

/// Parse, treating both errors and panics as "did not parse".
fn try_parse(lark: &Lark, input: &str) -> Option<lark_rs::Tree> {
    match catch_unwind(AssertUnwindSafe(|| lark.parse(input))) {
        Ok(Ok(tree)) => Some(tree),
        _ => None,
    }
}

#[test]
fn test_compliance_bank() {
    let Some(records) = load_bank_or_skip() else { return };
    let records = records.as_array().expect("bank is an array");

    // Silence panic backtraces from the many expected-to-fail grammars.
    std::panic::set_hook(Box::new(|_| {}));

    let mut failures: BTreeSet<String> = BTreeSet::new();
    let mut total_parse = 0usize;
    let mut total_construct = 0usize;

    // Grammars that abort the lark-rs process (e.g. unbounded recursion in
    // template expansion). A stack overflow cannot be caught with catch_unwind,
    // so these are skipped by content until the underlying loader bug is fixed.
    let skip: BTreeSet<String> = load_json("skip.json")
        .and_then(|v| v.as_array().cloned())
        .map(|a| a.into_iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let mut skipped = 0usize;

    let trace = std::env::var("LARK_COMPLIANCE_TRACE").is_ok();
    for (ri, rec) in records.iter().enumerate() {
        let grammar = rec["grammar"].as_str().unwrap_or("");
        if skip.contains(grammar) {
            skipped += 1;
            continue;
        }
        if trace {
            eprintln!("[{ri}] grammar={:?}", grammar);
        }
        let opts = record_options(rec);

        if rec["construct_error"].as_bool().unwrap_or(false) {
            total_construct += 1;
            // Outcome parity: Python Lark raised at construction; lark-rs must too.
            if try_build(grammar, opts).is_some() {
                failures.insert(format!("construct:{ri}"));
            }
            continue;
        }

        let cases = rec["cases"].as_array().map(Vec::as_slice).unwrap_or(&[]);
        if cases.is_empty() {
            continue;
        }

        let Some(lark) = try_build(grammar, opts) else {
            // Grammar that Python Lark built but lark-rs cannot (yet).
            failures.insert(format!("build:{ri}"));
            for ci in 0..cases.len() {
                failures.insert(format!("parse:{ri}:{ci}"));
                total_parse += 1;
            }
            continue;
        };

        for (ci, case) in cases.iter().enumerate() {
            total_parse += 1;
            let input = case["input"].as_str().unwrap_or("");
            let should_parse = case["should_parse"].as_bool().unwrap_or(false);
            let parsed = try_parse(&lark, input);

            let agree = match (should_parse, &parsed) {
                (true, Some(tree)) => tree_matches_oracle(tree, &case["tree"]).is_ok(),
                (true, None) => false,
                (false, None) => true,
                (false, Some(_)) => false,
            };
            if !agree {
                failures.insert(format!("parse:{ri}:{ci}"));
            }
        }
    }

    let xfail: BTreeSet<String> = load_json("xfail.json")
        .and_then(|v| v.as_array().cloned())
        .map(|a| a.into_iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();

    let passing = total_parse + total_construct - failures.len();
    let pct = if total_parse + total_construct == 0 {
        100.0
    } else {
        100.0 * passing as f64 / (total_parse + total_construct) as f64
    };
    eprintln!(
        "compliance bank: {passing}/{} agree with oracle ({pct:.1}%); \
         {} known-XFAIL, {skipped} skipped (process-aborting grammars)",
        total_parse + total_construct,
        xfail.len()
    );

    if std::env::var("LARK_COMPLIANCE_WRITE_XFAIL").is_ok() {
        let list: Vec<&String> = failures.iter().collect();
        let path = fixtures_dir().join("xfail.json");
        std::fs::write(&path, serde_json::to_string_pretty(&list).unwrap())
            .expect("write xfail.json");
        eprintln!("wrote {} XFAIL entries to {}", failures.len(), path.display());
        return;
    }

    // Regressions: failures that are not in the allow-list.
    let regressions: Vec<&String> = failures.difference(&xfail).collect();
    let fixed: Vec<&String> = xfail.difference(&failures).collect();
    if !fixed.is_empty() {
        eprintln!(
            "note: {} XFAIL entries now pass — consider regenerating xfail.json",
            fixed.len()
        );
    }
    assert!(
        regressions.is_empty(),
        "compliance regressions ({} cases newly failing and not in xfail.json):\n{}",
        regressions.len(),
        regressions
            .iter()
            .take(40)
            .map(|s| format!("  - {s}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

fn load_bank_or_skip() -> Option<Value> {
    match load_json("bank.json") {
        Some(v) => Some(v),
        None => {
            eprintln!("compliance bank.json not found — run tools/extract_lark_compliance.py");
            None
        }
    }
}
