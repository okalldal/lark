pub mod cyk;
pub mod earley;
pub mod interactive;
pub mod lalr;
pub mod token_source;
pub mod tree_builder;

pub use cyk::CykParser;
pub use earley::EarleyParser;
pub use interactive::InteractiveParser;
pub use lalr::{build_lalr_table, LalrParser, ParseTable, RecoveryContext};
pub use token_source::{
    BasicRecovering, Contextual, ContextualRecovering, LexFailure, PreLexed, TokenSource,
};
// OutputBuilder, Slot, TreeOutputBuilder (and their backward-compat aliases
// NodeValue, TreeBuilder) are crate-internal — issue #231 defers the public
// trait shape. Internal code imports via `super::tree_builder::*` directly.

use crate::error::{GrammarError, LarkError, ParseError, RecoveredTree, RecoveryAction};
use crate::grammar::intern::{SymbolId, SymbolTable};
use crate::grammar::{CompiledGrammar, Grammar};
use crate::lexer::{
    check_regex_collisions, check_zero_width_terminals, BasicLexer, ContextualLexer,
    DynamicMatcher, Lexer, LexerConf,
};
use crate::postlex::Indenter;
use crate::tree::ParseTree;
use crate::{LarkOptions, LexerType, ParserAlgorithm};

/// Assemble the basic-lexer configuration from an interned grammar: pair every
/// terminal with its id (the lexer dispatches on the interned id) and carry the
/// `%ignore` set plus any global regex flags. Shared by the LALR frontend and the
/// Earley recognizer so both lex through one identical `Scanner` setup.
pub fn basic_lexer_conf(cg: &CompiledGrammar, g_regex_flags: u32) -> LexerConf {
    let terminals = cg
        .terminals
        .iter()
        // `%declare`d terminals have no pattern and are never lexed — a postlex
        // hook injects them. Keep them out of every scanner; they are still
        // interned, so rules and the parse table still see them.
        .filter(|t| !t.declared)
        .map(|t| {
            (
                cg.symbols.id(&t.name).expect("terminal interned"),
                t.clone(),
            )
        })
        .collect();
    LexerConf::new(terminals, cg.ignore.clone()).with_global_flags(g_regex_flags)
}

/// Resolve a default (`None`) or explicit start name against the ordered list of
/// configured start symbols, mirroring Python Lark's `_verify_start`
/// (`lark/parser_frontends.py`). This is the single definition shared by **every**
/// backend — LALR, Earley, and CYK — so the diagnostics are byte-identical
/// (issues #251, #256):
///   * `None` + exactly one configured start → use it;
///   * `None` + more than one configured start → reject (Python's
///     `ConfigurationError` "more than 1 possible start rule");
///   * `None` + no configured start → reject ("no start rule configured");
///   * `Some(name)` that is one of the configured starts → use it;
///   * `Some(name)` that is *not* a configured start → reject with Python's
///     "Unknown start rule …. Must be one of […]" message (rendered Python-`repr`
///     style, single-quoted, to match `_verify_start`'s `%r` formatting).
///
/// Being *more permissive* than the oracle here (silently picking the first start,
/// or accepting an explicit non-start rule name) is the divergence #256 closes for
/// Earley + CYK, where LALR already matched (#251).
pub fn resolve_start(
    starts: &[SymbolId],
    symbols: &SymbolTable,
    start: Option<&str>,
) -> Result<SymbolId, ParseError> {
    match start {
        None => match starts {
            [only] => Ok(*only),
            [] => Err(ParseError::unexpected_eof(
                0,
                0,
                vec!["no start rule configured".to_string()],
            )),
            _ => Err(ParseError::unexpected_eof(
                0,
                0,
                vec!["Lark initialized with more than 1 possible start rule. \
                      Must specify which start rule to parse"
                    .to_string()],
            )),
        },
        Some(name) => match symbols.id(name) {
            Some(id) if starts.contains(&id) => Ok(id),
            _ => {
                let one_of = starts
                    .iter()
                    .map(|&id| format!("'{}'", symbols.name(id)))
                    .collect::<Vec<_>>()
                    .join(", ");
                Err(ParseError::unexpected_eof(
                    0,
                    0,
                    vec![format!(
                        "Unknown start rule {}. Must be one of [{}]",
                        name, one_of
                    )],
                ))
            }
        },
    }
}

#[derive(Debug, Clone)]
pub struct ParserConf {
    pub rules: Vec<crate::grammar::rule::Rule>,
    pub start: Vec<String>,
}

// ─── ParserDriver: one fully-wired parser × lexer configuration ───────────────

/// One fully-wired parse strategy: a parser engine plus the lexer (and any
/// postlex hook) it consumes tokens through. The frontend holds exactly one
/// driver; a new `parser × lexer × postlex` configuration is a new impl of this
/// trait, not a new match arm threaded through every frontend method.
///
/// `Send` is a supertrait deliberately: the old enum frontend was a concrete
/// type whose `Send`ness was inferred, so `Lark` was `Send`; a bare
/// `Box<dyn ParserDriver>` would silently drop that from the public API
/// (build-on-one-thread/parse-on-another, and `Mutex<Lark>` being `Sync`).
/// Pinned at compile time in `lib.rs` so the bound cannot be dropped again.
trait ParserDriver: Send {
    /// Parse the full input from `start` (or the grammar's default start).
    fn parse(&self, text: &str, start: Option<&str>) -> Result<ParseTree, ParseError>;

