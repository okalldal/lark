//! Parity guard for the contextual lexer's follow-set union on a *shared* EBNF
//! repetition helper — the adversarial case raised while reviewing the
//! `rules_cache` EBNF-helper dedup (PR #98).
//!
//! Two structurally-identical `item*` loops sit in different parents whose
//! terminator terminals differ, and the second parent's terminator (`HIGH.2`)
//! overlaps the first parent's at higher priority:
//!
//! ```text
//! a: "(" item* "stop" ")"
//! e: "[" item* HIGH   "]"
//! ```
//!
//! The concern was that sharing the `*` wrapper could union the two parents'
//! follow-sets and widen a contextual scanner past what Python Lark does, making
//! lark-rs reject an `x*` grammar Python accepts. It does not — and *can't*:
//! `__star → __plus | ε` means `FOLLOW(__plus) ⊇ FOLLOW(__star)`, and Python
//! already shares the non-nullable `__plus` recurse core, so the scanner at the
//! shared `__plus`-reduce state already admits a superset of whatever the shared
//! `__star`-exit state could add, at the same input position. Sharing the wrapper
//! therefore widens no scanner beyond the core Python shares.
//!
//! These expectations are byte-for-byte what Python Lark 1.3.1 produces under
//! `parser="lalr", lexer="contextual"` on the same grammar and inputs:
//!   * `( foo, stop )` → REJECT (`stop` lexes as `HIGH` @ col 8 in both engines)
//!   * `( stop )`      → ACCEPT as `a` (the 0-item path is per-parent, no leak)
//!   * `[ foo, end ]`  → REJECT (`,` unexpected @ col 6)

use lark_rs::{Lark, LarkOptions, LexerType, ParserAlgorithm};

fn build() -> Lark {
    let grammar = r#"
start: a | e
a: "(" item* "stop" ")"
e: "[" item* HIGH "]"
item: NAME ","
NAME: /[a-z]+/
HIGH.2: /[a-z]+/
%ignore " "
"#;
    Lark::new(
        grammar,
        LarkOptions {
            parser: ParserAlgorithm::Lalr,
            lexer: LexerType::Contextual,
            start: vec!["start".to_string()],
            ..Default::default()
        },
    )
    .expect("grammar with two shared `item*` loops should build under LALR")
}

#[test]
fn shared_repetition_follow_union_matches_python_oracle() {
    let p = build();

    // ≥1 item: the post-`foo,` token is lexed at the *shared* loop-body reduce
    // state, whose follow-union admits `HIGH`; `HIGH.2` outranks the `"stop"`
    // literal, so both engines mis-lex and reject. This is the leak — and it is
    // identical in Python, because the shared core (not the wrapper) carries it.
    assert!(
        p.parse("( foo, stop )").is_err(),
        "`( foo, stop )` must be rejected, matching Python Lark 1.3.1 \
         (HIGH outranks the \"stop\" literal at the shared loop-body reduce)"
    );

    // 0 items: the empty-loop decision lives in a per-parent state (distinct LR(0)
    // kernel), so no follow-union — `stop` lexes as the literal and `a` accepts.
    assert!(
        p.parse("( stop )").is_ok(),
        "`( stop )` must parse as `a` — the 0-item path is per-parent, no leak"
    );

    // The `e` branch: `end` is HIGH, but `foo,` injects a stray COMMA that `e`
    // never admits, so both engines reject at the comma.
    assert!(
        p.parse("[ foo, end ]").is_err(),
        "`[ foo, end ]` must be rejected at the unexpected `,`, matching Python"
    );
}
