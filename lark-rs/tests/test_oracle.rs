mod common;

use common::{load_oracle, make_lalr_from_file, tree_matches_oracle};

// ─── Arithmetic oracle tests ─────────────────────────────────────────────────

#[test]
fn test_arithmetic_oracle() {
    let lark = make_lalr_from_file("arithmetic");
    let oracle = load_oracle("arithmetic", "cases");
    let cases = oracle.as_array().expect("oracle must be an array");

    let mut failures = Vec::new();

    for case in cases {
        let input = case["input"].as_str().unwrap_or("");
        let should_pass = case["should_pass"].as_bool().unwrap_or(false);
        let oracle_ok = case["ok"].as_bool().unwrap_or(false);

        let result = lark.parse(input);

        match (should_pass, oracle_ok, &result) {
            // Case should pass and Python Lark passed: compare trees
            (true, true, Ok(tree)) => {
                let oracle_tree = &case["tree"];
                if let Err(msg) = tree_matches_oracle(tree, oracle_tree) {
                    failures.push(format!("input={input:?}: tree mismatch: {msg}"));
                }
            }
            // Case should pass, Python Lark passed, but we failed
            (true, true, Err(e)) => {
                failures.push(format!("input={input:?}: expected parse success, got error: {e}"));
            }
            // Case should fail and we correctly fail
            (false, false, Err(_)) => {}
            // Case should fail and Python Lark failed, but we succeed
            (false, false, Ok(_tree)) => {
                failures.push(format!(
                    "input={input:?}: expected parse failure, but parsing succeeded"
                ));
            }
            // Should pass but Python Lark itself failed (skip - known limitation)
            (true, false, _) => {}
            // Should fail but Python Lark passed (shouldn't happen in our oracle)
            (false, true, _) => {}
        }
    }

    if !failures.is_empty() {
        panic!("Arithmetic oracle failures:\n{}", failures.join("\n"));
    }
}

// ─── JSON oracle tests ───────────────────────────────────────────────────────

#[test]
fn test_json_oracle() {
    let lark = make_lalr_from_file("json");
    let oracle = load_oracle("json", "cases");
    let cases = oracle.as_array().expect("oracle must be an array");

    let mut failures = Vec::new();

    for case in cases {
        let input = case["input"].as_str().unwrap_or("");
        let should_pass = case["should_pass"].as_bool().unwrap_or(false);
        let oracle_ok = case["ok"].as_bool().unwrap_or(false);

        let result = lark.parse(input);

        match (should_pass, oracle_ok, &result) {
            (true, true, Ok(tree)) => {
                let oracle_tree = &case["tree"];
                if let Err(msg) = tree_matches_oracle(tree, oracle_tree) {
                    failures.push(format!("input={input:?}: tree mismatch: {msg}"));
                }
            }
            (true, true, Err(e)) => {
                failures.push(format!("input={input:?}: expected parse success, got error: {e}"));
            }
            (false, false, Err(_)) => {}
            (false, false, Ok(_)) => {
                failures.push(format!(
                    "input={input:?}: expected parse failure, but parsing succeeded"
                ));
            }
            _ => {}
        }
    }

    if !failures.is_empty() {
        panic!("JSON oracle failures:\n{}", failures.join("\n"));
    }
}

// ─── CSV (transparent `_rule` inlining) oracle tests ─────────────────────────

/// A single-underscore rule (`_anything`) is transparent: its children splice
/// into the parent (`row`) rather than appearing as a `Tree("_anything", …)`
/// wrapper. Regression net for BUG-4.
#[test]
fn test_csv_oracle() {
    let lark = make_lalr_from_file("csv");
    let oracle = load_oracle("csv", "cases");
    let cases = oracle.as_array().expect("oracle must be an array");

    let mut failures = Vec::new();

    for case in cases {
        let input = case["input"].as_str().unwrap_or("");
        let should_pass = case["should_pass"].as_bool().unwrap_or(false);
        let oracle_ok = case["ok"].as_bool().unwrap_or(false);

        let result = lark.parse(input);

        match (should_pass, oracle_ok, &result) {
            (true, true, Ok(tree)) => {
                let oracle_tree = &case["tree"];
                if let Err(msg) = tree_matches_oracle(tree, oracle_tree) {
                    failures.push(format!("input={input:?}: tree mismatch: {msg}"));
                }
            }
            (true, true, Err(e)) => {
                failures.push(format!("input={input:?}: expected parse success, got error: {e}"));
            }
            (false, false, Err(_)) => {}
            (false, false, Ok(_)) => {
                failures.push(format!(
                    "input={input:?}: expected parse failure, but parsing succeeded"
                ));
            }
            _ => {}
        }
    }

    if !failures.is_empty() {
        panic!("CSV oracle failures:\n{}", failures.join("\n"));
    }
}

// ─── Terminal-reference oracle tests ─────────────────────────────────────────

