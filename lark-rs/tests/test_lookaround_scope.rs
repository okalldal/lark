//! **THE LOOKAROUND SCOPE SCOREBOARD** — the end-to-end contract for every pattern
//! the lexer refuses, under the two-category taxonomy of `docs/LOOKAROUND_SCOPE.md`:
//!
//!   * **`Scope::OutOfScope`** — by-design non-goals. Each case asserts the pattern
//!     is rejected at grammar build with the categorized error. These rejections are
//!     the *contract*: a case that starts building is a violation, full stop.
//!   * **`Scope::NotYetImplemented`** — in-principle-lowerable patterns rejected
//!     conservatively (never silently mis-lowered). Each case asserts the *clean
//!     categorized rejection*, and doubles as a **promotion tripwire**:
//!
//! # The promotion protocol (read this before "fixing" a red NYI case)
//!
//! If an NYI case here fails because the pattern **started building**, that is a
//! deliberate tripwire, not a bug in this test. A pattern moves from
//! NotYetImplemented to supported only through the Stage-B audit ladder
//! (`docs/LEXER_DFA_PLAN.md`): an exact recognizer or a proven gate extension, a
//! generative equivalence sweep against the `fancy-regex` dev-oracle, a mutation
//! canary showing the net catches a wrong lowering, a route-level pin, and a
//! scanner-differential population entry. Then — and only then — move the case out
//! of this table into a `*_lowers` pin and update `docs/LOOKAROUND_SCOPE.md`.
//! (Precedents: the M4 STRING splice; the Stage-B REGEXP/LONG_STRING idioms; the
//! `is_leftmost_longest` semantic-gate widening that admitted `python.DEC_NUMBER`.)
//!
//! An OutOfScope case that starts building is worse: a by-design rejection was
//! silently promoted. Revert it, or take the scope change through a documented
//! decision in `docs/LOOKAROUND_SCOPE.md` first.
//!
//! The **exhaustiveness meta-test** at the bottom forces every `Rejection` and
//! `DeclineReason` variant to either appear in this table or carry an explicit
//! "defensive (unreachable end-to-end)" justification — so adding a refusal reason
//! without scoring it does not compile.

use lark_rs::{
    DeclineReason, GrammarError, Lark, LarkError, LarkOptions, LexerType, LookaroundIssue,
    ParserAlgorithm, Rejection, Scope,
};

struct ScopeCase {
    name: &'static str,
    grammar: &'static str,
    /// `LarkOptions::g_regex_flags` for the build — non-zero only for the
    /// global-VERBOSE case (the verbose hazard has two spellings: a `(?x:…)`
    /// wrapper in the grammar, and this option).
    g_regex_flags: u32,
    scope: Scope,
    issue: LookaroundIssue,
    /// Substrings the user-facing message must contain (the terminal name and the
    /// scope-doc pointer are asserted for every case automatically).
    msg_contains: &'static [&'static str],
}

