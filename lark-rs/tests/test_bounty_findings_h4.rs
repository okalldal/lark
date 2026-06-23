//! Bug-bounty findings, round 4 (h4) — oracle tests and remaining XFAILs.
//!
//! Rounds 1–3 (`test_bounty_findings.rs` RC, `_h2.rs` N, `_h3.rs` H) harvested the
//! missing-validation-gate layer, the lexer terminal-ordering bugs, config legality,
//! char-vs-byte positions, and the first wave of Python-`re` *regex* dialect divergences.
//! Round 4 retargeted the surfaces those rounds declared clean or never reached:
//! **grammar string-literal** (not regex) escape decoding, **regex-crate-only** dialect
//! silently accepted, `%ignore` of a **named** terminal, **error/ParseError parity**,
//! **import-closure mangling**, a **nested optional** collision gate, **named-terminal-vs-
//! literal** rule unification, a **nullable+recursive Earley** derivation under-count, and
//! a **DFA-build** determinization blow-up.
//!
//! Each test asserts the **Python Lark 1.3.1** (oracle) behavior. Tests that still carry
//! `#[ignore]` are open XFAILs; drop a test's `#[ignore]` when its bug is fixed to turn
//! it into a permanent regression guard. Run the still-open XFAILs with:
//!
//!     cargo test --test test_bounty_findings_h4 -- --ignored
//!
//! The DFA-build determinization gate (H4-12) additionally needs the deterministic work
//! counters, so run it with:
//!
//!     cargo test --features perf-counters --test test_bounty_findings_h4 -- --ignored
//!
//! Baseline SHA: a74841ac21d0ab1d115ba5b5d93de814d399ba12. Catalog with repros, severity,
//! blast radius, and fix contracts: `docs/BOUNTY_FINDINGS_H4.md`.
//!
//! NONE of these reduce to a round-1/2/3 root cause (RC1–RC10, N1–N10, V1–V4, H1–H12,
//! P1–P2) or the open known-issue set (#208, #275, #281, #282, #286, #293, #299, #302,
//! #304, #329–#338). Where a find is adjacent to a known issue the distinction is noted
//! at the test. The two H4 *variants* (regex `(?a:)`/`\N{}`/`a{}` mislabel = variant of
//! H9/#333; explicit-`start=` panic = variant of H1/#330) are documented in the catalog,
//! not re-counted here.

use lark_rs::{
    Ambiguity, Child, Lark, LarkOptions, LexerType, ParseError, ParseTree, ParserAlgorithm,
};
use std::collections::HashSet;

fn opts(parser: ParserAlgorithm, lexer: LexerType) -> LarkOptions {
    LarkOptions {
        parser,
        lexer,
        start: vec!["start".to_string()],
        ..Default::default()
    }
}

/// Collect every token's `type_` in the tree (pre-order), so a find can assert a
/// specific token's terminal name without pinning the whole shape.
fn collect_token_types<'a>(t: &'a ParseTree, out: &mut Vec<&'a str>) {
    fn walk<'a>(c: &'a Child, out: &mut Vec<&'a str>) {
        match c {
            Child::Token(tok) => out.push(&tok.type_),
            Child::Tree(tr) => {
                for ch in &tr.children {
                    walk(ch, out);
                }
            }
            Child::None => {}
        }
    }
    match t {
        ParseTree::Token(tok) => out.push(&tok.type_),
        ParseTree::Tree(tr) => {
            for ch in &tr.children {
                walk(ch, out);
            }
        }
    }
}

