//! Route-level tests for the **typed lowering decision** (`LoweringRoute`) introduced by
//! the routing-contract split (`docs/LEXER_DFA_PLAN.md`, "Runtime routing taxonomy").
//!
//! PR #131 documented as design debt that `lower_terminal_dotall`'s
//! `Result<Lowered, GrammarError>` conflated two very different `Err`s: a *transitional
//! decline* that should route to `fancy-regex`, and a *permanent out-of-shape rejection*
//! the classifier refuses. [`route_terminal`] / [`route_terminal_dotall`] now make that
//! split explicit in the type. These tests pin each route so a future change that, say,
//! makes an unsupported assertion suddenly lower (or a declined idiom reject) is caught.
//!
//! The companion `lower_terminal` API tests (the flattened `Result` view) stay in
//! `test_lowering_reject.rs`. The L4 policy flip (refusals are categorized build errors; no fancy
//! fallback) is pinned both here (`unsupported_user_lookaround_is_now_a_categorized_build_error`)
//! and by the bundled-terminal status tripwire in `test_string_splice.rs`.

use lark_rs::{
    basic_lexer_conf, load_grammar, lower, route_terminal, route_terminal_dotall, BasicLexer,
    DeclineReason, Lexer, LexerBackend, LoweringRoute, Rejection,
};

/// The bundled `python.STRING` pattern, verbatim — lowers via the M4 opening-guard splice.
const STRING_RAW: &str =
    r#"([ubf]?r?|r[ubf])("(?!"").*?(?<!\\)(\\\\)*?"|'(?!'').*?(?<!\\)(\\\\)*?')"#;
/// The bundled `python.LONG_STRING` (DOTALL): its `(?<!\\)` sits after a variable-width
/// `.*?` — the position the generic fixed-offset lowering declines — but the whole
/// terminal is the exact Stage-B **long-string idiom** (`recognize_long_string_idiom`),
/// which lowers it by absorbing the escape-parity close into escape-pair body items.
const LONG_STRING_RAW: &str =
    r#"([ubf]?r?|r[ubf])(""".*?(?<!\\)(\\\\)*?"""|'''.*?(?<!\\)(\\\\)*?''')"#;
/// The bundled `lark.REGEXP`: its `(?!\/)` is internal to the top-level walk, but the
/// whole terminal is the exact Stage-B **regex-literal idiom**
/// (`recognize_regexp_idiom`), which lowers it — the guard reduces to a non-empty-body
/// condition (`*?` → `+?`), one unguarded lookaround-free branch.
const REGEXP_RAW: &str = r#"\/(?!\/)(\\\/|\\\\|[^\/])*?\/[imslux]*"#;

/// A plain terminal (no lookaround) routes to [`LoweringRoute::Plain`].
#[test]
fn plain_pattern_routes_to_plain() {
    assert_eq!(
        route_terminal("PLAIN", r"[A-Za-z_][A-Za-z0-9_]*"),
        LoweringRoute::Plain
    );
}

/// A known supported shape (a trailing-boundary lookahead) routes to
/// [`LoweringRoute::Lowered`] with non-empty branches.
#[test]
fn known_lowered_pattern_routes_to_lowered() {
    match route_terminal("TRAIL", "[0-9]+(?![0-9])") {
        LoweringRoute::Lowered(branches) => assert!(!branches.is_empty()),
        other => panic!("a trailing-boundary terminal must lower, got {other:?}"),
    }
    // A leading boundary and a bounded lookbehind lower too.
    assert!(matches!(
        route_terminal("LEAD", "(?!--)[a-z]+"),
        LoweringRoute::Lowered(_)
    ));
    assert!(matches!(
        route_terminal("BEHIND", "(?<!_)/"),
        LoweringRoute::Lowered(_)
    ));
}

/// `python.STRING` is the marquee M4 splice — it routes to [`LoweringRoute::Lowered`]
/// (four branches: a non-empty + an empty arm per quote kind).
#[test]
fn python_string_routes_to_lowered() {
    match route_terminal_dotall("STRING", STRING_RAW, false) {
        LoweringRoute::Lowered(branches) => assert_eq!(
            branches.len(),
            4,
            "STRING lowers to 2 arms × {{non-empty, empty}} = 4 branches"
        ),
        other => panic!("python.STRING must lower, got {other:?}"),
    }
}

