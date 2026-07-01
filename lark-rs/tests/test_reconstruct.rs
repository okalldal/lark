//! Metamorphic tests for the tree → text reconstructor (`src/reconstruct.rs`).
//!
//! Reconstruction has no Python-Lark byte oracle (Python's own `Reconstructor`
//! is experimental and its output text is not canonical), so it is grounded by
//! the **metamorphic round-trip property** instead (ADR-0040):
//!
//! > for any grammar G and input x accepted by G,
//! > `parse(reconstruct(parse(x)))` is structurally equal to `parse(x)`.
//!
//! This file exercises the property over curated grammars covering each
//! tree-shaping feature the reconstructor must invert (filtered punctuation,
//! transparent rules, expand1 collapse, aliases, `!`/keep_all_tokens, EBNF
//! helpers, templates, `%ignore`), plus the typed refusals. The whole-bank
//! sweep lives in `tests/test_reconstruct_bank.rs`.

use lark_rs::reconstruct::{ReconstructError, Reconstructor};
use lark_rs::{Child, Lark, LarkOptions, LexerType, ParseTree, ParserAlgorithm};

// ─── Structural tree equality (positions ignored) ───────────────────────────
//
// Reconstructed text has different byte offsets than the original, so the
// round-trip compares tree *structure*: node labels, child shapes, and token
// (type, value) pairs — exactly the fields the oracle fixtures pin.

// Iterative (worklist) comparison: parse trees are as deep as the input is
// nested, and the deep-tree test below runs on a small stack on purpose.
fn children_eq(a: &[Child], b: &[Child]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut stack: Vec<(&Child, &Child)> = a.iter().zip(b).collect();
    while let Some(pair) = stack.pop() {
        match pair {
            (Child::Tree(x), Child::Tree(y)) => {
                if x.data != y.data || x.children.len() != y.children.len() {
                    return false;
                }
                stack.extend(x.children.iter().zip(&y.children));
            }
            (Child::Token(x), Child::Token(y)) => {
                if x.type_ != y.type_ || x.value != y.value {
                    return false;
                }
            }
            (Child::None, Child::None) => {}
            _ => return false,
        }
    }
    true
}

fn parse_tree_eq(a: &ParseTree, b: &ParseTree) -> bool {
    match (a, b) {
        (ParseTree::Tree(x), ParseTree::Tree(y)) => {
            x.data == y.data && children_eq(&x.children, &y.children)
        }
        (ParseTree::Token(x), ParseTree::Token(y)) => x.type_ == y.type_ && x.value == y.value,
        (ParseTree::None, ParseTree::None) => true,
        _ => false,
    }
}

// ─── Harness ─────────────────────────────────────────────────────────────────

/// Assert the metamorphic round-trip for every input: parse → reconstruct →
/// re-parse → structurally identical tree. Returns the reconstructed texts so
/// a test can additionally pin an exact rendering when it is meaningful.
fn assert_round_trips(lark: &Lark, inputs: &[&str]) -> Vec<String> {
    let recons = Reconstructor::new(lark).expect("Reconstructor builds");
    assert_round_trips_with(lark, &recons, inputs)
}

/// As [`assert_round_trips`], with a caller-built reconstructor (term_subs).
fn assert_round_trips_with(lark: &Lark, recons: &Reconstructor, inputs: &[&str]) -> Vec<String> {
    inputs
        .iter()
        .map(|input| {
            let tree = lark
                .parse(input)
                .unwrap_or_else(|e| panic!("input {input:?} must parse: {e:?}"));
            let text = recons
                .reconstruct(&tree)
                .unwrap_or_else(|e| panic!("input {input:?} must reconstruct: {e}"));
            let tree2 = lark.parse(&text).unwrap_or_else(|e| {
                panic!("reconstructed text {text:?} (from {input:?}) must re-parse: {e:?}")
            });
            assert!(
                parse_tree_eq(&tree, &tree2),
                "round-trip must preserve the tree for {input:?}\n\
                 reconstructed: {text:?}\n  original: {tree}\n  round-trip: {tree2}"
            );
            text
        })
        .collect()
}

fn lalr(grammar: &str) -> Lark {
    Lark::new(grammar, LarkOptions::default()).expect("grammar builds")
}

// ─── Feature-by-feature round trips ──────────────────────────────────────────

