//! Standalone parser generation (issue #42).
//!
//! Two guarantees, in the repo's "committed artifact + freshness gate" style:
//!
//!   1. **Round-trip correctness** — the committed generated parsers
//!      (`tests/standalone/*.rs`) are `include!`d here as ordinary modules, which
//!      proves they *compile* with no dependency on lark-rs (only `regex` + std),
//!      and their parse output is compared tree-for-tree against the live lark-rs
//!      engine (the oracle) on a set of inputs.
//!
//!   2. **Freshness** — regenerating from the same grammar must reproduce the
//!      committed file byte-for-byte, so the generator stays deterministic and the
//!      checked-in artifact never drifts. Run with `LARK_STANDALONE_WRITE=1` to
//!      rewrite the fixtures after an intentional generator change.

use lark_rs::{generate_standalone, Lark, LarkOptions, LexerType, ParserAlgorithm};

// The generated parsers, compiled as isolated modules. Each defines its own
// `Parser`, `Tree`, `Token`, `ParseTree` — nothing is shared with lark-rs.
mod gen_json {
    include!("standalone/json.rs");
}
mod gen_arithmetic {
    include!("standalone/arithmetic.rs");
}

/// Build the lark-rs oracle for `grammar_path` using the **basic** lexer, so the
/// only thing under test is the standalone driver (the standalone lexer is the
/// basic lexer; comparing against the contextual lexer would conflate the two).
fn oracle(grammar_path: &str) -> Lark {
    let src = std::fs::read_to_string(grammar_path).expect("grammar file");
    Lark::new(
        &src,
        LarkOptions {
            parser: ParserAlgorithm::Lalr,
            lexer: LexerType::Basic,
            ..Default::default()
        },
    )
    .expect("oracle grammar builds")
}

fn oracle_str(lark: &Lark, input: &str) -> String {
    match lark.parse(input) {
        Ok(tree) => tree.to_string(),
        Err(e) => panic!("oracle failed to parse {input:?}: {e}"),
    }
}

/// Assert the committed file equals a fresh generation of `grammar_path`.
fn assert_fresh(grammar_path: &str, committed_path: &str, starts: &[&str]) {
    let src = std::fs::read_to_string(grammar_path).expect("grammar file");
    let options = LarkOptions {
        parser: ParserAlgorithm::Lalr,
        start: starts.iter().map(|s| s.to_string()).collect(),
        ..Default::default()
    };
    let generated = generate_standalone(&src, &options).expect("generation succeeds");

    let path = concat_manifest(committed_path);
    if std::env::var("LARK_STANDALONE_WRITE").is_ok() {
        std::fs::write(&path, &generated).expect("write fixture");
        return;
    }
    let committed = std::fs::read_to_string(&path).expect("committed fixture exists");
    assert_eq!(
        committed, generated,
        "Committed standalone parser {committed_path} is stale.\n\
         Regenerate with: LARK_STANDALONE_WRITE=1 cargo test --test test_standalone"
    );
}

fn concat_manifest(rel: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
}

#[test]
fn json_standalone_matches_oracle() {
    let lark = oracle("tests/grammars/json.lark");
    let parser = gen_json::Parser::new();
    let inputs = [
        r#"42"#,
        r#""hello""#,
        r#"true"#,
        r#"false"#,
        r#"null"#,
        r#"-3.14"#,
        r#"[]"#,
        r#"[1, 2, 3]"#,
        r#"{"a": 1}"#,
        r#"{"a": 1, "b": [true, false, null]}"#,
        r#"[{"x": [1, 2]}, {"y": "z"}]"#,
    ];
    for input in inputs {
        let got = match parser.parse(input) {
            Ok(t) => t.to_string(),
            Err(e) => panic!("standalone failed to parse {input:?}: {e}"),
        };
        assert_eq!(got, oracle_str(&lark, input), "mismatch on {input:?}");
    }
}

#[test]
fn arithmetic_standalone_matches_oracle() {
    let lark = oracle("tests/grammars/arithmetic.lark");
    let parser = gen_arithmetic::Parser::new();
    let inputs = [
        "1",
        "1+2",
        "1 + 2 * 3",
        "(1 + 2) * 3",
        "-5",
        "-(1 + 2)",
        "a + b * c",
        "1 - 2 - 3",
        "2 * 3 / 4",
        "+x",
    ];
    for input in inputs {
        let got = match parser.parse(input) {
            Ok(t) => t.to_string(),
            Err(e) => panic!("standalone failed to parse {input:?}: {e}"),
        };
        assert_eq!(got, oracle_str(&lark, input), "mismatch on {input:?}");
    }
}

/// A `?start: NUMBER`-style rule that collapses to a bare token must round-trip as
/// a `Token`, not a wrapping tree — the expand1-at-root case.
#[test]
fn arithmetic_single_token_is_bare() {
    let parser = gen_arithmetic::Parser::new();
    let result = parser.parse("7").expect("parses");
    assert!(
        matches!(result, gen_arithmetic::ParseTree::Token(_)),
        "expected a bare Token for a single-number input, got a Tree"
    );
}

/// Errors surface as `Err` rather than panicking or mis-parsing.
#[test]
fn standalone_reports_errors() {
    let parser = gen_json::Parser::new();
    assert!(parser.parse("[1, 2").is_err(), "unterminated array");
    assert!(parser.parse("@").is_err(), "invalid character");
}

#[test]
fn json_fixture_is_fresh() {
    assert_fresh(
        "tests/grammars/json.lark",
        "tests/standalone/json.rs",
        &["start"],
    );
}

#[test]
fn arithmetic_fixture_is_fresh() {
    assert_fresh(
        "tests/grammars/arithmetic.lark",
        "tests/standalone/arithmetic.rs",
        &["start"],
    );
}

/// Unsupported configurations are rejected with a clear error rather than emitting
/// a broken parser.
#[test]
fn rejects_unsupported_backends() {
    let src = "start: \"a\"\n";
    let earley = LarkOptions {
        parser: ParserAlgorithm::Earley,
        ..Default::default()
    };
    assert!(
        generate_standalone(src, &earley).is_err(),
        "Earley unsupported"
    );
}

/// RC7 (#272, ADR-0013) at the standalone-generation boundary. The standalone bake
/// path now runs the same post-lowering reduce/reduce audit the live LALR build runs
/// (`bake()` → `audit_lalr_reduce_reduce`), so a grammar whose shared EBNF helpers
/// mask a reduce/reduce collision Python rejects must be rejected *at generation*,
/// not baked into a broken parser. This is the RC7 core repro (`r0*` vs `(r0)*`):
/// the live LALR build rejects it (`rc7_lalr_reduce_reduce_collision_rejected` in
/// `test_bounty_findings.rs`), and standalone generation must mirror that rejection so
/// the two LALR build paths can never diverge. Guards against a regression that drops
/// the audit call from the standalone path.
#[test]
fn rc7_standalone_generation_rejects_reduce_reduce_overshare() {
    let src = "start: r0* | (r0)*\nr0: \"a\"\n";
    let options = LarkOptions {
        parser: ParserAlgorithm::Lalr,
        ..Default::default()
    };
    assert!(
        generate_standalone(src, &options).is_err(),
        "RC7: standalone generation must reject the masked reduce/reduce over-share \
         (start: r0* | (r0)*), mirroring the live LALR build's rejection"
    );
}
