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

use lark_rs::{Child, Lark, LarkOptions, LexerType, ParserAlgorithm};

fn build(grammar: &str) -> Lark {
    Lark::new(
        grammar,
        LarkOptions {
            parser: ParserAlgorithm::Lalr,
            lexer: LexerType::Contextual,
            start: vec!["start".to_string()],
            maybe_placeholders: true,
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