/// One scoreboard row per refused shape. `start: T "x"` keeps every grammar trivially
/// LALR-clean so the only possible build failure is the lexer's scope error.
fn scoreboard() -> Vec<ScopeCase> {
    use DeclineReason as D;
    use LookaroundIssue::{Declined, Rejected};
    use Rejection as R;
    vec![
        // ── OutOfScope: by-design non-goals ────────────────────────────────────
        ScopeCase {
            name: "internal_lookahead",
            grammar: "start: T \"x\"\nT: /a(?=b)c/\n",
            g_regex_flags: 0,
            scope: Scope::OutOfScope,
            issue: Rejected(R::Internal),
            msg_contains: &["(?=b)", "not supported (by design)"],
        },
        ScopeCase {
            // The classic block-comment shape: the guard nested inside a quantified
            // group. Python Lark accepts it (backtracking engine) — a NAMED parity
            // break; the audited delimited-token idioms are the only growth path
            // for internal lookahead. (Its lookaround-free rewritability is pinned
            // by `test_lookaround.rs::block_comment_match_length_equivalence`.)
            name: "internal_lookahead_in_quantified_group",
            grammar: "start: T \"x\"\nT: /\\/\\*(\\*(?!\\/)|[^*])*\\*\\//\n",
            g_regex_flags: 0,
            scope: Scope::OutOfScope,
            issue: Rejected(R::Internal),
            msg_contains: &["not supported (by design)"],
        },
        ScopeCase {
            name: "backref_in_assertion",
            grammar: "start: T \"x\"\nT: /(a)(?=\\1)b/\n",
            g_regex_flags: 0,
            scope: Scope::OutOfScope,
            issue: Rejected(R::Backref),
            msg_contains: &["backreference"],
        },
        ScopeCase {
            name: "nested_assertion",
            grammar: "start: T \"x\"\nT: /(?=(?!a)b)c/\n",
            g_regex_flags: 0,
            scope: Scope::OutOfScope,
            issue: Rejected(R::Nested),
            msg_contains: &["nested"],
        },
        ScopeCase {
            name: "quantified_assertion",
            grammar: "start: T \"x\"\nT: /a(?=b)?/\n",
            g_regex_flags: 0,
            scope: Scope::OutOfScope,
            issue: Rejected(R::QuantifiedAssertion),
            msg_contains: &["quantifier"],
        },
        ScopeCase {
            // Python `re` rejects variable-width lookbehind too ("look-behind
            // requires fixed-width pattern") — this rejection is oracle PARITY,
            // not a break.
            name: "variable_width_lookbehind",
            grammar: "start: T \"x\"\nT: /(?<!a*)b/\n",
            g_regex_flags: 0,
            scope: Scope::OutOfScope,
            issue: Rejected(R::VariableWidthBehind),
            msg_contains: &["lookbehind"],
        },
        ScopeCase {
            // No lookaround at all — backtracking-only syntax (the one named parity
            // break class with Python Lark's backtracking engine).
            name: "top_level_backref",
            grammar: "start: T \"x\"\nT: /(a)\\1b/\n",
            g_regex_flags: 0,
            scope: Scope::OutOfScope,
            issue: Declined(D::BacktrackingOnlySyntax),
            msg_contains: &["backtracking-only", "parity"],
        },
        ScopeCase {
            name: "zero_width_lookbehind_body",
            grammar: "start: T \"x\"\nT: /a(?<=())b/\n",
            g_regex_flags: 0,
            scope: Scope::OutOfScope,
            issue: Declined(D::ZeroWidthLookbehindBody),
            msg_contains: &["zero-width"],
        },
        ScopeCase {
            name: "assertion_only_zero_width_branch",
            grammar: "start: T \"x\"\nT: /(?!a)/\n",
            g_regex_flags: 0,
            scope: Scope::OutOfScope,
            issue: Declined(D::ZeroWidthBranch),
            msg_contains: &["zero-width"],
        },
        // ── NotYetImplemented: conservative rejections / promotion tripwires ───
        ScopeCase {
            // Regular trailing context (flex's `r/s`); implementable via a
            // reverse-scan or product construction. NYI, no current plan.
            name: "unbounded_trailing_lookahead",
            grammar: "start: T \"x\"\nT: /[a-z]+(?=ab+)/\n",
            g_regex_flags: 0,
            scope: Scope::NotYetImplemented,
            issue: Rejected(R::Unbounded),
            msg_contains: &["not yet implemented"],
        },
        ScopeCase {
            // Python `re` accepts this (the lookbehind itself is fixed-width); our
            // M3 lowering needs a fixed offset from the match start. The headline
            // NYI case.
            name: "variable_offset_lookbehind",
            grammar: "start: T \"x\"\nT: /\\w+(?<!_)q/\n",
            g_regex_flags: 0,
            scope: Scope::NotYetImplemented,
            issue: Declined(D::VariableOffsetLookbehind),
            msg_contains: &["variable-width prefix"],
        },
        ScopeCase {
            // The longest-accept accumulator cannot reproduce a base that prefers a
            // shorter match; the `is_leftmost_longest` semantic gate proves the
            // tractable cases and declines exactly these.
            name: "order_sensitive_guarded_base",
            grammar: "start: T \"x\"\nT: /(ab|abc)(?!z)/\n",
            g_regex_flags: 0,
            scope: Scope::NotYetImplemented,
            issue: Declined(D::NonRealizableGuardedBase),
            msg_contains: &["guard-realizable"],
        },
        ScopeCase {
            name: "lazy_guarded_base",
            grammar: "start: T \"x\"\nT: /ab??(?!c)/\n",
            g_regex_flags: 0,
            scope: Scope::NotYetImplemented,
            issue: Declined(D::NonRealizableGuardedBase),
            msg_contains: &["guard-realizable"],
        },
        ScopeCase {
            // A lookbehind buried in an interior group (not a vacuous whole-arm
            // wrapper, which IS unwrapped and lowers) — needs group-aware peeling.
            name: "lookbehind_in_interior_group",
            grammar: "start: T \"x\"\nT: /(a(?<!b))c/\n",
            g_regex_flags: 0,
            scope: Scope::NotYetImplemented,
            issue: Declined(D::NestedInGroup),
            msg_contains: &["nested inside a group"],
        },
        ScopeCase {
            // The lookaround analyzer is not verbose-aware; the flag-wrapper strip
            // deliberately refuses `x` (a stripped body would miscount whitespace
            // as literal width — a false-accept hazard).
            name: "verbose_wrapped_lookaround",
            grammar: "start: T \"x\"\nT: /(?x:[0-9]+ (?![0-9]))/\n",
            g_regex_flags: 0,
            scope: Scope::NotYetImplemented,
            issue: Declined(D::VerboseMode),
            msg_contains: &["VERBOSE"],
        },
        ScopeCase {
            // The same hazard's OTHER spelling (PR #137 review, blocker 1): no
            // wrapper in the grammar — `g_regex_flags = VERBOSE` puts the whole
            // scanner under `(?x)` while the analyzer would count the pattern's
            // whitespace as literal width. Must refuse identically.
            name: "verbose_global_lookaround",
            grammar: "start: T \"x\"\nT: /[0-9]+ (?![0-9])/\n",
            g_regex_flags: lark_rs::grammar::terminal::flags::VERBOSE,
            scope: Scope::NotYetImplemented,
            issue: Declined(D::VerboseMode),
            msg_contains: &["VERBOSE"],
        },
    ]
}

