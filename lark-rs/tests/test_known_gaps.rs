//! Committed reproductions of known lark-rs gaps vs Python Lark.
//!
//! Each test asserts the **correct** (Python-oracle) behavior and is `#[ignore]`d,
//! so `cargo test` stays green while the gap is documented and reproducible. Run
//!
//! ```bash
//! cargo test --features perf-counters --test test_known_gaps -- --ignored  # gap 3
//! ```
//!
//! and watch them fail — that failure *is* the proof the gap exists. When a gap is
//! fixed, delete the `#[ignore]` and the test becomes a normal regression guard.
//!
//! These were surfaced while implementing Joop-Leo (#58); none is caused by it.
//! Gaps 1 and 2 pre-date it (the Leo work just walked into them); gap 3 is the
//! deliberate scope boundary of the Leo implementation.

use lark_rs::tree::{Child, ParseTree};
use lark_rs::{Ambiguity, Lark, LarkOptions, LexerType, ParserAlgorithm};

fn earley(grammar: &str, ambiguity: Ambiguity) -> Result<Lark, String> {
    Lark::new(
        grammar,
        LarkOptions {
            start: vec!["start".to_string()],
            parser: ParserAlgorithm::Earley,
            lexer: LexerType::Basic,
            ambiguity,
            ..LarkOptions::default()
        },
    )
    .map_err(|e| e.to_string())
}

// ─── Gap 1 (#62, FIXED): loader accepts the trailing-bar empty alternative ─────
//
// `a: X a |` is valid Lark: the bar with nothing after it is an empty (ε)
// production, so `a` derives `X a` or nothing. Python Lark accepts it and parses
// `"xx"` as a right-nested `a` bottoming out in an empty `a`. lark-rs's grammar
// loader (`GrammarParser`) used to reject a trailing `|`, raising a syntax error
// ("Expected value, got Some(Colon)" — it ran the empty alternative into the next
// rule). Fixed in the loader's alternation parsing (`parse_alt_after_bar`): a `|`
// followed only by a newline/EOF now lowers to an ε production. This test is now a
// regression guard. (The #58 oracles still use a named empty rule.)
#[test]
fn gap1_loader_accepts_trailing_bar_empty_alt() {
    let lark = earley("start: a\na: X a |\nX: \"x\"\n", Ambiguity::Resolve)
        .expect("Python Lark accepts a trailing-bar empty alternative; lark-rs must too");
    let tree = lark
        .parse("xx")
        .expect("'xx' must parse (a -> X a -> X X a -> X X ε)");
    // Sanity that it is the right-nested shape with an empty `a` at the bottom.
    assert!(matches!(tree, ParseTree::Tree(_)));
}

// ─── Gap 2 (#63, FIXED): explicit `_ambig` nesting on deeply ambiguous input ───
//
// For a grammar ambiguous N>2 ways over a span, Python Lark emits ONE `_ambig`
// node with all N full derivations as flat children. lark-rs used to emit a
// *binarized* (nested) `_ambig`: an ambiguous NON-transparent child stayed
// nested even at a `keep_all_tokens` position, where Python's
// `AmbiguousExpander` to_expand covers every position and lifts the child's
// `_ambig` into the parent (and `_collapse_ambig` splices `_ambig`-valued
// derivations flat at the symbol node). Fixed in the explicit forest walk
// (`expand_right`'s `lift_keep` distribution + the `_collapse_ambig` splice in
// `DerivsNext`); the full shape contract is pinned by the `ambig_flat_*`
// oracle groups in `fixtures/oracles/earley/cases.json`. This test is now a
// regression guard. Grammar: `!start: "x" start | start "x" | "x"` (both left-
// and right-recursive); for "xxx" the root `_ambig` has 4 flat children.
#[test]
fn gap2_explicit_ambig_is_flat_n_way() {
    let lark = earley(
        "!start: \"x\" start | start \"x\" | \"x\"\n",
        Ambiguity::Explicit,
    )
    .expect("grammar must build");
    let ParseTree::Tree(tree) = lark.parse("xxx").expect("'xxx' must parse") else {
        panic!("expected a tree at the root");
    };
    assert_eq!(
        tree.data, "_ambig",
        "root should be an _ambig forest for the ambiguous parse"
    );
    // Python Lark: 4 flat derivations. lark-rs currently nests → 2.
    assert_eq!(
        tree.children.len(),
        4,
        "Python Lark flattens all derivations into one _ambig (4 children for 'xxx'); \
         lark-rs nests them ({} children). Each child should be a full `start` tree.",
        tree.children.len()
    );
    for c in &tree.children {
        assert!(
            matches!(c, Child::Tree(t) if t.data == "start"),
            "each _ambig child should be a full `start` derivation, not a nested _ambig"
        );
    }
}

// ─── Gap 3 (#64, FIXED): Joop-Leo linearizes nullable-tail right recursion ─────
//
// Leo (#58) was originally restricted to STRICT right recursion: the recognized
// symbol had to be the rule's last symbol. A rule whose recursive symbol is
// followed by a nullable tail — e.g. the dangling-else `if_stmt: "if" c "then"
// stmt ("else" stmt)?`, or any `a: X a opt | X` with `opt:` nullable — has the
// recursive `a` NOT last, so `is_quasi_complete` declined and the regular
// completer ran, leaving the forest O(n²). #64 extended `is_quasi_complete` to
// admit a nullable tail and taught the Leo SPPF spine reconstruction
// (`materialize_leo_paths` + `eps_node`) to thread the skipped ε-tail
// completions through the non-complete topmost item — the subtle case upstream
// Lark's Leo never finished (lark-parser/lark#397). The forest is now LINEAR
// (≤2.3× per doubling) AND byte-identical to the non-Leo / Python-Lark ground
// truth (the `right_rec_nulltail` Earley oracle group). This test is now a
// positive linearity regression guard.
#[cfg(feature = "perf-counters")]
#[test]
fn gap3_nullable_tail_right_recursion_is_linearized() {
    use lark_rs::perf;

    // `a: X a opt | X` with `opt:` nullable — the minimal dangling-else shape.
    let lark = earley(
        "start: a\na: X a opt | X\nopt:\nX: \"x\"\n",
        Ambiguity::Resolve,
    )
    .expect("grammar must build");
    perf::set_leo_disabled(false); // Leo ON — this is about Leo's coverage, not the toggle

    let sizes = [64usize, 128, 256, 512];
    let nodes: Vec<u64> = sizes
        .iter()
        .map(|&n| {
            perf::reset();
            lark.parse(&"x".repeat(n)).expect("must parse");
            perf::forest_nodes()
        })
        .collect();

    for w in nodes.windows(2) {
        let ratio = w[1] as f64 / w[0] as f64;
        assert!(
            ratio <= 2.3,
            "nullable-tail right recursion must stay linear now that Leo covers it (#64), \
             but a doubling grew the forest {ratio:.2}× ({} → {}) — a regression: the Leo \
             completer is declining this shape again, or the spine reconstruction is \
             eagerly expanding every column's path back to O(n²). counts={nodes:?}",
            w[0],
            w[1]
        );
    }
}
