pub mod lalr;
pub mod earley;

pub use lalr::{build_lalr_table, ParseTable, LalrParser};

use crate::grammar::Grammar;
use crate::lexer::{LexerConf, BasicLexer, ContextualLexer, Lexer};
use crate::tree::Tree;
use crate::error::{ParseError, LarkError};
use crate::{LarkOptions, ParserAlgorithm, LexerType};

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
    LalrBasic { parser: LalrParser, lexer: BasicLexer },
    LalrContextual { parser: LalrParser, lexer: ContextualLexer },
}

impl ParsingFrontend {
    pub fn parse(
        &self,
        text: &str,
        start: Option<&str>,
    ) -> Result<Tree, ParseError> {
        match &self.kind {
            FrontendKind::LalrBasic { parser, lexer } => {
                let tokens = lexer.lex(text)?;
                parser.parse(tokens, start)
            }
            FrontendKind::LalrContextual { parser, lexer } => {
                parser.parse_contextual(text, lexer, start)
            }
        }
    }
}

pub fn build_frontend(
    grammar: &Grammar,
    options: &LarkOptions,
) -> Result<ParsingFrontend, LarkError> {
    match options.parser {
        ParserAlgorithm::Lalr => {
            let table = build_lalr_table(grammar)?;
            let parser = LalrParser::new(table);

            let lexer_conf = LexerConf::new(
                grammar.terminals.clone(),
                grammar.ignore.clone(),
            );

            let kind = match options.lexer {
                LexerType::Basic => {
                    let lexer = BasicLexer::new(&lexer_conf)?;
                    FrontendKind::LalrBasic { parser, lexer }
                }
                LexerType::Contextual | LexerType::Auto => {
                    let state_terminals = parser.state_terminals();
                    let always_accept = grammar.ignore.clone();
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
            // Phase 2: Earley not yet implemented; fall back to LALR.
            build_frontend(grammar, &LarkOptions {
                parser: ParserAlgorithm::Lalr,
                ..options.clone()
            })
        }
        ParserAlgorithm::Cyk => {
            Err(LarkError::Grammar(crate::error::GrammarError::Other {
                msg: "CYK parser not yet implemented".to_string(),
            }))
        }
    }
}
