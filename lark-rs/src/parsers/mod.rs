pub mod earley;
pub mod lalr;
pub mod token_source;
pub mod tree_builder;

pub use earley::EarleyParser;
pub use lalr::{build_lalr_table, LalrParser, ParseTable};
pub use token_source::{Contextual, LexFailure, PreLexed, TokenSource};
pub use tree_builder::{NodeValue, TreeBuilder};

use crate::error::{LarkError, ParseError};
use crate::grammar::{CompiledGrammar, Grammar};
use crate::lexer::{BasicLexer, ContextualLexer, DynamicMatcher, Lexer, LexerConf};
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
        }
    }
}

pub fn build_frontend(
    grammar: &Grammar,
    options: &LarkOptions,
) -> Result<ParsingFrontend, LarkError> {
    match options.parser {
        ParserAlgorithm::Lalr => {
            // Lower the surface grammar to the interned IR once, then build the
            // parse table and lexer from it.
            let cg = crate::grammar::lower(grammar);
            let table = build_lalr_table(&cg, options.strict)?;
            let parser = LalrParser::new(table);

            let lexer_conf = basic_lexer_conf(&cg, options.g_regex_flags);

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
        ParserAlgorithm::Cyk => Err(LarkError::Grammar(crate::error::GrammarError::Other {
            msg: "CYK parser not yet implemented".to_string(),
        })),
    }
}
