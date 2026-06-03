pub mod lalr;
pub mod earley;
pub mod token_source;

pub use lalr::{build_lalr_table, ParseTable, LalrParser};
pub use token_source::{Contextual, LexFailure, PreLexed, TokenSource};

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
            // Lower the surface grammar to the interned IR once, then build the
            // parse table and lexer from it.
            let cg = crate::grammar::lower(grammar);
            let table = build_lalr_table(&cg)?;
            let parser = LalrParser::new(table);

            let terminals: Vec<(crate::grammar::SymbolId, crate::grammar::terminal::TerminalDef)> = cg
                .terminals
                .iter()
                .map(|t| (cg.symbols.id(&t.name).expect("terminal interned"), t.clone()))
                .collect();
            let lexer_conf = LexerConf::new(terminals, cg.ignore.clone());

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
            // Phase 2: Earley is not implemented yet. Fail loudly rather than
            // silently substituting LALR — LALR rejects grammars Earley accepts,
            // so a silent fallback would give wrong results on ambiguous grammars.
            Err(LarkError::Grammar(crate::error::GrammarError::Other {
                msg: "Earley parser not yet implemented".to_string(),
            }))
        }
        ParserAlgorithm::Cyk => {
            Err(LarkError::Grammar(crate::error::GrammarError::Other {
                msg: "CYK parser not yet implemented".to_string(),
            }))
        }
    }
}
