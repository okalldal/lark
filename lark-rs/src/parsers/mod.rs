pub mod cyk;
pub mod earley;
pub mod lalr;
pub mod token_source;
pub mod tree_builder;

pub use cyk::CykParser;
pub use earley::EarleyParser;
pub use lalr::{build_lalr_table, LalrParser, ParseTable};
pub use token_source::{Contextual, LexFailure, PreLexed, TokenSource};
pub use tree_builder::{NodeValue, TreeBuilder};

use crate::error::{LarkError, ParseError};
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

/// A unified frontend that wires together a lexer and a parser.
pub struct ParsingFrontend {
    kind: FrontendKind,
}

enum FrontendKind {
    LalrBasic {
        parser: LalrParser,
        lexer: BasicLexer,
    },
    LalrContextual {
        parser: LalrParser,
        lexer: ContextualLexer,
    },
    /// LALR driven by a postlex hook: the basic lexer produces the whole token
    /// stream, the [`Indenter`] rewrites it (injecting INDENT/DEDENT), then the
    /// parser replays it. The contextual lexer is bypassed because postlex needs
    /// the materialized stream, and `symbols` lets the indenter resolve its
    /// `%declare`d terminal ids.
    LalrPostlex {
        parser: LalrParser,
        lexer: BasicLexer,
        postlex: Indenter,
        symbols: SymbolTable,
    },
    /// LALR + contextual lexer + postlex hook (issue #67). Unlike `LalrPostlex`,
    /// the contextual lexer can't be materialized up front (it narrows terminals
    /// by parser state), so the [`Indenter`] runs as a streaming `TokenSource`
    /// adapter inside the lazy pull loop. The hook's newline terminal is forced
    /// into every state's scanner via `always_accept` (see `build_frontend`).
    LalrContextualPostlex {
        parser: LalrParser,
        lexer: ContextualLexer,
        postlex: Indenter,
        symbols: SymbolTable,
    },
    Earley {
        parser: EarleyParser,
        lexer: BasicLexer,
        /// `ambiguity='resolve'` (pick one tree) vs `'explicit'` (`_ambig` forests).
        resolve: bool,
    },
    EarleyDynamic {
        parser: EarleyParser,
        matcher: DynamicMatcher,
        resolve: bool,
        /// `dynamic_complete`: explore every shorter tokenization, not just the
        /// longest match at each position.
        complete_lex: bool,
    },
    /// CYK over the basic lexer. Like Earley, CYK has no parser-state-driven
    /// lexer, so it always uses the basic lexer; the grammar is converted to
    /// Chomsky Normal Form once when the parser is built.
    Cyk {
        parser: CykParser,
        lexer: BasicLexer,
    },
}

impl ParsingFrontend {
    pub fn parse(&self, text: &str, start: Option<&str>) -> Result<ParseTree, ParseError> {
        match &self.kind {
            FrontendKind::LalrBasic { parser, lexer } => {
                let tokens = lexer.lex(text)?;
                parser.parse(tokens, start)
            }
            FrontendKind::LalrContextual { parser, lexer } => {
                parser.parse_contextual(text, lexer, start)
            }
            FrontendKind::LalrPostlex {
                parser,
                lexer,
                postlex,
                symbols,
            } => {
                let tokens = lexer.lex(text)?;
                let tokens = postlex.process(tokens, symbols)?;
                parser.parse(tokens, start)
            }
            FrontendKind::LalrContextualPostlex {
                parser,
                lexer,
                postlex,
                symbols,
            } => parser.parse_contextual_postlex(text, lexer, postlex, symbols, start),
            FrontendKind::Earley {
                parser,
                lexer,
                resolve,
            } => {
                let tokens = lexer.lex(text)?;
                parser.parse(&tokens, start, *resolve)
            }
            FrontendKind::EarleyDynamic {
                parser,
                matcher,
                resolve,
                complete_lex,
            } => parser.parse_dynamic(text, start, *resolve, *complete_lex, matcher),
            FrontendKind::Cyk { parser, lexer } => {
                let tokens = lexer.lex(text)?;
                parser.parse(&tokens, start)
            }
        }
    }
}

