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
//! **Arm 1 — completer origin-column rescan + right recursion.** The unindexed
//! `.filter` was O(column) per completion, ~O(n³) on right recursion; the
//! per-column `waiting` index (#56) fixed the rescan factor (JSON / arith / nested
//! / left-recursion all flat per byte). The *residual* O(n²) on hand-written right
//! recursion (`a: X a | X`) — O(n²) completed items, which Python Lark shares since
//! its Leo transitives are dead code — is now removed too: the **Joop-Leo**
//! optimization (#58) records a transitive per column so the completer jumps to the
//! topmost item instead of cascading, making the completer scan **flat per byte**
//! (in fact zero) on right recursion. We gate that flatness; a relapse to the
//! quadratic cascade trips it.
//!
//! **Arm 2 — `ambiguity='explicit'` forest walk.** The issue *guessed* the culprit
//! was `expand_packed`'s `l = list.clone()` cartesian-product loop. Measuring it
//! **disproves that**: that loop is *linear* even on a transparent left-recursive
//! helper (its prefix is bounded by the rule arity). The genuine quadratic was the
//! per-symbol-node derivation-value rebuild in `symbol_derivations` — a transparent
//! helper materialized Inlines of size 1,2,…,n = O(n²) (exactly the cost #55
//! streamed away in resolve mode). #59 ports that streaming to the explicit walk: an
//! *unambiguous* transparent helper is spliced into a single shared buffer in one
//! pass (the `Stream*` frames, the explicit reuse of resolve's `Splice`/`AppendRule`)
//! instead of re-materializing each growing prefix, so the node-child count is now
//! O(total children) = flat per byte. We gate **both**: the named loop stays linear
//! (the committed disproof) and the real cost is now flat per byte (the #59 fix);
//! the cartesian product is preserved for genuine ambiguity, which the unchanged
//! `_ambig` oracles + compliance bank pin byte-for-byte.
//!
//! ## Arm 3 — cyclic explicit-mode forest→tree re-assembly (#518)
//!
//! The Arm-2 sweeps above are all *acyclic* (`x*`/`x+`). The #348 fix made
//! explicit-mode enumeration over a **cyclic** (nullable+recursive) grammar faithful
//! to Python by disabling the per-symbol `deriv_memo`/`memo` for cycle nodes and
//! governing them via the per-packed-node `packed_cache` (Python's `_cache` model).
//! `packed_cache` bounds re-*descent*, but a cyclic symbol node's derivation list is
//! still re-`assemble`d on each reach (its `deriv_memo` is never written), so a future
//! super-linearity could creep into the re-assembly path. Cyclic ambiguous grammars
//! have an *inherently exponential* distinct-derivation count (`1,1,2,8,48,352` for
//! `z: | "b" z | z z` — the true answer, not an artifact), so the gate cannot key on
//! raw total work; it keys on **per-materialized-derivation** re-assembly work
//! (`perf::explicit_assemble_children / perf::explicit_derivations`, §2.5 / BENCH.md)
//! and asserts that envelope stays flat across a size sweep — concretely, that the
//! step-over-step *growth* of the per-derivation ratio keeps decelerating (converging)
//! rather than staying large. Demonstrated to have teeth: an injected regression
//! re-assembling the accumulated derivation set on each reach (O(reaches × derivs ×
//! size)) keeps the ratio's last step high (last-step `z`=1.25, `e/f`=1.34 over the
//! `[4,6,8]` sweep) where the current bounded path converges (last-step `z`=1.04,
//! `e/f`=1.10) — the denominator is identical in both, so the rise is purely
//! re-assembly work per derivation. See [`assert_flat_per_derivation`] for the band.

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

/// A right-recursive list `a: X a | X` — the canonical minimal shape that
/// demonstrated the Arm-1 residual. Non-Leo Earley completes `a` over every suffix
/// (O(n²) completed items); the Joop-Leo optimization (#58) collapses that to a
/// flat per-byte completer scan.
const RIGHTREC_GRAMMAR: &str = "start: a\na: X a | X\nX: \"x\"\n";

/// **Realistic Leo case #1 — a right-associative binary operator.** Assignment
/// `a = b = c`, exponentiation `2 ** 3 ** 4`, type arrows `A -> B -> C` and cons
/// are *naturally* right-recursive (`?e: atom OP e | atom`) and CANNOT be written
/// with `+`/`*` (those expand to left recursion and would give the wrong, flat
/// tree — associativity is encoded in the right-nested shape). A long chain under
/// Earley is exactly the O(n²) pathology Leo removes. The input is `x=x=…=x`.
const ASSIGN_GRAMMAR: &str = "?start: a\n?a: NAME \"=\" a | NAME\nNAME: /[a-z]+/\n";