/// Enumerate the set of *distinct* disambiguated derivations encoded in a forest:
/// an `_ambig` node is a union over its children, every other tree is the cartesian
/// product over its children. Returns canonical strings (deduped), so it counts the
/// same "distinct derivations" Python's explicit `_ambig` forest does (after both
/// sides drop byte-identical duplicates — the only collapse ADR-0017 permits).
fn enum_derivations(c: &Child) -> HashSet<String> {
    match c {
        Child::None => HashSet::from(["None".to_string()]),
        Child::Token(t) => HashSet::from([format!("{}:{}", t.type_, t.value)]),
        Child::Tree(tr) if tr.data == "_ambig" => {
            let mut s = HashSet::new();
            for ch in &tr.children {
                s.extend(enum_derivations(ch));
            }
            s
        }
        Child::Tree(tr) => {
            let mut acc: Vec<String> = vec![String::new()];
            for ch in &tr.children {
                let mut sorted: Vec<String> = enum_derivations(ch).into_iter().collect();
                sorted.sort();
                let mut next = Vec::new();
                for prefix in &acc {
                    for piece in &sorted {
                        next.push(format!("{prefix},{piece}"));
                    }
                }
                acc = next;
            }
            acc.into_iter()
                .map(|inner| format!("{}({})", tr.data, inner.trim_start_matches(',')))
                .collect()
        }
    }
}

fn derivation_count(t: &ParseTree) -> usize {
    // `Tree`/`Token` have manual `Clone` impls (#151); wrap the root as a `Child`.
    let root = match t {
        ParseTree::Tree(tr) => Child::Tree(tr.clone()),
        ParseTree::Token(tok) => Child::Token(tok.clone()),
    };
    enum_derivations(&root).len()
}

// ─────────────────────────────────────────────────────────────────────────────
// Grammar string-literal & escape-sequence dialect.
// ─────────────────────────────────────────────────────────────────────────────