#[test]
fn filtered_punctuation_is_written_back() {
    // The core move: `"("`, `","`, `")"` are filtered from the tree; the
    // reconstructor re-inserts them from the grammar.
    let lark = lalr(
        "start: \"(\" item (\",\" item)* \")\"\n\
         item: NUMBER\n\
         NUMBER: /[0-9]+/\n\
         %ignore \" \"\n",
    );
    let texts = assert_round_trips(&lark, &["(1)", "(1, 2, 3)", "( 42 ,7 )"]);
    // Exact rendering is meaningful here: all punctuation is fixed-string.
    assert_eq!(texts, ["(1)", "(1,2,3)", "(42,7)"]);
}

#[test]
fn identifier_fusion_gets_a_space() {
    // Two kept alphanumeric tokens with no punctuation between them would fuse
    // ("f" + "x" → "fx"); the insert_spaces heuristic must separate them.
    let lark = lalr(
        "start: NAME NAME\n\
         NAME: /[a-z]+/\n\
         %ignore \" \"\n",
    );
    let texts = assert_round_trips(&lark, &["f x", "foo    bar"]);
    assert_eq!(texts, ["f x", "foo bar"]);
}

#[test]
fn transparent_rules_are_reinflated() {
    // `_pair`'s children are spliced into `start`; matching must re-derive the
    // nesting through the transparent rule to place the "=" back.
    let lark = lalr(
        "start: _pair (\";\" _pair)*\n\
         _pair: NAME \"=\" NUMBER\n\
         NAME: /[a-z]+/\n\
         NUMBER: /[0-9]+/\n\
         %ignore \" \"\n",
    );
    let texts = assert_round_trips(&lark, &["a = 1", "a=1; b=2 ; c=3"]);
    assert_eq!(texts, ["a=1", "a=1;b=2;c=3"]);
}

#[test]
fn expand1_collapse_is_reversed() {
    // `?value` collapses to its single child; the matcher must expand the
    // reference structurally. Uncollapsed nodes (multi-child `add`) still match.
    let lark = lalr(
        "start: expr\n\
         ?expr: add | atom\n\
         add: atom \"+\" expr\n\
         ?atom: NUMBER | \"(\" expr \")\"\n\
         NUMBER: /[0-9]+/\n\
         %ignore \" \"\n",
    );
    assert_round_trips(&lark, &["1", "1+2", "1 + (2 + 3)", "((7))+1+2"]);
}

#[test]
fn aliases_label_and_match() {
    // Aliased alternatives produce nodes named by the alias; bridging rules
    // must route a `stmt` reference to either alias node.
    let lark = lalr(
        "start: stmt+\n\
         stmt: \"go\" NAME \";\"   -> go_stmt\n\
            | \"stop\" \";\"      -> stop_stmt\n\
         NAME: /[a-z]+/\n\
         %ignore \" \"\n",
    );
    let texts = assert_round_trips(&lark, &["go north; stop;", "stop;"]);
    assert_eq!(texts, ["go north;stop;", "stop;"]);
}

#[test]
fn keep_all_tokens_rule_keeps_its_punctuation() {
    // `!range` keeps its ".." token in the tree: the recons rule must *consume*
    // it (not re-emit from the grammar), while plain `pair` still filters.
    let lark = lalr(
        "start: range | pair\n\
         !range: NUMBER \"..\" NUMBER\n\
         pair: NUMBER \",\" NUMBER\n\
         NUMBER: /[0-9]+/\n\
         %ignore \" \"\n",
    );
    let texts = assert_round_trips(&lark, &["1 .. 9", "1, 9"]);
    assert_eq!(texts, ["1..9", "1,9"]);
}

#[test]
fn global_keep_all_tokens_option() {
    let lark = Lark::new(
        "start: \"(\" NUMBER \")\"\nNUMBER: /[0-9]+/\n%ignore \" \"\n",
        LarkOptions {
            keep_all_tokens: true,
            ..Default::default()
        },
    )
    .expect("grammar builds");
    let texts = assert_round_trips(&lark, &["( 4 )"]);
    assert_eq!(texts, ["(4)"]);
}