fn build(
    grammar: &str,
    g_regex_flags: u32,
    parser: ParserAlgorithm,
    lexer: LexerType,
) -> Result<Lark, LarkError> {
    Lark::new(
        grammar,
        LarkOptions {
            parser,
            lexer,
            g_regex_flags,
            start: vec!["start".to_string()],
            ..Default::default()
        },
    )
}

fn assert_case(case: &ScopeCase, parser: ParserAlgorithm, lexer: LexerType, tag: &str) {
    match build(case.grammar, case.g_regex_flags, parser, lexer) {
        Err(LarkError::Grammar(GrammarError::LookaroundScope {
            terminal,
            scope,
            issue,
            msg,
            ..
        })) => {
            assert_eq!(
                scope, case.scope,
                "{tag}: wrong scope category\n  msg: {msg}"
            );
            assert_eq!(
                issue, case.issue,
                "{tag}: wrong issue variant\n  msg: {msg}"
            );
            assert_eq!(terminal, "T", "{tag}: wrong terminal named");
            for want in case
                .msg_contains
                .iter()
                .chain(["T", "docs/LOOKAROUND_SCOPE.md"].iter())
            {
                assert!(
                    msg.contains(want),
                    "{tag}: message must contain {want:?}:\n  {msg}"
                );
            }
        }
        Err(other) => panic!("{tag}: expected the categorized LookaroundScope error, got {other}"),
        Ok(_) => match case.scope {
            Scope::OutOfScope => panic!(
                "{tag}: an OUT-OF-SCOPE pattern built — a by-design rejection was \
                 silently promoted. Revert, or change the scope through a documented \
                 decision in docs/LOOKAROUND_SCOPE.md first."
            ),
            Scope::NotYetImplemented => panic!(
                "{tag}: an NYI pattern started building — the PROMOTION TRIPWIRE. If \
                 this lowering is intentional, complete the Stage-B audit ladder and \
                 move this row to a `*_lowers` pin (see the module doc's promotion \
                 protocol); never just delete the case."
            ),
        },
    }
}