pub fn build_frontend(
    grammar: &Grammar,
    options: &LarkOptions,
) -> Result<ParsingFrontend, LarkError> {
    // postlex is currently wired only through the LALR frontend (issue #67 tracks
    // the contextual-lexer/other-backend support). Fail loudly rather than
    // silently ignoring a configured hook on Earley/CYK.
    if options.postlex.is_some() && options.parser != ParserAlgorithm::Lalr {
        return Err(LarkError::Grammar(crate::error::GrammarError::Other {
            msg: "postlex (Indenter) is only supported with parser='lalr'".to_string(),
        }));
    }
    match options.parser {
        ParserAlgorithm::Lalr => {
            // Lower the surface grammar to the interned IR once, then build the
            // parse table and lexer from it.
            let cg = crate::grammar::lower(grammar);
            let table = build_lalr_table(&cg, options.strict)?;
            let parser = LalrParser::new(table);

            let lexer_conf = basic_lexer_conf(&cg, options.g_regex_flags);

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
            // adapter inside its lazy pull loop (issue #67).
            if let Some(postlex) = &options.postlex {
                postlex.validate(&cg.symbols)?;
                match options.lexer {
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
                        let lexer =
                            ContextualLexer::new(&lexer_conf, &state_terminals, always_accept)?;
                        return Ok(ParsingFrontend {
                            kind: FrontendKind::LalrContextualPostlex {
                                parser,
                                lexer,
                                postlex: postlex.clone(),
                                symbols: cg.symbols.clone(),
                            },
                        });
                    }
                    // Basic lexer (and any other explicit choice): materialize the
                    // whole stream, then rewrite it.
                    _ => {
                        let lexer = BasicLexer::new(&lexer_conf)?;
                        return Ok(ParsingFrontend {
                            kind: FrontendKind::LalrPostlex {
                                parser,
                                lexer,
                                postlex: postlex.clone(),
                                symbols: cg.symbols.clone(),
                            },
                        });
                    }
                }
            }

            let kind = match options.lexer {
                LexerType::Basic => {
                    let lexer = BasicLexer::new(&lexer_conf)?;
                    FrontendKind::LalrBasic { parser, lexer }
                }
                LexerType::Contextual | LexerType::Auto => {
                    let state_terminals = parser.state_terminals();
                    let always_accept = cg.ignore.clone();
                    let lexer = ContextualLexer::new(&lexer_conf, &state_terminals, always_accept)?;
                    FrontendKind::LalrContextual { parser, lexer }
                }
                _ => {
                    let lexer = BasicLexer::new(&lexer_conf)?;
                    FrontendKind::LalrBasic { parser, lexer }
                }
            };
            Ok(ParsingFrontend { kind })
        }
        ParserAlgorithm::Earley => {
            // Earley never uses the contextual lexer (it narrows terminals by LALR
            // parser state, which Earley has none of). Its lexer options are the
            // basic lexer (Sprints 1–4; `Auto`/`Basic`/`Contextual` resolve here)
            // and the dynamic lexer (Sprint 5; `Dynamic` / `DynamicComplete`).
            let cg = crate::grammar::lower(grammar);
            let lexer_conf = basic_lexer_conf(&cg, options.g_regex_flags);
            let resolve = match options.ambiguity {
                crate::Ambiguity::Resolve => true,
                crate::Ambiguity::Explicit => false,
                // Returning the raw SPPF (`ambiguity='forest'`) is not supported;
                // fail loudly rather than silently substituting another mode.
                crate::Ambiguity::Forest => {
                    return Err(LarkError::Grammar(crate::error::GrammarError::Other {
                        msg: "Earley ambiguity='forest' (raw SPPF) is not supported".to_string(),
                    }))
                }
            };
            let kind = match options.lexer {
                LexerType::Dynamic | LexerType::DynamicComplete => {
                    let matcher = DynamicMatcher::new(&lexer_conf)?;
                    let parser = EarleyParser::new(cg);
                    FrontendKind::EarleyDynamic {
                        parser,
                        matcher,
                        resolve,
                        complete_lex: options.lexer == LexerType::DynamicComplete,
                    }
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
                    let parser = EarleyParser::new(cg);
                    FrontendKind::Earley {
                        parser,
                        lexer,
                        resolve,
                    }
                }
            };
            Ok(ParsingFrontend { kind })
        }
        ParserAlgorithm::Cyk => {
            // CYK uses the basic lexer (it has no parser-state-driven lexer, like
            // Earley). The grammar is lowered, the basic lexer built with the same
            // validation the LALR/Earley basic paths apply, and the parser
            // converts the grammar to CNF up front — so an unconvertible grammar
            // (e.g. one with ε-rules) is rejected here as a build error, exactly as
            // Python Lark rejects it while constructing the CYK frontend.
            let cg = crate::grammar::lower(grammar);
            let lexer_conf = basic_lexer_conf(&cg, options.g_regex_flags);
            check_zero_width_terminals(&lexer_conf)?;
            check_regex_collisions(&lexer_conf, options.strict, None)?;
            let lexer = BasicLexer::new(&lexer_conf)?;
            let parser = CykParser::new(cg)?;
            Ok(ParsingFrontend {
                kind: FrontendKind::Cyk { parser, lexer },
            })
        }
    }
}