#[test]
fn ebnf_helpers_star_plus_opt() {
    // EBNF `*`/`+`/`?`/groups lower to `__anon_*` helpers spliced into the
    // parent; the recons rules must re-derive through them.
    let lark = lalr(
        "start: NAME (\"[\" idx (\",\" idx)* \"]\")? \"!\"+\n\
         idx: NUMBER\n\
         NAME: /[a-z]+/\n\
         NUMBER: /[0-9]+/\n\
         %ignore \" \"\n",
    );
    let texts = assert_round_trips(&lark, &["a!", "a[1]!!", "a[1, 2, 3] !!!"]);
    // The `!` tokens are filtered from the tree, so their *count* is not
    // recoverable: `a!!!` and `a!` parse to the identical tree, and the
    // canonical reconstruction emits the minimal repetition. The metamorphic
    // property (tree preserved) is what's guaranteed, not the byte count.
    assert_eq!(texts, ["a!", "a[1]!", "a[1,2,3]!"]);
}

#[test]
fn empty_productions_match() {
    // A node with zero children (all-discarded or empty alternative) needs the
    // nullable-safe matcher (empty recons expansions).
    let lark = lalr(
        "start: unit unit\n\
         unit: \"go\" | \"wait\"\n\
         %ignore \" \"\n",
    );
    // Both alternatives of `unit` reconstruct as one canonical literal (the
    // tree cannot distinguish them — dedup picks the first), so the round-trip
    // holds even though the text may differ from the input.
    assert_round_trips(&lark, &["go go", "go wait", "wait wait"]);
}

#[test]
fn templates_reconstruct() {
    // Template instances are named `base{N}` internally but labeled `base` in
    // the tree; transparent `_sep` templates splice.
    let lark = lalr(
        "start: _sep{item, \",\"}\n\
         _sep{x, sep}: x (sep x)*\n\
         item: NAME\n\
         NAME: /[a-z]+/\n\
         %ignore \" \"\n",
    );
    let texts = assert_round_trips(&lark, &["a", "a, b, c"]);
    assert_eq!(texts, ["a", "a,b,c"]);
}

#[test]
fn json_grammar_round_trips() {
    // json.lark's `pair` *requires* the discarded regex terminal `_WS`
    // (`pair: string ":" _WS value`), so it needs a substitution — while the
    // optional `_WS?` slots resolve to their empty alternative and cost
    // nothing. The optional-cosmetic vs. required-separator split is exactly
    // what the dedup cost policy exists for.
    let grammar = include_str!("grammars/json.lark");
    let lark = lalr(grammar);
    let recons = Reconstructor::with_term_subs(&lark, [("_WS", " ")]).unwrap();
    assert_round_trips_with(
        &lark,
        &recons,
        &[
            "{}",
            "[]",
            "true",
            "-1.5e3",
            r#"{"a": [1, 2.5, null], "b": {"c": false, "d": "s\"x"}}"#,
            r#"[[[]], {}, "", 0]"#,
        ],
    );
}

#[test]
fn arithmetic_grammar_round_trips() {
    let grammar = include_str!("grammars/arithmetic.lark");
    let lark = lalr(grammar);
    assert_round_trips(&lark, &["1+2*3", "(1+2)*-3", "2 * (3.5 - 1) / 7"]);
}

#[test]
fn earley_trees_reconstruct_too() {
    // The reconstructor works on the surface grammar + tree, independent of
    // which parser produced the tree.
    let lark = Lark::new(
        "start: a | b\na: \"x\" NAME\nb: NAME \"x\"\nNAME: /[a-z]+/\n%ignore \" \"\n",
        LarkOptions {
            parser: ParserAlgorithm::Earley,
            lexer: LexerType::Basic,
            ..Default::default()
        },
    )
    .expect("grammar builds");
    assert_round_trips(&lark, &["x foo", "foo x"]);
}

#[test]
fn token_root_from_collapsed_start() {
    // `?start: NUMBER` collapses the root to a bare token; its text is the value.
    let lark = lalr("?start: NUMBER\nNUMBER: /[0-9]+/\n");
    let tree = lark.parse("42").unwrap();
    assert!(tree.is_token());
    let recons = Reconstructor::new(&lark).unwrap();
    assert_eq!(recons.reconstruct(&tree).unwrap(), "42");
}

