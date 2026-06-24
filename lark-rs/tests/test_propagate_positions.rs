//! `propagate_positions` / `Tree.meta` span oracle (bug-bounty H6-5, #402).
//!
//! With `propagate_positions`, Python derives a tree's `meta` from its rule's
//! *unfiltered* children (`PropagatePositions` wraps the node builder outside the
//! child filter), so filtered punctuation (`"(" A ")"`) contributes to the span.
//! lark-rs used to compute `meta` from the *already-filtered* children, reporting a
//! span that omitted the parens (`2..6` instead of Python's `0..8`). The default
//! diffcheck strips `Tree.meta`, so this surface had never been exercised — this
//! suite is the regression net the issue called for.
//!
//! The fixture (`tools/generate_oracles.py::generate_propagate_positions`) records,
//! for each grammar+input, Python Lark's tree **with `meta`** for every legal
//! `(parser, lexer)` pairing (propagate_positions is engine-agnostic). The replay
//! holds lark-rs to Python's recorded span per pairing — not to a hardcoded number
//! — via [`common::tree_matches_oracle_with_meta`].

mod common;

use lark_rs::{Lark, LarkOptions, LexerType, ParserAlgorithm};

fn engine(spec: &str) -> (ParserAlgorithm, LexerType) {
    match spec {
        "lalr/contextual" => (ParserAlgorithm::Lalr, LexerType::Contextual),
        "lalr/basic" => (ParserAlgorithm::Lalr, LexerType::Basic),
        "earley/basic" => (ParserAlgorithm::Earley, LexerType::Basic),
        "earley/dynamic" => (ParserAlgorithm::Earley, LexerType::Dynamic),
        "cyk/basic" => (ParserAlgorithm::Cyk, LexerType::Basic),
        other => panic!("unknown engine pairing in oracle: {other}"),
    }
}

#[test]
fn propagate_positions_meta_matches_oracle() {
    let cases = common::load_oracle("propagate_positions", "cases");
    let cases = cases.as_array().expect("oracle is an array of cases");

    let mut checked = 0usize;
    let mut failures = Vec::new();
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let grammar = case["grammar"].as_str().unwrap();
        let input = case["input"].as_str().unwrap();
        let engines = case["engines"].as_object().unwrap();

        for (spec, expected) in engines {
            // Honestly skip a pairing Python itself refused to build (none today,
            // but the fixture records refusals rather than dropping them).
            if expected.get("error").is_some() {
                continue;
            }
            let (parser, lexer) = engine(spec);
            let mut opts = LarkOptions {
                parser,
                lexer,
                start: vec!["start".to_string()],
                ..Default::default()
            };
            opts.propagate_positions = true;

            let lark = match Lark::new(grammar, opts) {
                Ok(l) => l,
                Err(e) => {
                    failures.push(format!("[{name}/{spec}] build failed: {e:?}"));
                    continue;
                }
            };
            let tree = match lark.parse(input) {
                Ok(t) => t,
                Err(e) => {
                    failures.push(format!("[{name}/{spec}] parse failed: {e:?}"));
                    continue;
                }
            };
            if let Err(msg) = common::tree_matches_oracle_with_meta(&tree, expected) {
                failures.push(format!("[{name}/{spec}] {msg}"));
            }
            checked += 1;
        }
    }

    assert!(
        failures.is_empty(),
        "propagate_positions meta mismatches:\n{}",
        failures.join("\n")
    );
    // Guard against the fixture silently emptying (a regenerate that drops cases).
    assert!(
        checked >= 60,
        "expected many engine×case checks, ran {checked}"
    );
}

/// The minimal repro from the issue, pinned directly (independent of the fixture):
/// `start: "(" A ")"` on `( cafX )` must span the filtered parens (`0..8`), not the
/// kept `A` (`2..6`). Mirrors the un-#[ignore]d `h6_5` bounty test, kept here so the
/// contract is legible in this suite too.
#[test]
fn issue_402_minimal_repro_spans_filtered_parens() {
    let mut opts = LarkOptions {
        parser: ParserAlgorithm::Lalr,
        lexer: LexerType::Contextual,
        start: vec!["start".to_string()],
        ..Default::default()
    };
    opts.propagate_positions = true;
    let g = "start: \"(\" A \")\"\nA: /caf./\n%import common.WS\n%ignore WS\n";
    let lark = Lark::new(g, opts).expect("builds");
    let tree = lark.parse("( cafX )").expect("parses");
    let t = tree.as_tree().expect("a tree root");
    assert_eq!((t.meta.start_pos, t.meta.end_pos), (Some(0), Some(8)));
}

/// `propagate_positions=false` (the default) must leave the prior behavior intact:
/// lark-rs still populates `meta` from the *post-filter* children, so the span is
/// the kept `A` (`2..6`), not the filtered parens. This pins that the #402 fix is
/// gated strictly on the flag and never widens the span when it is off.
#[test]
fn propagate_positions_off_keeps_post_filter_span() {
    let opts = LarkOptions {
        parser: ParserAlgorithm::Lalr,
        lexer: LexerType::Contextual,
        start: vec!["start".to_string()],
        propagate_positions: false,
        ..Default::default()
    };
    let g = "start: \"(\" A \")\"\nA: /caf./\n%import common.WS\n%ignore WS\n";
    let lark = Lark::new(g, opts).expect("builds");
    let tree = lark.parse("( cafX )").expect("parses");
    let t = tree.as_tree().expect("a tree root");
    assert_eq!((t.meta.start_pos, t.meta.end_pos), (Some(2), Some(6)));
}
