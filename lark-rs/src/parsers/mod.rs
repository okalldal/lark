pub mod cyk;
pub mod earley;
pub mod lalr;
pub mod token_source;
pub mod tree_builder;

pub use cyk::CykParser;
pub use earley::EarleyParser;
pub use lalr::{build_lalr_table, LalrParser, ParseTable};
pub use token_source::{Contextual, ContextualRecovering, LexFailure, PreLexed, TokenSource};
pub use tree_builder::{NodeValue, TreeBuilder};

use crate::error::{GrammarError, LarkError, ParseError, RecoveredTree};
use crate::grammar::intern::SymbolTable;
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

    /// Parse with panic-mode single-token-deletion recovery (issue #43). The
    /// default is the typed refusal — a driver that supports recovery overrides
    /// this. Only the LALR drivers without a postlex hook do: recovery deletes
    /// tokens from the stream, which would desync an indenter's synthetic
    /// INDENT/DEDENT injection, and the Earley/CYK engines have no equivalent of
    /// Python Lark's `on_error` resume.
    fn parse_recovering(
        &self,
        _text: &str,
        _start: Option<&str>,
        _on_error: &mut dyn FnMut(&ParseError) -> bool,
    ) -> Result<RecoveredTree, LarkError> {
        Err(recovery_unsupported())
    }
}

/// The typed refusal for a configuration without recovery support — shared by
/// the trait default and [`lalr_recover`]'s missing-lexer arm so the message
/// cannot drift between them.
fn recovery_unsupported() -> LarkError {
    LarkError::Grammar(GrammarError::Other {
        msg: "error recovery requires parser='lalr' without a postlex hook".to_string(),
    })
}

/// Shared recovery body for the **basic-lexer** LALR driver (issues #43 + #93):
/// lex the whole stream with the basic (global) lexer, then drive the recovering
/// LALR loop. (The contextual driver does not use this — it recovers over its own
/// contextual lexer via [`LalrParser::parse_contextual_recovering`], issue #166.)
/// Lexing uses the recovering entry point ([`BasicLexer::lex_recovering`]): a
/// genuinely un-lexable character is no longer a hard error but is *skipped one
/// char at a time*, recording each skip in `errors` (Python's
/// `UnexpectedCharacters` branch). The character-level skips and the token-level
/// deletions both flow through the same `on_error` handler and accumulate into one
/// `errors` list, so editor tooling sees a complete diagnostic record. `lexer` is
/// `None` only if the recovery lexer's construction failed at build time (not
/// expected in practice); recovery is then unavailable rather than the whole build
/// failing.
///
/// [`BasicLexer::lex_recovering`]: crate::lexer::BasicLexer::lex_recovering
fn lalr_recover(
    parser: &LalrParser,
    lexer: Option<&BasicLexer>,
    text: &str,
    start: Option<&str>,
    on_error: &mut dyn FnMut(&ParseError) -> bool,
) -> Result<RecoveredTree, LarkError> {
    let Some(lexer) = lexer else {
        return Err(recovery_unsupported());
    };
    let mut errors = Vec::new();
    let tokens = lexer.lex_recovering(text, on_error, &mut errors);
    let tree = parser.parse_recovering(tokens, start, on_error, &mut errors)?;
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
        on_error: &mut dyn FnMut(&ParseError) -> bool,
    ) -> Result<RecoveredTree, LarkError> {
        lalr_recover(
            &self.parser,
            self.recovery_lexer.as_ref(),
            text,
            start,
            on_error,
        )
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
        on_error: &mut dyn FnMut(&ParseError) -> bool,
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
}

/// LALR driven by a postlex hook over the basic lexer: the lexer produces the
/// whole token stream, the [`Indenter`] rewrites it (injecting INDENT/DEDENT),
/// then the parser replays it. The contextual lexer is bypassed because postlex
/// needs the materialized stream, and `symbols` lets the indenter resolve its
/// `%declare`d terminal ids. No recovery (the trait default's typed refusal):
/// deletion could desync the indenter's synthetic tokens.
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

    /// Parse with panic-mode error recovery (issue #43). On a token the parser
    /// can't act on, `on_error` is consulted; returning `true` deletes that token
    /// and resumes (single-token-deletion recovery, identical to Python Lark's
    /// `on_error` driver), `false` stops with `tree: None` (no fabricated
    /// derivation — issue #167) and the errors collected so far.
    ///
    /// Only the LALR backend without a postlex hook supports recovery; other
    /// configurations return a [`GrammarError::Other`]. Lexing uses the basic
    /// (global) lexer so out-of-context-but-valid tokens are deletable tokens
    /// rather than lexer errors. A genuinely un-lexable character (issue #93) is
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
        on_error: &mut dyn FnMut(&ParseError) -> bool,
    ) -> Result<RecoveredTree, LarkError> {
        self.driver.parse_recovering(text, start, on_error)
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

pub fn build_frontend(
    grammar: &Grammar,
    options: &LarkOptions,
) -> Result<ParsingFrontend, LarkError> {
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