    /// Parse with panic-mode single-token-deletion recovery (issues #43, #94). The
    /// default is the typed refusal — a driver that supports recovery overrides
    /// this. All four LALR drivers do (basic / contextual × plain / postlex): the
    /// postlex (Indenter) drivers recover by injecting INDENT/DEDENT *upstream* of
    /// the parser's token deletion, so a deleted token never reaches the indenter
    /// (issue #94). Only the Earley/CYK engines refuse — they have no equivalent of
    /// Python Lark's `on_error`/`resume_parse` (recovery is LALR-only upstream).
    fn parse_recovering(
        &self,
        _text: &str,
        _start: Option<&str>,
        _on_error: &mut dyn FnMut(&ParseError, &mut RecoveryContext<'_>) -> RecoveryAction,
    ) -> Result<RecoveredTree, LarkError> {
        Err(recovery_unsupported())
    }

    /// Begin an interactive parse (issue #168). The default is the typed refusal;
    /// the LALR drivers override it (basic lexer: #168, contextual: #222). The
    /// postlex drivers are a follow-up (mirroring how recovery was extended to
    /// them in #94).
    fn parse_interactive(
        &self,
        _text: &str,
        _start: Option<&str>,
    ) -> Result<InteractiveParser<'_>, LarkError> {
        Err(interactive_unsupported())
    }
}

/// The typed refusal for a configuration without recovery support — shared by
/// the trait default and [`lalr_recover`]'s missing-lexer arm so the message
/// cannot drift between them. Recovery is LALR-only (on every lexer/postlex
/// wiring); Earley and CYK have no `on_error` resume to mirror.
fn recovery_unsupported() -> LarkError {
    LarkError::Grammar(GrammarError::Other {
        msg: "error recovery requires parser='lalr'".to_string(),
    })
}

/// The typed refusal for a configuration without interactive-parsing support
/// (issue #168). Supported on LALR with the basic or contextual lexer; the
/// postlex drivers are a follow-up.
fn interactive_unsupported() -> LarkError {
    LarkError::Grammar(GrammarError::Other {
        msg: "interactive parsing requires parser='lalr' (without postlex)".to_string(),
    })
}

