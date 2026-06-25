//! Bug-bounty findings, round 5 (h5) — failing oracle tests (XFAIL).
//!
//! Rounds 1–4 (`test_bounty_findings.rs` RC, `_h2.rs` N, `_h3.rs` H, `_h4.rs` H4-*)
//! harvested the validation-gate layer, the lexer terminal-ordering bugs, config
//! legality, char-vs-byte positions, error/`ParseError` parity, import-closure
//! mangling, tree-shaping lone-`None`, the standalone bake, the bindings surface, and
//! four waves of Python-`re` *regex* dialect divergences. Round 5 retargeted the
//! surfaces those rounds declared clean or never reached: the **grammar name-token
//! lexer** (`__`-leading names), a **lookaround-terminal width-inference** residual,
//! a **cross-`|`-alternative empty-arm** LALR collision, two **new regex-dialect**
//! divergences (`\w`/`\W` Unicode membership; `\N{NAME}` reject + mis-categorization),
//! a **regex-crate-only named-group** spelling, a **Turkish-i case-fold** boundary,
//! and the **anonymous-terminal naming table** (`\\`→`BACKSLASH`, `\r\n`→`CRLF`).
//!
//! Each test asserts the **Python Lark 1.3.1** (oracle) behavior. This file is an XFAIL
//! catalog: every test below is `#[ignore]`d and fails today. Drop a test's `#[ignore]`
//! when its bug is fixed to turn it into a permanent regression guard. Run the still-open
//! XFAILs with:
//!
//!     cargo test --test test_bounty_findings_h5 -- --ignored
//!
//! Baseline SHA: 325444f5c0a16a284b362289194b6f97402b3053. Catalog with repros, severity,
//! blast radius, fix contracts, the provisional/perf finding (H5-9, LALR dense parse
//! table), and the dedup against rounds 1–4: `docs/BOUNTY_FINDINGS_H5.md`.
//!
//! NONE of these reduce to a round-1/2/3/4 root cause (RC1–RC10, N1–N10, V1–V4, H1–H12,
//! P1–P2, H4-1…H4-12) or the open known-issue set. Adjacencies are noted at each test:
//! H5-1 is the lookaround-fallback *residual* of RC5/#268 (a distinct code branch);
//! H5-4/H5-5 are new escapes not in the H4-2 dialect set and (for `\N{}`) carry the
//! opposite fix contract (Python *accepts* it). The known dialect variants this round
//! re-confirmed but does **not** re-count — `\b`/`\B` (RC6/#275), `\Z` (N10), POSIX
//! classes (H5/#332), `(?#)`/octal (H8/H9) — are documented in the catalog, not here.

use lark_rs::{Child, Lark, LarkOptions, LexerType, ParseTree, ParserAlgorithm};

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
        // A bare `None` parse result (root `?start: [A]` collapse, #289/ADR-0033)
        // carries no tokens.
        ParseTree::None => {}
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Lexer: terminal width inference / ranking.
// ─────────────────────────────────────────────────────────────────────────────