/// Every scoreboard case, end-to-end through `Lark::new` on the default engine
/// (LALR × contextual, Dfa backend).
#[test]
fn scoreboard_rejects_every_case_with_its_category() {
    for case in scoreboard() {
        assert_case(
            &case,
            ParserAlgorithm::Lalr,
            LexerType::Contextual,
            case.name,
        );
    }
}

/// The same categorized refusal on every other engine path: the basic lexer, and the
/// Earley dynamic / dynamic_complete per-terminal matchers (which lower terminals
/// individually — `LoweredTerminalMatcher` — and must refuse identically).
#[test]
fn scoreboard_rejections_are_identical_across_engines() {
    for case in scoreboard() {
        for (parser, lexer, tag) in [
            (ParserAlgorithm::Lalr, LexerType::Basic, "lalr/basic"),
            (
                ParserAlgorithm::Earley,
                LexerType::Dynamic,
                "earley/dynamic",
            ),
            (
                ParserAlgorithm::Earley,
                LexerType::DynamicComplete,
                "earley/dynamic_complete",
            ),
        ] {
            // Oracle carve-out (#276): a pure-assertion, min-width-0 terminal is a
            // *zero-width regexp* on the dynamic Earley lexer. Python's
            // `EarleyRegexpMatcher` rejects it with "Dynamic Earley doesn't allow
            // zero-width regexps" at matcher construction — *before* any lookaround
            // classification — so on the two dynamic paths the categorized
            // LookaroundScope error this row asserts on every other engine is
            // pre-empted by the zero-width error (verified against Python Lark 1.3.1).
            // lark-rs mirrors that ordering in `DynamicMatcher::new`. The LALR/basic
            // path still gives the LookaroundScope error (its zero-width check runs on
            // the combined scanner, not per-terminal), so only the dynamic rows differ.
            let dynamic = matches!(lexer, LexerType::Dynamic | LexerType::DynamicComplete);
            if case.name == "assertion_only_zero_width_branch" && dynamic {
                assert_dynamic_zero_width_rejected(
                    &case,
                    parser,
                    lexer,
                    &format!("{} [{tag}]", case.name),
                );
                continue;
            }
            assert_case(&case, parser, lexer, &format!("{} [{tag}]", case.name));
        }
    }
}

/// Assert a case is rejected on a dynamic Earley lexer with the zero-width-regexp
/// error (Python's `EarleyRegexpMatcher` gate), not the LookaroundScope error.
fn assert_dynamic_zero_width_rejected(
    case: &ScopeCase,
    parser: ParserAlgorithm,
    lexer: LexerType,
    tag: &str,
) {
    match build(case.grammar, case.g_regex_flags, parser, lexer) {
        Err(LarkError::Grammar(GrammarError::Other { msg })) => assert!(
            msg.contains("Dynamic Earley doesn't allow zero-width regexps"),
            "{tag}: expected the zero-width-regexp rejection, got: {msg}"
        ),
        Err(other) => panic!("{tag}: expected the zero-width-regexp rejection, got {other}"),
        Ok(_) => {
            panic!("{tag}: a zero-width regexp built under the dynamic lexer (Python rejects it)")
        }
    }
}

