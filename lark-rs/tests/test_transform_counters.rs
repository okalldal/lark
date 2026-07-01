//! C8b (#242) zero-tree counter gate: an **embedded transformer** driven through
//! the `OutputBuilder` `token`/`reduce` seam (`parse_into`) folds a parse into a
//! value **without materializing a generic `Tree`/`Token` graph**.
//!
//! The embedded transformer is C8b's named consumer (ADR-0039). Its *semantic*
//! parity — value + ordered callback trace over the bank — is gated by
//! `tests/test_transformer_oracle.rs` against the Python embedded-transformer
//! oracle. This file adds the missing *performance* proof the semantic gate cannot
//! see: the deterministic counters (ADR-0007) show the event path builds **no**
//! generic tree node (`tree_nodes_built == 0`) and copies **no** generic token value
//! (`token_value_string_bytes == 0`), with the default `parse()` tree backend as the
//! `> 0` positive control so the assertion is a real result, not a vacuous zero.
//!
//! The counters are process-global atomics compiled in only under `perf-counters`
//! (zero overhead otherwise), so — like the other counter gates — the whole gate is
//! one `#[test]` in its own test binary, with sequential resets and no competing
//! parse.
//!
//! Run: `cargo test --features perf-counters --test test_transform_counters`

#![cfg(feature = "perf-counters")]

use lark_rs::{
    perf, Lark, LarkOptions, LexerType, Meta, OutputBuilder, OutputContext, ParserAlgorithm, Token,
};

/// A minimal **embedded transformer**: it folds the arithmetic parse into an `i64`
/// during the parse (rule/token callbacks through the `OutputBuilder` seam), exactly
/// the C8b consumer shape — just with hand-written callbacks instead of the
/// action-spec DSL `test_transformer_oracle.rs` uses. It builds no `Tree`/`Token`.
struct EvalSink;

impl<'i> OutputBuilder<'i> for EvalSink {
    type Value = i64;

    fn token(&mut self, token: Token, _input: &'i str, _ctx: &OutputContext) -> i64 {
        // A value for every shifted terminal (the stack always needs one). Only
        // NUMBER carries meaning; punctuation ("+"/"*") is filtered per-position by
        // the engine before `reduce`, so its dummy value is never folded.
        if token.type_ == "NUMBER" {
            token.value.parse().expect("NUMBER is digits")
        } else {
            0
        }
    }

    fn reduce(
        &mut self,
        rule: usize,
        children: &mut Vec<i64>,
        _meta: &Meta,
        ctx: &OutputContext,
    ) -> i64 {
        // Dispatch on the rule's callback name (alias→template→origin), just as a
        // real transformer's `create_callback` does.
        match ctx.callback_name(rule) {
            "expr" => children.iter().sum(),
            "term" => children.iter().product(),
            // factor / start: pass the single child through.
            _ => children[0],
        }
    }
}

const ARITH: &str = r#"
    start: expr
    expr: expr "+" term
        | term
    term: term "*" factor
        | factor
    factor: NUMBER
    NUMBER: /[0-9]+/
    %ignore /\s+/
"#;

fn opts(lexer: LexerType) -> LarkOptions {
    LarkOptions {
        parser: ParserAlgorithm::Lalr,
        lexer,
        start: vec!["start".to_string()],
        ..Default::default()
    }
}

/// `1 + 1 + … + 1` with `n` ones — sums to `n`, so the fold result is a known
/// closed form and the input grows with `n` (the counters must stay at 0 regardless
/// of size, i.e. flat, not merely 0 on a tiny input).
fn ones(n: usize) -> String {
    vec!["1"; n].join(" + ")
}

#[test]
fn embedded_transform_builds_no_generic_tree() {
    assert!(
        perf::ENABLED,
        "built with perf-counters but counters report disabled"
    );

    for lexer in [LexerType::Basic, LexerType::Contextual] {
        let lark = Lark::new(ARITH, opts(lexer.clone())).expect("arith grammar builds");

        for &n in &[1usize, 2, 4, 8, 16] {
            let input = ones(n);
            perf::reset();
            let mut sink = EvalSink;
            let value = lark
                .parse_into(&input, &mut sink)
                .expect("embedded transform parse ok");

            let nodes = perf::tree_nodes_built();
            let out_bytes = perf::token_value_string_bytes();
            let reduces = perf::semantic_reduce_calls();

            // ── The C8b zero-tree gate: the event path materializes no generic
            //    `Tree` node and copies no generic `Token` value. ──────────────────
            assert_eq!(
                nodes, 0,
                "embedded transform must build NO generic Tree node (lexer={lexer:?}, n={n}); \
                 tree_nodes_built counts only the TreeOutputBuilder path"
            );
            assert_eq!(
                out_bytes, 0,
                "embedded transform must copy NO generic Token value bytes \
                 (lexer={lexer:?}, n={n}); token_value_string_bytes counts only the \
                 TreeOutputBuilder path"
            );
            // Sanity: the fold actually ran (guards against a vacuous "0 because
            // nothing happened") — reductions shaped, and the value is the closed form.
            assert!(
                reduces > 0,
                "sanity: reductions must have been shaped (lexer={lexer:?}, n={n})"
            );
            assert_eq!(
                value, n as i64,
                "sanity: `1 + … + 1` ({n} ones) folds to {n} (lexer={lexer:?})"
            );
        }

        // The product path too, on a fixed known input: 2 + 3 * 4 == 14.
        perf::reset();
        let mut sink = EvalSink;
        let value = lark.parse_into("2 + 3 * 4", &mut sink).expect("parse ok");
        assert_eq!(value, 14, "2 + 3 * 4 == 14 (lexer={lexer:?})");
        assert_eq!(
            perf::tree_nodes_built(),
            0,
            "product path builds no tree either (lexer={lexer:?})"
        );
    }

    // ── Positive control: the DEFAULT `parse()` (tree backend) over the same
    //    grammar/input DOES build generic tree nodes, so the event path's 0 is a
    //    real discriminator, not a counter that is always 0. ────────────────────
    let lark = Lark::new(ARITH, opts(LexerType::Contextual)).expect("builds");
    perf::reset();
    let _ = lark.parse("2 + 3 * 4").expect("tree parse ok");
    assert!(
        perf::tree_nodes_built() > 0,
        "positive control: the default tree backend must build generic Tree nodes \
         (got {})",
        perf::tree_nodes_built()
    );
}
