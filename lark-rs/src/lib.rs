pub mod error;
pub mod grammar;
pub mod lexer;
pub mod parsers;
pub mod perf;
pub mod postlex;
pub mod standalone;
pub mod tree;

pub use error::{GrammarError, LarkError, ParseError, RecoveredTree};
pub use grammar::{
    load_grammar, lower,
    rule::{Rule, RuleOptions},
    symbol::{NonTerminal, Symbol, Terminal},
    terminal::TerminalDef,
    CompiledGrammar, CompiledRule, Grammar, SymbolId, SymbolKind, SymbolTable,
};
pub use lexer::{BasicLexer, ContextualLexer, DynamicMatcher, Lexer, LexerConf};
pub use parsers::{
    basic_lexer_conf, lalr, EarleyParser, LexFailure, ParseTable, ParserConf, TokenSource,
};
pub use postlex::Indenter;
pub use standalone::generate as generate_standalone;
pub use tree::{Child, ParseTree, Token, Tree};

/// Main entry point — mirrors Python's `Lark(grammar, parser=..., lexer=...)`
pub struct Lark {
    pub grammar: Grammar,
    frontend: parsers::ParsingFrontend,
}

impl Lark {
    pub fn new(grammar_text: &str, options: LarkOptions) -> Result<Self, LarkError> {
        let grammar = grammar::load_grammar_with_base(
            grammar_text,
            &options.start,
            options.maybe_placeholders,
            options.keep_all_tokens,
            options.base_path.clone(),
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

    /// Parse with built-in panic-mode error recovery (issue #43).
    ///
    /// Instead of aborting on the first parse error, the parser deletes the
    /// offending token and continues (single-token-deletion recovery), returning a
    /// best-effort [`RecoveredTree`]: the partial tree plus every error recovered
    /// from. This is exactly Python Lark's `parse(text, on_error=lambda e: True)`.
    ///
    /// Only the LALR backend without a postlex hook supports recovery; other
    /// configurations return an error. See [`RecoveredTree`] for the partial-tree
    /// and error-node semantics.
    pub fn parse_with_recovery(&self, text: &str) -> Result<RecoveredTree, LarkError> {
        self.parse_on_error(text, |_| true)
    }

    /// Parse with a custom `on_error` handler, mirroring Python Lark's `on_error`
    /// callback. The handler is invoked for each parse error; return `true` to
    /// recover (delete the offending token and resume) or `false` to stop and
    /// return the partial tree built so far. The recovered errors are collected in
    /// the returned [`RecoveredTree::errors`].
    pub fn parse_on_error(
        &self,
        text: &str,
        mut on_error: impl FnMut(&ParseError) -> bool,
    ) -> Result<RecoveredTree, LarkError> {
        self.frontend.parse_recovering(text, None, &mut on_error)
    }

    /// As [`parse_on_error`](Self::parse_on_error), from an explicit start symbol.
    pub fn parse_on_error_with_start(
        &self,
        text: &str,
        start: &str,
        mut on_error: impl FnMut(&ParseError) -> bool,
    ) -> Result<RecoveredTree, LarkError> {
        self.frontend
            .parse_recovering(text, Some(start), &mut on_error)
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
    /// Reject grammars with shift/reduce conflicts (and same-priority regex
    /// terminal collisions) at construction time instead of silently resolving
    /// them. Mirrors Python Lark's `strict=True`.
    pub strict: bool,
    /// Global regex flags applied to every terminal pattern, as a bitset over
    /// [`grammar::terminal::flags`] (`IGNORECASE` etc.). Mirrors Python Lark's
    /// `g_regex_flags`. Zero (the default) leaves every terminal's own flags
    /// untouched.
    pub g_regex_flags: u32,
    /// Directory that relative `%import .module (...)` (and other non-`common`
    /// file imports) resolve against. Mirrors the base path Python Lark derives
    /// from the importing grammar's file. `None` (the default) means file imports
    /// cannot be resolved — only the bundled libraries (`common`, `python`,
    /// `unicode`, `lark`) are available, as when a grammar is built from an
    /// in-memory string with no source location.
    pub base_path: Option<std::path::PathBuf>,
    /// Post-lexer hook applied to the token stream before it reaches the parser.
    /// Currently an [`Indenter`], which injects `%declare`d `INDENT` / `DEDENT`
    /// tokens for Python-style significant-whitespace grammars. Mirrors Python
    /// Lark's `postlex` option. Only the LALR backend honours it. `None` (the
    /// default) leaves the token stream untouched.
    pub postlex: Option<postlex::Indenter>,
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
            strict: false,
            g_regex_flags: 0,
            base_path: None,
            postlex: None,
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
        )
        .unwrap();
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
        )
        .unwrap();
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
        assert!(
            matches!(res, Err(GrammarError::Other { .. })),
            "got {res:?}"
        );
    }

    #[test]
    fn test_undefined_symbol_is_rejected() {
        // Python Lark rejects a grammar that references an undefined symbol when the
        // grammar is compiled (`GrammarError("... used but not defined")`), rather
        // than deferring to a confusing parse-time failure. The loader's
        // use-before-definition pass matches that: an undefined uppercase reference
        // is an `UndefinedTerminal`, an undefined lowercase one an `UndefinedRule`.
        let res = grammar::load_grammar("start: UNDEFINED\n", &["start".to_string()], false, false);
        assert!(
            matches!(res, Err(GrammarError::UndefinedTerminal { .. })),
            "expected undefined-terminal rejection, got {res:?}"
        );
        let res = grammar::load_grammar("start: undefined\n", &["start".to_string()], false, false);
        assert!(
            matches!(res, Err(GrammarError::UndefinedRule { .. })),
            "expected undefined-rule rejection, got {res:?}"
        );
    }

    #[test]
    fn test_grammar_ignore_directive() {
        let grammar = grammar::load_grammar(
            "start: WORD\nWORD: /[a-z]+/\n%ignore /[ \\t]+/\n",
            &["start".to_string()],
            false,
            false,
        )
        .unwrap();
        assert!(!grammar.ignore.is_empty());
    }
}
