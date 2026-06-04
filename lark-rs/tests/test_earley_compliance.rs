//! Earley compliance bank: replays grammars/inputs strip-mined from Python Lark's
//! *Earley* test classes (tools/extract_lark_compliance.py::run_earley_suite) and
//! checks lark-rs against the captured oracle trees and construct-error outcomes.
//!
//! This is the regression net Phase-2 Sprints 1–4 burn down, exactly as the LALR
//! bank (test_compliance.rs) was burned from 75.6% to 99.6%. It is gated by an
//! XFAIL allow-list (compliance/earley_xfail.json): the build fails only on a
//! *regression* — a case that newly fails and is not allow-listed.
//!
//! While the Earley backend is still a stub, every entry is XFAIL (the engine
//! does not build any grammar yet). The probe `earley_unimplemented()` makes that
//! state explicit and honest: construct-error records are forced into the failure
//! set too, so nothing "passes for the wrong reason" (a build that fails because
//! Earley is unimplemented is not the same as a build that fails because the
//! grammar is invalid). Regenerate the allow-list after an intentional change with
//! `LARK_COMPLIANCE_WRITE_XFAIL=1 cargo test --test test_earley_compliance`.

mod common;

use common::{earley_unimplemented, tree_matches_oracle};
use lark_rs::grammar::terminal::flags;
use lark_rs::{Ambiguity, Lark, LarkOptions, LexerType, ParserAlgorithm};
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
    let ambiguity = match rec["ambiguity"].as_str() {
        Some("explicit") => Ambiguity::Explicit,
        Some("forest") => Ambiguity::Forest,
        _ => Ambiguity::Resolve,
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
        parser: ParserAlgorithm::Earley,
        // Earley uses the basic lexer (contextual is LALR-state-driven); the bank
        // only records basic/auto configurations.
        lexer: LexerType::Basic,
        ambiguity,
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
fn test_earley_compliance_bank() {
    let Some(records) = load_json("earley_bank.json") else {
        eprintln!("earley_bank.json not found — run tools/extract_lark_compliance.py");
        return;
    };
    let records = records.as_array().expect("bank is an array");

    // Until Earley builds anything, every entry is a uniform XFAIL.
    let unimplemented = earley_unimplemented();

    std::panic::set_hook(Box::new(|_| {}));

    let mut failures: BTreeSet<String> = BTreeSet::new();
    let mut total_parse = 0usize;
    let mut total_construct = 0usize;

    let trace = std::env::var("LARK_COMPLIANCE_TRACE").is_ok();
    for (ri, rec) in records.iter().enumerate() {
        let grammar = rec["grammar"].as_str().unwrap_or("");
        if trace {
            eprintln!(
                "[{ri}] ambiguity={:?} grammar={:?}",
                rec["ambiguity"], grammar
            );
        }
        let opts = record_options(rec);

        if rec["construct_error"].as_bool().unwrap_or(false) {
            total_construct += 1;
            // Outcome parity: Python Lark raised at construction; lark-rs must too.
            // While Earley is unimplemented, a build failure is not a *grammar*
            // rejection, so force it into the XFAIL set rather than let it count
            // as a (spurious) agreement.
            if unimplemented || try_build(grammar, opts).is_some() {
                failures.insert(format!("construct:{ri}"));
            }
            continue;
        }

        let cases = rec["cases"].as_array().map(Vec::as_slice).unwrap_or(&[]);
        if cases.is_empty() {
            continue;
        }

        let built = if unimplemented {
            None
        } else {
            try_build(grammar, opts)
        };
        let Some(lark) = built else {
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

    let xfail: BTreeSet<String> = load_json("earley_xfail.json")
        .and_then(|v| v.as_array().cloned())
        .map(|a| {
            a.into_iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let total = total_parse + total_construct;
    // `build:{ri}` markers are extra failure ids not counted in `total`, so this
    // can exceed `total` when many grammars fail to build (every one, while Earley
    // is a stub) — saturate rather than underflow.
    let passing = total.saturating_sub(failures.len());
    let pct = if total == 0 {
        100.0
    } else {
        100.0 * passing as f64 / total as f64
    };
    eprintln!(
        "earley compliance bank: {passing}/{total} agree with oracle ({pct:.1}%); \
         {} known-XFAIL{}",
        xfail.len(),
        if unimplemented {
            " (Earley backend not implemented yet)"
        } else {
            ""
        }
    );

    if std::env::var("LARK_COMPLIANCE_WRITE_XFAIL").is_ok() {
        let list: Vec<&String> = failures.iter().collect();
        let path = fixtures_dir().join("earley_xfail.json");
        std::fs::write(&path, serde_json::to_string_pretty(&list).unwrap() + "\n")
            .expect("write earley_xfail.json");
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
            "note: {} Earley XFAIL entries now pass — consider regenerating earley_xfail.json",
            fixed.len()
        );
    }
    assert!(
        regressions.is_empty(),
        "earley compliance regressions ({} cases newly failing and not in earley_xfail.json):\n{}",
        regressions.len(),
        regressions
            .iter()
            .take(40)
            .map(|s| format!("  - {s}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}
