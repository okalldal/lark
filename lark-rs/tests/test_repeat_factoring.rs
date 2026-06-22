//! #279 / bounty N9: a large bounded repeat `x~mn..mx` (`mx ≥ 50`, Python's
//! `REPEAT_BREAK_THRESHOLD`) must lower to a **factored** grammar — a logarithmic
//! stack of shared transparent sub-rules (`EBNF_to_BNF._add_repeat_rule` /
//! `_add_repeat_opt_rule`), not the naive one-rule-per-count expansion whose total
//! RHS-symbol count grows ≈ n(n+1)/2 (O(n²)).
//!
//! Two things are pinned, both oracle-grounded:
//!
//!  1. **Deterministic grammar-size gate** — the total RHS-symbol count of the
//!     lowered grammar grows *sub-quadratically* in the bound (a 4× / 8× bound must
//!     not 16× / 64× the size). This is a DETERMINISTIC resource bound counted off
//!     the lowered grammar, NOT wall-clock (PRINCIPLES §6 perf rule).
//!
//!  2. **Tree parity** — the factoring must not change the produced parse tree. The
//!     sub-rules are transparent `__anon_*` helpers, so for a kept terminal `X` the
//!     tree is a flat `start[X, X, …]` of exactly the matched-count children, and
//!     counts outside `[mn, mx]` are rejected — byte-identical to Python Lark, both
//!     across the threshold (`~49`/`~50`) and for groups (`(A B)~60`).
//!
//! "Banks-green is necessary-but-not-sufficient" here: the compliance/wild banks
//! don't exercise a `~n` with `n ≥ 50`, so these pins are the falsifiable net.

mod common;

use lark_rs::{Child, Lark, LarkOptions, LexerType, ParserAlgorithm};

fn build(grammar: &str) -> Lark {
    Lark::new(
        grammar,
        LarkOptions {
            parser: ParserAlgorithm::Lalr,
            lexer: LexerType::Contextual,
            start: vec!["start".to_string()],
            maybe_placeholders: false,
            ..Default::default()
        },
    )
    .unwrap_or_else(|e| panic!("Grammar failed to load: {e}"))
}

fn shape(c: &Child) -> String {
    match c {
        Child::Token(t) => format!("{}:{}", t.type_, t.value),
        Child::None => "_".into(),
        Child::Tree(t) => format!(
            "{}[{}]",
            t.data,
            t.children.iter().map(shape).collect::<Vec<_>>().join(",")
        ),
    }
}

fn parsed(lark: &Lark, input: &str) -> String {
    let tree = lark.parse(input).expect("parse").as_tree().unwrap().clone();
    shape(&Child::Tree(tree))
}

/// Total RHS-symbol count of the lowered grammar for `start: "x"~0..n` — the
/// deterministic size proxy the bounty finding uses.
fn total_rhs(n: usize) -> usize {
    let g = lark_rs::load_grammar(
        &format!("start: \"x\"~0..{n}\n"),
        &["start".to_string()],
        false,
        false,
    )
    .expect("grammar loads");
    lark_rs::lower(&g)
        .rules
        .iter()
        .map(|r| r.expansion.len())
        .sum()
}

#[test]
fn factored_grammar_size_is_subquadratic() {
    // A naive per-count lowering grows ≈ n²/2: doubling/quadrupling the bound
    // squares the growth (4× → ≈16×, 8× → ≈64×). Python's factoring keeps the
    // total near-flat (logarithmic). Assert the growth is far below the quadratic
    // envelope — a generous constant that still fails hard on the O(n²) lowering.
    let (r100, r200, r400, r800) = (
        total_rhs(100),
        total_rhs(200),
        total_rhs(400),
        total_rhs(800),
    );
    // Quadratic would give 4×/16× here; factored stays roughly flat. The bound is
    // deliberately loose (×3 / ×4) so it pins "sub-quadratic" without depending on
    // the exact factoring constants.
    assert!(
        r400 < r100 * 3,
        "4× bound must not ≈16× the grammar (was {r100}→{r400})"
    );
    assert!(
        r800 < r100 * 4,
        "8× bound must not ≈64× the grammar (was {r100}→{r800})"
    );
    // Monotone but slow: 800 vs 200 (a 4× bound) likewise stays well under 16×.
    assert!(
        r800 < r200 * 3,
        "4× bound (200→800) must not ≈16× the grammar (was {r200}→{r800})"
    );
}

#[test]
fn exact_repeat_tree_parity_across_threshold() {
    // `X~49` (below threshold, flat) and `X~50` (at threshold, factored) must both
    // produce a flat `start[X, …]` of exactly the count's tokens — byte-identical
    // to Python Lark, which inlines every factoring sub-rule.
    let expect = |n: usize| format!("start[{}]", vec!["X:x"; n].join(","));

    let l49 = build("start: X~49\nX: \"x\"\n");
    assert_eq!(parsed(&l49, &"x".repeat(49)), expect(49));
    assert!(l49.parse(&"x".repeat(48)).is_err(), "48 < 49 rejected");
    assert!(l49.parse(&"x".repeat(50)).is_err(), "50 > 49 rejected");

    let l50 = build("start: X~50\nX: \"x\"\n");
    assert_eq!(parsed(&l50, &"x".repeat(50)), expect(50));
    assert!(l50.parse(&"x".repeat(49)).is_err(), "49 < 50 rejected");
    assert!(l50.parse(&"x".repeat(51)).is_err(), "51 > 50 rejected");

    // A larger exact repeat exercising a deeper factor stack.
    let l60 = build("start: X~60\nX: \"x\"\n");
    assert_eq!(parsed(&l60, &"x".repeat(60)), expect(60));
    assert!(l60.parse(&"x".repeat(59)).is_err());
    assert!(l60.parse(&"x".repeat(61)).is_err());
}