/// **Realistic Leo case #2 — a hand-written right-recursive list** with a
/// separator (`list: item "," list | item`), the shape people write from habit or
/// when the recursion must carry structure. Same O(n²)→O(n) story. Input `i,i,…,i`.
const RRLIST_GRAMMAR: &str = "start: lst\nlst: ITEM \",\" lst | ITEM\nITEM: \"i\"\n";

/// A deeply nested unambiguous grammar — a control: it is linear, so it pins the
/// claim that the Arm-1 fix keeps realistic recursion flat.
const NESTED_GRAMMAR: &str = "start: e\ne: \"(\" e \")\" | \"x\"\n";

/// `X+` expands to a *transparent left-recursive helper* — the Arm-2 shape whose
/// explicit-mode derivation-value rebuild is O(n²) (and whose `expand_packed` clone
/// loop is, contrary to the issue's guess, only linear).
const LIST_GRAMMAR: &str = "start: X+\nX: \"x\"\n";

/// **Arm 3 — the canonical cyclic (nullable + recursive) grammar (#518/#348).**
/// `z` is nullable (the empty first alternative) and self-recursive both linearly
/// (`"b" z`) and via a binary split (`z z`), so its SPPF has genuine cycles. Its
/// distinct-derivation count is exponential (`1,1,2,8,48,352`) — the textbook
/// shape the #348 cycle-governed enumeration targets. Input is `"b"×n`.
const CYCLIC_Z_GRAMMAR: &str = "z: | \"b\" z | z z\n";

/// **Arm 3 — interacting cycles (#518/#348).** Two mutually recursive nullable
/// rules whose cycles *share* SPPF nodes (the H4-10 audit case): a one-root
/// back-edge DFS mis-settles a shared node as acyclic, which is why #348 uses SCC
/// membership. Exercising the re-assembly gate over this shape pins the harder
/// cycle topology too. Input is `"d"×n`.
const CYCLIC_EF_GRAMMAR: &str = "e: e e | f | \nf: e | \"d\" f |\n";

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
    earley_start(grammar, ambiguity, "start")
}

