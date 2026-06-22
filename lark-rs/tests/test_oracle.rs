mod common;

use common::{load_oracle, make_lalr_from_file, replay_oracle_cases};

// All oracle replays below hold lark-rs to Python Lark's *recorded* behavior via
// [`common::replay_oracle_cases`] — both engines must agree (reject together, or
// accept to a byte-identical tree). There are no silent skips: a fixture whose
// author `should_pass` contradicts Python's `ok` is caught at generation time by
// `tools/generate_oracles.py` (#253), not papered over here.

// ─── Arithmetic oracle tests ─────────────────────────────────────────────────

#[test]
fn test_arithmetic_oracle() {
    let lark = make_lalr_from_file("arithmetic");
    let oracle = load_oracle("arithmetic", "cases");
    let cases = oracle.as_array().expect("oracle must be an array");
    let failures = replay_oracle_cases(&lark, cases, "arithmetic", &[]);
    assert!(
        failures.is_empty(),
        "Arithmetic oracle failures:\n{}",
        failures.join("\n")
    );
}

// ─── JSON oracle tests ───────────────────────────────────────────────────────

#[test]
fn test_json_oracle() {
    let lark = make_lalr_from_file("json");
    let oracle = load_oracle("json", "cases");
    let cases = oracle.as_array().expect("oracle must be an array");
    let failures = replay_oracle_cases(&lark, cases, "json", &[]);
    assert!(
        failures.is_empty(),
        "JSON oracle failures:\n{}",
        failures.join("\n")
    );
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
    let failures = replay_oracle_cases(&lark, cases, "csv", &[]);
    assert!(
        failures.is_empty(),
        "CSV oracle failures:\n{}",
        failures.join("\n")
    );
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
    let failures = replay_oracle_cases(&lark, cases, "terminal_refs", &[]);
    assert!(
        failures.is_empty(),
        "Terminal-reference oracle failures:\n{}",
        failures.join("\n")
    );
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
    let failures = replay_oracle_cases(&lark, cases, "keywords", &[]);
    assert!(
        failures.is_empty(),
        "Keyword/identifier oracle failures:\n{}",
        failures.join("\n")
    );
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
    // `3.14j`, `.5j`, `0x_1A`, `0b_1010`, `0o_17` are labeled "valid" by the case
    // author but Python Lark rejects them over THIS grammar (terminal ordering /
    // prefixed underscore). lark-rs matches Python (also rejects), so no
    // `more_permissive` entry is needed — the generator's allow-list documents the
    // expectation gap (tools/oracle_contradictions.json). If lark-rs ever started
    // *accepting* one, this replay would fail loudly rather than log INFO.
    let failures = replay_oracle_cases(&lark, cases, "python_numbers/valid", &[]);
    assert!(
        failures.is_empty(),
        "Python number (valid) oracle failures:\n{}",
        failures.join("\n")
    );
}

#[test]
fn test_python_numbers_invalid_oracle() {
    let lark = common::make_lalr(PYTHON_NUMBER_GRAMMAR);
    let oracle = load_oracle("python_numbers", "invalid");
    let cases = oracle.as_array().expect("oracle must be an array");

    let mut failures = Vec::new();
    for case in cases {
        let input = case["input"].as_str().unwrap_or("");
        if lark.parse(input).is_ok() {
            failures.push(format!(
                "input={input:?}: expected parse failure for invalid number, but parsing succeeded"
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "Python number (invalid) oracle failures:\n{}",
        failures.join("\n")
    );
}