/// `python.LONG_STRING` now routes to [`LoweringRoute::Lowered`] via the Stage-B
/// long-string idiom: two **unguarded** lookaround-free branches (one per quote arm,
/// prefix duplicated), the `(?<!\\)(\\\\)*?` escape-parity close absorbed by the
/// escape-pair body normalization, the lazy `*?` kept. It is in particular no longer
/// `Declined` — that route was its pre-idiom outcome — and was never
/// `Unsupported`.
#[test]
fn python_long_string_routes_to_lowered() {
    match route_terminal_dotall("LONG_STRING", LONG_STRING_RAW, true) {
        LoweringRoute::Lowered(branches) => {
            assert_eq!(
                branches.len(),
                2,
                "one branch per quote arm, got {branches:#?}"
            );
            assert_eq!(
                branches[0].regex,
                r#"([ubf]?r?|r[ubf])"""(?:[^\\]|\\.)*?""""#
            );
            assert_eq!(
                branches[1].regex,
                r#"([ubf]?r?|r[ubf])'''(?:[^\\]|\\.)*?'''"#
            );
            for b in &branches {
                assert!(
                    b.leading.is_none() && b.trailing.is_none() && b.lookbehind.is_empty(),
                    "the branches are unguarded — the lookbehind is absorbed, not carried"
                );
            }
        }
        other => {
            panic!("python.LONG_STRING must now lower via the long-string idiom, got {other:?}")
        }
    }
    // Non-DOTALL instances of the same shape exclude the newline the original `.`
    // excludes — the `dotall` threading is per-instance, not baked into the idiom.
    match route_terminal_dotall("LONG_STRING", LONG_STRING_RAW, false) {
        LoweringRoute::Lowered(branches) => assert!(
            branches[0].regex.contains(r"[^\\\n]"),
            "the non-dotall lowering must exclude \\n from the body class, got {:?}",
            branches[0].regex
        ),
        other => panic!("the non-dotall instance must lower too, got {other:?}"),
    }
}

/// A **per-instance decline is still constructible** — the route LONG_STRING vacated. A
/// variable-offset lookbehind outside any recognized idiom (`\w+(?<!_)x`) routes to
/// [`LoweringRoute::Declined`] with the **typed reason**
/// [`DeclineReason::VariableOffsetLookbehind`]: every assertion is a *supported shape*
/// (bounded lookbehind), so it is not `Unsupported`; the lowering declines this instance
/// because the offset is not fixed — a clean categorized refusal, not a reject.
#[test]
fn variable_offset_lookbehind_routes_to_declined() {
    match route_terminal("VAROFF", r"\w+(?<!_)x") {
        LoweringRoute::Declined { reason, message } => {
            assert_eq!(reason, DeclineReason::VariableOffsetLookbehind);
            assert!(
                message.contains("VAROFF") && message.contains("not yet implemented"),
                "the message names the terminal and the NYI category: {message}"
            );
        }
        other => panic!("a variable-offset lookbehind must be Declined, got {other:?}"),
    }
}

/// `lark.REGEXP` now routes to [`LoweringRoute::Lowered`] via the Stage-B regex-literal
/// idiom: a **single unguarded** lookaround-free branch (the `(?!\/)` reduces to a
/// non-empty body, `*?` → `+?`). It is in particular no longer `Unsupported(Internal)` —
/// that route was its pre-idiom verdict — and not `Declined`.
#[test]
fn lark_regexp_routes_to_lowered() {
    match route_terminal("REGEXP", REGEXP_RAW) {
        LoweringRoute::Lowered(branches) => {
            assert_eq!(
                branches.len(),
                1,
                "the regexp idiom lowers to exactly one branch, got {branches:#?}"
            );
            let b = &branches[0];
            assert_eq!(b.regex, r"\/(\\\/|\\\\|[^\/])+?\/[imslux]*");
            assert!(
                b.leading.is_none() && b.trailing.is_none() && b.lookbehind.is_empty(),
                "the branch is unguarded — the guard is absorbed, not carried"
            );
        }
        other => panic!("lark.REGEXP must now lower via the regex-literal idiom, got {other:?}"),
    }
}

/// The identical `(?!\/)` **outside** the recognized idiom is still out-of-shape: the
/// verilog block-comment pattern (the guard nested inside a `(…)*`) and a greedy
/// near-miss of the idiom both stay [`LoweringRoute::Unsupported`] with
/// [`Rejection::Internal`] — the recognizer is the gate, not the assertion's spelling.
#[test]
fn forbid_slash_outside_the_idiom_still_routes_to_unsupported_internal() {
    for (name, pat) in [
        ("MULTILINE_COMMENT", r"\/\*(\*(?!\/)|[^*])*\*\/"),
        ("GREEDY_NEAR_MISS", r"\/(?!\/)(\\\/|\\\\|[^\/])*\/[imslux]*"),
    ] {
        match route_terminal(name, pat) {
            LoweringRoute::Unsupported { rejection, .. } => {
                assert_eq!(rejection, Rejection::Internal, "{name}");
            }
            other => panic!("{name} must stay Unsupported(Internal), got {other:?}"),
        }
    }
}

/// A reject-corpus *internal* lookahead routes to [`LoweringRoute::Unsupported`] with
/// [`Rejection::Internal`], carrying the assertion source and a build message.
#[test]
fn internal_lookahead_routes_to_unsupported_internal() {
    match route_terminal("INT", "a(?=b)c") {
        LoweringRoute::Unsupported {
            assertion,
            rejection,
            message,
        } => {
            assert_eq!(rejection, Rejection::Internal);
            assert_eq!(assertion, "(?=b)");
            assert!(
                message.contains("INT"),
                "message names the terminal: {message}"
            );
            assert!(
                message.contains("(?=b)"),
                "message shows the assertion: {message}"
            );
        }
        other => panic!("an internal lookahead must be Unsupported(Internal), got {other:?}"),
    }
}

