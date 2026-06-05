//! CYK compliance bank: replays grammars/inputs strip-mined from Python Lark's
//! *CYK* test class (tools/extract_lark_compliance.py::run_cyk_suite) and checks
//! lark-rs's CYK backend against the captured oracle trees and construct-error
//! outcomes.
//!
//! This is the Phase-3 regression net for `parser='cyk'`, the same XFAIL-gated
//! burndown methodology the LALR and Earley banks use: the build fails only on a
//! *regression* — a case that newly fails and is not allow-listed in
//! `compliance/cyk_xfail.json`. Regenerate the allow-list after an intentional
//! change with `LARK_COMPLIANCE_WRITE_XFAIL=1 cargo test --test test_cyk_compliance`
//! and commit the (ideally shrunk) `cyk_xfail.json`.
//!
//! CYK always uses the basic lexer and always resolves ambiguity, so neither the
//! lexer nor an ambiguity dimension varies here. Equal-weight ambiguous
//! derivations, whose winner Python Lark's CYK picks by (non-deterministic) set
//! iteration order, are expected to land in the XFAIL set — exactly the
//! arbitrary-tie-break cases the project does not chase (see lark-rs/CLAUDE.md).

mod common;

use common::tree_matches_oracle;
use lark_rs::grammar::terminal::flags;
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
        Value::Array(a) => a
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect(),
        _ => vec!["start".to_string()],
    };
    let mut g_regex_flags = 0u32;
    if let Some(letters) = rec["g_regex_flags"].as_str() {
        for ch in letters.chars() {
            g_regex_flags |= match ch {
                'i' => flags::IGNORECASE,
                'm' => flags::MULTILINE,
                's' => flags::DOTALL,
                'x' => flags::VERBOSE,
                _ => 0,
            };
        }
    }
    LarkOptions {
        start,
        parser: ParserAlgorithm::Cyk,
        // CYK uses the basic lexer (the contextual lexer is LALR-state-driven);
        // the bank only records basic/auto configurations.
        lexer: LexerType::Basic,
        maybe_placeholders: rec["maybe_placeholders"].as_bool().unwrap_or(true),
        keep_all_tokens: rec["keep_all_tokens"].as_bool().unwrap_or(false),
        strict: rec["strict"].as_bool().unwrap_or(false),
        g_regex_flags,
        ..Default::default()
    }
}

fn try_build(grammar: &str, opts: LarkOptions) -> Option<Lark> {
    match catch_unwind(AssertUnwindSafe(|| Lark::new(grammar, opts))) {
        Ok(Ok(lark)) => Some(lark),
        _ => None,
    }
}

fn try_parse(lark: &Lark, input: &str) -> Option<lark_rs::ParseTree> {
    match catch_unwind(AssertUnwindSafe(|| lark.parse(input))) {
        Ok(Ok(tree)) => Some(tree),
        _ => None,
    }
}

#[test]
fn test_cyk_compliance_bank() {
    let Some(records) = load_json("cyk_bank.json") else {
        eprintln!("cyk_bank.json not found — run tools/extract_lark_compliance.py");
        return;
    };
    let records = records.as_array().expect("bank is an array");

    std::panic::set_hook(Box::new(|_| {}));

    let mut failures: BTreeSet<String> = BTreeSet::new();
    let mut total_parse = 0usize;
    let mut total_construct = 0usize;

    let trace = std::env::var("LARK_COMPLIANCE_TRACE").is_ok();
    for (ri, rec) in records.iter().enumerate() {
        let grammar = rec["grammar"].as_str().unwrap_or("");
        if trace {
            eprintln!("[{ri}] grammar={grammar:?}");
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

    let xfail: BTreeSet<String> = load_json("cyk_xfail.json")
        .and_then(|v| v.as_array().cloned())
        .map(|a| {
            a.into_iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let total = total_parse + total_construct;
    let passing = total.saturating_sub(failures.len());
    let pct = if total == 0 {
        100.0
    } else {
        100.0 * passing as f64 / total as f64
    };
    eprintln!(
        "cyk compliance bank: {passing}/{total} agree with oracle ({pct:.1}%); \
         {} known-XFAIL",
        xfail.len(),
    );

    if std::env::var("LARK_COMPLIANCE_WRITE_XFAIL").is_ok() {
        let list: Vec<&String> = failures.iter().collect();
        let path = fixtures_dir().join("cyk_xfail.json");
        std::fs::write(&path, serde_json::to_string_pretty(&list).unwrap() + "\n")
            .expect("write cyk_xfail.json");
        eprintln!(
            "wrote {} XFAIL entries to {}",
            failures.len(),
            path.display()
        );
        return;
    }

    let regressions: Vec<&String> = failures.difference(&xfail).collect();
    let fixed: Vec<&String> = xfail.difference(&failures).collect();
    if !fixed.is_empty() {
        eprintln!(
            "note: {} CYK XFAIL entries now pass — consider regenerating cyk_xfail.json",
            fixed.len()
        );
    }
    assert!(
        regressions.is_empty(),
        "cyk compliance regressions ({} cases newly failing and not in cyk_xfail.json):\n{}",
        regressions.len(),
        regressions
            .iter()
            .take(40)
            .map(|s| format!("  - {s}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}
