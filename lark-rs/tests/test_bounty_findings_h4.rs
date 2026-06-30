//! Bug-bounty findings, round 4 (h4) — failing oracle tests (XFAIL).
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
//! Each test asserts the **Python Lark 1.3.1** (oracle) behavior. This file is an XFAIL
//! catalog: a test is `#[ignore]`d while its bug is open and fails today; once the bug is
//! fixed its `#[ignore]` is dropped so it runs as a permanent regression guard (e.g.
//! `h4_2_*`, `h4_5_*`, `h4_6_*`, `h4_9_*`, and `h4_12_*` are fixed and now run by
//! default). Run the still-open
//! XFAILs with:
//!
//!     cargo test --test test_bounty_findings_h4 -- --ignored
//!
//! The DFA-build determinization gate (H4-12, now fixed — ADR-0037) additionally needs
//! the deterministic work counters, so run it with:
//!
//!     cargo test --features perf-counters --test test_bounty_findings_h4
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
        ParseTree::None => {}
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
        ParseTree::None => Child::None,
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
/// lark-rs additionally decoded `\v`→VT, `\0`→NUL, `\'`→`'`, so the `PatternStr` value
/// (and the input it matched) diverged. Engine-independent (loader bug). FIXED (#344):
/// `unescape_string` now drops those three arms so they fall through to the keep-backslash
/// arm, leaving `\v`/`\0`/`\'` as literal backslash+char — matching `eval_escaping`. Live
/// regression guard.
#[test]
fn h4_1_string_literal_escape_overdecoded() {
    // Python reads `"\v"` as the 2-char literal backslash+`v`, so it accepts the
    // 2-byte input `\v` and rejects a bare vertical tab. lark-rs decodes to U+000B
    // and does the opposite. Assert the Python-accepted literal parses.
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

    // Negative control — escapes inside the `Uuxnftr` set (plus `\\`/`\"`) must STILL
    // decode after the fix. `\n`→LF, `\t`→TAB, `\x41`/`A`/`\U00000041`→'A',
    // `\\`→one backslash, `\"`→'"', and a bare literal char are unchanged. Each grammar
    // accepts the *decoded* byte(s) and rejects the literal escape source, exactly opposite
    // to the over-decoded set above.
    for (g, decoded, literal_src) in [
        ("start: \"\\n\"\n", "\n", "\\n"),
        ("start: \"\\t\"\n", "\t", "\\t"),
        ("start: \"\\x41\"\n", "A", "\\x41"),
        ("start: \"\\u0041\"\n", "A", "\\u0041"),
        ("start: \"\\\\\"\n", "\\", "\\\\"),
        ("start: \"A\"\n", "A", "AA"),
    ] {
        let lark = Lark::new(g, opts(ParserAlgorithm::Lalr, LexerType::Contextual))
            .expect("H4-1 negative control: grammar builds");
        assert!(
            lark.parse(decoded).is_ok(),
            "H4-1 negative control: {g:?} must still decode and accept {decoded:?}"
        );
        assert!(
            lark.parse(literal_src).is_err(),
            "H4-1 negative control: {g:?} decodes its escape, so the literal source \
             {literal_src:?} must NOT match"
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
// Fixed (#345): when a `%ignore` directive is a single reference to a named
// terminal, the loader adds *that* terminal to the ignore set with its declared
// priority intact (Python's `_ignore` "keep terminal name" short-circuit,
// `grammar/loader/compiler.rs::IgnoreEntry::Named`), instead of minting a
// priority-0 `__IGNORE_n` clone. Only inline patterns synthesize a fresh terminal.
#[test]
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

/// H4-3 negative control (#345): the *inline* form `%ignore /[a-z]/` must still
/// synthesize a fresh priority-0 `__IGNORE_n` terminal — the fix only changes the
/// *named* form. Same grammar shape as H4-3a but with an inline pattern: the
/// priority-0 clone loses the lexer tie to `A`, so `ab` parses as `start(A, A)` in
/// **both** engines (verified against Python Lark 1.3.1). If the fix had wrongly
/// also short-circuited the inline form to the declared `SKIP.5`, this would reject.
#[test]
fn h4_3_inline_ignore_still_synthesizes_terminal() {
    let g = "start: A+\nA: /[a-z]/\nSKIP.5: /[a-z]/\n%ignore /[a-z]/\n";
    let lark = Lark::new(g, opts(ParserAlgorithm::Lalr, LexerType::Contextual))
        .expect("inline %ignore: grammar builds");
    let tree = lark
        .parse("ab")
        .expect("inline %ignore mints a priority-0 clone that loses to A → 'ab' parses");
    let mut types = Vec::new();
    collect_token_types(&tree, &mut types);
    assert_eq!(
        types,
        vec!["A", "A"],
        "inline %ignore /[a-z]/ synthesizes a priority-0 terminal (not the declared SKIP.5), \
         so each char is an A — Python yields start(A, A); got {types:?}"
    );
}

/// H4-4 (LOW, loader / priority). Terminal/rule priority is parsed as `i128` then
/// **clamped to `i32`** (`tokenizer.rs`), while Python uses arbitrary-precision `int`.
/// Two priorities that both exceed `i32::MAX` saturate to the same value and tie, so
/// lark-rs picks the wrong terminal (name order) where Python honors the true ordering.
/// Narrow (needs priorities > 2.1e9) but an honest, explicit-priority-determined
/// divergence. Expected fix: store priorities wide enough to not collide (or reject
/// out-of-range). Control: both ≤ `i32::MAX` agree.
// Fixed (#352): priority storage widened `i32` → `i64` (loader clamp + `RuleOptions`/
// `TerminalDef` fields + the Earley/CYK priority accumulators), so 5e9 and 9e9 no
// longer both saturate to `i32::MAX` and tie — `B` (9e9) outranks `A` (5e9), matching
// Python (which uses unbounded ints). Python accepts arbitrarily large priorities (no
// rejection even at 9e20), so the contract is store-wide-not-reject.
#[test]
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

/// H4-4 negative control: priorities at and *below* `i32::MAX` (where there was never
/// any saturation) must still order correctly — the widening is not allowed to perturb
/// the ordinary case. Two small distinct priorities and a pair straddling the old
/// `i32::MAX` boundary both pick the higher one, matching Python.
#[test]
fn h4_4_priority_small_and_boundary_still_order() {
    for (a_prio, b_prio) in [("5", "9"), ("2000000000", "2100000000")] {
        let g = format!("start: A | B\nA.{a_prio}: \"x\"\nB.{b_prio}: \"x\"\n");
        let lark = Lark::new(&g, opts(ParserAlgorithm::Lalr, LexerType::Contextual))
            .expect("H4-4 control: builds");
        let tree = lark.parse("x").expect("H4-4 control: parses");
        let mut types = Vec::new();
        collect_token_types(&tree, &mut types);
        assert_eq!(
            types,
            vec!["B"],
            "H4-4 control: B (priority {b_prio}) outranks A ({a_prio}); the i32→i64 widening \
             must not regress ordinary (non-saturating) priority ordering (got {types:?})"
        );
    }
}

/// H4-4 follow-up (#384 / omnibus #354 blocking finding): the Earley forest priority
/// **accumulator** must saturate, not wrap/panic. `packed_priority_value` sums a rule's
/// own priority with its children's priorities along a derivation; with the `i32`→`i64`
/// widening (#352) the *storage* no longer collides, but a derivation that sums two
/// priorities each near `i64::MAX` would overflow the plain `+`. Pre-fix this panicked
/// (debug `attempt to add with overflow`) / wrapped (release: the high-priority sum
/// wraps negative and *loses*, picking the wrong derivation). Post-fix the sum uses
/// `saturating_add` (mirroring CYK's `weight.saturating_add`), so it pins at `i64::MAX`
/// and the high-priority alternative still wins, deterministically.
///
/// Grammar: `"xx"` is ambiguous between `hh` (two `high` rules, each priority
/// `i64::MAX/2`, so their sum is `i64::MAX + 1` — the overflow point) and `ll` (two
/// `low` rules, total priority 2). Python (unbounded ints) resolves to `hh`; lark-rs
/// must do the same without overflowing, saturating the `hh` sum to `i64::MAX`.
#[test]
fn h4_4_earley_priority_accumulation_saturates_no_overflow() {
    // 4611686018427387904 == 2^62 == (i64::MAX + 1) / 2; two of them sum to
    // exactly i64::MAX + 1 — the minimal overflow point for the plain `+`.
    let g = "start: hh | ll\n\
             hh: high high\n\
             ll: low low\n\
             high.4611686018427387904: \"x\"\n\
             low.1: \"x\"\n";
    for lexer in [LexerType::Dynamic, LexerType::Basic] {
        let lark = Lark::new(g, opts(ParserAlgorithm::Earley, lexer.clone()))
            .unwrap_or_else(|e| panic!("H4-4 earley ({lexer:?}): builds: {e:?}"));
        // Must not panic on overflow during forest priority resolution.
        let tree = lark
            .parse("xx")
            .unwrap_or_else(|e| panic!("H4-4 earley ({lexer:?}): parses: {e:?}"));
        // The summed-priority winner is `hh` (≈i64::MAX, saturating) over `ll` (2).
        let names: Vec<&str> = tree
            .as_tree()
            .unwrap_or_else(|| panic!("H4-4 earley ({lexer:?}): root is a tree"))
            .children
            .iter()
            .filter_map(|c| c.as_tree().map(|ct| ct.data.as_str()))
            .collect();
        assert_eq!(
            names,
            vec!["hh"],
            "H4-4 earley ({lexer:?}): the saturating priority sum of `hh` (2^62 + 2^62 \
             == i64::MAX + 1, pinned at i64::MAX) must outrank `ll` (2); a wrapping `+` \
             would flip `hh` negative and pick `ll` (got {names:?})"
        );
    }
}

/// H4-4 boundary policy (ADR-0034): lark-rs intentionally supports a **bounded i64**
/// priority domain — values beyond ±i64::MAX *saturate*, a deliberate narrowing from
/// Python's unbounded ints. This pins that policy as tested, not accidental:
///   (a) two *distinct* priorities both beyond i64::MAX both clamp to i64::MAX and
///       **tie** (the documented bounded behavior — there is no Python oracle here
///       since Python is unbounded), so the tie breaks by rule order (first wins); and
///   (b) two *distinct in-range* large priorities still order correctly (the higher
///       wins), so saturation only bites past the boundary.
#[test]
fn h4_4_priority_beyond_i64_saturates_to_bounded_domain() {
    // (a) Both priorities are > i64::MAX (≈9.22e18); both clamp to i64::MAX and tie.
    //     With a tie, Earley breaks by rule order, so the first alternative (`A`) wins.
    let g_tie = "start: A | B\n\
                 A.99999999999999999999: \"x\"\n\
                 B.88888888888888888888: \"x\"\n";
    let lark = Lark::new(g_tie, opts(ParserAlgorithm::Earley, LexerType::Dynamic))
        .expect("H4-4 boundary: builds");
    let tree = lark.parse("x").expect("H4-4 boundary: parses");
    let mut types = Vec::new();
    collect_token_types(&tree, &mut types);
    assert_eq!(
        types,
        vec!["A"],
        "H4-4 boundary (a): two priorities beyond i64::MAX both saturate to i64::MAX and \
         tie (bounded-i64 policy, ADR-0034); the tie breaks by rule order so `A` wins \
         (got {types:?})"
    );

    // (b) Two distinct *in-range* large priorities still order: B (9e9) > A (5e9).
    let g_order = "start: A | B\nA.5000000000: \"x\"\nB.9000000000: \"x\"\n";
    let lark = Lark::new(g_order, opts(ParserAlgorithm::Earley, LexerType::Dynamic))
        .expect("H4-4 boundary: in-range builds");
    let tree = lark.parse("x").expect("H4-4 boundary: in-range parses");
    let mut types = Vec::new();
    collect_token_types(&tree, &mut types);
    assert_eq!(
        types,
        vec!["B"],
        "H4-4 boundary (b): distinct in-range priorities still order (B 9e9 > A 5e9); \
         saturation must only bite beyond the i64 boundary (got {types:?})"
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
// Fixed (#343): the per-module merged import-alias map (`import_alias_map`) leaves a
// closure symbol that is also independently imported unmangled, mirroring Python's
// `_get_mangle(prefix, aliases)` `if s in aliases` short-circuit.
#[test]
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
fn h4_6_contextual_unlexable_char_is_unexpected_character() {
    let lark = Lark::new(
        "start: \"a\" \"b\"\n",
        opts(ParserAlgorithm::Lalr, LexerType::Contextual),
    )
    .expect("H4-6: builds");
    let err = lark.parse("ax").expect_err("H4-6: 'ax' rejects");
    match err {
        ParseError::UnexpectedCharacter {
            ch,
            line,
            col,
            ref expected,
            ..
        } => {
            // Python: UnexpectedCharacters, line 1, col 2, char 'x', allowed {'B'}.
            assert_eq!(ch, 'x', "H4-6: offending char");
            assert_eq!((line, col), (1, 2), "H4-6: position (line 1, col 2)");
            // The `allowed`/expected set is the lexable terminals at the state
            // — here just `B` — and must NOT include the `$END` sentinel.
            assert!(
                !expected.contains("$END"),
                "H4-6: `$END` must be excluded from the allowed set, got {expected:?}"
            );
            assert!(
                expected.contains('B'),
                "H4-6: expected set should name the lexable terminal `B`, got {expected:?}"
            );
        }
        other => panic!(
            "H4-6: 'x' matches no terminal → Python raises UnexpectedCharacters; \
             lark-rs's contextual path raised {other:?}"
        ),
    }
}

/// H4-6 companion (regression guard). The H4-6 fix builds `UnexpectedCharacter` from a
/// contextual `LexFailure`, but a non-recovering contextual `LexFailure` must mean
/// *genuinely un-lexable* — NOT merely *invalid in this parser state*. A globally-valid
/// but state-invalid token (`}` while the parser is inside `a_part`, where the per-state
/// scanner only offers `AWORD`/`]`) is matched by the contextual lexer's root fallback,
/// fed to the parser, and rejected as `UnexpectedToken` — byte-for-byte what Python's
/// batch contextual parse raises (`l_ctx.parse("[}")` → `UnexpectedToken(RBRACE)`,
/// Python Lark 1.3.1). If the H4-6 fix ever converts *every* `LexFailure` to
/// `UnexpectedCharacter` (dropping the root fallback), this case regresses to the wrong
/// variant. Pinned alongside `tests/test_interactive.rs::contextual_state_invalid_token_rbrace`
/// (the interactive-cursor sibling).
#[test]
fn h4_6_contextual_state_invalid_token_is_unexpected_token() {
    let lark = Lark::new(
        "start: a_part b_part\n\
         a_part: \"[\" AWORD \"]\"\n\
         b_part: \"{\" BWORD \"}\"\n\
         AWORD: /[a-z]+/\n\
         BWORD: /[A-Z]+/\n\
         %ignore \" \"\n",
        opts(ParserAlgorithm::Lalr, LexerType::Contextual),
    )
    .expect("companion: builds");
    // `}` after `[` is globally lexable (it is the `b_part` closer) but invalid in the
    // `a_part` state → Python: UnexpectedToken(RBRACE), NOT UnexpectedCharacters.
    let err = lark.parse("[}").expect_err("companion: '[}' rejects");
    assert!(
        matches!(err, ParseError::UnexpectedToken { .. }),
        "companion: a state-invalid-but-globally-valid token must raise UnexpectedToken \
         (Python parity via the contextual root fallback), got {err:?}"
    );
    // And a genuinely un-lexable char on the same grammar is still UnexpectedCharacter.
    let unlexable = lark.parse("[x]@").expect_err("companion: '[x]@' rejects");
    assert!(
        matches!(unlexable, ParseError::UnexpectedCharacter { ch: '@', .. }),
        "companion: a truly un-lexable char must raise UnexpectedCharacter, got {unlexable:?}"
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
/// empty `start()` derivation. Distinct from RC7/#272 (recurse-helper over-share).
///
/// FIXED (#347): `dedup_and_check_alts` (`grammar/loader/compiler.rs`) now compares
/// alternatives by a filter-out-agnostic symbol key (`sym_key`), mirroring Python's
/// `Symbol.__eq__`/`Rule.__eq__`, so the two `start -> A` arms collapse to a single
/// arm keeping the first occurrence's `filter_out`.
#[test]
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

/// H4-9 differential audit (#347). The named banks under-sample the
/// literal-vs-named-terminal-unification dedup, and the issue warns of
/// adjacent-but-distinct dedup bugs (#272/#159). This pins a hand-rolled
/// differential against Python Lark 1.3.1 over the shapes around the H4-9 root —
/// source order (which decides kept vs dropped), multi-position, optional pairs,
/// and the `+`/`*` recurse helper — all of which lower to byte-identical
/// expansions differing only in per-occurrence `filter_out`. Each expected value
/// is what Python actually produces (recorded at fix time); a `None` LALR entry
/// means Python rejects the grammar at build.
#[test]
fn h4_9_literal_vs_named_dedup_differential() {
    // (grammar, input, expected LALR token-types | None if Python rejects at build)
    let lalr_cases: &[(&str, &str, Option<&[&str]>)] = &[
        // First-occurrence wins: `A | "a"` keeps the named `A` (token kept);
        // `"a" | A` keeps the literal (token dropped → no children).
        ("start: A | \"a\"\nA: \"a\"\n", "a", Some(&["A"])),
        ("start: \"a\" | A\nA: \"a\"\n", "a", Some(&[])),
        ("start: A | \"a\" | \"a\"\nA: \"a\"\n", "a", Some(&["A"])),
        ("start: \"a\" | \"a\" | A\nA: \"a\"\n", "a", Some(&[])),
        // `_A` is filtered by its `_` prefix, so `_A | "a"` drops the token too.
        ("start: _A | \"a\"\n_A: \"a\"\n", "a", Some(&[])),
        // Multi-position: only the unified slot dedups; siblings stay.
        (
            "start: A B | \"a\" B\nA: \"a\"\nB: \"b\"\n",
            "ab",
            Some(&["A", "B"]),
        ),
        // Distributed optional pair: the two absent arms differ only in their
        // placeholder count (filtered literal = size 0), which must still dedup.
        ("start: [A] | [\"a\"]\nA: \"a\"\n", "a", Some(&["A"])),
        // `+`/`*` recurse helper: `(A | "a")` collapses to one inner arm.
        ("start: (A | \"a\")+\nA: \"a\"\n", "aa", Some(&["A", "A"])),
        ("start: (A | \"a\")*\nA: \"a\"\n", "aa", Some(&["A", "A"])),
        // Distinctness preserved — two genuinely distinct named terminals over the
        // same pattern do NOT dedup (Python keeps both → LALR resolves to the first).
        ("start: A | B\nA: \"a\"\nB: \"a\"\n", "a", Some(&["A"])),
        // Alias-differing arms collapse to the same `(origin, expansion)` and Python
        // rejects "Rules defined twice" — the dedup must not silently swallow them.
        ("start: A -> x | \"a\" -> y\nA: \"a\"\n", "a", None),
    ];
    for (g, inp, expect) in lalr_cases {
        let built = Lark::new(g, opts(ParserAlgorithm::Lalr, LexerType::Contextual));
        match expect {
            None => assert!(
                built.is_err(),
                "audit: Python rejects {g:?} at build; lark-rs accepted it"
            ),
            Some(want) => {
                let lark = built.unwrap_or_else(|e| {
                    panic!("audit: Python accepts {g:?}; lark-rs rejected: {e:?}")
                });
                let tree = lark
                    .parse(inp)
                    .unwrap_or_else(|e| panic!("audit: {g:?} should parse {inp:?}: {e:?}"));
                let mut types = Vec::new();
                collect_token_types(&tree, &mut types);
                assert_eq!(
                    &types[..],
                    *want,
                    "audit: {g:?} on {inp:?} — token-type mismatch vs Python"
                );
            }
        }
    }

    // Earley `explicit`: a unified literal-vs-named pair yields a single tree (no
    // phantom empty/extra derivation), while two genuinely distinct named
    // terminals stay a real `_ambig` (the dedup must collapse byte-identical only,
    // never structurally-distinct derivations — ADR-0017).
    let earley_cases: &[(&str, &str, bool)] = &[
        ("start: A | \"a\"\nA: \"a\"\n", "a", false), // single tree
        ("start: (A | \"a\")*\nA: \"a\"\n", "aa", false), // single tree
        ("start: A | B\nA: \"a\"\nB: \"a\"\n", "a", true), // real ambiguity kept
    ];
    for (g, inp, want_ambig) in earley_cases {
        let mut eopts = opts(ParserAlgorithm::Earley, LexerType::Dynamic);
        eopts.ambiguity = Ambiguity::Explicit;
        let lark =
            Lark::new(g, eopts).unwrap_or_else(|e| panic!("audit: earley builds {g:?}: {e:?}"));
        let tree = lark
            .parse(inp)
            .unwrap_or_else(|e| panic!("audit: earley parses {g:?} on {inp:?}: {e:?}"));
        let is_ambig = tree.as_tree().map(|t| t.data == "_ambig").unwrap_or(false);
        assert_eq!(
            is_ambig, *want_ambig,
            "audit: earley `_ambig`-ness mismatch vs Python for {g:?} on {inp:?}"
        );
    }
}

/// H4-9 cross-site recurse-helper sharing (#377). The H4-9 fix (#347) deduped the
/// *inner arms* of a `+`/`*` recurse helper filter-out-agnostically (`sym_key`), so
/// a single site `(A | "a")+` keeps the first arm's `filter_out` and is correct. But
/// the recurse-helper *sharing* key (`recurse_cache`, keyed on `Vec<CompiledAlt>`
/// whose `Symbol` `Eq`/`Hash` include `filter_out`) was still filter-out-*sensitive*:
/// two sites referencing the same unified terminal in **opposite source order** —
/// `(A | "a")+` and `("a" | A)+`, both unifying the literal `"a"` onto `A` — produced
/// different first-occurrence arms (one keeps `A`, one keeps the filtered literal) and
/// therefore different cache keys, so lark-rs minted **two** helpers with opposite
/// token-keep fate. Python's `EBNF_to_BNF.rules_cache` keys on the inner `expr` Tree
/// (filter-out-agnostic), so **both** sites share the ONE helper minted at the
/// first-defined site, and both keep the token.
///
/// Repro: `start: drop "|" keep` / `keep: (A | "a")+` / `drop: ("a" | A)+` / `A: "a"`
/// on `"a a | a a"`. Python (LALR & Earley): `start(drop(A,A), keep(A,A))` — the shared
/// helper keeps the token at both sites. Before the fix lark-rs emitted
/// `start(drop(), keep(A,A))` — the `drop` site's helper carried `filter_out=true`
/// (its first arm is the literal `"a"`) and dropped both tokens, the forbidden
/// more-permissive direction (ADR-0017): lark-rs produced a tree Python never emits.
///
/// FIXED (#377): the real-pass `recurse_cache` (and the audit's over-share origin map)
/// now key on the **filter-out-agnostic** arm shape (`sym_key` per symbol + gaps),
/// mirroring Python's Tree-keyed `rules_cache`, while `emit_recurse_rule` still builds
/// from the first-occurrence arms (so the shared helper's `filter_out` matches the
/// first site). The RC7/#272 share/split decision is unchanged (LALR bank stays 512).
#[test]
fn h4_9_cross_site_recurse_helper_sharing_filterout() {
    let g =
        "start: drop \"|\" keep\nkeep: (A | \"a\")+\ndrop: (\"a\" | A)+\nA: \"a\"\n%ignore \" \"\n";
    let input = "a a | a a";
    // Python (oracle), LALR & Earley: start(drop(A,A), keep(A,A)) — every leaf is an
    // `A` token (both sites keep it via the one shared helper).
    let want = vec!["A", "A", "A", "A"];

    for (parser, lexer) in [
        (ParserAlgorithm::Lalr, LexerType::Contextual),
        (ParserAlgorithm::Earley, LexerType::Dynamic),
    ] {
        let lark = Lark::new(g, opts(parser.clone(), lexer))
            .unwrap_or_else(|e| panic!("#377 ({parser:?}): grammar should build: {e:?}"));
        let tree = lark
            .parse(input)
            .unwrap_or_else(|e| panic!("#377 ({parser:?}): should parse {input:?}: {e:?}"));
        let mut types = Vec::new();
        collect_token_types(&tree, &mut types);
        assert_eq!(
            types, want,
            "#377 ({parser:?}): both `+` sites must share one token-keeping helper \
             (Python: start(drop(A,A), keep(A,A))); lark-rs dropped the `drop` site's tokens"
        );
    }
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
fn h4_10_nullable_recursive_earley_enumerates_all_derivations() {
    // FIXED (#348): the explicit forest→tree walk now mirrors Python's per-packed-node
    // `_cache` + cycle-retreat reuse (`packed_cache` + `is_cycle_node` gating of the
    // per-node memos in `parsers/earley/tree_walk.rs`), so a derivation a cyclic
    // re-descent used to discard is recovered. Python (oracle): 1,1,2,8,48 distinct
    // derivations of ""/"b"/"bb"/"bbb"/"bbbb". Pin every length, on BOTH lexers — the
    // bug was identical on basic and dynamic.
    let g = "start: z\nz: | \"b\" z | z z\n";
    let expected = [("", 1), ("b", 1), ("bb", 2), ("bbb", 8), ("bbbb", 48)];
    for lexer in [LexerType::Dynamic, LexerType::Basic] {
        let mut eopts = opts(ParserAlgorithm::Earley, lexer.clone());
        eopts.ambiguity = Ambiguity::Explicit;
        let lark = Lark::new(g, eopts).expect("H4-10: builds");
        for (inp, want) in expected {
            let tree = lark.parse(inp).expect("H4-10: parses");
            let n = derivation_count(&tree);
            assert_eq!(
                n, want,
                "H4-10 ({lexer:?}): {inp:?} — Python enumerates {want} distinct derivations, got {n}"
            );
        }
    }
}

/// H4-10 differential audit (#348): the fix is forest-walk cycle handling, an area the
/// banks under-sample. Pin lark-rs's distinct-derivation count against Python Lark 1.3.1
/// over a spread of *adversarial* nullable + recursive ambiguous grammars (left-, right-,
/// and binary-recursive, with a nullable arm), on BOTH lexers. Counts are Python-derived
/// (the oracle). The last case is the #159 byte-identical-collapse pin: distinct SPPF
/// derivations that assemble identically (filtered tokens) must still collapse to ONE tree
/// — the cycle fix must not turn that into spurious `_ambig` alternatives (ADR-0017).
#[test]
fn h4_10_differential_audit_nullable_recursive_ambiguous() {
    // (grammar, [(input, python_distinct_derivation_count)])
    let cases: &[(&str, &[(&str, usize)])] = &[
        // nullable + binary-recursive (the H4-10 root)
        (
            "start: z\nz: | \"b\" z | z z\n",
            &[("", 1), ("b", 1), ("bb", 2), ("bbb", 8), ("bbbb", 48)],
        ),
        // nullable + both-recursive single symbol
        (
            "start: a\na: | a a | \"x\"\n",
            &[("", 1), ("x", 1), ("xx", 1), ("xxx", 2), ("xxxx", 5)],
        ),
        // nullable + binary-recursive with a left-token arm
        (
            "start: p\np: p p | \"a\" p | \n",
            &[("", 1), ("a", 1), ("aa", 2), ("aaa", 6)],
        ),
        // nullable middle, binary-recursive
        (
            "start: e\ne: e e | \"n\" | \n",
            &[("", 1), ("n", 1), ("nn", 1), ("nnn", 2)],
        ),
        // *interacting* cycles: `e` and `f` are mutually recursive and each
        // nullable, so their SPPF cycles share nodes (one SCC). This is the case a
        // single-pass back-edge DFS mis-classified (false-negative → under-report);
        // SCC-based cycle detection (`ensure_cycle_nodes`) is what gets it right.
        (
            "start: e\ne: e e | f | \nf: e | \"d\" f | \n",
            &[("", 2), ("d", 2), ("dd", 8), ("ddd", 44)],
        ),
        // #159 byte-identical collapse: every derivation assembles identically (the
        // distinguishing "x" tokens are filtered), so the dedup must yield ONE tree.
        (
            "start: \"x\" start | start \"x\" | \"x\"\n",
            &[("x", 1), ("xx", 1), ("xxx", 1)],
        ),
    ];
    for lexer in [LexerType::Dynamic, LexerType::Basic] {
        for (g, inputs) in cases {
            let mut eopts = opts(ParserAlgorithm::Earley, lexer.clone());
            eopts.ambiguity = Ambiguity::Explicit;
            let lark = Lark::new(g, eopts)
                .unwrap_or_else(|e| panic!("audit: builds {g:?} ({lexer:?}): {e:?}"));
            for (inp, want) in *inputs {
                let tree = lark.parse(inp).unwrap_or_else(|e| {
                    panic!("audit: parses {g:?} on {inp:?} ({lexer:?}): {e:?}")
                });
                let n = derivation_count(&tree);
                assert_eq!(
                    n, *want,
                    "audit ({lexer:?}): {g:?} on {inp:?} — Python yields {want} distinct \
                     derivations, lark-rs got {n}"
                );
            }
        }
    }
}

/// H4-11 (LOW, loader). `%declare` of a lowercase (rule-cased) name is accepted by
/// lark-rs; Python rejects it at build (a declared symbol must be an UPPERCASE terminal).
/// Per ADR-0017's corollary, accepting what the oracle rejects is unfalsifiable → a bug.
/// Oracle caveat: Python's rejection surfaces as an internal `AttributeError` rather than
/// a clean `GrammarError`, so only the accept/reject verdict is asserted, not the message.
/// Expected fix: reject `%declare` of a non-terminal-cased name with a `GrammarError`.
#[test]
fn h4_11_declare_lowercase_name_rejected() {
    let g = "%declare foo\nstart: \"a\"\n";
    assert!(
        Lark::new(g, opts(ParserAlgorithm::Lalr, LexerType::Contextual)).is_err(),
        "H4-11: Python rejects `%declare foo` (lowercase); lark-rs accepted it"
    );
    // Negative control: a normal UPPERCASE `%declare` must still build (Python
    // accepts `%declare FOO`) — the fix gates only on the case convention, it
    // must not over-reject a legitimate declared terminal.
    let ok = "%declare FOO\nstart: \"a\"\n";
    assert!(
        Lark::new(ok, opts(ParserAlgorithm::Lalr, LexerType::Contextual)).is_ok(),
        "H4-11 negative control: Python accepts `%declare FOO` (uppercase); lark-rs must too"
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
///
/// **FIXED (ADR-0037, the hybrid-fallback contract).** `build_combined_dfa`'s engine
/// builder now determinizes each source under a per-source `dfa_size_limit`; a source
/// that would overflow it (the `.*a.{N}` family) is routed to the lazy/hybrid DFA
/// instead (`lexer/dfa.rs::build_partitioned_dfa`). The hybrid DFA matches
/// byte-identically (Python accepts these inputs — a resource pathology, not a behavioral
/// divergence), so the grammar still builds and lexes; only the eager determinization is
/// skipped. `dense_build_bytes` therefore stays ~flat across N (the over-budget terminal
/// contributes ~0 — its states are realized lazily, off the counter), where it used to
/// roughly double per +1 in N (N=4 = 5184 B, N=10 = 311616 B; ≈60× — the pre-fix XFAIL).
#[cfg(feature = "perf-counters")]
#[test]
fn h4_12_dense_dfa_build_is_subexponential() {
    use lark_rs::perf;

    // Build the same terminal at increasing N and record the eager-determinization heap
    // size per build. Pre-fix this doubled per +1 in N (exponential); post-fix the
    // over-budget terminal falls back to the lazy DFA, so the dense counter stays flat.
    let measure = |n: usize| -> Option<u64> {
        let g = format!("start: T\nT: /[01]*1[01]{{{n}}}/\n");
        let mut o = opts(ParserAlgorithm::Lalr, LexerType::Basic);
        o.start = vec!["start".to_string()];
        perf::reset();
        // The fix bounds (not refuses) over-budget terminals: the grammar still builds.
        let lark = Lark::new(&g, o).ok()?;
        // Force the combined-DFA build by lexing a valid input of length > n+1.
        let input = "1".repeat(n + 6);
        let parsed = lark.parse(&input);
        assert!(
            parsed.is_ok(),
            "H4-12: the hybrid-fallback terminal must still LEX byte-identically (Python \
             accepts {input:?}); the fallback preserves oracle parity"
        );
        Some(perf::dense_build_bytes())
    };

    let bytes = |n: usize| measure(n).expect("H4-12: the grammar still builds after the fix");
    let (b4, b10) = (bytes(4), bytes(10));
    // Post-fix the over-budget terminal is lazy, so its eager-determinization contribution
    // collapses to ~0 — `dense_build_bytes` is flat-or-shrinking across N, never the ≈60×
    // exponential climb of the pre-fix XFAIL. Assert sub-exponential (well under the 8×
    // the XFAIL pinned), which the hybrid fallback satisfies by construction.
    assert!(
        b10 <= b4.saturating_mul(8),
        "H4-12: determinized DFA size is exponential in the terminal's counted repeat \
         (bytes N=4 = {b4}, N=10 = {b10}; ratio {:.1}× ≫ linear) — the dense build is not \
         bounded by the hybrid fallback",
        b10 as f64 / b4.max(1) as f64
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// #343 adjacent import/alias shapes — closure mangle vs the per-module alias map.
//
// The #343 fix builds a per-module *merged* alias map and consults it for every
// closure symbol. These four pins (token types verified against Python Lark 1.3.1,
// `parser='lalr', lexer='contextual'`) bracket the fix so a future refactor cannot
// over- or under-mangle: a closure symbol is unmangled iff it is independently
// imported from the same module, across **all** directives, honoring the alias.
// ─────────────────────────────────────────────────────────────────────────────
fn h4_5_token_types(g: &str, inp: &str) -> Vec<String> {
    let lark = Lark::new(g, opts(ParserAlgorithm::Lalr, LexerType::Contextual))
        .expect("#343 adjacent: builds");
    let tree = lark.parse(inp).expect("#343 adjacent: parses");
    let mut t = Vec::new();
    collect_token_types(&tree, &mut t);
    t.into_iter().map(|s| s.to_string()).collect()
}

/// Cross-directive merge: `pattern` and `NAME` arrive in **separate** `%import
/// python` directives. Python merges the per-dotted-path `aliases` dict before
/// mangling, so the closure reference to `NAME` is still left unmangled (`NAME`).
#[test]
fn h4_5_cross_directive_sibling_import_unmangled() {
    let types = h4_5_token_types(
        "start: pattern\n%import python (pattern)\n%import python (NAME)\n%ignore \" \"\n",
        "x",
    );
    assert!(
        types.contains(&"NAME".to_string()) && !types.contains(&"python__NAME".to_string()),
        "#343: `NAME` imported by a separate directive of the same module must stay \
         unmangled (Python merges aliases across directives); got {types:?}"
    );
}

/// Control: when the sibling is **not** independently imported, the closure
/// reference *is* mangled — `python__NAME`. Confirms the fix did not blanket-exempt.
#[test]
fn h4_5_unimported_sibling_stays_mangled() {
    let types = h4_5_token_types(
        "start: pattern\n%import python (pattern)\n%ignore \" \"\n",
        "x",
    );
    assert!(
        types.contains(&"python__NAME".to_string()) && !types.contains(&"NAME".to_string()),
        "#343 control: `NAME` not independently imported must stay prefix-mangled \
         (`python__NAME`); got {types:?}"
    );
}

/// Aliased sibling: `%import python.NAME -> ID` registers `NAME → ID`, so the
/// closure reference is rewritten to the **alias** `ID` (Python's `aliases[s]`),
/// not the mangled `python__NAME` nor the bare `NAME`.
#[test]
fn h4_5_aliased_sibling_uses_alias_in_closure() {
    let types = h4_5_token_types(
        "start: pattern ID\n%import python (pattern)\n%import python.NAME -> ID\n%ignore \" \"\n",
        "x y",
    );
    assert_eq!(
        types,
        vec!["ID".to_string(), "ID".to_string()],
        "#343: an aliased sibling import (`NAME -> ID`) must rename the closure \
         reference to `ID` too (Python `aliases[NAME] == ID`); got {types:?}"
    );
}

/// A closure **non-terminal** sub-rule that is also independently imported is left
/// unmangled too — but an alias node (`-> capture_pattern`) that is *not* imported
/// stays mangled. Verified against Python: nodes `[start, python__capture_pattern]`,
/// token `NAME`. Confirms the alias map exempts only what is in the import list, and
/// that the dedup (separately-imported `closed_pattern`/`NAME` copies) does not
/// duplicate or drop a rule.
#[test]
fn h4_5_closure_subrule_imported_alias_still_mangled() {
    let g = "start: pattern\n%import python (pattern, closed_pattern, NAME)\n%ignore \" \"\n";
    let lark = Lark::new(g, opts(ParserAlgorithm::Lalr, LexerType::Contextual)).expect("builds");
    let tree = lark.parse("x").expect("parses");
    let mut tokens = Vec::new();
    collect_token_types(&tree, &mut tokens);
    fn node_names(c: &Child, out: &mut Vec<String>) {
        if let Child::Tree(tr) = c {
            out.push(tr.data.clone());
            for ch in &tr.children {
                node_names(ch, out);
            }
        }
    }
    let mut nodes = Vec::new();
    if let ParseTree::Tree(tr) = &tree {
        nodes.push(tr.data.clone());
        for ch in &tr.children {
            node_names(ch, &mut nodes);
        }
    }
    assert_eq!(
        nodes,
        vec!["start".to_string(), "python__capture_pattern".to_string()],
        "#343: un-imported alias node stays mangled even when a closure sub-rule is \
         independently imported; got nodes {nodes:?}"
    );
    assert_eq!(
        tokens,
        vec!["NAME".to_string()],
        "#343: independently-imported `NAME` stays unmangled in the closure; got {tokens:?}"
    );
}
