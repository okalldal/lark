//! Deterministic super-linearity regression net for the Earley engine (#56).
//!
//! `BENCH.md` keeps the wall-clock bench a *recorded trend, not a gate* — shared
//! runners are too noisy to enforce, and a flaky red perf gate gets muted. This
//! file is the noise-free analog the issue prescribes: it keys on the
//! **deterministic work counters** in [`lark_rs::perf`] and asserts a fixed scaling
//! shape (flat per byte, or a quadratic ceiling), which can actually gate.
//!
//! The counters only exist when the crate is built with `--features perf-counters`
//! (off by default, so the hot parse path carries no overhead). When the feature
//! is off this test is a single trivial pass, so `cargo test --all` stays green and
//! fast; CI runs the gating variant separately:
//!
//! ```bash
//! cargo test --features perf-counters --test test_earley_scaling
//! ```
//!
//! ## The #56 outcome these assertions pin down
//!
//! **Arm 1 — completer origin-column rescan.** Demonstrated super-linear (the
//! unindexed `.filter` was O(column) per completion, ~O(n³) on right recursion).
//! *Fixed* by the per-column `waiting` index for the *named* rescan factor: JSON /
//! arith / nested / left-recursion now keep **flat per-byte** completer scan
//! (gated). The *residual* on hand-written right recursion (`a: X a | X`) is the
//! omitted Joop-Leo optimization — non-Leo Earley builds O(n²) completed items
//! there regardless of the rescan (Python Lark shares this; its Leo transitives are
//! dead code). The index drops it ~O(n³)→O(n²); we gate that **quadratic ceiling**
//! so a regression to the cubic full-rescan is caught, and track Leo as a follow-up.
//!
//! **Arm 2 — `ambiguity='explicit'` forest walk.** The issue *guessed* the culprit
//! was `expand_packed`'s `l = list.clone()` cartesian-product loop. Measuring it
//! **disproves that**: that loop is *linear* even on a transparent left-recursive
//! helper (its prefix is bounded by the rule arity). The genuine quadratic is the
//! per-symbol-node derivation-value rebuild in `symbol_derivations` — a transparent
//! helper materializes Inlines of size 1,2,…,n = O(n²) (exactly the cost #55
//! streamed away in resolve mode, here still present). We gate **both**: the named
//! loop stays linear (the committed disproof) and the real cost stays within its
//! quadratic ceiling (characterization). The streaming fix is a tracked follow-up;
//! this PR does not claim it is fixed — being explicit about that is the whole point.

use lark_rs::{Ambiguity, Lark, LarkOptions, LexerType, ParserAlgorithm};

const JSON_GRAMMAR: &str = r#"
    ?start: value
    ?value: object
          | array
          | string
          | SIGNED_NUMBER  -> number
          | "true"         -> true
          | "false"        -> false
          | "null"         -> null
    array  : "[" [value ("," value)*] "]"
    object : "{" [pair ("," pair)*] "}"
    pair   : string ":" value
    string : ESCAPED_STRING
    %import common.ESCAPED_STRING
    %import common.SIGNED_NUMBER
    %import common.WS
    %ignore WS
"#;

const ARITH_GRAMMAR: &str = r#"
    ?start : expr
    ?expr  : expr "+" term  -> add
           | expr "-" term  -> sub
           | term
    ?term  : term "*" factor -> mul
           | term "/" factor -> div
           | factor
    ?factor : "+" factor    -> pos
            | "-" factor    -> neg
            | atom
    ?atom  : NUMBER
           | NAME
           | "(" expr ")"
    %import common.NUMBER
    %import common.CNAME -> NAME
    %import common.WS_INLINE
    %ignore WS_INLINE
"#;

/// A right-recursive list `a: X a | X` — the adversarial *unambiguous* shape that
/// demonstrated the Arm-1 residual. Non-Leo Earley completes `a` over every
/// suffix, so the chart holds O(n²) completed items.
const RIGHTREC_GRAMMAR: &str = "start: a\na: X a | X\nX: \"x\"\n";

