pub mod error;
pub mod grammar;
pub mod lexer;
pub mod tree;
pub mod parsers;

pub use error::{LarkError, ParseError, GrammarError};
pub use grammar::{
    load_grammar, lower, CompiledGrammar, CompiledRule, Grammar, SymbolId, SymbolKind, SymbolTable,
    rule::{Rule, RuleOptions},
    symbol::{Symbol, Terminal, NonTerminal},
    terminal::TerminalDef,
};
pub use lexer::{Lexer, LexerConf, BasicLexer, ContextualLexer};
pub use tree::{Tree, Token, Child, ParseTree};
pub use parsers::{ParserConf, ParseTable, lalr, TokenSource, LexFailure};

/// Main entry point — mirrors Python's `Lark(grammar, parser=..., lexer=...)`
pub struct Lark {
    pub grammar: Grammar,
    frontend: parsers::ParsingFrontend,
}

impl Lark {
    pub fn new(grammar_text: &str, options: LarkOptions) -> Result<Self, LarkError> {
        let grammar = load_grammar(
            grammar_text,
            &options.start,
            options.maybe_placeholders,
            options.keep_all_tokens,
        )?;
        let frontend = parsers::build_frontend(&grammar, &options)?;
        Ok(Lark { grammar, frontend })
    }

    /// Parse `text` from the default start symbol.
    ///
    /// Returns a [`ParseTree`] — normally a [`Tree`], but a `?start` rule that
    /// collapses via expand1 to a single token yields that bare [`Token`], exactly
    /// as Python Lark does.
    pub fn parse(&self, text: &str) -> Result<ParseTree, ParseError> {
        self.frontend.parse(text, None)
    }

    pub fn parse_with_start(&self, text: &str, start: &str) -> Result<ParseTree, ParseError> {
        self.frontend.parse(text, Some(start))
    }
}

#[derive(Debug, Clone)]
pub struct LarkOptions {
    pub start: Vec<String>,
    pub parser: ParserAlgorithm,
    pub lexer: LexerType,
    pub ambiguity: Ambiguity,
    pub propagate_positions: bool,
    pub keep_all_tokens: bool,
    pub maybe_placeholders: bool,
}

impl Default for LarkOptions {
    fn default() -> Self {
        LarkOptions {
            start: vec!["start".to_string()],
            // LALR is the only implemented backend (Earley is Phase 2). Python
            // Lark defaults to Earley, but here that would make the default
            // options fail to build, so we default to the working backend.
            parser: ParserAlgorithm::Lalr,
            lexer: LexerType::Auto,
            ambiguity: Ambiguity::Resolve,
            propagate_positions: false,
            keep_all_tokens: false,
            maybe_placeholders: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParserAlgorithm {
    Earley,
    Lalr,
    Cyk,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LexerType {
    Auto,
    Basic,
    Contextual,
    Dynamic,
    DynamicComplete,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ambiguity {
    Resolve,
    Explicit,
    Forest,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_grammar_load_simple() {
        let grammar = grammar::load_grammar(
            "start: WORD\nWORD: /[a-z]+/\n",
            &["start".to_string()],
            false,
            false,
        ).unwrap();
        assert!(!grammar.rules.is_empty());
        assert!(grammar.terminals.iter().any(|t| t.name == "WORD"));
    }

    #[test]
    fn test_grammar_load_with_import() {
        let grammar = grammar::load_grammar(
            "%import common.WORD\nstart: WORD\n",
            &["start".to_string()],
            false,
            false,
        ).unwrap();
        assert!(grammar.terminals.iter().any(|t| t.name == "WORD"));
    }

    #[test]
    fn test_grammar_load_alternation() {
        let res = grammar::load_grammar(
            "start: \"hello\" | \"world\"\n",
            &["start".to_string()],
            false,
            false,
        );
        assert!(res.is_ok());
        let grammar = res.unwrap();
        assert!(grammar.rules.len() >= 2);
    }

    #[test]
    fn test_grammar_ebnf_operators() {
        // star, plus, optional
        let res = grammar::load_grammar(
            "start: item* sep item+\nitem: /[a-z]/\nsep: \",\"?\n",
            &["start".to_string()],
            false,
            false,
        );
        assert!(res.is_ok(), "Grammar load failed: {:?}", res.err());
    }

    #[test]
    fn test_terminal_reference_is_inlined() {
        // A terminal may reference another terminal (defined in any order); the
        // referenced pattern is inlined and the referenced-only terminal is pruned.
        let grammar = grammar::load_grammar(
            "start: GREETING\nGREETING: HELLO | HI\nHELLO: \"hello\"\nHI: \"hi\"\n",
            &["start".to_string()],
            false,
            false,
        )
        .unwrap();
        let names: Vec<&str> = grammar.terminals.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"GREETING"), "GREETING kept: {names:?}");
        assert!(!names.contains(&"HELLO"), "HELLO inlined+pruned: {names:?}");
        assert!(!names.contains(&"HI"), "HI inlined+pruned: {names:?}");
    }

    #[test]
    fn test_cyclic_terminal_is_rejected() {
        // Terminals denote regular languages, so a reference cycle is an error
        // (Python Lark raises GrammarError too) — not a hang or stack overflow.
        let res = grammar::load_grammar(
            "start: A\nA: \"a\" B\nB: \"b\" A\n",
            &["start".to_string()],
            false,
            false,
        );
        assert!(matches!(res, Err(GrammarError::Other { .. })), "got {res:?}");
    }

    #[test]
    fn test_grammar_ignore_directive() {
        let grammar = grammar::load_grammar(
            "start: WORD\nWORD: /[a-z]+/\n%ignore /[ \\t]+/\n",
            &["start".to_string()],
            false,
            false,
        ).unwrap();
        assert!(!grammar.ignore.is_empty());
    }
}