/// A known unbounded assertion routes to [`LoweringRoute::Unsupported`] with
/// [`Rejection::Unbounded`]. Leading unbounded lookaheads are now supported
/// (LeadingBoundary), so this test uses a *trailing* unbounded one.
#[test]
fn unbounded_lookahead_routes_to_unsupported_unbounded() {
    match route_terminal("UNB", "[a-z]+(?=ab+)") {
        LoweringRoute::Unsupported { rejection, .. } => assert_eq!(rejection, Rejection::Unbounded),
        other => panic!("an unbounded lookahead must be Unsupported(Unbounded), got {other:?}"),
    }
}

/// A backref assertion and a nested assertion each route to [`LoweringRoute::Unsupported`]
/// with their expected rejection.
#[test]
fn backref_and_nested_route_to_unsupported() {
    match route_terminal("BR", r#"(a)(?=\1)b"#) {
        LoweringRoute::Unsupported { rejection, .. } => assert_eq!(rejection, Rejection::Backref),
        other => panic!("a backref assertion must be Unsupported(Backref), got {other:?}"),
    }
    match route_terminal("NEST", "(?=(?!a)b)c") {
        LoweringRoute::Unsupported { rejection, .. } => assert_eq!(rejection, Rejection::Nested),
        other => panic!("a nested assertion must be Unsupported(Nested), got {other:?}"),
    }
}

/// The route flattens back to the historical `lower_terminal` API consistently: every
/// non-`Plain`/`Lowered` route is an `Err`, every `Lowered`/`Plain` is the matching `Ok`.
#[test]
fn route_flattens_to_lower_terminal_api() {
    use lark_rs::{lower_terminal, Lowered};

    assert!(matches!(
        lower_terminal("PLAIN", r"[a-z]+"),
        Ok(Lowered::Plain)
    ));
    assert!(matches!(
        lower_terminal("TRAIL", "[0-9]+(?![0-9])"),
        Ok(Lowered::Branches(_))
    ));
    // Declined, Unsupported both flatten to Err. (LONG_STRING lowers now, so the
    // decline leg uses a per-instance variable-offset lookbehind instead.)
    assert!(lower_terminal("VAROFF", r"\w+(?<!_)x").is_err());
    assert!(lower_terminal("INT", "a(?=b)c").is_err());
}

/// **The L4 policy-flip pin** (the inversion of the historical
/// `unsupported_user_lookaround_currently_compat_falls_back_to_fancy`). An
/// out-of-shape *user* lookahead routes to [`LoweringRoute::Unsupported`] at the route
/// level, and since L4 the engine path turns that into a **categorized build error**
/// (`GrammarError::LookaroundScope`, scope `OutOfScope`) — there is no `fancy-regex`
/// compatibility fallback any more. This is the test the old pin's doc said would
/// "flag that the policy flip happened": it has, and this asserts the new contract
/// end-to-end (grammar load → lexer build).
#[test]
fn unsupported_user_lookaround_is_now_a_categorized_build_error() {
    use lark_rs::{GrammarError, LookaroundIssue, Scope};

    // `TOK`'s `a(?=b)b` is a genuinely *internal* lookahead (between two elements, so
    // neither leading nor trailing) — the route says Unsupported(Internal).
    assert!(matches!(
        route_terminal("TOK", "a(?=b)b"),
        LoweringRoute::Unsupported {
            rejection: Rejection::Internal,
            ..
        }
    ));

    // The grammar still LOADS (terminal validation defers the lookaround verdict to
    // the lexer build), but the Dfa BasicLexer build refuses with the categorized
    // OutOfScope error.
    let grammar = "start: TOK B\nTOK: /a(?=b)b/\nB: \"c\"\n";
    let g = load_grammar(grammar, &["start".to_string()], false, false)
        .expect("grammar loads; the scope verdict lands at lexer build");
    let cg = lower(&g);
    let conf = basic_lexer_conf(&cg, 0).with_backend(LexerBackend::Dfa);
    match BasicLexer::new(&conf) {
        Err(GrammarError::LookaroundScope {
            scope,
            issue,
            terminal,
            msg,
            ..
        }) => {
            assert_eq!(scope, Scope::OutOfScope);
            assert_eq!(issue, LookaroundIssue::Rejected(Rejection::Internal));
            assert_eq!(terminal, "TOK");
            assert!(
                msg.contains("docs/LOOKAROUND_SCOPE.md"),
                "the error points the user at the scope doc: {msg}"
            );
        }
        Err(other) => panic!("expected the categorized LookaroundScope error, got {other:?}"),
        Ok(_) => panic!(
            "an out-of-shape user lookahead BUILT — the L4 reject contract regressed \
             (or the pattern was silently promoted; see docs/LOOKAROUND_SCOPE.md)"
        ),
    }
}