/// A deeply nested unambiguous grammar — a control: it is linear, so it pins the
/// claim that the Arm-1 fix keeps realistic recursion flat.
const NESTED_GRAMMAR: &str = "start: e\ne: \"(\" e \")\" | \"x\"\n";

/// `X+` expands to a *transparent left-recursive helper* — the Arm-2 shape whose
/// explicit-mode derivation-value rebuild is O(n²) (and whose `expand_packed` clone
/// loop is, contrary to the issue's guess, only linear).
const LIST_GRAMMAR: &str = "start: X+\nX: \"x\"\n";

fn gen_json(records: usize, fields: usize) -> String {
    let mut s = String::from("[");
    for r in 0..records {
        if r > 0 {
            s.push(',');
        }
        s.push('{');
        for f in 0..fields {
            if f > 0 {
                s.push(',');
            }
            s.push_str(&format!(
                "\"key{f}\": {}, \"name{f}\": \"value{r}_{f}\"",
                r * 10 + f
            ));
        }
        s.push('}');
    }
    s.push(']');
    s
}

fn gen_arith(terms: usize) -> String {
    let ops = ["+", "*", "-", "/"];
    let mut s = String::from("1");
    for i in 0..terms {
        s.push(' ');
        s.push_str(ops[i % ops.len()]);
        s.push(' ');
        s.push_str(&(i % 9 + 2).to_string());
    }
    s
}

fn gen_nested(depth: usize) -> String {
    format!("{}x{}", "(".repeat(depth), ")".repeat(depth))
}

fn gen_x(n: usize) -> String {
    "x".repeat(n)
}

fn earley(grammar: &str, ambiguity: Ambiguity) -> Lark {
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
    .expect("scaling-test grammar must build")
}

