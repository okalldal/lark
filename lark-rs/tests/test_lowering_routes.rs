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
//! `test_lowering_reject.rs`. The runtime compatibility fallback is pinned both here
//! (`unsupported_user_lookaround_currently_compat_falls_back_to_fancy`, the direct pin) and
//! by the bundled-terminal status tripwire in `test_string_splice.rs`.

use lark_rs::{
    basic_lexer_conf, load_grammar, lower, route_terminal, route_terminal_dotall, BasicLexer,
    Lexer, LexerBackend, LoweringRoute, Rejection,
};

/// The bundled `python.STRING` pattern, verbatim — lowers via the M4 opening-guard splice.
const STRING_RAW: &str =
    r#"([ubf]?r?|r[ubf])("(?!"").*?(?<!\\)(\\\\)*?"|'(?!'').*?(?<!\\)(\\\\)*?')"#;
/// The bundled `python.LONG_STRING` (DOTALL): a lazy body with a multi-char `"""` close and
/// no opening guard — its `(?<!\\)` sits after a variable-width `.*?`, so the fixed-offset
/// lowering **declines** it.
const LONG_STRING_RAW: &str =
    r#"([ubf]?r?|r[ubf])(""".*?(?<!\\)(\\\\)*?"""|'''.*?(?<!\\)(\\\\)*?''')"#;
/// The bundled `lark.REGEXP`: its `(?!\/)` is neither the first nor the last element of the
/// match, so the classifier rejects it as `Internal` (an out-of-shape `Unsupported` route —
/// the DfaScanner's compatibility fallback still sends it to `fancy-regex` at runtime).
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

/// `python.LONG_STRING` is **not lowered** — its lookbehind sits after a variable-width
/// `.*?`, so the fixed-offset lowering declines it to [`LoweringRoute::DeclinedToFancy`].
/// Crucially it is **not** `Unsupported`: every assertion is a *supported shape*; the
/// decline is per-instance, a transitional route, not a classifier reject.
#[test]
fn python_long_string_routes_to_declined_to_fancy() {
    let route = route_terminal_dotall("LONG_STRING", LONG_STRING_RAW, true);
    assert!(
        matches!(route, LoweringRoute::DeclinedToFancy { .. }),
        "LONG_STRING must decline to fancy (a per-instance decline, not a reject), got {route:?}"
    );
    // It is in particular neither lowered nor an out-of-shape rejection.
    assert!(!matches!(route, LoweringRoute::Lowered(_)));
    assert!(!matches!(route, LoweringRoute::Unsupported { .. }));
}

/// `lark.REGEXP`'s `(?!\/)` is a genuinely *internal* lookahead, so the classifier rejects
/// it: the route is [`LoweringRoute::Unsupported`] with [`Rejection::Internal`]. This is an
/// honest finding the typed split surfaces — the prose "declined-to-fancy" label describes
/// the **runtime** outcome (the DfaScanner compatibility fallback still routes Unsupported
/// to `fancy-regex`), not the classifier verdict. Either way it is **not** lowered.
#[test]
fn lark_regexp_routes_to_unsupported_internal() {
    match route_terminal("REGEXP", REGEXP_RAW) {
        LoweringRoute::Unsupported {
            rejection,
            assertion,
            ..
        } => {
            assert_eq!(rejection, Rejection::Internal);
            assert!(
                assertion.contains("(?!"),
                "the unsupported assertion must be REGEXP's `(?!\\/)`, got {assertion:?}"
            );
        }
        other => {
            panic!("lark.REGEXP's internal `(?!\\/)` must be Unsupported(Internal), got {other:?}")
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
/// [`Rejection::Unbounded`].
#[test]
fn unbounded_lookahead_routes_to_unsupported_unbounded() {
    match route_terminal("UNB", "(?![ ]*X)Y") {
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
    // DeclinedToFancy, Unsupported both flatten to Err.
    assert!(lower_terminal("LONG_STRING", LONG_STRING_RAW).is_err());
    assert!(lower_terminal("INT", "a(?=b)c").is_err());
}

/// **Transitional compatibility-fallback pin.** An out-of-shape *user* lookahead routes to
/// [`LoweringRoute::Unsupported`] at the route level, but the `DfaScanner` build path's
/// compatibility fallback **still routes it to `fancy-regex`** today, so the lexer builds
/// and lexes correctly. This proves the routing-contract split did **not** silently change
/// runtime behavior in this PR.
///
/// L4 should **delete or invert** this test when unsupported user lookaround becomes a hard
/// build error, or when an explicit compatibility mode is introduced — at that point the
/// grammar below should fail to build (or build only under the compatibility flag), and
/// this assertion is what flags that the policy flip happened.
#[test]
fn unsupported_user_lookaround_currently_compat_falls_back_to_fancy() {
    // `TOK`'s `a(?=b)b` is a genuinely *internal* lookahead (between two elements, so
    // neither leading nor trailing) — the route says Unsupported(Internal). (Note a
    // *trailing* `a(?=b)` would instead lower; the lookahead must sit mid-pattern to be
    // out-of-shape.)
    assert!(matches!(
        route_terminal("TOK", "a(?=b)b"),
        LoweringRoute::Unsupported {
            rejection: Rejection::Internal,
            ..
        }
    ));

    // Yet under the default Dfa backend's compatibility fallback the grammar still builds
    // and lexes "abc" as TOK="ab", B="c" (via the fancy-regex side-probe).
    let grammar = "start: TOK B\nTOK: /a(?=b)b/\nB: \"c\"\n";
    let g = load_grammar(grammar, &["start".to_string()], false, false)
        .expect("grammar with an unsupported user lookahead still builds (compat fallback)");
    let cg = lower(&g);
    let conf = basic_lexer_conf(&cg, 0).with_backend(LexerBackend::Dfa);
    let lexer =
        BasicLexer::new(&conf).expect("Dfa BasicLexer builds despite the unsupported lookahead");

    let tokens = lexer
        .lex("abc")
        .expect("\"abc\" lexes via the fancy-regex compatibility fallback");
    let got: Vec<(String, String)> = tokens
        .into_iter()
        .filter(|t| t.type_ != "$END")
        .map(|t| (t.type_.to_string(), t.value))
        .collect();
    assert_eq!(
        got,
        vec![
            ("TOK".to_string(), "ab".to_string()),
            ("B".to_string(), "c".to_string()),
        ],
        "the unsupported lookahead must still lex via fancy-regex"
    );
}
