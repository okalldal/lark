//! Compliance milestone M8: EBNF repetition under shared recurse helpers, rule
//! priority on ambiguous alternations, and redundant nested nullable optionals.
//!
//! The unifying fix is that identical `x+`/`x*` occurrences share one recurse rule
//! (`P: x | P x`), exactly as Python Lark caches them. Sharing collapses the
//! duplicate `… -> x` reductions that were otherwise an unresolvable reduce/reduce,
//! making `a+ b | a+`, `a* b | a+`, and the rule-priority case `a.2 | b.1` (both
//! starting `"A"+`) all LALR-parseable. Separately, a `?` over an already-nullable
//! `?`/`*` helper is collapsed so `("A"?)?` does not stack two empty rules.
//!
//! Expected values come from Python Lark (the oracle); the compliance bank covers
//! these too (ids 77/78, 156/157, 160/161, 108/109), but this file pins them.

mod common;

use lark_rs::{Child, Lark, LarkError, LarkOptions, LexerType, ParserAlgorithm};

fn build(grammar: &str) -> Lark {
    build_with(grammar, true).unwrap_or_else(|e| panic!("Grammar failed to load: {e}"))
}

/// Load `grammar` at a chosen `maybe_placeholders`, returning the `Result` so a
/// rejection test can assert the build *fails* (the parity-gap cases below).
fn build_with(grammar: &str, maybe_placeholders: bool) -> Result<Lark, LarkError> {
    Lark::new(
        grammar,
        LarkOptions {
            parser: ParserAlgorithm::Lalr,
            lexer: LexerType::Contextual,
            start: vec!["start".to_string()],
            maybe_placeholders,
            ..Default::default()
        },
    )
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

#[test]
fn test_plus_shared_between_branches() {
    // `"a"+ "b" | "a"+` — both `"a"+` share one recurse rule, so this is LALR.
    let lark = build("start: \"a\"+ \"b\"\n     | \"a\"+");
    assert_eq!(parsed(&lark, "aaaa"), "start[]");
    assert_eq!(parsed(&lark, "aaaab"), "start[]");
}

#[test]
fn test_star_and_plus_share_recurse() {
    // `"a"* "b" | "a"+` — the `*` and `+` share the same recurse rule.
    let lark = build("start: \"a\"* \"b\"\n     | \"a\"+");
    assert_eq!(parsed(&lark, "aaaa"), "start[]");
    assert_eq!(parsed(&lark, "aaaab"), "start[]");
    assert_eq!(parsed(&lark, "b"), "start[]");
}

#[test]
fn test_rule_priority_disambiguates_shared_plus() {
    // `a.2: "A"+` and `b.1: "A"+ "B"?` both start with the shared `"A"+`; the
    // reduce/reduce on end-of-input is resolved by rule priority (a > b).
    let lark = build("start: a | b\na.2: \"A\"+\nb.1: \"A\"+ \"B\"?");
    assert_eq!(parsed(&lark, "AAAA"), "start[a[]]");
    assert_eq!(parsed(&lark, "AAAB"), "start[b[]]");
}

#[test]
fn test_redundant_nested_optional_collapses() {
    // `("A"?)?` is just `"A"?` — the redundant outer `?` is collapsed instead of
    // building a second ambiguous empty rule.
    let lark = build("!start: (\"A\"?)?");
    assert_eq!(parsed(&lark, "A"), "start[A:A]");
    assert_eq!(parsed(&lark, ""), "start[]");
}

#[test]
fn test_repetition_trees_unaffected() {
    // Sharing must not change ordinary repetition trees: a kept `"a"+` still yields
    // one token per repeat, and a multi-symbol group repeats as a unit.
    let plus = build("!start: \"a\"+");
    assert_eq!(parsed(&plus, "aaa"), "start[A:a,A:a,A:a]");
    let group = build("!start: (\"a\" \"b\")+");
    assert_eq!(parsed(&group, "abab"), "start[A:a,B:b,A:a,B:b]");
}

// ─── #176: bounded `~n` must inline, not mint a colliding helper rule ──────────
//
// Python Lark's `EBNF_to_BNF._generate_repeats` inlines a small `x~n..m`
// (`mx < 50`) directly into the parent expansion as one alternative per count —
// it never materializes a helper rule. lark-rs used to give every exact/range
// repeat its own `__anon_rep_*` helper, so `"d"~1` became `__anon_rep: D`
// *alongside* a sibling literal `D` alternative; both reduce on `D` in one state,
// an unresolvable reduce/reduce that Python never reports. Found by the
// `--fuzz-grammars` differential mode (#38, seed 13); expected trees are the
// Python-Lark oracle.

#[test]
fn test_exact_repeat_one_inlines_no_helper() {
    // The minimal collision core: `foo: "d"~1 | "d"`. After inlining, `~1` is just
    // `D`, the duplicate `foo -> D` alternatives dedup, and the grammar is LALR.
    let lark = build("start: foo\nfoo: \"d\"~1 | \"d\"\n");
    assert_eq!(parsed(&lark, "d"), "start[foo[]]");
}

#[test]
fn test_exact_repeat_one_keeps_token() {
    // `!start: "d"~1` keeps the single inlined token (oracle: `start[D:d]`).
    let lark = build("!start: \"d\"~1\n");
    assert_eq!(parsed(&lark, "d"), "start[D:d]");
}

#[test]
fn test_template_plus_optional_repeat_one() {
    // The full #176 repro: a template instance next to an optional rule whose body
    // contains a `"d"~1`. Python builds it cleanly; lark-rs used to reject it with a
    // spurious reduce/reduce between `__anon_rep_2` and `r0`.
    let lark = build("start: rep{r0} r0?\nr0: \"b\"+ | \"d\"~1 | \"d\"\nrep{x}: x x?\n");
    assert_eq!(parsed(&lark, "b"), "start[rep[r0[]]]");
    assert_eq!(parsed(&lark, "bb"), "start[rep[r0[]]]");
    assert_eq!(parsed(&lark, "bbb"), "start[rep[r0[]]]");
}

// ─── #210: a `*`/`+` over a group with a duplicate alternative must dedup ───────
//
// Python Lark's `EBNF_to_BNF` builds the one-or-more recurse rule from the *set*
// of inner expansions, so `("b" | "b")*` collapses to a single recurse arm.
// lark-rs's `recurse_helper` used to inline every arm verbatim, so two identical
// arms produced two byte-identical `__anon_plus_0 -> B` reductions in one state —
// an unresolvable reduce/reduce Python never reports. Found by the
// `--fuzz-grammars` differential mode (#38, seed 99); expected trees are the
// Python-Lark oracle.

#[test]
fn test_star_over_duplicate_alt_dedups() {
    // The minimal #210 core: `("b" | "b")*`. Python builds it; lark-rs used to
    // reject it with a self-collision (`__anon_plus_0 -> B` vs `__anon_plus_0 -> B`).
    let lark = build("start: (\"b\" | \"b\")*\n");
    assert_eq!(parsed(&lark, ""), "start[]");
    assert_eq!(parsed(&lark, "b"), "start[]");
    assert_eq!(parsed(&lark, "bb"), "start[]");
    assert_eq!(parsed(&lark, "bbb"), "start[]");
}

#[test]
fn test_plus_over_duplicate_alt_dedups() {
    // The `+` form of the same core: `("b" | "b")+`.
    let lark = build("start: (\"b\" | \"b\")+\n");
    assert_eq!(parsed(&lark, "b"), "start[]");
    assert_eq!(parsed(&lark, "bb"), "start[]");
}

#[test]
fn test_star_over_duplicate_alt_keeps_tokens() {
    // With `!`, the deduped recurse rule still yields one token per repeat (the
    // dedup collapses identical *rules*, not the tokens matched). Oracle:
    // `start[B:b, B:b]`.
    let lark = build("!start: (\"b\" | \"b\")*\n");
    assert_eq!(parsed(&lark, ""), "start[]");
    assert_eq!(parsed(&lark, "b"), "start[B:b]");
    assert_eq!(parsed(&lark, "bb"), "start[B:b,B:b]");
}

#[test]
fn test_seed99_template_star_duplicate_alt_builds() {
    // The full seed-99 minimized fuzzer repro: a `*` group with a duplicate `r0`
    // alternative, a template instance, and a `~1`. Python builds it cleanly;
    // lark-rs used to reject it with a self-collision in the inlined `*` recurse
    // rule. Oracle tree for "bdddcc cbb" is six `r0` children + a `rep`.
    let lark = build(
        "start: (\"b\" | r0 | r0)* r0 rep{\"b\"} | rep{r0}\n\
         r0: \"d\"~1 | \"c\"\n\
         rep{x}: x x?\n\
         %ignore \" \"\n",
    );
    assert_eq!(
        parsed(&lark, "bdddcc cbb"),
        "start[r0[],r0[],r0[],r0[],r0[],r0[],rep[]]"
    );
}

// ─── #252: colliding `[X]` optionals are rejected under *both* maybe_placeholders ─
//
// Python Lark's `EBNF_to_BNF.maybe` always emits positional `_EMPTY` markers
// (`[_EMPTY] * FindRuleSize(rule)`), independent of `maybe_placeholders` — the
// option only controls whether those markers become `None` *children* in the
// tree, not whether the markers exist. So two adjacent `[X]` optionals whose
// present/absent expansions collapse to the same symbol sequence (`[A] [A]` →
// `A A | A | A | ε`, two byte-identical `A` arms once `_EMPTY` is stripped) are
// rejected with "Rules defined twice" in *both* modes (`Rule.__eq__` ignores
// `empty_indices`). lark-rs used to zero the absent-arm placeholder structure
// when `maybe_placeholders=False`, so the colliding arms deduped silently and the
// grammar was wrongly *accepted* — more permissive than the oracle, the
// unfalsifiable direction (PRINCIPLES §2.2 corollary / ADR-0017). PR #245 fixed
// the `maybe_placeholders=True` half (#212); this closes the non-placeholder half.
//
// Counter-cases: `A? A?` (which lowers via `expr '?'` → `expansion([])`, *no*
// `_EMPTY` markers) dedups cleanly and Python *accepts* it in both modes — the
// fix must not over-reject those.

fn assert_rejected_both_modes(grammar: &str, what: &str) {
    for mp in [false, true] {
        let err = build_with(grammar, mp)
            .err()
            .unwrap_or_else(|| panic!("{what}: expected rejection at maybe_placeholders={mp}"));
        let msg = err.to_string();
        assert!(
            msg.contains("Rules defined twice"),
            "{what}: maybe_placeholders={mp} rejected with the wrong error: {msg}"
        );
    }
}

fn assert_accepted_both_modes(grammar: &str, what: &str) {
    for mp in [false, true] {
        build_with(grammar, mp)
            .unwrap_or_else(|e| panic!("{what}: maybe_placeholders={mp} should build: {e}"));
    }
}

#[test]
fn test_repeat_optional_collides_under_non_placeholders() {
    // The headline #252 repro: `[A]~2 C`. `[A]~2` ≡ `[A] [A]`, whose two single-`A`
    // present arms collide. Python rejects this under both modes; lark-rs used to
    // accept it under maybe_placeholders=False.
    assert_rejected_both_modes("start: [A]~2 C\nA: \"a\"\nC: \"c\"\n", "[A]~2 C");
}

#[test]
fn test_repeat_optional_collides_no_tail() {
    // `[A]~2` on its own — same collision, no trailing symbol.
    assert_rejected_both_modes("start: [A]~2\nA: \"a\"\n", "[A]~2");
}

#[test]
fn test_literal_optional_pair_collides() {
    // The un-repeated form the `~2` expands to: `[A] [A]`. Same rejection.
    assert_rejected_both_modes("start: [A] [A]\nA: \"a\"\n", "[A] [A]");
    assert_rejected_both_modes("start: [A] [A] C\nA: \"a\"\nC: \"c\"\n", "[A] [A] C");
}

#[test]
fn test_mixed_optional_and_maybe_collides() {
    // Mixing `?` and `[]`: `[A] A?` / `A? [A]` still collide via the maybe arm's
    // `_EMPTY` markers. Python rejects both modes.
    assert_rejected_both_modes("start: [A] A?\nA: \"a\"\n", "[A] A?");
    assert_rejected_both_modes("start: A? [A]\nA: \"a\"\n", "A? [A]");
}

#[test]
fn test_plain_optional_pair_still_accepted() {
    // Guardrail: `A? A?` lowers without `_EMPTY` markers, so its two `A` arms dedup
    // cleanly — Python *accepts* it. The #252 fix must not over-reject the `?` form.
    assert_accepted_both_modes("start: A? A?\nA: \"a\"\n", "A? A?");
}

#[test]
fn test_disjoint_optionals_still_accepted() {
    // Guardrail: distinct optionals don't collide — `[A] [B]` expands to four
    // distinct arms (`A B | A | B | ε`), accepted in both modes.
    assert_accepted_both_modes("start: [A] [B]\nA: \"a\"\nB: \"b\"\n", "[A] [B]");
}

#[test]
fn test_maybe_nested_under_optional_collides() {
    // A `[X]` nested under an outer `?` (`[A]~0..1 C` ≡ `([A])? C`) still surfaces
    // its `_EMPTY` markers onto the trailing `C`, so the absent-`[A]` and empty-`?`
    // arms both reduce to `start -> C` and collide. Python rejects under both modes.
    assert_rejected_both_modes("start: [A]~0..1 C\nA: \"a\"\nC: \"c\"\n", "[A]~0..1 C");
    assert_rejected_both_modes("start: ([A])? C\nA: \"a\"\nC: \"c\"\n", "([A])? C");
}

#[test]
fn test_lone_nested_maybe_optional_still_accepted() {
    // Guardrail: a lone `([A])?` is non-colliding (`A | ε`, the duplicate empty arms
    // collapse — Python tolerates duplicate *empty* rules), accepted under
    // maybe_placeholders=False. The collision fix must not turn the redundant empty
    // arm into a spurious second empty production.
    build_with("start: ([A])?\nA: \"a\"\n", false)
        .expect("([A])? should build under maybe_placeholders=False");
}

#[test]
fn test_large_repeat_optional_rejects_without_blowup() {
    // #252 part 2 — combinatorial blow-up. `[A]~n` distributes each copy as a
    // present + an absent (placeholder-marked) arm, so the naive product is `2^n`
    // alternatives before dedup. Python Lark has the same exponential in
    // `_generate_repeats` (`[A]~15` already takes seconds before raising). lark-rs
    // raises the same "Rules defined twice" on the *first* colliding step, so a
    // near-threshold `[A]~49` (`< REPEAT_BREAK_THRESHOLD`) is rejected in
    // sub-millisecond time. The mere fact that this test *terminates* is the gate:
    // a `2^49` materialization would never finish.
    for n in [22usize, 49] {
        let g = format!("start: [A]~{n}\nA: \"a\"\n");
        for mp in [false, true] {
            let err = build_with(&g, mp)
                .err()
                .unwrap_or_else(|| panic!("[A]~{n} (mp={mp}) must be rejected, not accepted"));
            assert!(
                err.to_string().contains("Rules defined twice"),
                "[A]~{n} (mp={mp}): wrong error: {err}"
            );
        }
    }
}

// ─── #258: `([A])?` / `[A]~0..1` under maybe_placeholders=True must *build* ────────
//
// A lone `[X]` nested under an outer `?` (`([A])?`) — or its repeat form `[A]~0..1`
// — is a *non*-colliding nullable Python accepts under both `maybe_placeholders`
// modes. lark-rs used to reject it under `maybe_placeholders=True` with "unresolvable
// LALR conflicts": the inner `[A]`'s placeholder-bearing absent arm and the outer
// wrapper's ε each minted a `start ->` empty production, a reduce/reduce Python never
// reports. The fix distributes the inner maybe's present + absent forms and lets the
// empty-arm dedup collapse the two empties to one — keeping the right `None` count.
//
// Crucially `?` and `~0..1` *differ* in that count even though they share the
// `(min: 0, max: Some(1))` AST shape: `?` is Python's `maybe()` (the empty arm
// inherits the inner `[A]`'s placeholder → `""` is `[None]`), whereas `~0..1` is
// `_generate_repeats` whose `k == 0` count is a pristine empty → `""` is `[]`. The
// `RepeatKind` tag carries that distinction (#258).

/// Parse `input` under a chosen `maybe_placeholders`, returning the tree shape so an
/// absent-case `None` count can be pinned against Python.
fn parsed_with(grammar: &str, maybe_placeholders: bool, input: &str) -> String {
    let lark = build_with(grammar, maybe_placeholders)
        .unwrap_or_else(|e| panic!("{grammar:?} (mp={maybe_placeholders}) should build: {e}"));
    parsed(&lark, input)
}

#[test]
fn test_nested_maybe_optional_builds_and_parses_under_placeholders() {
    // The headline #258 repro: `([A])?` must build under maybe_placeholders=True and
    // parse `""`→`[None]` (the inner `[A]`'s placeholder survives the outer `?`) and
    // `"a"`→`[A]`, byte-identical to Python's oracle.
    let g = "start: ([A])?\nA: \"a\"\n";
    assert_eq!(parsed_with(g, true, ""), "start[_]");
    assert_eq!(parsed_with(g, true, "a"), "start[A:a]");
    // maybe_placeholders=False: the placeholder is dropped, so `""`→`[]`.
    assert_eq!(parsed_with(g, false, ""), "start[]");
    assert_eq!(parsed_with(g, false, "a"), "start[A:a]");
}

#[test]
fn test_repeat_optional_zero_one_builds_and_parses_under_placeholders() {
    // The `~0..1` repeat form. Same acceptance, but its `k == 0` count is a pristine
    // empty — *no* placeholder — so `""`→`[]` even under maybe_placeholders=True
    // (where `([A])?` yields `[None]`). This is the load-bearing `?`-vs-`~` split.
    let g = "start: [A]~0..1\nA: \"a\"\n";
    assert_eq!(parsed_with(g, true, ""), "start[]");
    assert_eq!(parsed_with(g, true, "a"), "start[A:a]");
    assert_eq!(parsed_with(g, false, ""), "start[]");
    assert_eq!(parsed_with(g, false, "a"), "start[A:a]");
}

#[test]
fn test_nested_maybe_placeholder_sibling_shapes_match_oracle() {
    // Differential audit (#258): sibling nullable shapes vs Python under
    // maybe_placeholders=True, pinning the absent-case `None` count.
    //   `[A]?`        ≡ `([A])?` — empty is `[None]` (the `?`/`maybe` placeholder).
    assert_eq!(parsed_with("start: [A]?\nA: \"a\"\n", true, ""), "start[_]");
    //   `([A])~0..1`  ≡ `[A]~0..1` — empty is `[]` (the `~0..1` pristine `k==0`).
    assert_eq!(
        parsed_with("start: ([A])~0..1\nA: \"a\"\n", true, ""),
        "start[]"
    );
    //   `[[A]]`       — a doubly-nested maybe is one placeholder deep: empty `[None]`.
    assert_eq!(
        parsed_with("start: [[A]]\nA: \"a\"\n", true, ""),
        "start[_]"
    );
    //   `[A]~1..1`    — the lone `k==1` count is the maybe present, absent → `[None]`.
    assert_eq!(
        parsed_with("start: [A]~1..1\nA: \"a\"\n", true, ""),
        "start[_]"
    );
    //   plain `A?` / `A~0..1` carry no placeholder (no `[]`): empty is `[]`.
    assert_eq!(parsed_with("start: A?\nA: \"a\"\n", true, ""), "start[]");
    assert_eq!(
        parsed_with("start: A~0..1\nA: \"a\"\n", true, ""),
        "start[]"
    );
}

#[test]
fn test_nested_maybe_with_tail_placeholder_counts_match_oracle() {
    // `([A] B)?` is non-colliding (`A B | B | ε`), and the absent-`A` present arm
    // carries one leading `None` before `B`. Pin the placeholder positions vs Python:
    //   `"ab"`→`[A, B]`, `"b"`→`[None, B]`, `""`→`[]` (the outer `?` pristine ε).
    let g = "start: ([A] B)?\nA: \"a\"\nB: \"b\"\n";
    assert_eq!(parsed_with(g, true, "ab"), "start[A:a,B:b]");
    assert_eq!(parsed_with(g, true, "b"), "start[_,B:b]");
    assert_eq!(parsed_with(g, true, ""), "start[]");
}

#[test]
fn test_nested_maybe_collision_still_rejected_under_placeholders() {
    // Guardrail: the fix must not start *accepting* a genuine collision. `([A] [A])?`
    // (two single-`A` present arms) and `([A])? C` (the absent-`[A]` + outer-ε both
    // reduce to `start -> C`) are rejected by Python under both modes (#252).
    assert_rejected_both_modes("start: ([A] [A])?\nA: \"a\"\n", "([A] [A])?");
    assert_rejected_both_modes("start: ([A])? C\nA: \"a\"\nC: \"c\"\n", "([A])? C");
}