/// The whole net runs as ONE test function: the `perf` counters are process-global
/// atomics, so a second `#[test]` racing in parallel would corrupt the reads. A
/// single sequential reset→parse→read loop keeps every measurement clean.
#[cfg(feature = "perf-counters")]
#[test]
fn earley_scaling_is_pinned() {
    use lark_rs::perf;

    assert!(
        perf::ENABLED,
        "test built with the perf-counters feature but counters report disabled"
    );

    // ── Arm 1 (fix): completer scan flat per byte on realistic shapes ─────────
    // JSON, arith (left recursion), nested parens. With the per-column waiting
    // index the rescan is O(matches), so per-byte cost must not grow with size.
    let json = earley(JSON_GRAMMAR, Ambiguity::Resolve);
    assert_flat_per_byte(
        "json",
        &json,
        &[
            gen_json(8, 3),
            gen_json(64, 4),
            gen_json(256, 5),
            gen_json(512, 5),
        ],
    );

    let arith = earley(ARITH_GRAMMAR, Ambiguity::Resolve);
    assert_flat_per_byte(
        "arith",
        &arith,
        &[
            gen_arith(32),
            gen_arith(128),
            gen_arith(512),
            gen_arith(1024),
        ],
    );

    let nested = earley(NESTED_GRAMMAR, Ambiguity::Resolve);
    assert_flat_per_byte(
        "nested",
        &nested,
        &[
            gen_nested(32),
            gen_nested(128),
            gen_nested(512),
            gen_nested(1024),
        ],
    );

    // ── Arm 1 (residual, characterized): right recursion ≤ O(n²) ──────────────
    // Non-Leo Earley is genuinely O(n²) here (O(n²) completed items, shared with
    // the Python reference). The index dropped it from ~O(n³) to O(n²); gate that
    // ceiling so a regression to the cubic full-column rescan is caught. NOT a
    // claim of linearity — the Leo optimization is a tracked follow-up. Measured
    // ~0.5·n², so the n² ceiling holds with margin while the old ~0.5·n³ blows it.
    {
        let p = earley(RIGHTREC_GRAMMAR, Ambiguity::Resolve);
        for &n in &[64usize, 128, 256, 512] {
            perf::reset();
            p.parse(&gen_x(n))
                .expect("right-recursion input must parse");
            let scan = perf::completer_scan_steps();
            assert!(
                scan <= (n as u64) * (n as u64),
                "Arm 1 residual regression: right-recursion completer scan {scan} at \
                 n={n} exceeds the n² ceiling {} — the O(column) full rescan has \
                 returned (the waiting index is not being used)",
                (n as u64) * (n as u64)
            );
        }
    }

    // ── Arm 2 (disproof): the named clone loop is LINEAR ──────────────────────
    // `start: X+` is a transparent left-recursive helper. The issue guessed
    // `expand_packed`'s `l = list.clone()` loop was the quadratic; it is not — it
    // copies exactly one bounded prefix per node, so the count is ~n (linear).
    {
        let p = earley(LIST_GRAMMAR, Ambiguity::Explicit);
        for &n in &[128usize, 256, 512, 1024] {
            perf::reset();
            p.parse(&gen_x(n))
                .expect("list input must parse (explicit)");
            let copies = perf::explicit_prefix_copies();
            assert!(
                copies <= 2 * n as u64,
                "Arm 2: the expand_packed clone loop is supposed to be LINEAR \
                 (the issue's guessed quadratic was disproved), but copies {copies} \
                 at n={n} exceed the linear envelope 2·n — re-investigate before \
                 trusting the root-cause note"
            );
        }
    }

    // ── Arm 2 (real cost, characterized): node rebuild ≤ O(n²) ────────────────
    // The genuine explicit super-linearity: a transparent helper materializes
    // Inlines of size 1,2,…,n = O(n²) derivation children (what #55 streamed away
    // in resolve mode, still present in explicit). Gate the quadratic ceiling so a
    // regression to worse-than-quadratic is caught; the streaming fix that would
    // make this linear is a tracked follow-up. Measured ~0.5·n².
    {
        let p = earley(LIST_GRAMMAR, Ambiguity::Explicit);
        for &n in &[128usize, 256, 512, 1024] {
            perf::reset();
            p.parse(&gen_x(n))
                .expect("list input must parse (explicit)");
            let children = perf::explicit_node_children();
            assert!(
                children <= (n as u64) * (n as u64),
                "Arm 2 residual regression: explicit node-child materialization \
                 {children} at n={n} exceeds the n² ceiling {} — the explicit walk \
                 got worse than quadratic",
                (n as u64) * (n as u64)
            );
        }
    }
}

/// Assert the completer scan stays flat per byte across a size sweep: the largest
/// input's per-byte cost is within `1.6×` of the smallest's. Super-linear growth
/// makes the per-byte cost climb and trips this; flat or decreasing passes.
#[cfg(feature = "perf-counters")]
fn assert_flat_per_byte(label: &str, parser: &Lark, inputs: &[String]) {
    use lark_rs::perf;

    let mut per_byte = Vec::new();
    for input in inputs {
        perf::reset();
        parser
            .parse(input)
            .unwrap_or_else(|e| panic!("{label} must parse: {e:?}"));
        let scan = perf::completer_scan_steps();
        per_byte.push((input.len(), scan as f64 / input.len() as f64));
    }
    let first = per_byte.first().unwrap().1;
    let last = per_byte.last().unwrap().1;
    assert!(
        last <= first * 1.6,
        "Arm 1 regression: {label} completer scan is NOT flat per byte — \
         grew from {first:.3} to {last:.3} scan/byte across the sweep \
         (per-byte rows: {per_byte:?}); the origin-column rescan is super-linear again"
    );
}

/// Without the `perf-counters` feature the counters are no-ops, so the gate cannot
/// run. Keep a visible placeholder documenting how to run it, so the file is never
/// silently empty.
#[cfg(not(feature = "perf-counters"))]
#[test]
fn earley_scaling_requires_perf_counters_feature() {
    // Intentionally trivial: `cargo test --all` stays fast; CI runs the real gate
    // with `cargo test --features perf-counters --test test_earley_scaling`.
    assert!(!lark_rs::perf::ENABLED);
}