#[test]
fn multiline_input_with_term_subs() {
    // `_NL` is a discarded *regex* terminal: unreconstructable by itself, but
    // `with_term_subs` supplies its text — Python's `term_subs` contract.
    let grammar = "start: line+\nline: NAME _NL\n_NL: /\\n/\nNAME: /[a-z]+/\n";
    let lark = lalr(grammar);
    let tree = lark.parse("a\nb\n").unwrap();

    // Without a substitution: a typed error naming the terminal.
    let plain = Reconstructor::new(&lark).unwrap();
    assert_eq!(
        plain.reconstruct(&tree),
        Err(ReconstructError::NonLiteralTerminal {
            name: "_NL".to_string()
        })
    );

    // With one: the round trip holds.
    let subs = Reconstructor::with_term_subs(&lark, [("_NL", "\n")]).unwrap();
    let text = subs.reconstruct(&tree).unwrap();
    assert_eq!(text, "a\nb\n");
    let tree2 = lark.parse(&text).unwrap();
    assert!(parse_tree_eq(&tree, &tree2));
}

#[test]
fn reconstruct_exact_skips_spacing() {
    let lark = lalr("start: NAME NAME\nNAME: /[a-z]+/\n%ignore \" \"\n");
    let tree = lark.parse("f x").unwrap();
    let recons = Reconstructor::new(&lark).unwrap();
    assert_eq!(recons.reconstruct(&tree).unwrap(), "f x");
    assert_eq!(recons.reconstruct_exact(&tree).unwrap(), "fx");
}

// ─── Typed refusals ──────────────────────────────────────────────────────────

#[test]
fn maybe_placeholders_is_refused_up_front() {
    let lark = Lark::new(
        "start: [NAME] NUMBER\nNAME: /[a-z]+/\nNUMBER: /[0-9]+/\n%ignore \" \"\n",
        LarkOptions {
            maybe_placeholders: true,
            ..Default::default()
        },
    )
    .expect("grammar builds");
    assert_eq!(
        Reconstructor::new(&lark).err(),
        Some(ReconstructError::MaybePlaceholders)
    );
}

#[test]
fn foreign_tree_is_a_no_match() {
    // A tree the grammar cannot produce (hand-edited shape) is a typed NoMatch,
    // not a panic or silent garbage.
    let lark = lalr("start: NAME\nNAME: /[a-z]+/\n");
    let recons = Reconstructor::new(&lark).unwrap();
    let bogus = ParseTree::Tree(lark_rs::Tree::new(
        "start",
        vec![
            Child::Token(lark_rs::Token::new("NAME", "a")),
            Child::Token(lark_rs::Token::new("NAME", "b")),
        ],
    ));
    assert_eq!(
        recons.reconstruct(&bogus),
        Err(ReconstructError::NoMatch {
            data: "start".to_string()
        })
    );
    let unknown = ParseTree::Tree(lark_rs::Tree::new("nonexistent_rule", vec![]));
    assert_eq!(
        recons.reconstruct(&unknown),
        Err(ReconstructError::NoMatch {
            data: "nonexistent_rule".to_string()
        })
    );
}

// ─── Robustness ──────────────────────────────────────────────────────────────

#[test]
fn deep_tree_reconstruction_is_iterative() {
    // Right-recursive nesting: the tree is as deep as the input. The write walk
    // and derivation extraction are explicit-stack, so this must not overflow
    // even on a small thread stack (the #151 discipline). 512 KB thread, like
    // tests/test_earley_stack.rs.
    let lark = lalr("start: item\nitem: \"(\" item \")\" | NAME\nNAME: /[a-z]+/\n");
    let depth = 2_000;
    let input = format!("{}{}{}", "(".repeat(depth), "x", ")".repeat(depth));
    let handle = std::thread::Builder::new()
        .stack_size(512 * 1024)
        .spawn(move || {
            let tree = lark.parse(&input).unwrap();
            let recons = Reconstructor::new(&lark).unwrap();
            let text = recons.reconstruct(&tree).unwrap();
            let tree2 = lark.parse(&text).unwrap();
            assert!(parse_tree_eq(&tree, &tree2));
        })
        .expect("spawn");
    handle.join().expect("no stack overflow");
}

#[test]
fn long_flat_list_reconstructs() {
    // EBNF star lists flatten into one node with many children; the recurse
    // helpers make the *derivation* deep even when the tree is flat.
    let lark = lalr("start: NUMBER (\",\" NUMBER)*\nNUMBER: /[0-9]+/\n%ignore \" \"\n");
    let input = (0..500)
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    assert_round_trips(&lark, &[&input]);
}