/// Like [`earley`] but with an explicit start rule — the cyclic Arm-3 grammars use
/// `z`/`e` as their start, not `start`.
fn earley_start(grammar: &str, ambiguity: Ambiguity, start: &str) -> Lark {
    Lark::new(
        grammar,
        LarkOptions {
            start: vec![start.to_string()],
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

    // ── Arm 1 (Joop-Leo, #58): right recursion linearized — BEFORE vs AFTER ────
    // The headline of #58. For each right-recursive grammar we measure the SAME
    // engine twice: with Leo OFF (the toggle reproduces the pre-fix behavior) it
    // must be super-linear, and with Leo ON it must be linear. We key on the
    // mode-neutral *forest size* (`perf::forest_nodes`), not the completer scan:
    // Leo zeroes the scan by skipping the cascade, but the real question is whether
    // *total* work is now linear — i.e. the SPPF stopped holding O(n²) nodes. The
    // canonical `a: X a | X` plus two grammars people actually hand-write as right
    // recursion (a right-associative operator and a separated list — neither
    // expressible with `+`, which expands to flat left recursion). On a 64→512
    // sweep, a quadratic forest ~quadruples per doubling and a linear one doubles;
    // we assert OFF grows ≥3× per doubling (genuinely super-linear) and ON ≤2.3×
    // (linear), so the gate proves the fix is *necessary* and *sufficient*.
    assert_leo_before_after("right_rec", RIGHTREC_GRAMMAR, &|n| gen_x(n));
    assert_leo_before_after("assign", ASSIGN_GRAMMAR, &|n| vec!["x"; n].join("="));
    assert_leo_before_after("rrlist", RRLIST_GRAMMAR, &|n| vec!["i"; n].join(","));

    // The #58 done-when, kept explicit: the completer's own waiter-scan counter on
    // the canonical shape is now flat per byte (the old `≤ n²` ceiling tightened to
    // flat), with the size sweep as the regression net.
    {
        let p = earley(RIGHTREC_GRAMMAR, Ambiguity::Resolve);
        assert_flat_per_byte(
            "right_rec",
            &p,
            &[gen_x(64), gen_x(128), gen_x(256), gen_x(512)],
        );
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

    // ── Arm 2 (fixed, #59): node-child materialization is FLAT per byte ───────
    // The genuine explicit super-linearity used to be the per-symbol-node
    // derivation-value rebuild: a transparent left-recursive helper materialized
    // Inlines of size 1,2,…,n = O(n²) derivation children (what #55 streamed away
    // in resolve mode). #59 ports that streaming to the explicit walk — an
    // unambiguous transparent helper is now spliced into a single shared buffer in
    // one pass instead of re-materializing every growing prefix, so the total
    // node-child materialization is O(total children) = O(n), i.e. flat per byte.
    // The cartesian product is preserved for *genuine* ambiguity (the part that
    // legitimately fans out); only the single-derivation helper case streams. We
    // tighten the former `≤ n²` ceiling to a flat-per-byte envelope: a relapse to
    // the quadratic prefix rebuild makes the per-byte count climb and trips this.
    {
        let p = earley(LIST_GRAMMAR, Ambiguity::Explicit);
        let mut per_byte = Vec::new();
        for &n in &[128usize, 256, 512, 1024] {
            perf::reset();
            p.parse(&gen_x(n))
                .expect("list input must parse (explicit)");
            let children = perf::explicit_node_children();
            per_byte.push((n, children as f64 / n as f64));
        }
        let first = per_byte.first().unwrap().1;
        let last = per_byte.last().unwrap().1;
        assert!(
            last <= first * 1.6,
            "Arm 2 (#59) regression: explicit node-child materialization is NOT \
             flat per byte — grew from {first:.3} to {last:.3} children/byte across \
             the sweep (per-byte rows: {per_byte:?}); the transparent-helper \
             derivation rebuild is super-linear again (the streaming splice broke)"
        );
    }

    // ── Arm 3 (#518): cyclic explicit re-assembly is FLAT per materialized
    //    derivation ─────────────────────────────────────────────────────────────
    // A cyclic explicit node's derivation list is re-`assemble`d on each reach
    // (its `deriv_memo` is never written; #348 governs cycles via `packed_cache`).
    // The distinct-derivation count is exponential, so we gate the re-assembly work
    // *per materialized derivation* (`explicit_assemble_children /
    // explicit_derivations`) — an envelope over the output size, never raw work.
    // The current bounded path plateaus; a future regression that re-assembles a
    // memoizable subtree per reach (O(reaches × derivs × size)) makes the ratio
    // climb (demonstrated: 0.67→4.7 vs the current 0.67→1.85 over the same sweep).
    assert_flat_per_derivation("cyclic_z", CYCLIC_Z_GRAMMAR, "z", &|n| "b".repeat(n));
    assert_flat_per_derivation("cyclic_ef", CYCLIC_EF_GRAMMAR, "e", &|n| "d".repeat(n));
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

/// Arm 3 (#518): assert cyclic explicit-mode forest→tree **re-assembly** stays flat
/// *per materialized derivation*. The metric is `explicit_assemble_children /
/// explicit_derivations` — the child slots fed to `TreeOutputBuilder::assemble` in
/// the per-packed-node re-build (`DerivsNext`), amortized over the materialized
/// derivation count. A cyclic node is re-`assemble`d on each reach (no `deriv_memo`,
/// #348), and its distinct-derivation count is exponential, so only the *per-
/// derivation* ratio is meaningful (raw work is exponential by construction, §2.5).
///
/// The current bounded path makes this ratio *converge* (a flat plateau with a small
/// additive constant): the step-over-step growth of the ratio **decelerates** toward
/// 1. A future regression that re-assembles a memoizable subtree per reach
/// (O(reaches × derivs × size)) keeps the step growth high. We gate on the *last*
/// step's growth with the threshold `1.15`. Measured at the `[4, 6, 8]` sweep (debug):
/// baseline last-step `z`=1.04, `e/f`=1.10; the injected super-linear teeth last-step
/// `z`=1.25, `e/f`=1.34. So `1.15` sits between them — ~5% above the worst baseline
/// (`e/f`=1.10) and ~9% below the nearest regression (`z`=1.25). The margin is real but
/// not huge, so the full per-step trace is in the panic message to diagnose a trip; a
/// benign assemble/dedup refactor that nudges the ratio up should still land well under
/// the regression band. Sizes stay modest on purpose: the exponential derivation count
/// materializes large forests (n=8 already builds a ~870ms/8s debug parse on `z`/`e/f`,
/// n≳12 OOMs on `z`), which is exactly *why* the gate must be per-derivation, not raw.
#[cfg(feature = "perf-counters")]
fn assert_flat_per_derivation(
    label: &str,
    grammar: &str,
    start: &str,
    mk: &dyn Fn(usize) -> String,
) {
    use lark_rs::perf;

    let parser = earley_start(grammar, Ambiguity::Explicit, start);
    let sizes = [4usize, 6, 8];
    let mut ratios: Vec<(usize, f64)> = Vec::new();
    for &n in &sizes {
        let input = mk(n);
        perf::reset();
        parser
            .parse(&input)
            .unwrap_or_else(|e| panic!("{label} must parse (explicit): {e:?}"));
        let assemble = perf::explicit_assemble_children();
        let derivs = perf::explicit_derivations();
        // Both must be positive: `derivs == 0` means the cyclic grammar didn't fan
        // out (the gate would measure nothing), and `assemble == 0` would make this
        // point's ratio 0.0 — which the next step-over-step division would turn into
        // a divide-by-zero (`inf`/`NaN`, and `NaN <= 1.15` is false → spurious trip).
        // Guard both so a trip always means a real super-linearity, never a 0/0.
        assert!(
            derivs > 0 && assemble > 0,
            "{label}: degenerate measurement at n={n} (assemble={assemble}, \
             derivs={derivs}) — the cyclic grammar must actually fan out and feed \
             `assemble`, else the per-derivation ratio is undefined; check grammar/start"
        );
        ratios.push((n, assemble as f64 / derivs as f64));
    }
    // Step-over-step growth of the per-derivation ratio. A converging (flat) envelope
    // decelerates toward 1; a super-linear re-assembly keeps the steps large. Every
    // ratio is > 0 (guarded above), so the division is always well-defined.
    let steps: Vec<f64> = ratios.windows(2).map(|w| w[1].1 / w[0].1).collect();
    let last_step = *steps.last().unwrap();
    assert!(
        last_step <= 1.15,
        "Arm 3 (#518) regression: {label} cyclic explicit re-assembly is NOT flat \
         per materialized derivation — the per-derivation ratio's last step grew \
         {last_step:.4}× (envelope ≤ 1.15). Per-derivation ratios (n, asm/deriv): \
         {ratios:?}; step growths: {steps:?}. The forest→tree re-assembly path went \
         super-linear in reaches (a cyclic node is being re-assembled per reach \
         without the per-derivation bound — e.g. `packed_cache`/cycle-memo broke)."
    );
}

/// Prove Joop-Leo both *necessary* and *sufficient* on a right-recursive grammar:
/// measure the SAME engine with Leo disabled (pre-fix behavior, via the
/// `perf`-only toggle) and enabled, keying on the mode-neutral forest size. Over a
/// 64→128→256→512 sweep we check the node-count ratio per doubling: OFF must grow
/// super-linearly (≥3× per doubling — a quadratic quadruples, a linear only
/// doubles) and ON must be linear (≤2.3× per doubling). Restores the default
/// (Leo on) before returning.
#[cfg(feature = "perf-counters")]
fn assert_leo_before_after(label: &str, grammar: &str, mk: &dyn Fn(usize) -> String) {
    use lark_rs::perf;

    let sizes = [64usize, 128, 256, 512];
    let inputs: Vec<String> = sizes.iter().map(|&n| mk(n)).collect();
    let parser = earley(grammar, Ambiguity::Resolve);

    let nodes = |leo_disabled: bool| -> Vec<u64> {
        perf::set_leo_disabled(leo_disabled);
        inputs
            .iter()
            .map(|input| {
                perf::reset();
                parser
                    .parse(input)
                    .unwrap_or_else(|e| panic!("{label} must parse: {e:?}"));
                perf::forest_nodes()
            })
            .collect()
    };

    let off = nodes(true);
    let on = nodes(false);
    perf::set_leo_disabled(false); // restore default for any later measurement

    // Without Leo: super-linear. Every doubling must grow the forest by ≥3×.
    for w in off.windows(2) {
        let ratio = w[1] as f64 / w[0] as f64;
        assert!(
            ratio >= 3.0,
            "{label}: WITHOUT Leo the forest is supposed to be super-linear \
             (so the fix is *necessary*), but a doubling grew it only {ratio:.2}× \
             ({} → {}); the OFF baseline no longer demonstrates the pathology — \
             counts off={off:?}",
            w[0],
            w[1]
        );
    }

    // With Leo: linear. Every doubling must grow the forest by ≤2.3×.
    for w in on.windows(2) {
        let ratio = w[1] as f64 / w[0] as f64;
        assert!(
            ratio <= 2.3,
            "{label}: WITH Leo the forest must be linear (the fix is *sufficient*), \
             but a doubling grew it {ratio:.2}× ({} → {}) — Leo is not collapsing \
             the right-recursion spine (regression). counts on={on:?}",
            w[0],
            w[1]
        );
    }

    // And the headline number: at the largest size, Leo shrinks the forest by a
    // wide margin (sanity that OFF and ON really are the two regimes, not noise).
    let (big_off, big_on) = (*off.last().unwrap(), *on.last().unwrap());
    assert!(
        big_off >= big_on * 4,
        "{label}: expected Leo to shrink the largest forest by ≥4× (O(n²)→O(n)), \
         got off={big_off} on={big_on}"
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