/// H4-1 (MEDIUM, grammar-loader). lark-rs's `unescape_string`
/// (`src/grammar/loader/tokenizer.rs`) decodes a *superset* of the escapes Python
/// Lark's `eval_escaping` (`lark/load_grammar.py`) recognizes. Python decodes only
/// `\\ \U \u \x \n \f \t \r`; **every other** escape keeps a literal backslash.
/// Older lark-rs additionally decoded `\v`→VT, `\0`→NUL, `\'`→`'`, so the `PatternStr`
/// value (and the input it matched) diverged. Engine-independent (loader bug). Guard:
/// reject-like-Python at the value level — leave `\v`/`\0`/`\'` as literal backslash+char.
#[test]
fn h4_1_string_literal_escape_overdecoded() {
    // Python reads `"\v"` as the 2-char literal backslash+`v`, so it accepts the
    // 2-byte input `\v` and rejects a bare vertical tab. The bug decoded to U+000B
    // and did the opposite. Assert the Python-accepted literal parses.
    for (g, accepted_literal) in [
        ("start: \"\\v\"\n", "\\v"),
        ("start: \"\\0\"\n", "\\0"),
        ("start: \"\\'\"\n", "\\'"),
    ] {
        let lark = Lark::new(g, opts(ParserAlgorithm::Lalr, LexerType::Contextual))
            .expect("H4-1: grammar builds");
        assert!(
            lark.parse(accepted_literal).is_ok(),
            "H4-1: Python treats the escape as a literal backslash+char and accepts {accepted_literal:?}, \
             but lark-rs over-decoded it and rejects"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Python-`re` dialect: regex-crate-only constructs silently accepted.
// ─────────────────────────────────────────────────────────────────────────────

/// H4-2 (HIGH, lexer dialect). A terminal regex using a construct the Rust `regex`
/// crate supports but **Python `re` has no syntax for** is *silently accepted* by
/// lark-rs (it delegates to `regex` without screening). Python Lark rejects each at
/// build (`LexError`/`GrammarError: Cannot compile token`). Per ADR-0017's corollary,
/// being more permissive than the oracle is unfalsifiable → a bug. Three surfaces:
/// `\p{L}`/`\pL`/`\P{L}` unicode-property, `\x{..}` braced hex, `\z` lowercase
/// end-of-text anchor. Distinct from H5/#332 (char-class POSIX/set-op — *inside* `[]`),
/// H6–H9/#333 (quantifier/octal/comment), and #275 (`\b`/`\B`/`\Z`, which Python
/// *accepts*/parks). Expected fix: reject-like-Python (categorized `InvalidRegex`).
#[test]
#[ignore = "XFAIL (bounty H4-2): regex-crate-only \\p{} / \\x{} / \\z silently accepted; Python rejects at build"]
fn h4_2_regex_crate_only_dialect_rejected() {
    for g in [
        "start: T\nT: /\\p{L}+/\n",
        "start: T\nT: /\\pL+/\n",
        "start: T\nT: /\\P{L}+/\n",
        "start: T\nT: /\\x{41}/\n",
        "start: T\nT: /abc\\z/\n",
    ] {
        assert!(
            Lark::new(g, opts(ParserAlgorithm::Lalr, LexerType::Contextual)).is_err(),
            "H4-2: Python `re` cannot compile {g:?} (rejected at build), but lark-rs accepted it"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// `%ignore` of a named terminal mints a duplicate instead of marking it ignored.
// ─────────────────────────────────────────────────────────────────────────────

/// H4-3 (MEDIUM, lexer / loader). `%ignore NAME` (a directive naming an existing
/// terminal) mints a **fresh** `__IGNORE_n` clone at priority 0
/// (`compiler.rs::expansion_to_pattern` + the push) instead of adding the existing
/// terminal's id to the ignore set, the way Python's `%ignore` adds the name to
/// `lexer_conf.ignore`. Two surfaces of one root cause:
///   (a) the clone drops the named terminal's **declared priority**, so a higher-priority
///       ignore terminal that should win the lexer tie loses;
///   (b) the named terminal, still present un-ignored, is **kept** when a rule also
///       references it, so it leaks into the tree.
/// Decisive control (both agree): the inline form `%ignore /\s+/` mints a fresh terminal
/// in *both* engines, so only the *named* form diverges. Expected fix: when a `%ignore`
/// directive is a single reference to a named terminal, mark *that* terminal ignored
/// (preserving its priority); only inline patterns synthesize a fresh terminal.
#[test]
#[ignore = "XFAIL (bounty H4-3): %ignore NAME mints a priority-0 __IGNORE_n clone, dropping priority and failing to filter the named terminal"]
fn h4_3_ignore_named_terminal_priority_and_filter() {
    // (a) priority: SKIP.5 outranks A and should ignore each char, leaving nothing for
    // A → Python rejects. lark-rs keeps the priority-0 clone, A wins, parse succeeds.
    let g_prio = "start: A+\nA: /[a-z]/\nSKIP.5: /[a-z]/\n%ignore SKIP\n";
    let lark = Lark::new(g_prio, opts(ParserAlgorithm::Lalr, LexerType::Contextual))
        .expect("H4-3a: grammar builds");
    assert!(
        lark.parse("ab").is_err(),
        "H4-3a: SKIP.5 (declared priority) should win the lexer tie and be ignored, \
         leaving nothing for A — Python rejects 'ab'; lark-rs dropped the priority and accepted"
    );

    // (b) filter: WS is %ignore'd AND referenced in `item`. Python drops every WS
    // globally → two items. lark-rs keeps the rule-referenced WS as a third item.
    let g_filter = "start: item+\nitem: \"a\" | WS\nWS: /\\s+/\n%ignore WS\n";
    let lark = Lark::new(g_filter, opts(ParserAlgorithm::Lalr, LexerType::Contextual))
        .expect("H4-3b: grammar builds");
    let tree = lark.parse("a a").expect("H4-3b: parses");
    let n = tree
        .as_tree()
        .expect("H4-3b: start is a tree")
        .children
        .len();
    assert_eq!(
        n, 2,
        "H4-3b: Python ignores WS globally → start has 2 items; lark-rs kept the \
         rule-referenced WS as an extra child (got {n})"
    );
}

/// H4-4 (LOW, loader / priority). Terminal/rule priority is parsed as `i128` then
/// **clamped to `i32`** (`tokenizer.rs`), while Python uses arbitrary-precision `int`.
/// Two priorities that both exceed `i32::MAX` saturate to the same value and tie, so
/// lark-rs picks the wrong terminal (name order) where Python honors the true ordering.
/// Narrow (needs priorities > 2.1e9) but an honest, explicit-priority-determined
/// divergence. Expected fix: store priorities wide enough to not collide (or reject
/// out-of-range). Control: both ≤ `i32::MAX` agree.
#[test]
#[ignore = "XFAIL (bounty H4-4): terminal priority clamped to i32 ties two distinct >i32::MAX priorities"]
fn h4_4_priority_i32_saturation_tie() {
    let g = "start: A | B\nA.5000000000: \"x\"\nB.9000000000: \"x\"\n";
    let lark =
        Lark::new(g, opts(ParserAlgorithm::Lalr, LexerType::Contextual)).expect("H4-4: builds");
    let tree = lark.parse("x").expect("H4-4: parses");
    let mut types = Vec::new();
    collect_token_types(&tree, &mut types);
    assert_eq!(
        types,
        vec!["B"],
        "H4-4: B (priority 9e9) outranks A (5e9); Python picks B, lark-rs saturated both to \
         i32::MAX and picked A by name order (got {types:?})"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// %import closure mangling.
// ─────────────────────────────────────────────────────────────────────────────

/// H4-5 (MEDIUM-HIGH, loader / imports). When a symbol referenced inside an imported
/// rule's dependency closure is **also independently imported** from the same module,
/// Python leaves that reference unmangled (its `_get_mangle` aliases dict, merged across
/// all `%import`s of the path); lark-rs's `import_rule_closure` (`imports.rs`) exempts
/// only the single requested name and prefix-mangles every other closure symbol → a
/// **wrong token type / node name** in the tree, silently (never errors). Repro uses the
/// bundled `python.lark` so it is fully in-memory. Distinct from #286/#299 (%extend /
/// import-vs-import collision) and RC2 (duplicate definition). Expected fix: build a
/// per-module alias map from the full merged import list and consult it for every
/// closure symbol, mirroring `_get_mangle`.
#[test]
#[ignore = "XFAIL (bounty H4-5): import-closure mangles a sibling that is independently imported (token type python__NAME vs NAME)"]
fn h4_5_import_closure_mangle_exemption() {
    let g = "start: pattern\n%import python (pattern, NAME)\n%ignore \" \"\n";
    let lark =
        Lark::new(g, opts(ParserAlgorithm::Lalr, LexerType::Contextual)).expect("H4-5: builds");
    let tree = lark.parse("x").expect("H4-5: parses");
    let mut types = Vec::new();
    collect_token_types(&tree, &mut types);
    assert!(
        types.contains(&"NAME"),
        "H4-5: `NAME` is independently imported, so Python leaves the closure reference \
         unmangled (token type `NAME`); lark-rs mangled it to `python__NAME` (got {types:?})"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Error / ParseError parity.
// ─────────────────────────────────────────────────────────────────────────────

/// H4-6 (HIGH, error parity). On the default LALR + **contextual** lexer path, the
/// non-recovering driver turns an *unlexable character* into `UnexpectedToken`
/// (`lalr.rs::lex_failure`), where Python (and lark-rs's own basic-lexer and recovering
/// paths) raise `UnexpectedCharacters`. A consumer matching on the error class
/// mis-routes. Expected fix: build `ParseError::UnexpectedCharacter` from the lex
/// failure, mirroring the recovering path. (Distinct from N8/#307, token positions.)
#[test]
#[ignore = "XFAIL (bounty H4-6): contextual lexer reports UnexpectedToken for an unlexable char; Python+basic say UnexpectedCharacter"]
fn h4_6_contextual_unlexable_char_is_unexpected_character() {
    let lark = Lark::new(
        "start: \"a\" \"b\"\n",
        opts(ParserAlgorithm::Lalr, LexerType::Contextual),
    )
    .expect("H4-6: builds");
    let err = lark.parse("ax").expect_err("H4-6: 'ax' rejects");
    assert!(
        matches!(err, ParseError::UnexpectedCharacter { ch: 'x', .. }),
        "H4-6: 'x' matches no terminal → Python raises UnexpectedCharacters; \
         lark-rs's contextual path raised {err:?}"
    );
}

/// H4-7 (MEDIUM, error parity). At end-of-input the `$END` error token is built at the
/// live lexer **cursor** (`token_source.rs`), so its reported position is the end of the
/// consumed input. Python borrows the **start position of the last real token**
/// (`Token.new_borrow_pos`), or `(1,1,0)` when there were none. For `start: "a" "b"` on
/// `"a"`, Python reports column 1 (start of `a`); lark-rs reports column 2. The error's
/// *position* is the falsifiable bug here; the error *type* at EOF (lark-rs's
/// `UnexpectedEof` vs Python LALR's `UnexpectedToken($END)`) is the API-shape fork
/// tracked as needs-decision in the catalog. Expected fix (position): borrow the last
/// token's start position for the EOF error.
#[test]
#[ignore = "XFAIL (bounty H4-7): EOF error position is the end cursor, not the last token's start (Python new_borrow_pos)"]
fn h4_7_eof_error_borrows_last_token_position() {
    let lark = Lark::new(
        "start: \"a\" \"b\"\n",
        opts(ParserAlgorithm::Lalr, LexerType::Contextual),
    )
    .expect("H4-7: builds");
    let err = lark
        .parse("a")
        .expect_err("H4-7: 'a' rejects (missing 'b')");
    let col = match err {
        ParseError::UnexpectedEof { col, .. } => col,
        ParseError::UnexpectedToken { col, .. } => col,
        other => panic!("H4-7: unexpected error variant {other:?}"),
    };
    assert_eq!(
        col, 1,
        "H4-7: Python borrows the last token ('a') start position → column 1; \
         lark-rs reported the end cursor (column {col})"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// EBNF / rule-unification gates.
// ─────────────────────────────────────────────────────────────────────────────

/// H4-8 (MEDIUM, ebnf-loader). A single nested optional term — `([A]?) B`, `[[A]?] B`,
/// `[[[A]?]?] B` — expands to two arms that both reduce to the *same* `(syms, gaps)`
/// `CompiledAlt`, so `dedup_and_check_alts` (`compiler.rs`) merges them at its stage-1
/// `seen.insert` **before** the stage-2 `seen_syms` collision check ever sees the
/// duplicate. Python keeps `_EMPTY`-marker provenance through dedup and rejects:
/// `GrammarError: Rules defined twice ... (colliding expansion of optionals)`. The
/// #252/#259 fix covers *sibling* collisions (`[A] [A]`); this single-term self-collision
/// slips past it. Expected fix: reject-like-Python (keep enough provenance that the two
/// arms collide at stage 2). Distinct from #289/RC9 (lone-None expand1 parse divergence).
#[test]
#[ignore = "XFAIL (bounty H4-8): nested optional-of-optional ([A]?) B silently accepted; Python rejects 'Rules defined twice'"]
fn h4_8_nested_optional_of_optional_collision_rejected() {
    for g in [
        "start: ([A]?) B\nA: \"a\"\nB: \"b\"\n",
        "start: [[A]?] B\nA: \"a\"\nB: \"b\"\n",
        "start: [[[A]?]?] B\nA: \"a\"\nB: \"b\"\n",
    ] {
        assert!(
            Lark::new(g, opts(ParserAlgorithm::Lalr, LexerType::Contextual)).is_err(),
            "H4-8: Python rejects {g:?} as a colliding optional expansion; lark-rs accepted it"
        );
    }
}

/// H4-9 (MEDIUM-HIGH, terminal unification). A rule alternative that is a string literal
/// equal to a named terminal — `start: A | "a"` with `A: "a"` — unifies the literal onto
/// `A` for lexing but keeps **two** `CompiledRule`s differing only in `filter_pos`
/// (`terminals.rs`/`intern.rs`), a duplicate alternative Python collapses to a single
/// `<start : A>`. The duplicate manifests as a spurious LALR reduce/reduce **build
/// rejection** (Python accepts and parses) and, under Earley `explicit`, a spurious extra
/// empty `start()` derivation. Distinct from RC7/#272 (recurse-helper over-share). Expected
/// fix: dedup rule alternatives that lower to byte-identical expansions, preferring the
/// kept-token occurrence.
#[test]
#[ignore = "XFAIL (bounty H4-9): equal named-terminal-vs-literal alternation is a spurious LALR reduce/reduce; Python accepts"]
fn h4_9_terminal_vs_literal_alternation() {
    let g = "start: A | \"a\"\nA: \"a\"\n";

    // LALR: Python builds & parses start(A='a'); lark-rs rejects at build (reduce/reduce).
    let built = Lark::new(g, opts(ParserAlgorithm::Lalr, LexerType::Contextual));
    let lark = built.expect(
        "H4-9: Python accepts this grammar; lark-rs raised a spurious reduce/reduce at build",
    );
    let tree = lark.parse("a").expect("H4-9: parses 'a'");
    let mut types = Vec::new();
    collect_token_types(&tree, &mut types);
    assert_eq!(
        types,
        vec!["A"],
        "H4-9: expected the single derivation start(A='a')"
    );

    // Earley explicit: a single tree, no phantom `_ambig` (no extra empty derivation).
    let mut eopts = opts(ParserAlgorithm::Earley, LexerType::Dynamic);
    eopts.ambiguity = Ambiguity::Explicit;
    let lark = Lark::new(g, eopts).expect("H4-9: earley builds");
    let tree = lark.parse("a").expect("H4-9: earley parses");
    let data = tree.as_tree().map(|t| t.data.as_str()).unwrap_or("<token>");
    assert_ne!(
        data, "_ambig",
        "H4-9: Python yields a single unambiguous tree; lark-rs added a phantom empty derivation"
    );
}

/// H4-10 (HIGH, earley). A nullable + directly-recursive grammar — `start: z` /
/// `z: | "b" z | z z` — makes lark-rs's SPPF forest→tree enumeration (`earley.rs`)
/// **under-report** distinct derivations: on `"bbb"` Python yields 8 distinct
/// disambiguated derivations, lark-rs only 6 (a strict subset; the deficit grows
/// 2→26→262 across `bbb`/`bbbb`/`bbbbb`). This is the **forbidden** direction of
/// ADR-0017: structurally-distinct derivations lost, not byte-identical duplicates
/// collapsed. Distinct from #159 (byte-identical dedup, which is intentional). Expected
/// fix: enumerate every distinct derivation Python does; the dedup may only ever collapse
/// byte-identical trees.
#[test]
#[ignore = "XFAIL (bounty H4-10): nullable+recursive Earley under-reports distinct derivations (6 vs Python's 8 on 'bbb')"]
fn h4_10_nullable_recursive_earley_enumerates_all_derivations() {
    let g = "start: z\nz: | \"b\" z | z z\n";
    let mut eopts = opts(ParserAlgorithm::Earley, LexerType::Dynamic);
    eopts.ambiguity = Ambiguity::Explicit;
    let lark = Lark::new(g, eopts).expect("H4-10: builds");
    let tree = lark.parse("bbb").expect("H4-10: parses");
    let n = derivation_count(&tree);
    assert_eq!(
        n, 8,
        "H4-10: Python enumerates 8 distinct derivations of 'bbb'; lark-rs lost some (got {n})"
    );
}

/// H4-11 (LOW, loader). `%declare` of a lowercase (rule-cased) name is accepted by
/// lark-rs; Python rejects it at build (a declared symbol must be an UPPERCASE terminal).
/// Per ADR-0017's corollary, accepting what the oracle rejects is unfalsifiable → a bug.
/// Oracle caveat: Python's rejection surfaces as an internal `AttributeError` rather than
/// a clean `GrammarError`, so only the accept/reject verdict is asserted, not the message.
/// Expected fix: reject `%declare` of a non-terminal-cased name with a `GrammarError`.
#[test]
#[ignore = "XFAIL (bounty H4-11): %declare of a lowercase name accepted; Python rejects (terminal-case convention)"]
fn h4_11_declare_lowercase_name_rejected() {
    let g = "%declare foo\nstart: \"a\"\n";
    assert!(
        Lark::new(g, opts(ParserAlgorithm::Lalr, LexerType::Contextual)).is_err(),
        "H4-11: Python rejects `%declare foo` (lowercase); lark-rs accepted it"
    );
}

/// H4-12 (HIGH, perf — deterministic counter). The default DFA lexer backend eagerly,
/// fully determinizes each terminal's NFA with `dense::Builder::new()` under **no**
/// `dfa_size_limit` (`lexer/dfa.rs::build_combined_dfa`). A terminal whose minimal DFA is
/// exponential in its source — `T: /[01]*1[01]{N}/` (the classic `.*a.{N}` family) —
/// blows the determinizer to `2^(N+1)` states and hangs unbounded; Python `re` compiles
/// it in linear time (no determinization). Measured deterministically via the
/// `dense_build_bytes` work counter (the determinized heap size), which grows
/// exponentially in N today. Distinct from #335/H11 (dynamic-lexer per-position *scan*
/// O(n²)) and the existing `test_lexer_dfa_build_scaling` gate (sweeps only lowered
/// lookaround, never a user counted-repeat terminal). Expected fix: bound the
/// determinized size — fall back to the lazy/hybrid DFA (as the `regex` scanner backend
/// already does) for over-budget terminals so `dense_build_bytes` stays ~flat per source,
/// or refuse with a categorized `GrammarError` (a needs-decision fork; see catalog).
#[cfg(feature = "perf-counters")]
#[test]
#[ignore = "XFAIL (bounty H4-12): DFA backend eagerly determinizes a counted-repeat terminal to 2^N states (unbounded); Python re is linear"]
fn h4_12_dense_dfa_build_is_subexponential() {
    use lark_rs::perf;

    // Build the same terminal at increasing N (all small enough to determinize fast
    // today: N=10 ⇒ 2^11 states) and record the determinized heap size per build.
    let measure = |n: usize| -> Option<u64> {
        let g = format!("start: T\nT: /[01]*1[01]{{{n}}}/\n");
        let mut o = opts(ParserAlgorithm::Lalr, LexerType::Basic);
        o.start = vec!["start".to_string()];
        perf::reset();
        // A fix may refuse over-budget terminals: an Err is acceptable (bounded by refusal).
        let lark = Lark::new(&g, o).ok()?;
        // Force the combined-DFA build by lexing a valid input of length > n+1.
        let input = "1".repeat(n + 6);
        let _ = lark.parse(&input);
        Some(perf::dense_build_bytes())
    };

    let bytes = |n: usize| measure(n).expect("H4-12: builds today (no size limit)");
    let (b4, b10) = (bytes(4), bytes(10));
    // Today the determinized size roughly doubles per +1 in N (≈2^6 = 64× across this
    // span); a bounded fix keeps it ~linear in the +6 input growth. Assert sub-exponential.
    assert!(
        b10 <= b4.saturating_mul(8),
        "H4-12: determinized DFA size is exponential in the terminal's counted repeat \
         (bytes N=4 = {b4}, N=10 = {b10}; ratio {:.1}× ≫ linear) — unbounded eager \
         determinization with no dfa_size_limit",
        b10 as f64 / b4.max(1) as f64
    );
}