/// **Exhaustiveness meta-test.** Every `Rejection` and `DeclineReason` variant must
/// either map to scoreboard case(s) or carry an explicit defensive justification.
/// The `match` arms are deliberately non-wildcard: adding a variant fails to compile
/// until it is consciously scored here AND categorized in `scope()`.
#[test]
fn every_refusal_variant_is_scored() {
    enum Entry {
        Cases(&'static [&'static str]),
        /// Unreachable end-to-end; say why.
        Defensive(&'static str),
    }
    use Entry::{Cases, Defensive};

    let rejection_entry = |r: Rejection| -> Entry {
        match r {
            Rejection::Unbounded => Cases(&["unbounded_trailing_lookahead"]),
            Rejection::Internal => Cases(&[
                "internal_lookahead",
                "internal_lookahead_in_quantified_group",
            ]),
            Rejection::Backref => Cases(&["backref_in_assertion"]),
            Rejection::Nested => Cases(&["nested_assertion"]),
            Rejection::VariableWidthBehind => Cases(&["variable_width_lookbehind"]),
            Rejection::QuantifiedAssertion => Cases(&["quantified_assertion"]),
        }
    };
    let decline_entry = |d: DeclineReason| -> Entry {
        match d {
            DeclineReason::QuantifiedLookbehind => Defensive(
                "shadowed by the classifier: a quantified assertion is rejected as \
                 Rejection::QuantifiedAssertion before the lowering runs",
            ),
            DeclineReason::VariableOffsetLookbehind => Cases(&["variable_offset_lookbehind"]),
            DeclineReason::UnboundedLookbehindBody => Defensive(
                "shadowed by the classifier: an unbounded lookbehind body is rejected \
                 as Rejection::VariableWidthBehind before the lowering runs",
            ),
            DeclineReason::ZeroWidthLookbehindBody => Cases(&["zero_width_lookbehind_body"]),
            DeclineReason::InteriorLookahead => Defensive(
                "shadowed by the classifier: an interior lookahead is rejected as \
                 Rejection::Internal before the lowering runs",
            ),
            DeclineReason::ZeroWidthBranch => Cases(&["assertion_only_zero_width_branch"]),
            DeclineReason::NestedInGroup => Cases(&["lookbehind_in_interior_group"]),
            DeclineReason::NonRealizableGuardedBase => {
                Cases(&["order_sensitive_guarded_base", "lazy_guarded_base"])
            }
            DeclineReason::EmptyArmNotRealizable => Defensive(
                "requires a string-idiom shape whose empty arm has a \
                 non-length-deterministic prefix; the bundled prefix is \
                 length-deterministic and the recognizer admits no other — kept as a \
                 conservative in-lowering guard",
            ),
            DeclineReason::FrontendParse => Defensive(
                "the lookaround analyzer parses a superset of what terminal loading \
                 accepts (`PatternRe::new` gates load on the same parser), so no \
                 loadable pattern reaches routing unparsable — kept as the \
                 conservative catch-all",
            ),
            DeclineReason::VerboseMode => {
                Cases(&["verbose_wrapped_lookaround", "verbose_global_lookaround"])
            }
            DeclineReason::BacktrackingOnlySyntax => Cases(&["top_level_backref"]),
        }
    };

    let all_rejections = [
        Rejection::Unbounded,
        Rejection::Internal,
        Rejection::Backref,
        Rejection::Nested,
        Rejection::VariableWidthBehind,
        Rejection::QuantifiedAssertion,
    ];
    let all_declines = [
        DeclineReason::QuantifiedLookbehind,
        DeclineReason::VariableOffsetLookbehind,
        DeclineReason::UnboundedLookbehindBody,
        DeclineReason::ZeroWidthLookbehindBody,
        DeclineReason::InteriorLookahead,
        DeclineReason::ZeroWidthBranch,
        DeclineReason::NestedInGroup,
        DeclineReason::NonRealizableGuardedBase,
        DeclineReason::EmptyArmNotRealizable,
        DeclineReason::FrontendParse,
        DeclineReason::VerboseMode,
        DeclineReason::BacktrackingOnlySyntax,
    ];

    let table: Vec<ScopeCase> = scoreboard();
    let table_names: std::collections::HashSet<&str> = table.iter().map(|c| c.name).collect();
    let mut referenced: std::collections::HashSet<&str> = std::collections::HashSet::new();

    let entries = all_rejections
        .iter()
        .map(|&r| (format!("{r:?}"), rejection_entry(r)))
        .chain(
            all_declines
                .iter()
                .map(|&d| (format!("{d:?}"), decline_entry(d))),
        );
    for (variant, entry) in entries {
        match entry {
            Cases(names) => {
                assert!(!names.is_empty(), "{variant}: empty case list");
                for n in names {
                    assert!(
                        table_names.contains(n),
                        "{variant} references a scoreboard case {n:?} that does not exist"
                    );
                    referenced.insert(n);
                }
            }
            Defensive(why) => assert!(!why.is_empty()),
        }
    }
    // …and the mapping is onto: no scoreboard row is orphaned from the type system.
    for n in &table_names {
        assert!(
            referenced.contains(n),
            "scoreboard case {n:?} is not referenced by any refusal variant"
        );
    }
}