/// Shared recovery body for the **basic-lexer** LALR driver (issues #43, #93,
/// #223). Uses a lazy [`BasicRecovering`] source so character-level lex failures
/// surface during parsing (not up front) and the `on_error` handler's
/// [`RecoveryContext`] reflects the real parser state at the error — not a
/// fabricated initial-state context. `lexer` is `None` only if the recovery
/// lexer's construction failed at build time (not expected in practice).
fn lalr_recover(
    parser: &LalrParser,
    lexer: Option<&BasicLexer>,
    text: &str,
    start: Option<&str>,
    on_error: &mut dyn FnMut(&ParseError, &mut RecoveryContext<'_>) -> RecoveryAction,
) -> Result<RecoveredTree, LarkError> {
    let Some(lexer) = lexer else {
        return Err(recovery_unsupported());
    };
    let mut errors = Vec::new();
    let mut source = BasicRecovering::new(text, lexer);
    let tree = parser.run_recovering(&mut source, start, on_error, &mut errors)?;
    Ok(RecoveredTree { tree, errors })
}

/// Shared recovery body for the **basic-lexer + Indenter (postlex)** driver (issue
/// #94, sub-target 1). Mirrors Python Lark's `lexer → PostLexConnector(postlex) →
/// parser` wiring: `on_error`/`resume_parse` operate on the *post-indenter* token
/// stream, so the [`Indenter`] injects INDENT/DEDENT over the clean lex and
/// token-deletion recovery happens *downstream* of that injection — a deleted token
/// never reaches the indenter, so its bracket/indent bookkeeping cannot desync.
///
/// Concretely: lex lazily with a [`BasicRecovering`] source (so an un-lexable
/// character is skipped one at a time, issue #93), run the streaming indenter over
/// each token as it is produced, then drive the recovering LALR loop over the
/// indented tokens. An indenter error (e.g. a `DedentError`: a dedent to an unknown
/// column) is raised by the postlex hook itself, *before* any parser token error —
/// Python re-raises it through the postlex generator without consulting `on_error`.
/// lark-rs surfaces it the same way: as a hard [`ParseError`] → `LarkError`, distinct
/// from the `Ok(tree: None)` premature-`$END` convention.
///
/// [`BasicRecovering`]: super::token_source::BasicRecovering
fn lalr_recover_postlex(
    parser: &LalrParser,
    lexer: &BasicLexer,
    postlex: &Indenter,
    symbols: &SymbolTable,
    text: &str,
    start: Option<&str>,
    on_error: &mut dyn FnMut(&ParseError, &mut RecoveryContext<'_>) -> RecoveryAction,
) -> Result<RecoveredTree, LarkError> {
    let mut errors = Vec::new();
    // Lazily lex the global terminal set and drive the streaming indenter +
    // per-resume-reset machine over it (a `BasicRecovering` source). The indenter
    // sits upstream of the parser's token deletion and resets on each resume exactly
    // as Python's `Indenter.process` does — so a multi-deletion recovery, and a char
    // skip interleaved with the indenter, both stay byte-for-byte faithful (an "indent
    // the whole stream once" model would not). A char skip and a token deletion both
    // accumulate into `errors`; an indenter error (e.g. a bad dedent) surfaces as a
    // hard error.
    let tree = parser.parse_basic_postlex_recovering(
        text,
        lexer,
        postlex,
        symbols,
        start,
        on_error,
        &mut errors,
    )?;
    Ok(RecoveredTree { tree, errors })
}

/// LALR over the basic lexer: materialize the whole token stream, then parse.
struct LalrBasic {
    parser: LalrParser,
    lexer: BasicLexer,
    /// The basic (global) lexer recovery re-lexes with — see [`lalr_recover`].
    recovery_lexer: Option<BasicLexer>,
}

impl ParserDriver for LalrBasic {
    fn parse(&self, text: &str, start: Option<&str>) -> Result<ParseTree, ParseError> {
        let tokens = self.lexer.lex(text)?;
        self.parser.parse(tokens, start)
    }

    fn parse_recovering(
        &self,
        text: &str,
        start: Option<&str>,
        on_error: &mut dyn FnMut(&ParseError, &mut RecoveryContext<'_>) -> RecoveryAction,
    ) -> Result<RecoveredTree, LarkError> {
        lalr_recover(
            &self.parser,
            self.recovery_lexer.as_ref(),
            text,
            start,
            on_error,
        )
    }

    /// Interactive parse over the basic lexer (issue #168). Construction does **not**
    /// lex — the [`InteractiveParser`] lexes lazily as the caller drives it, so it can
    /// be created over broken editor text and an un-lexable character surfaces only
    /// when `exhaust_lexer`/`resume` reaches it (matching Python).
    fn parse_interactive(
        &self,
        text: &str,
        start: Option<&str>,
    ) -> Result<InteractiveParser<'_>, LarkError> {
        let stack = self.parser.initial_stack(start)?;
        Ok(InteractiveParser::new_basic(
            &self.parser,
            &self.lexer,
            stack,
            text.to_string(),
        ))
    }
}

/// LALR over the contextual lexer (the default): the parser state narrows which
/// terminals the lexer tries at each position — Lark's key LALR innovation.
struct LalrContextual {
    parser: LalrParser,
    lexer: ContextualLexer,
}

impl ParserDriver for LalrContextual {
    fn parse(&self, text: &str, start: Option<&str>) -> Result<ParseTree, ParseError> {
        self.parser.parse_contextual(text, &self.lexer, start)
    }

    /// Recover over the *contextual* stream (issue #166), not a stored basic lexer:
    /// the contextual lexer narrows terminals by parser state and falls back to its
    /// root (full-terminal) scanner only where the per-state scanner refuses —
    /// Python Lark's `ContextualLexer.lex` except-branch. This makes recovery
    /// faithful for grammars whose contextual lexer is load-bearing (overlapping
    /// terminals disambiguated only by parser state); a stored basic lexer would
    /// mis-tokenize them and diverge from a contextual parse.
    fn parse_recovering(
        &self,
        text: &str,
        start: Option<&str>,
        on_error: &mut dyn FnMut(&ParseError, &mut RecoveryContext<'_>) -> RecoveryAction,
    ) -> Result<RecoveredTree, LarkError> {
        let mut errors = Vec::new();
        let tree = self.parser.parse_contextual_recovering(
            text,
            &self.lexer,
            start,
            on_error,
            &mut errors,
        )?;
        Ok(RecoveredTree { tree, errors })
    }

    /// Interactive parse over the contextual lexer (issue #222). The lazy cursor
    /// lexes via `ContextualLexer::next_token` at the live parser state, with
    /// root-lexer fallback — exactly the machinery the contextual recovery source
    /// uses (#166). A grammar whose contextual lexer is load-bearing (AWORD vs
    /// BWORD) gets correctly typed tokens under `exhaust_lexer`/`resume`.
    fn parse_interactive(
        &self,
        text: &str,
        start: Option<&str>,
    ) -> Result<InteractiveParser<'_>, LarkError> {
        let stack = self.parser.initial_stack(start)?;
        Ok(InteractiveParser::new_contextual(
            &self.parser,
            &self.lexer,
            stack,
            text.to_string(),
        ))
    }
}

/// LALR driven by a postlex hook over the basic lexer: the lexer produces the
/// whole token stream, the [`Indenter`] rewrites it (injecting INDENT/DEDENT),
/// then the parser replays it. The contextual lexer is bypassed because postlex
/// needs the materialized stream, and `symbols` lets the indenter resolve its
/// `%declare`d terminal ids. Recovery (issue #94) lexes with the basic recovery
/// lexer, injects INDENT/DEDENT over the survivors, then deletes offending tokens
/// downstream of the indenter — see [`lalr_recover_postlex`].
struct LalrPostlex {
    parser: LalrParser,
    lexer: BasicLexer,
    postlex: Indenter,
    symbols: SymbolTable,
}

impl ParserDriver for LalrPostlex {
    fn parse(&self, text: &str, start: Option<&str>) -> Result<ParseTree, ParseError> {
        let tokens = self.lexer.lex(text)?;
        let tokens = self.postlex.process(tokens, &self.symbols)?;
        self.parser.parse(tokens, start)
    }

    fn parse_recovering(
        &self,
        text: &str,
        start: Option<&str>,
        on_error: &mut dyn FnMut(&ParseError, &mut RecoveryContext<'_>) -> RecoveryAction,
    ) -> Result<RecoveredTree, LarkError> {
        lalr_recover_postlex(
            &self.parser,
            &self.lexer,
            &self.postlex,
            &self.symbols,
            text,
            start,
            on_error,
        )
    }
}

/// LALR + contextual lexer + postlex hook (issue #67). Unlike [`LalrPostlex`],
/// the contextual lexer can't be materialized up front (it narrows terminals
/// by parser state), so the [`Indenter`] runs as a streaming `TokenSource`
/// adapter inside the lazy pull loop. The hook's newline terminal is forced
/// into every state's scanner via `always_accept` (see [`build_lalr`]).
struct LalrContextualPostlex {
    parser: LalrParser,
    lexer: ContextualLexer,
    postlex: Indenter,
    symbols: SymbolTable,
}

impl ParserDriver for LalrContextualPostlex {
    fn parse(&self, text: &str, start: Option<&str>) -> Result<ParseTree, ParseError> {
        self.parser
            .parse_contextual_postlex(text, &self.lexer, &self.postlex, &self.symbols, start)
    }

    /// Recover over the streaming indenter on the *contextual* stream (issue #94):
    /// the indenter injects INDENT/DEDENT into the recovering contextual lexer's
    /// output (root-lexer fallback included, issue #166), and the parser deletes
    /// offending tokens downstream of that injection. This keeps recovery faithful
    /// for grammars whose contextual lexer is load-bearing (overlapping terminals
    /// disambiguated only by parser state) — a stored basic lexer would mis-tokenize
    /// them and diverge — exactly as the non-postlex contextual driver does.
    fn parse_recovering(
        &self,
        text: &str,
        start: Option<&str>,
        on_error: &mut dyn FnMut(&ParseError, &mut RecoveryContext<'_>) -> RecoveryAction,
    ) -> Result<RecoveredTree, LarkError> {
        let mut errors = Vec::new();
        let tree = self.parser.parse_contextual_postlex_recovering(
            text,
            &self.lexer,
            &self.postlex,
            &self.symbols,
            start,
            on_error,
            &mut errors,
        )?;
        Ok(RecoveredTree { tree, errors })
    }
}

/// Earley over the basic lexer.
struct EarleyBasic {
    parser: EarleyParser,
    lexer: BasicLexer,
    /// `ambiguity='resolve'` (pick one tree) vs `'explicit'` (`_ambig` forests).
    resolve: bool,
}

impl ParserDriver for EarleyBasic {
    fn parse(&self, text: &str, start: Option<&str>) -> Result<ParseTree, ParseError> {
        let tokens = self.lexer.lex(text)?;
        self.parser.parse(&tokens, start, self.resolve)
    }
}

/// Earley driven by a postlex hook over the basic lexer (issue #78): the same
/// materialized-stream wiring as [`LalrPostlex`] — lex everything, let the
/// [`Indenter`] rewrite the stream (injecting INDENT/DEDENT), then parse.
/// Earley has no contextual lexer (nothing narrows terminals by parser state),
/// and the dynamic lexer folds scanning into the parse loop so there is no
/// token stream to rewrite — Python Lark refuses that pairing too.
struct EarleyPostlex {
    parser: EarleyParser,
    lexer: BasicLexer,
    postlex: Indenter,
    symbols: SymbolTable,
    resolve: bool,
}

impl ParserDriver for EarleyPostlex {
    fn parse(&self, text: &str, start: Option<&str>) -> Result<ParseTree, ParseError> {
        let tokens = self.lexer.lex(text)?;
        let tokens = self.postlex.process(tokens, &self.symbols)?;
        self.parser.parse(&tokens, start, self.resolve)
    }
}

/// Earley with the dynamic lexer (Sprint 5): scanning folded into the parse
/// loop, trying exactly the terminals the parser predicts at each position.
struct EarleyDynamic {
    parser: EarleyParser,
    matcher: DynamicMatcher,
    resolve: bool,
    /// `dynamic_complete`: explore every shorter tokenization, not just the
    /// longest match at each position.
    complete_lex: bool,
}

impl ParserDriver for EarleyDynamic {
    fn parse(&self, text: &str, start: Option<&str>) -> Result<ParseTree, ParseError> {
        self.parser
            .parse_dynamic(text, start, self.resolve, self.complete_lex, &self.matcher)
    }
}

/// CYK over the basic lexer. Like Earley, CYK has no parser-state-driven
/// lexer, so it always uses the basic lexer; the grammar is converted to
/// Chomsky Normal Form once when the parser is built. `postlex` rewrites the
/// materialized stream before the DP, exactly like the other basic-lexer
/// postlex drivers — Python Lark wires its `PostLexConnector` in front of
/// every parser the same way.
struct Cyk {
    parser: CykParser,
    lexer: BasicLexer,
    postlex: Option<(Indenter, SymbolTable)>,
}

impl ParserDriver for Cyk {
    fn parse(&self, text: &str, start: Option<&str>) -> Result<ParseTree, ParseError> {
        let mut tokens = self.lexer.lex(text)?;
        if let Some((postlex, symbols)) = &self.postlex {
            tokens = postlex.process(tokens, symbols)?;
        }
        self.parser.parse(&tokens, start)
    }
}

// ─── ParsingFrontend ──────────────────────────────────────────────────────────

/// A unified frontend that wires together a lexer and a parser — a thin shell
/// over the one [`ParserDriver`] the options selected.
pub struct ParsingFrontend {
    driver: Box<dyn ParserDriver>,
}

impl ParsingFrontend {
    pub fn parse(&self, text: &str, start: Option<&str>) -> Result<ParseTree, ParseError> {
        self.driver.parse(text, start)
    }

    /// Parse with panic-mode error recovery (issues #43, #94, #223). On a token the
    /// parser can't act on, `on_error` is consulted with a [`RecoveryContext`]; the
    /// handler returns a [`RecoveryAction`] — `Delete` (delete the token and resume),
    /// `Resume` (retry after feeding corrective tokens), or `Stop` (no derivation).
    ///
    /// Every LALR configuration supports recovery — basic or contextual lexer, with
    /// or without a postlex (Indenter) hook (issue #94: the indenter injects
    /// INDENT/DEDENT *upstream* of the parser's token deletion, mirroring Python's
    /// `lexer → PostLexConnector(postlex) → parser` wiring). Only Earley/CYK refuse
    /// with a [`GrammarError::Other`]. A genuinely un-lexable character (issue #93) is
    /// likewise recovered from: it is skipped one character at a time, each skip
    /// recorded in [`RecoveredTree::errors`] just like a deleted token (Python
    /// Lark's `UnexpectedCharacters` branch of `on_error`).
    ///
    /// [`RecoveredTree::errors`]: crate::error::RecoveredTree::errors
    ///
    /// [`GrammarError::Other`]: crate::error::GrammarError::Other
    pub fn parse_recovering(
        &self,
        text: &str,
        start: Option<&str>,
        on_error: &mut dyn FnMut(&ParseError, &mut RecoveryContext<'_>) -> RecoveryAction,
    ) -> Result<RecoveredTree, LarkError> {
        self.driver.parse_recovering(text, start, on_error)
    }

    /// Begin an interactive parse (issues #168, #222). Supported on LALR with the
    /// basic or contextual lexer; other configurations return a typed error.
    pub fn parse_interactive(
        &self,
        text: &str,
        start: Option<&str>,
    ) -> Result<InteractiveParser<'_>, LarkError> {
        self.driver.parse_interactive(text, start)
    }
}

// ─── Frontend construction ────────────────────────────────────────────────────

/// Lower the surface grammar to the interned IR and assemble the shared
/// basic-lexer configuration — the common preamble of every backend builder.
fn lower_with_lexer_conf(grammar: &Grammar, options: &LarkOptions) -> (CompiledGrammar, LexerConf) {
    let cg = crate::grammar::lower(grammar);
    let lexer_conf =
        basic_lexer_conf(&cg, options.g_regex_flags).with_backend(options.lexer_backend);
    (cg, lexer_conf)
}

/// Front-door config-legality check, mirroring Python Lark's `ConfigurationError`
/// gates so an illegal `(parser, lexer)` pairing or an `ambiguity=` on a parser
/// that doesn't disambiguate is **rejected at build** rather than silently
/// accepted (lark-rs used to substitute a working lexer; bug-bounty N5/N6, #273).
///
/// Two rules, both lifted from the oracle:
///
///  * **Parser → allowed-lexer matrix** (Python `parser_frontends._validate_frontend_args`):
///    `lalr → {basic, contextual}`, `earley → {basic, dynamic, dynamic_complete}`,
///    `cyk → {basic}`. `LexerType::Auto` is always legal — Python resolves `'auto'`
///    to a per-parser concrete lexer (`lalr→contextual`, `earley→dynamic|basic`,
///    `cyk→basic`) *before* this check, so it never reaches the matrix; lark-rs
///    resolves it inside each `build_*` the same way, so `Auto` is admitted here.
///
///  * **`ambiguity=` only for parsers that disambiguate** (Python `lark.py`:
///    `"%r doesn't support disambiguation"`): Python rejects any *explicitly set*
///    non-`auto` ambiguity unless `parser ∈ {earley, cyk}`. lark-rs has no `Auto`
///    ambiguity variant — `Ambiguity::Resolve` *is* the auto-default — so the only
///    user-distinguishable, oracle-falsifiable settings are `Explicit`/`Forest`.
///    We reject those on `lalr` (the sole parser Python excludes); `Resolve` stays
///    accepted everywhere (it is the default path, and Python accepts `earley`/`cyk`
///    explicit/forest too). CYK's `cyk_ignores_ambiguity_option` test pins that
///    `cyk + Explicit` still builds, matching the oracle.
fn validate_config(options: &LarkOptions) -> Result<(), LarkError> {
    let cfg_err = |msg: String| LarkError::Grammar(GrammarError::Other { msg });

    // Parser → allowed-lexer matrix. `Auto` is resolved per-parser downstream and
    // is legal for every parser, so it is never rejected here.
    if !matches!(options.lexer, LexerType::Auto) {
        let (allowed, lexer_str): (&[LexerType], &str) = (
            match options.parser {
                ParserAlgorithm::Lalr => &[LexerType::Basic, LexerType::Contextual],
                ParserAlgorithm::Earley => &[
                    LexerType::Basic,
                    LexerType::Dynamic,
                    LexerType::DynamicComplete,
                ],
                ParserAlgorithm::Cyk => &[LexerType::Basic],
            },
            lexer_name(&options.lexer),
        );
        if !allowed.contains(&options.lexer) {
            let parser_name = parser_name(&options.parser);
            let expected = allowed
                .iter()
                .map(|l| format!("'{}'", lexer_name(l)))
                .collect::<Vec<_>>()
                .join(", ");
            return Err(cfg_err(format!(
                "Parser '{parser_name}' does not support lexer '{lexer_str}', \
                 expected one of ({expected})"
            )));
        }
    }

    // `ambiguity=` only valid for parsers that disambiguate (earley, cyk). Only an
    // explicitly-set `Explicit`/`Forest` is user-distinguishable from the default;
    // reject those on lalr exactly as Python does.
    if matches!(
        options.ambiguity,
        crate::Ambiguity::Explicit | crate::Ambiguity::Forest
    ) && matches!(options.parser, ParserAlgorithm::Lalr)
    {
        return Err(cfg_err(
            "'lalr' doesn't support disambiguation. \
             Use one of these parsers instead: ('earley', 'cyk')"
                .to_string(),
        ));
    }

    Ok(())
}

fn parser_name(p: &ParserAlgorithm) -> &'static str {
    match p {
        ParserAlgorithm::Lalr => "lalr",
        ParserAlgorithm::Earley => "earley",
        ParserAlgorithm::Cyk => "cyk",
    }
}

fn lexer_name(l: &LexerType) -> &'static str {
    match l {
        LexerType::Auto => "auto",
        LexerType::Basic => "basic",
        LexerType::Contextual => "contextual",
        LexerType::Dynamic => "dynamic",
        LexerType::DynamicComplete => "dynamic_complete",
    }
}

pub fn build_frontend(
    grammar: &Grammar,
    options: &LarkOptions,
) -> Result<ParsingFrontend, LarkError> {
    // Front-door config-legality gate, mirroring Python Lark's
    // `ConfigurationError` checks (`lark.py` + `parser_frontends._validate_frontend_args`).
    // Run before any build work so an illegal pairing is rejected verbatim rather
    // than silently substituting a working lexer (bug-bounty N5/N6, issue #273).
    validate_config(options)?;

    // postlex rides every parser (LALR: basic + contextual; Earley/CYK: basic —
    // issue #78), but never the dynamic lexer: scanning is folded into the Earley
    // loop there, so no token stream exists for the hook to rewrite. Python Lark
    // refuses the same pairing. Fail loudly rather than silently ignoring the hook.
    if options.postlex.is_some()
        && matches!(
            options.lexer,
            LexerType::Dynamic | LexerType::DynamicComplete
        )
    {
        return Err(LarkError::Grammar(GrammarError::Other {
            msg: "Can't use postlex with a dynamic lexer. Use lexer='basic' instead".to_string(),
        }));
    }
    let driver = match options.parser {
        ParserAlgorithm::Lalr => build_lalr(grammar, options)?,
        ParserAlgorithm::Earley => build_earley(grammar, options)?,
        ParserAlgorithm::Cyk => build_cyk(grammar, options)?,
    };
    Ok(ParsingFrontend { driver })
}

/// Build the LALR driver: the parse table, then one of the four lexer/postlex
/// wirings (basic / contextual × plain / postlex).
fn build_lalr(
    grammar: &Grammar,
    options: &LarkOptions,
) -> Result<Box<dyn ParserDriver>, LarkError> {
    let (cg, lexer_conf) = lower_with_lexer_conf(grammar, options);
    let table = build_lalr_table(&cg, options.strict)?;
    let parser = LalrParser::new(table);

    // Lexer-build validation, mirroring Python Lark's `BasicLexer`
    // sanitization order: reject zero-width terminals (always), then — under
    // `strict=True` — reject same-priority regex terminals whose languages
    // overlap (issue #35). The contextual lexer scopes the collision check
    // per parser state (Python builds one BasicLexer per state); the basic
    // lexer compiles every terminal together, so it is one global set.
    check_zero_width_terminals(&lexer_conf)?;
    let use_contextual = matches!(options.lexer, LexerType::Contextual | LexerType::Auto);
    let state_terminals = use_contextual.then(|| parser.state_terminals());
    check_regex_collisions(&lexer_conf, options.strict, state_terminals.as_ref())?;

    // Validate the postlex hook's terminal names now, before parsing, so a
    // typo'd nl_type or an undeclared INDENT/DEDENT fails at build time. The
    // basic lexer materializes the whole stream and rewrites it; the
    // contextual lexer (the default) instead runs the hook as a streaming
    // adapter inside its lazy pull loop (issue #67). Neither postlex driver
    // supports recovery (the indenter injects synthetic tokens deletion could
    // desync) — the trait default refuses.
    if let Some(postlex) = &options.postlex {
        postlex.validate(&cg.symbols)?;
        return match options.lexer {
            LexerType::Contextual | LexerType::Auto => {
                let state_terminals = parser.state_terminals();
                // Force the indenter's newline terminal into every state's
                // scanner (Python Lark's `PostLex.always_accept`) so the
                // lazy lexer still emits the newlines the indenter measures
                // indentation from. `validate` already proved it resolves.
                let mut always_accept = cg.ignore.clone();
                if let Some(nl_id) = cg.symbols.id(&postlex.nl_type) {
                    if !always_accept.contains(&nl_id) {
                        always_accept.push(nl_id);
                    }
                }
                let lexer = ContextualLexer::new(&lexer_conf, &state_terminals, always_accept)?;
                Ok(Box::new(LalrContextualPostlex {
                    parser,
                    lexer,
                    postlex: postlex.clone(),
                    symbols: cg.symbols.clone(),
                }))
            }
            // Basic lexer (and any other explicit choice): materialize the
            // whole stream, then rewrite it.
            _ => {
                let lexer = BasicLexer::new(&lexer_conf)?;
                Ok(Box::new(LalrPostlex {
                    parser,
                    lexer,
                    postlex: postlex.clone(),
                    symbols: cg.symbols.clone(),
                }))
            }
        };
    }

    match options.lexer {
        LexerType::Contextual | LexerType::Auto => {
            // The contextual driver recovers over its own contextual lexer (issue
            // #166, via its root-lexer fallback), so it needs no stored basic lexer.
            let state_terminals = parser.state_terminals();
            let always_accept = cg.ignore.clone();
            let lexer = ContextualLexer::new(&lexer_conf, &state_terminals, always_accept)?;
            Ok(Box::new(LalrContextual { parser, lexer }))
        }
        // `Basic`, and any other explicit choice, takes the basic lexer.
        _ => {
            let lexer = BasicLexer::new(&lexer_conf)?;
            // Keep a basic lexer for error recovery (issue #43); for the basic-lexer
            // driver the recovery lexer is the global terminal set too. Building it
            // can't fail here — the construction just above already succeeded.
            let recovery_lexer = BasicLexer::new(&lexer_conf).ok();
            Ok(Box::new(LalrBasic {
                parser,
                lexer,
                recovery_lexer,
            }))
        }
    }
}

/// Build the Earley driver. Earley never uses the contextual lexer (it narrows
/// terminals by LALR parser state, which Earley has none of). Its lexer options
/// are the basic lexer (Sprints 1–4; `Auto`/`Basic`/`Contextual` resolve here)
/// and the dynamic lexer (Sprint 5; `Dynamic` / `DynamicComplete`).
fn build_earley(
    grammar: &Grammar,
    options: &LarkOptions,
) -> Result<Box<dyn ParserDriver>, LarkError> {
    let (cg, lexer_conf) = lower_with_lexer_conf(grammar, options);
    let resolve = match options.ambiguity {
        crate::Ambiguity::Resolve => true,
        crate::Ambiguity::Explicit => false,
        // Returning the raw SPPF (`ambiguity='forest'`) is not supported;
        // fail loudly rather than silently substituting another mode.
        crate::Ambiguity::Forest => {
            return Err(LarkError::Grammar(GrammarError::Other {
                msg: "Earley ambiguity='forest' (raw SPPF) is not supported".to_string(),
            }))
        }
    };
    match options.lexer {
        LexerType::Dynamic | LexerType::DynamicComplete => {
            let matcher = DynamicMatcher::new(&lexer_conf)?;
            let parser = EarleyParser::new(cg);
            Ok(Box::new(EarleyDynamic {
                parser,
                matcher,
                resolve,
                complete_lex: options.lexer == LexerType::DynamicComplete,
            }))
        }
        _ => {
            // The basic lexer is a `BasicLexer`, so it applies the same
            // build-time validation as the LALR basic path: zero-width
            // rejection (always) and, under `strict`, the global
            // regex-collision check. The dynamic lexer (above) has its own
            // scanning model and — like Python — skips both.
            check_zero_width_terminals(&lexer_conf)?;
            check_regex_collisions(&lexer_conf, options.strict, None)?;
            let lexer = BasicLexer::new(&lexer_conf)?;
            // postlex (issue #78): validate the hook's terminal names at build
            // time (same contract as the LALR builders), then rewrite the
            // materialized stream before the chart is built. Python Lark's
            // `lexer='auto'` resolves to 'basic' for Earley + postlex, which is
            // exactly the path every non-dynamic LexerType takes here (the
            // dynamic pairing was refused in `build_frontend`). The symbol
            // table is cloned out before the parser consumes the grammar.
            let postlex = match &options.postlex {
                Some(p) => {
                    p.validate(&cg.symbols)?;
                    Some((p.clone(), cg.symbols.clone()))
                }
                None => None,
            };
            let parser = EarleyParser::new(cg);
            if let Some((postlex, symbols)) = postlex {
                return Ok(Box::new(EarleyPostlex {
                    parser,
                    lexer,
                    postlex,
                    symbols,
                    resolve,
                }));
            }
            Ok(Box::new(EarleyBasic {
                parser,
                lexer,
                resolve,
            }))
        }
    }
}

/// Build the CYK driver. CYK uses the basic lexer (it has no parser-state-driven
/// lexer, like Earley). The grammar is lowered, the basic lexer built with the
/// same validation the LALR/Earley basic paths apply, and the parser converts
/// the grammar to CNF up front — so an unconvertible grammar (e.g. one with
/// ε-rules) is rejected here as a build error, exactly as Python Lark rejects it
/// while constructing the CYK frontend.
fn build_cyk(grammar: &Grammar, options: &LarkOptions) -> Result<Box<dyn ParserDriver>, LarkError> {
    let (cg, lexer_conf) = lower_with_lexer_conf(grammar, options);
    check_zero_width_terminals(&lexer_conf)?;
    check_regex_collisions(&lexer_conf, options.strict, None)?;
    let lexer = BasicLexer::new(&lexer_conf)?;
    // postlex (issue #78): same build-time validation + materialized-stream
    // rewrite as the Earley basic path; Python Lark supports CYK + postlex too.
    let postlex = match &options.postlex {
        Some(p) => {
            p.validate(&cg.symbols)?;
            Some((p.clone(), cg.symbols.clone()))
        }
        None => None,
    };
    let parser = CykParser::new(cg)?;
    Ok(Box::new(Cyk {
        parser,
        lexer,
        postlex,
    }))
}

#[cfg(test)]
mod config_validation_tests {
    //! Front-door config-legality gate (`validate_config`, issue #273 / bounty
    //! N5+N6). The accept/reject verdict for every `(parser, lexer)` pairing and
    //! every `ambiguity=` setting is pinned against the Python Lark oracle
    //! (`lark.py` + `parser_frontends._validate_frontend_args`), captured here as a
    //! committed matrix because the compliance banks under-sample config validation.
    //! The Python verdicts were taken directly from Lark 1.3.1 on 2026-06-22.
    use crate::{Ambiguity, Lark, LarkOptions, LexerType, ParserAlgorithm};

    fn opts(parser: ParserAlgorithm, lexer: LexerType) -> LarkOptions {
        LarkOptions {
            parser,
            lexer,
            start: vec!["start".to_string()],
            ..Default::default()
        }
    }

    fn builds(parser: ParserAlgorithm, lexer: LexerType) -> bool {
        Lark::new("start: \"a\"\n", opts(parser, lexer)).is_ok()
    }

    #[test]
    fn parser_lexer_matrix_matches_python() {
        use LexerType::*;
        use ParserAlgorithm::*;
        // (parser, lexer, expected-accept) — exactly Python's
        // `_validate_frontend_args` matrix, with `Auto` always legal (resolved
        // per-parser downstream). N5 illegal pairings are the `false` rows.
        let cases = [
            // lalr → {basic, contextual}; dynamic/dynamic_complete illegal
            (Lalr, Auto, true),
            (Lalr, Basic, true),
            (Lalr, Contextual, true),
            (Lalr, Dynamic, false),
            (Lalr, DynamicComplete, false),
            // earley → {basic, dynamic, dynamic_complete}; contextual illegal
            (Earley, Auto, true),
            (Earley, Basic, true),
            (Earley, Contextual, false),
            (Earley, Dynamic, true),
            (Earley, DynamicComplete, true),
            // cyk → {basic}; everything else illegal
            (Cyk, Auto, true),
            (Cyk, Basic, true),
            (Cyk, Contextual, false),
            (Cyk, Dynamic, false),
            (Cyk, DynamicComplete, false),
        ];
        for (parser, lexer, expect) in cases {
            assert_eq!(
                builds(parser.clone(), lexer.clone()),
                expect,
                "parser={parser:?} lexer={lexer:?}: lark-rs verdict diverges from Python oracle"
            );
        }
    }

    #[test]
    fn illegal_pairing_message_matches_python() {
        let err = match Lark::new(
            "start: \"a\"\n",
            opts(ParserAlgorithm::Lalr, LexerType::Dynamic),
        ) {
            Ok(_) => panic!("lalr+dynamic is illegal but built"),
            Err(e) => e,
        };
        assert_eq!(
            err.to_string(),
            "Grammar error: Parser 'lalr' does not support lexer 'dynamic', \
             expected one of ('basic', 'contextual')"
        );
    }

    #[test]
    fn ambiguity_on_lalr_rejected_others_accepted() {
        // `Explicit`/`Forest` are only valid on parsers that disambiguate. Python
        // rejects them on lalr ("'lalr' doesn't support disambiguation") but
        // accepts them on earley and cyk; `Resolve` (the auto-default) is accepted
        // everywhere.
        for amb in [Ambiguity::Explicit, Ambiguity::Forest] {
            let mut o = opts(ParserAlgorithm::Lalr, LexerType::Contextual);
            o.ambiguity = amb.clone();
            assert!(
                Lark::new("start: \"a\"\n", o).is_err(),
                "lalr + ambiguity={amb:?}: Python rejects disambiguation on lalr"
            );

            // earley accepts explicit; forest is a separate unsupported-feature
            // refusal inside build_earley, not a config-legality rejection — so we
            // only assert the config gate lets explicit through here.
            if matches!(amb, Ambiguity::Explicit) {
                let mut oe = opts(ParserAlgorithm::Earley, LexerType::Basic);
                oe.ambiguity = amb.clone();
                assert!(
                    Lark::new("start: \"a\"\n", oe).is_ok(),
                    "earley + ambiguity=explicit must build (Python accepts it)"
                );
                // cyk accepts explicit too (it is in Python's disambiguation set).
                let mut oc = opts(ParserAlgorithm::Cyk, LexerType::Basic);
                oc.ambiguity = amb.clone();
                assert!(
                    Lark::new("start: \"a\"\n", oc).is_ok(),
                    "cyk + ambiguity=explicit must build (Python accepts it)"
                );
            }
        }
        // The default (Resolve) path must remain accepted on lalr.
        assert!(
            Lark::new(
                "start: \"a\"\n",
                opts(ParserAlgorithm::Lalr, LexerType::Contextual)
            )
            .is_ok(),
            "lalr + default ambiguity (Resolve) must build"
        );
    }
}