/// H5-1 (MEDIUM, lexer) — **fixed (#360); now a regression guard.** `Pattern::max_width()`
/// (`src/grammar/terminal.rs`) used to size a regex by
/// `regex_syntax::parse(...).ok().and_then(hir_max_width_chars)`. For any
/// *lowerable-lookaround* terminal (`(?=…)`, `(?<=…)`, …) `regex_syntax` **rejects** the
/// source, so `.ok()` was `None`, which `plan.rs` maps to `usize::MAX` (unbounded).
/// Python's `get_regexp_width` parses via `sre_parse`, which sizes every standard
/// lookaround (assertions are zero-width) and returns a **finite** width. So at a
/// same-span tie lark-rs sorted the finite-but-`None`-sized lookaround terminal *ahead*
/// of a genuinely wider terminal and picked the wrong terminal type.
///
/// The fix sizes a lookaround terminal the parser rejects through the shared
/// assertion-aware width walk (`lookaround::pattern_max_width`, the analogue of Python's
/// `get_regexp_width(...)[1]`; assertions zero-width) instead of falling back to `None`.
/// The sort key itself was already correct.
///
/// Distinct from RC5/#268 ("max_width=None for finite regex"): #268 added the
/// `hir_max_width_chars` walk for patterns `regex_syntax` *can* parse and pinned it
/// with `/a+/`/`/aa?/`; this was the **parse-failure fallback branch** #268 left in
/// place, exercised only by lookaround terminals (which the RC5 pin never builds).
///
/// Repro contract (verified against Python Lark 1.3.1): with `keep_all_tokens`, both at
/// a same-span tie, the wider finite `REG=/a|zz/` (max_width 2) must beat the
/// max_width-1 lookaround terminal under **both** lexers and for **both** the lookahead
/// (`/a(?=b)/`) and lookbehind (`/(?<=x)a/`) forms — the catalog's noted variant.
#[test]
fn h5_1_lookaround_terminal_width_misrank() {
    // The lookahead terminal LA=/a(?=b)/ (max_width 1) ties REG=/a|zz/ (max_width 2) on
    // the span "a". Python's -max_width key puts REG (wider) first → token type REG.
    for lexer in [LexerType::Basic, LexerType::Contextual] {
        assert_picks_reg(
            "start: t B\nt: LA | REG\nLA: /a(?=b)/\nREG: /a|zz/\nB: \"b\"\n",
            "ab",
            lexer,
            "lookahead /a(?=b)/",
        );
    }
    // The lookbehind variant /(?<=x)a/ (the catalog's "H5-1 / lookbehind" form) also
    // sizes to max_width 1 in Python and must likewise lose to REG.
    for lexer in [LexerType::Basic, LexerType::Contextual] {
        assert_picks_reg(
            "start: B t\nt: LB | REG\nLB: /(?<=x)a/\nREG: /a|zz/\nB: \"x\"\n",
            "xa",
            lexer,
            "lookbehind /(?<=x)a/",
        );
    }
}

