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

/// #234 (C9): a **generated** standalone parser can drive a semantic backend
/// through its baked `OutputBuilder` seam (`Parser::parse_into`) — the seam is part
/// of the generated file's public surface, so a downstream consumer can implement a
/// builder with no dependency on lark-rs. Here an arithmetic evaluator over the
/// committed `arithmetic.rs` fixture computes an `i64` directly, materializing no
/// `Tree`. The oracle is arithmetic itself (a closed-form expected value per input),
/// and the byte-for-byte value/trace parity against the Python transformer oracle is
/// pinned by `standalone_semantic_output_matches_transformer_oracle` in
/// `src/standalone/mod.rs` (which can reach the runtime seam without codegen).
#[test]
fn generated_standalone_drives_semantic_builder() {
    use gen_arithmetic::{Meta, OutputBuilder, OutputContext, Token};

    // A semantic backend that *evaluates* the arithmetic expression during the parse,
    // carrying an `i64` on the stack instead of building a tree. Rule dispatch is by
    // the callback name the engine resolves (the `-> add`/`mul`/… aliases and the
    // expand1-collapsed origins), exactly the name world a Python transformer keys on.
    struct Eval;
    impl<'i> OutputBuilder<'i> for Eval {
        type Value = i64;
        fn token(&mut self, token: Token, _input: &'i str, _ctx: &OutputContext) -> i64 {
            // NUMBER evaluates to its integer; NAME is not exercised by the inputs
            // below (it has no numeric value), so treat a non-numeric token as 0.
            token.value.parse::<i64>().unwrap_or(0)
        }
        fn reduce(
            &mut self,
            rule: usize,
            children: &mut Vec<i64>,
            _meta: &Meta,
            ctx: &OutputContext,
        ) -> i64 {
            match ctx.callback_name(rule) {
                "add" => children[0] + children[1],
                "sub" => children[0] - children[1],
                "mul" => children[0] * children[1],
                "div" => children[0] / children[1],
                "pos" => children[0],
                "neg" => -children[0],
                // `start`/`atom`/parenthesized `expr` collapse via expand1 to a
                // single child; any surviving wrapper just forwards it.
                _ => children.first().copied().unwrap_or(0),
            }
        }
    }

    let parser = gen_arithmetic::Parser::new();
    // (input, expected value). Each is ordinary integer arithmetic with the grammar's
    // precedence — the oracle is the closed-form result.
    let cases: &[(&str, i64)] = &[
        ("1", 1),
        ("1+2", 3),
        ("1 + 2 * 3", 7),
        ("(1 + 2) * 3", 9),
        ("-5", -5),
        ("-(1 + 2)", -3),
        ("1 - 2 - 3", -4),
        ("2 * 3 / 4", 1),
        ("+7", 7),
    ];
    for (input, expected) in cases {
        let got = parser
            .parse_into(input, &mut Eval)
            .unwrap_or_else(|e| panic!("standalone parse_into failed on {input:?}: {e}"));
        assert_eq!(got, *expected, "semantic eval mismatch on {input:?}");
    }

    // The tree path stays available and byte-identical — the semantic seam is purely
    // additive (guards against parse_into cannibalizing the default parse()).
    assert!(
        matches!(parser.parse("7"), Ok(gen_arithmetic::ParseTree::Token(_))),
        "the default tree parse() must still return the expand1-collapsed bare Token"
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

/// #457 (option a): standalone bake now **accepts** `propagate_positions=true` —
/// the #425 fail-loud rejection is removed. The standalone runtime grew a
/// `Tree.meta` span (and the byte-offset fields on `Token` it is derived from), so a
/// generated parser produces real spans, byte-identical to the in-process LALR
/// engine (#402). This guards against the rejection creeping back.
#[test]
fn standalone_accepts_propagate_positions() {
    let src = "start: \"(\" NUMBER \")\"\nNUMBER: /[0-9]+/\n%ignore \" \"\n";
    let opts = LarkOptions {
        parser: ParserAlgorithm::Lalr,
        propagate_positions: true,
        ..Default::default()
    };

    // Oracle precondition: the in-process API accepts propagate_positions=true.
    assert!(
        Lark::new(src, opts.clone()).is_ok(),
        "#457 precondition: the in-process API must accept propagate_positions=true"
    );

    // The fix: standalone bake now succeeds (it used to reject under #425).
    assert!(
        generate_standalone(src, &opts).is_ok(),
        "#457: standalone bake must now accept propagate_positions=true \
         (the runtime has Tree.meta/span support)"
    );
}

// The byte-for-byte meta parity test against the in-process LALR oracle lives as a
// unit test in `src/standalone/mod.rs` (`standalone_meta_matches_in_process_lalr`),
// where it can read the shared runtime's `Tree.meta` (not part of lark-rs's public
// API) off a baked-and-run parser. This integration crate only sees the generated
// modules' public surface, so it pins the *acceptance* of propagate_positions here
// and the span values there.

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