#[test]
fn range_repeat_tree_parity_across_threshold() {
    let expect = |n: usize| format!("start[{}]", vec!["X:x"; n].join(","));

    // Small range (below threshold) — flat lowering, full parity at the boundaries.
    let small = build("start: X~3..7\nX: \"x\"\n");
    for n in 3..=7 {
        assert_eq!(parsed(&small, &"x".repeat(n)), expect(n));
    }
    assert!(small.parse(&"x".repeat(2)).is_err(), "2 < 3 rejected");
    assert!(small.parse(&"x".repeat(8)).is_err(), "8 > 7 rejected");

    // Range straddling the threshold (`~0..50`, factored). Check several counts
    // across the range plus both boundaries.
    let big = build("start: X~0..50\nX: \"x\"\n");
    for n in [0usize, 1, 25, 49, 50] {
        assert_eq!(parsed(&big, &"x".repeat(n)), expect(n));
    }
    assert!(big.parse(&"x".repeat(51)).is_err(), "51 > 50 rejected");

    // A wider factored range (`~0..60`).
    let big60 = build("start: X~0..60\nX: \"x\"\n");
    for n in [0usize, 17, 55, 60] {
        assert_eq!(parsed(&big60, &"x".repeat(n)), expect(n));
    }
    assert!(big60.parse(&"x".repeat(61)).is_err(), "61 > 60 rejected");
}

#[test]
fn grouped_repeat_tree_parity() {
    // `(A B)~60` over a threshold-crossing count — the inner group is one compiled
    // symbol the factoring repeats; the tree is the flat `start[A, B, A, B, …]`.
    let lark = build("start: (A B)~60\nA: \"a\"\nB: \"b\"\n");
    let expect = format!("start[{}]", vec!["A:a", "B:b"].repeat(60).join(","));
    assert_eq!(parsed(&lark, &"ab".repeat(60)), expect);
    assert!(lark.parse(&"ab".repeat(59)).is_err(), "59 groups rejected");
    assert!(lark.parse(&"ab".repeat(61)).is_err(), "61 groups rejected");
}

#[test]
fn keep_all_repeat_chunk_sharing_matches_oracle() {
    // Python Lark's `EBNF_to_BNF.rules_cache` is shared across all rules and keyed
    // WITHOUT keep-all, so the first rule to build a `~50` chunk freezes its
    // keep-all into the shared transparent sub-rule and a later sibling reuses it
    // verbatim — an order-dependent quirk. lark-rs reproduces it byte-for-byte
    // (ADR-0017: a circumstantial leak that is cheap to match → match it). The
    // oracle counts below were taken from Python Lark 1.3.1 (`maybe_placeholders=
    // False`) for the two rule orderings.
    let counts = |g: &str| -> Vec<(String, usize)> {
        let lark = build(g);
        let tree = lark
            .parse(&"x".repeat(100))
            .expect("parse")
            .as_tree()
            .unwrap()
            .clone();
        tree.children
            .iter()
            .map(|c| match c {
                Child::Tree(t) => (t.data.clone(), t.children.len()),
                _ => ("?".into(), 0),
            })
            .collect()
    };

    // keep-all rule `a` compiled FIRST → its kept-tokens leak into plain `b`
    // (Python: a:50, b:50).
    assert_eq!(
        counts("start: a b\n!a: \"x\"~50\nb: \"x\"~50\n"),
        vec![("a".to_string(), 50), ("b".to_string(), 50)],
        "keep-all-first: shared chunk keeps tokens in both (Python's quirk)"
    );
    // plain rule `b` compiled FIRST → its filtered chunk is reused by keep-all `a`
    // (Python: b:0, a:0).
    assert_eq!(
        counts("start: b a\nb: \"x\"~50\n!a: \"x\"~50\n"),
        vec![("b".to_string(), 0), ("a".to_string(), 0)],
        "plain-first: shared chunk filters tokens in both (Python's quirk)"
    );
}

#[test]
fn factored_repeat_parses_on_earley() {
    // The factored lowering is engine-agnostic — it must parse identically under
    // Earley (basic lexer), the same flat tree Python produces.
    let expect = |n: usize| format!("start[{}]", vec!["X:x"; n].join(","));
    let lark = common::make_earley("start: X~50\nX: \"x\"\n", lark_rs::Ambiguity::Resolve)
        .expect("earley builds");
    let tree = lark
        .parse(&"x".repeat(50))
        .expect("parse")
        .as_tree()
        .unwrap()
        .clone();
    assert_eq!(shape(&Child::Tree(tree)), expect(50));
    assert!(lark.parse(&"x".repeat(49)).is_err());
    assert!(lark.parse(&"x".repeat(51)).is_err());
}