/// Terminals that reference other terminals (`GREETING: HELLO | HOWDY`,
/// `HOWDY: HOW DY`, `WORD: LETTER+`, `HEY: "hey"i`): the referenced pattern is
/// inlined (with scoped flags), and terminals referenced only by other terminals
/// are pruned. Regression net for the terminal-algebra sprint.
#[test]
fn test_terminal_refs_oracle() {
    let lark = make_lalr_from_file("terminal_refs");
    let oracle = load_oracle("terminal_refs", "cases");
    let cases = oracle.as_array().expect("oracle must be an array");

    let mut failures = Vec::new();

    for case in cases {
        let input = case["input"].as_str().unwrap_or("");
        let should_pass = case["should_pass"].as_bool().unwrap_or(false);
        let oracle_ok = case["ok"].as_bool().unwrap_or(false);

        let result = lark.parse(input);

        match (should_pass, oracle_ok, &result) {
            (true, true, Ok(tree)) => {
                let oracle_tree = &case["tree"];
                if let Err(msg) = tree_matches_oracle(tree, oracle_tree) {
                    failures.push(format!("input={input:?}: tree mismatch: {msg}"));
                }
            }
            (true, true, Err(e)) => {
                failures.push(format!("input={input:?}: expected parse success, got error: {e}"));
            }
            (false, false, Err(_)) => {}
            (false, false, Ok(_)) => {
                failures.push(format!(
                    "input={input:?}: expected parse failure, but parsing succeeded"
                ));
            }
            _ => {}
        }
    }

    if !failures.is_empty() {
        panic!("Terminal-reference oracle failures:\n{}", failures.join("\n"));
    }
}

// ─── Keyword/identifier (maximal-munch) oracle tests ─────────────────────────

/// Reserved words must not shadow longer identifiers that merely start with them
/// ("iffy", "elsewhere"). This only holds with true maximal-munch lexing; a
/// preference-order lexer mis-tokenizes "iffy" as ["if", "fy"]. Regression net
/// for BUG-3.
#[test]
fn test_keywords_oracle() {
    let lark = make_lalr_from_file("keywords");
    let oracle = load_oracle("keywords", "cases");
    let cases = oracle.as_array().expect("oracle must be an array");

    let mut failures = Vec::new();

    for case in cases {
        let input = case["input"].as_str().unwrap_or("");
        let should_pass = case["should_pass"].as_bool().unwrap_or(false);
        let oracle_ok = case["ok"].as_bool().unwrap_or(false);

        let result = lark.parse(input);

        match (should_pass, oracle_ok, &result) {
            (true, true, Ok(tree)) => {
                let oracle_tree = &case["tree"];
                if let Err(msg) = tree_matches_oracle(tree, oracle_tree) {
                    failures.push(format!("input={input:?}: tree mismatch: {msg}"));
                }
            }
            (true, true, Err(e)) => {
                failures.push(format!("input={input:?}: expected parse success, got error: {e}"));
            }
            (false, false, Err(_)) => {}
            (false, false, Ok(_)) => {
                failures.push(format!(
                    "input={input:?}: expected parse failure, but parsing succeeded"
                ));
            }
            _ => {}
        }
    }

    if !failures.is_empty() {
        panic!("Keyword/identifier oracle failures:\n{}", failures.join("\n"));
    }
}

// ─── Python number literal oracle tests ──────────────────────────────────────

const PYTHON_NUMBER_GRAMMAR: &str = r#"
start: number+
number: INT | FLOAT | HEX | OCT | BIN | IMAG
INT: /[0-9][0-9_]*/
FLOAT: /[0-9][0-9_]*\.[0-9_]*/
     | /\.[0-9][0-9_]*/
     | /[0-9][0-9_]*[eE][+-]?[0-9][0-9_]*/
     | /[0-9][0-9_]*\.[0-9_]*[eE][+-]?[0-9][0-9_]*/
HEX: /0[xX][0-9a-fA-F][0-9a-fA-F_]*/
OCT: /0[oO][0-7][0-7_]*/
BIN: /0[bB][01][01_]*/
IMAG: /[0-9][0-9_]*[jJ]/
    | /[0-9][0-9_]*\.[0-9_]*[jJ]/
    | /\.[0-9][0-9_]*[jJ]/
%ignore /[ \t\n]+/
"#;

#[test]
fn test_python_numbers_valid_oracle() {
    let lark = common::make_lalr(PYTHON_NUMBER_GRAMMAR);
    let oracle = load_oracle("python_numbers", "valid");
    let cases = oracle.as_array().expect("oracle must be an array");

    let mut failures = Vec::new();

    for case in cases {
        let input = case["input"].as_str().unwrap_or("");
        let oracle_ok = case["ok"].as_bool().unwrap_or(false);
        let result = lark.parse(input);

        match (oracle_ok, &result) {
            (true, Ok(tree)) => {
                let oracle_tree = &case["tree"];
                if let Err(msg) = tree_matches_oracle(tree, oracle_tree) {
                    failures.push(format!("input={input:?}: tree mismatch: {msg}"));
                }
            }
            (true, Err(e)) => {
                failures.push(format!("input={input:?}: expected parse success, got: {e}"));
            }
            // Oracle says fail (Python Lark had a warning) — we just need to agree on fail
            (false, Err(_)) => {}
            (false, Ok(_)) => {
                // Python Lark failed but we succeeded — check if this is an improvement
                // or an incorrect success. Log as informational, not a hard failure.
                eprintln!("INFO: input={input:?}: Python Lark failed but Rust succeeded");
            }
        }
    }

    if !failures.is_empty() {
        panic!("Python number (valid) oracle failures:\n{}", failures.join("\n"));
    }
}

#[test]
fn test_python_numbers_invalid_oracle() {
    let lark = common::make_lalr(PYTHON_NUMBER_GRAMMAR);
    let oracle = load_oracle("python_numbers", "invalid");
    let cases = oracle.as_array().expect("oracle must be an array");

    let mut failures = Vec::new();

    for case in cases {
        let input = case["input"].as_str().unwrap_or("");
        let result = lark.parse(input);

        if result.is_ok() {
            failures.push(format!(
                "input={input:?}: expected parse failure for invalid number, but parsing succeeded"
            ));
        }
    }

    if !failures.is_empty() {
        panic!("Python number (invalid) oracle failures:\n{}", failures.join("\n"));
    }
}