/// Build `grammar` (lalr + `lexer`, `keep_all_tokens`), parse `input`, and assert the
/// wider finite `REG` terminal — not the max_width-1 lookaround terminal — was chosen
/// at the same-span tie (H5-1).
fn assert_picks_reg(grammar: &str, input: &str, lexer: LexerType, label: &str) {
    let mut o = opts(ParserAlgorithm::Lalr, lexer.clone());
    o.keep_all_tokens = true;
    let lark = Lark::new(grammar, o)
        .unwrap_or_else(|e| panic!("H5-1 ({label}, {lexer:?}): grammar builds: {e}"));
    let tree = lark
        .parse(input)
        .unwrap_or_else(|e| panic!("H5-1 ({label}, {lexer:?}): {input:?} parses: {e}"));
    let mut types = Vec::new();
    collect_token_types(&tree, &mut types);
    assert!(
        types.contains(&"REG"),
        "H5-1 ({label}, {lexer:?}): Python sizes REG (max_width 2) wider than the \
         max_width-1 lookaround terminal and picks REG; lark-rs must too (not size the \
         lookaround as unbounded). types={types:?}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Grammar loader: the name-token lexer.
// ─────────────────────────────────────────────────────────────────────────────

/// H5-2 (MEDIUM, grammar-loader). Python Lark's `RULE`/`TOKEN` name tokens are
/// `/_?[a-z]…/` and `/_?[A-Z]…/` — at most **one** leading underscore, then a letter.
/// lark-rs's `lex_rule`/`lex_terminal` (`src/grammar/loader/tokenizer.rs`) take a
/// permissive `[A-Za-z0-9_]*`, so a `__`-leading name (or `_`-then-non-letter) is
/// silently accepted where Python rejects the grammar at parse. Per ADR-0017's
/// corollary, accepting what the oracle rejects is unfalsifiable → a bug. Holds for
/// rule defs, terminal defs, references, alias targets, and template parameters.
/// Expected fix: reject-like-Python — mirror Lark's name-token shape in the tokenizer.
///
/// Scope is precisely the `__`-leading class — a name that *has* a letter but a
/// disallowed `__` prefix. The no-letter-at-all class (`_`/`__`/`_9`, which Python
/// also rejects) is the sibling finding H6-8/#405, a different predicate pinned by
/// `h6_8_letterless_names_rejected`; the fix here deliberately does not touch it.
#[test]
fn h5_2_double_underscore_name_rejected() {
    // All four surfaces a `__`-leading name can appear on. Each grammar parses input
    // "a" if it builds; Python rejects every one at grammar-parse with
    // `GrammarError: Unexpected input` (oracle-confirmed, lark 1.3.1).
    let reject = [
        ("rule def", "start: __x\n__x: \"a\"\n"),
        ("terminal def", "start: __X\n__X: \"a\"\n"),
        ("alias target", "start: \"a\" -> __x\n"),
        ("template param", "t{__x}: __x\nstart: t{\"a\"}\n"),
    ];
    for (surface, g) in reject {
        assert!(
            Lark::new(g, opts(ParserAlgorithm::Lalr, LexerType::Contextual)).is_err(),
            "H5-2 ({surface}): Python rejects a `__`-leading name token at grammar-parse; \
             lark-rs accepted it. grammar={g:?}"
        );
    }

    // Boundary — still accepted by both: a single leading underscore followed by a
    // letter (`_x`/`_X`), and non-leading underscores (`x__`/`a__b`). The fix must not
    // regress these.
    for (surface, g) in [
        ("single-underscore rule", "start: _x\n_x: \"a\"\n"),
        ("single-underscore terminal", "start: _X\n_X: \"a\"\n"),
        ("trailing underscores", "start: x__\nx__: \"a\"\n"),
        ("mid underscores", "start: a__b\na__b: \"a\"\n"),
    ] {
        let lark =
            Lark::new(g, opts(ParserAlgorithm::Lalr, LexerType::Contextual)).unwrap_or_else(|e| {
                panic!("H5-2 boundary ({surface}): must still build. grammar={g:?} err={e:?}")
            });
        assert!(
            lark.parse("a").is_ok(),
            "H5-2 boundary ({surface}): must still parse `a`. grammar={g:?}"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// EBNF loader: cross-alternative empty-arm dedup.
// ─────────────────────────────────────────────────────────────────────────────

/// H5-3 (MEDIUM, ebnf-loader). A bracket-optional `[A]` alternative distributes an
/// absent arm carrying a positional gap marker (`gaps=[..]`), while a sibling explicit
/// empty (`|`) alternative is a bare `(syms=[], gaps=[])`. `dedup_and_check_alts`
/// (`src/grammar/loader/compiler.rs`) keys dedup on the full `CompiledAlt` (syms+gaps),
/// so the two empty `x ->` arms differ by their gap marker and **both** survive into
/// lowering, colliding as a spurious LALR reduce/reduce. Python's `EBNF_to_BNF`
/// collapses them to one empty production and accepts. `A?`/`(A)?` in the same shape
/// route through the within-expansion canonicalizer and are fine — only `[...]` trips it.
/// Expected fix: canonicalize empty alternatives that differ only in gap markers in
/// `dedup_and_check_alts` (reusing `ebnf.rs`'s MP-vs-non-MP None-count rule).
#[test]
#[ignore = "XFAIL (bounty H5-3): [A] optional alternative beside an explicit empty | arm spuriously rejected as reduce/reduce; Python accepts"]
fn h5_3_optional_plus_empty_alt_accepted() {
    let g = "start: x\nx: [A]\n  |\nA: \"a\"\n";
    let lark = Lark::new(g, opts(ParserAlgorithm::Lalr, LexerType::Contextual))
        .expect("H5-3: Python accepts `[A] | (empty)`; lark-rs raised a spurious reduce/reduce");
    assert!(
        lark.parse("").is_ok(),
        "H5-3: empty input parses to start[x[]] under Python"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Python-`re` dialect: matched-set & escape divergences not in the H4-2 set.
// ─────────────────────────────────────────────────────────────────────────────

/// H5-4 (MEDIUM, lexer dialect). `\w`/`\W` are accepted as valid syntax by **both**
/// engines, but the *matched set* differs: the Rust `regex` crate's `\w` is the UTS#18
/// perl-word class (includes combining marks `\p{M}`, excludes `\p{No}`), while Python
/// `re`'s `\w` follows `str.isalnum()|"_"` (excludes combining marks, includes `No`/some
/// `Nl`). So lark-rs silently mis-tokenizes real Unicode text bidirectionally. `\d` and
/// `\s` are in parity — the divergence is `\w`/`\W`-specific. Distinct from H4-2 (which
/// is syntax Python *rejects at build*) and H5 (POSIX classes inside a `[...]`).
/// Expected fix: map `\w`/`\W` to Python `re`'s word set, or record an ADR-0004
/// deviation with this pin.
#[test]
#[ignore = "XFAIL (bounty H5-4): \\w Unicode word-class membership diverges (combining mark U+0301 over-accepted; superscript-two U+00B2 under-accepted)"]
fn h5_4_w_class_unicode_membership() {
    let g = "start: A\nA: /\\w/\n";
    let lark = Lark::new(g, opts(ParserAlgorithm::Lalr, LexerType::Basic)).expect("H5-4: builds");
    // U+00B2 SUPERSCRIPT TWO (category No): Python `\w` accepts (isalnum); lark-rs rejects.
    assert!(
        lark.parse("\u{00B2}").is_ok(),
        "H5-4: Python `\\w` matches U+00B2 (No, isalnum); lark-rs's Rust `\\w` excludes it"
    );
    // U+0301 COMBINING ACUTE (category Mn): Python `\w` rejects; lark-rs accepts.
    assert!(
        lark.parse("\u{0301}").is_err(),
        "H5-4: Python `\\w` excludes U+0301 (Mn combining mark); lark-rs's Rust `\\w` matches it"
    );
}

/// H5-5 (LOW–MEDIUM, lexer dialect / taxonomy). Python `re` supports the `\N{NAME}`
/// named-character escape (`\N{BULLET}` → U+2022); the Rust `regex` crate has no such
/// escape, so compilation fails and the failure is routed through the lookaround
/// analyzer's catch-all, which **mis-labels** it "backtracking-only syntax (backref /
/// atomic group / possessive)" — none of which `\N{}` is. Two defects: a parity break
/// (Python accepts) and a wrong error taxonomy. Distinct from H4-2 (`\p`/`\x{}`/`\z`):
/// those Python *rejects* (contract reject-like-Python), but Python *accepts* `\N{}`,
/// so the oracle-faithful contract is **support** (translate `\N{NAME}` to its
/// codepoint), or at minimum re-bucket the error as `InvalidRegex`, not `LookaroundScope`.
#[test]
#[ignore = "XFAIL (bounty H5-5): \\N{NAME} named-Unicode escape rejected (and mis-categorized as backtracking); Python accepts"]
fn h5_5_named_unicode_escape_supported() {
    let g = "start: A\nA: /\\N{BULLET}/\n";
    let lark = Lark::new(g, opts(ParserAlgorithm::Lalr, LexerType::Basic))
        .expect("H5-5: Python accepts `\\N{BULLET}` (→ U+2022); lark-rs rejects it at build");
    assert!(
        lark.parse("\u{2022}").is_ok(),
        "H5-5: `\\N{{BULLET}}` matches the bullet character under Python"
    );
}

/// H5-6 (LOW, lexer dialect). The Rust `regex` crate accepts `(?<name>...)` as a named
/// capture (angle syntax); Python `re` has **no** such spelling (only `(?P<name>...)`)
/// and rejects it at build (`unknown extension ?<x`). So lark-rs silently builds a
/// grammar Python rejects — unfalsifiable (ADR-0017). The lookbehind spellings
/// `(?<=...)`/`(?<!...)` must stay exempt; only `(?<` + name + `>` is the divergent form.
/// Expected fix: reject-like-Python (a categorized build error, alongside
/// `reject_global_inline_flags` in `PatternRe::new`).
#[test]
#[ignore = "XFAIL (bounty H5-6): regex-crate angle named-group (?<name>...) accepted; Python re rejects at build"]
fn h5_6_angle_named_group_rejected() {
    let g = "start: A\nA: /(?<x>a)/\n";
    assert!(
        Lark::new(g, opts(ParserAlgorithm::Lalr, LexerType::Basic)).is_err(),
        "H5-6: Python `re` has no `(?<name>...)` spelling and rejects it; lark-rs's regex crate accepted it"
    );
}

/// H5-7 (LOW, lexer dialect — NEEDS-DECISION contract). Under `/i`, Python `re` folds
/// ASCII `I`/`i` together with the Turkish dotted/dotless pair `İ`(U+0130)/`ı`(U+0131);
/// the Rust `regex` crate uses Unicode *simple* case folding, whose `I`/`i` class
/// excludes those two codepoints. So `A: /I/i` accepts `ı` under Python but lark-rs
/// rejects it (a *less*-permissive divergence — the only diverging pair; Kelvin/micro/
/// angstrom/ß/Σ all agree). Fix contract is a genuine fork: match Python's fold table
/// (expensive) vs preserve the divergence via an ADR (the `\<`/`\>` precedent). This
/// test pins the falsifiable Python behavior; if the decision is diverge-and-document,
/// delete it rather than un-ignore it.
#[test]
#[ignore = "XFAIL (bounty H5-7, needs-decision): Turkish dotless-i U+0131 not folded to ASCII i/I under /i; Python matches it"]
fn h5_7_turkish_i_casefold() {
    let g = "start: A\nA: /I/i\n";
    let lark = Lark::new(g, opts(ParserAlgorithm::Lalr, LexerType::Basic)).expect("H5-7: builds");
    assert!(
        lark.parse("\u{0131}").is_ok(),
        "H5-7: Python folds ASCII I against U+0131 (dotless i) under re.I; lark-rs's simple fold does not"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Grammar loader: the anonymous-terminal naming table.
// ─────────────────────────────────────────────────────────────────────────────

/// H5-8 (LOW, grammar-loader / naming). Python Lark's `_TERMINAL_NAMES` maps a set of
/// literal strings to friendly terminal names. lark-rs's `TERMINAL_NAMES`
/// (`src/grammar/loader/terminals.rs`) reproduced all 35 single-char entries but was
/// missing exactly two of Python's multi-char rows: `"\\"`→`BACKSLASH` and
/// `"\r\n"`→`CRLF`. So an anonymous `"\\"`/`"\r\n"` literal was named `__ANON_n` instead
/// of `BACKSLASH`/`CRLF`, diverging in the token's `type_` (value is correct). Surfaces
/// in the tree under `keep_all_tokens` and in error messages. One root cause, two
/// surfaces. Fixed (#366) by adding the two missing rows to `TERMINAL_NAMES`.
#[test]
fn h5_8_anon_terminal_naming_table() {
    for (g, input, expected) in [
        (
            "start: \"\\\\\" NAME\nNAME: /[a-z]+/\n",
            "\\foo",
            "BACKSLASH",
        ),
        (
            "start: \"\\r\\n\" NAME\nNAME: /[a-z]+/\n",
            "\r\nfoo",
            "CRLF",
        ),
    ] {
        let mut o = opts(ParserAlgorithm::Lalr, LexerType::Contextual);
        o.keep_all_tokens = true;
        let lark = Lark::new(g, o).expect("H5-8: grammar builds");
        let tree = lark.parse(input).expect("H5-8: input parses");
        let mut types = Vec::new();
        collect_token_types(&tree, &mut types);
        assert!(
            types.contains(&expected),
            "H5-8: Python names this anonymous literal {expected}; lark-rs used a generated __ANON name. types={types:?}"
        );
    }
}
