//! Phase 2 — recursive-descent grammar parser: [`Tok`] stream → [`Item`] AST.

use super::ast::*;
use super::tokenizer::{Lexer, Tok};
use crate::error::GrammarError;
use crate::grammar::symbol::{NonTerminal, Symbol, Terminal};

pub(super) struct GrammarParser<'a> {
    lexer: Lexer<'a>,
}

impl<'a> GrammarParser<'a> {
    pub(super) fn new(src: &'a str) -> Self {
        GrammarParser {
            lexer: Lexer::new(src),
        }
    }

    fn err(&self, msg: impl Into<String>) -> GrammarError {
        GrammarError::SyntaxError {
            line: self.lexer.line,
            col: self.lexer.col,
            msg: msg.into(),
        }
    }

    fn expect(&mut self, expected: &Tok) -> Result<(), GrammarError> {
        match self.lexer.next_tok()? {
            Some(ref t) if std::mem::discriminant(t) == std::mem::discriminant(expected) => Ok(()),
            Some(t) => Err(self.err(format!("Expected {:?}, got {:?}", expected, t))),
            None => Err(self.err(format!("Unexpected EOF, expected {:?}", expected))),
        }
    }

    fn skip_newlines(&mut self) -> Result<(), GrammarError> {
        while let Some(Tok::Newline) = self.lexer.peek_tok()? {
            self.lexer.next_tok()?;
        }
        Ok(())
    }

    pub(super) fn parse_start(&mut self) -> Result<Vec<Item>, GrammarError> {
        let mut items = Vec::new();
        self.skip_newlines()?;
        while self.lexer.peek_tok()?.is_some() {
            if let Some(item) = self.parse_item()? {
                items.push(item);
            }
            self.skip_newlines()?;
        }
        Ok(items)
    }

    fn parse_item(&mut self) -> Result<Option<Item>, GrammarError> {
        match self.lexer.peek_tok()? {
            None | Some(Tok::Newline) => {
                self.lexer.next_tok()?;
                Ok(None)
            }
            Some(Tok::Ignore) => {
                self.lexer.next_tok()?;
                let expansions = self.parse_expansions()?;
                self.consume_newline()?;
                Ok(Some(Item::IgnoreItem(
                    expansions.into_iter().map(|a| a.expansion).collect(),
                )))
            }
            Some(Tok::Import) => {
                self.lexer.next_tok()?;
                let spec = self.parse_import()?;
                self.consume_newline()?;
                Ok(Some(Item::ImportItem(spec)))
            }
            Some(Tok::Declare) => {
                self.lexer.next_tok()?;
                let syms = self.parse_declare_args()?;
                self.consume_newline()?;
                Ok(Some(Item::DeclareItem(syms)))
            }
            Some(Tok::Override) | Some(Tok::Extend) => {
                // `%override` / `%extend` modify the rule or terminal that
                // follows. Tag the parsed definition with the directive so the
                // compiler can replace (override) / prepend-to (extend) the
                // pre-existing definition and reject a missing target — Python
                // Lark's `_define(override=True)` / `_extend`.
                let directive = match self.lexer.next_tok()? {
                    Some(Tok::Override) => Directive::Override,
                    _ => Directive::Extend,
                };
                self.parse_directive_target(directive).map(Some)
            }
            Some(Tok::RuleModifiers(_)) => {
                let rule = self.parse_rule(Directive::Plain)?;
                Ok(Some(Item::RuleItem(rule)))
            }
            Some(Tok::Rule(_)) => {
                let rule = self.parse_rule(Directive::Plain)?;
                Ok(Some(Item::RuleItem(rule)))
            }
            Some(Tok::Terminal(_)) => {
                let term = self.parse_term(Directive::Plain)?;
                Ok(Some(Item::TermItem(term)))
            }
            Some(other) => {
                let msg = format!("Unexpected token at top level: {:?}", other);
                let (line, col) = (self.lexer.line, self.lexer.col);
                Err(GrammarError::SyntaxError { line, col, msg })
            }
        }
    }

    /// Parse the rule or terminal that an `%override` / `%extend` directive
    /// applies to, tagging it with `directive`. The directive grammar is
    /// `_OVERRIDE (rule | term)` / `_EXTEND (rule | term)` (Python Lark's
    /// `load_grammar.py`), so the next token must begin a rule or a terminal.
    fn parse_directive_target(&mut self, directive: Directive) -> Result<Item, GrammarError> {
        match self.lexer.peek_tok()?.cloned() {
            Some(Tok::RuleModifiers(_)) | Some(Tok::Rule(_)) => {
                Ok(Item::RuleItem(self.parse_rule(directive)?))
            }
            Some(Tok::Terminal(_)) => Ok(Item::TermItem(self.parse_term(directive)?)),
            other => Err(self.err(format!(
                "Expected a rule or terminal after %override/%extend, got {:?}",
                other
            ))),
        }
    }

    fn consume_newline(&mut self) -> Result<(), GrammarError> {
        // Consume a newline if present; it may have already been consumed by
        // parse_expansions() when handling multi-line alternatives.
        if let Some(Tok::Newline) = self.lexer.peek_tok()? {
            self.lexer.next_tok()?;
        }
        Ok(())
    }

    fn parse_rule(&mut self, directive: Directive) -> Result<RawRule, GrammarError> {
        // rule_modifiers?
        let modifiers = if let Some(Tok::RuleModifiers(_)) = self.lexer.peek_tok()? {
            if let Some(Tok::RuleModifiers(m)) = self.lexer.next_tok()? {
                m
            } else {
                String::new()
            }
        } else {
            String::new()
        };

        // RULE name
        let name = match self.lexer.next_tok()? {
            Some(Tok::Rule(n)) => n,
            other => return Err(self.err(format!("Expected rule name, got {:?}", other))),
        };

        // template_params: { A, B, ... }
        let params = if let Some(Tok::LBrace) = self.lexer.peek_tok()? {
            self.lexer.next_tok()?;
            let p = self.parse_template_params()?;
            self.expect(&Tok::RBrace)?;
            p
        } else {
            Vec::new()
        };

        // priority: .NUMBER
        let priority = if let Some(Tok::Dot) = self.lexer.peek_tok()? {
            self.lexer.next_tok()?;
            match self.lexer.next_tok()? {
                Some(Tok::Number(n)) => n,
                other => return Err(self.err(format!("Expected priority number, got {:?}", other))),
            }
        } else {
            0
        };

        self.expect(&Tok::Colon)?;
        let expansions = self.parse_expansions()?;
        self.consume_newline()?;

        Ok(RawRule {
            name,
            modifiers,
            params,
            priority,
            expansions,
            directive,
        })
    }

    fn parse_template_params(&mut self) -> Result<Vec<String>, GrammarError> {
        let mut params = Vec::new();
        loop {
            match self.lexer.next_tok()? {
                Some(Tok::Rule(n)) => params.push(n),
                other => {
                    return Err(self.err(format!("Expected template param name, got {:?}", other)))
                }
            }
            match self.lexer.peek_tok()? {
                Some(Tok::Comma) => {
                    self.lexer.next_tok()?;
                }
                _ => break,
            }
        }
        Ok(params)
    }

    fn parse_term(&mut self, directive: Directive) -> Result<RawTerm, GrammarError> {
        let name = match self.lexer.next_tok()? {
            Some(Tok::Terminal(n)) => n,
            other => return Err(self.err(format!("Expected terminal name, got {:?}", other))),
        };

        // optional priority: TERM.NUMBER : ...
        let priority = if let Some(Tok::Dot) = self.lexer.peek_tok()? {
            self.lexer.next_tok()?;
            match self.lexer.next_tok()? {
                Some(Tok::Number(n)) => n,
                other => return Err(self.err(format!("Expected priority number, got {:?}", other))),
            }
        } else {
            0
        };

        self.expect(&Tok::Colon)?;
        let expansions = self.parse_expansions()?;
        self.consume_newline()?;

        Ok(RawTerm {
            name,
            priority,
            expansions,
            directive,
        })
    }

    fn parse_expansions(&mut self) -> Result<Vec<AliasedExpansion>, GrammarError> {
        let mut alts = Vec::new();
        alts.push(self.parse_alias()?);
        loop {
            match self.lexer.peek_tok()? {
                Some(Tok::Or) => {
                    self.lexer.next_tok()?;
                    alts.push(self.parse_alt_after_bar()?);
                }
                Some(Tok::Newline) => {
                    // Continuation: newline followed by | on the next line.
                    // Consume the newline speculatively; if the next token is
                    // not Or, break and leave the cursor after the newline
                    // (consume_newline in the caller becomes a no-op).
                    self.lexer.next_tok()?;
                    if let Some(Tok::Or) = self.lexer.peek_tok()? {
                        self.lexer.next_tok()?; // consume |
                        alts.push(self.parse_alt_after_bar()?);
                    } else {
                        // No continuation — the newline is already consumed;
                        // caller's consume_newline() is now optional.
                        break;
                    }
                }
                _ => break,
            }
        }
        Ok(alts)
    }

    /// Parse the alternative following a `|`. An alternative's content sits on
    /// the same line as its bar, so a `|` with only a newline (or EOF) after it
    /// is a trailing empty (ε) alternative — `a: X a |` derives `X a` *or*
    /// nothing. In that case record an empty expansion and leave the newline as
    /// the rule terminator, rather than running the empty alternative into the
    /// next rule (which raised "Expected value, got Some(Colon)"). See issue #62.
    fn parse_alt_after_bar(&mut self) -> Result<AliasedExpansion, GrammarError> {
        if matches!(self.lexer.peek_tok()?, None | Some(Tok::Newline)) {
            Ok(AliasedExpansion {
                expansion: Vec::new(),
                alias: None,
            })
        } else {
            self.parse_alias()
        }
    }

    fn parse_alias(&mut self) -> Result<AliasedExpansion, GrammarError> {
        let expansion = self.parse_expansion()?;
        let alias = if let Some(Tok::Arrow) = self.lexer.peek_tok()? {
            self.lexer.next_tok()?;
            match self.lexer.next_tok()? {
                Some(Tok::Rule(name)) => Some(name),
                other => return Err(self.err(format!("Expected alias name, got {:?}", other))),
            }
        } else {
            None
        };
        Ok(AliasedExpansion { expansion, alias })
    }

    fn parse_expansion(&mut self) -> Result<Vec<Expr>, GrammarError> {
        let mut exprs = Vec::new();
        loop {
            match self.lexer.peek_tok()? {
                None | Some(Tok::Newline) | Some(Tok::Or) | Some(Tok::RPar) | Some(Tok::RBra)
                | Some(Tok::Arrow) => break,
                _ => exprs.push(self.parse_expr()?),
            }
        }
        Ok(exprs)
    }

    fn parse_expr(&mut self) -> Result<Expr, GrammarError> {
        let atom = self.parse_atom()?;
        match self.lexer.peek_tok()? {
            Some(Tok::Op('+')) => {
                self.lexer.next_tok()?;
                Ok(Expr::Repeat {
                    inner: Box::new(atom),
                    min: 1,
                    max: None,
                })
            }
            Some(Tok::Op('*')) => {
                self.lexer.next_tok()?;
                Ok(Expr::Repeat {
                    inner: Box::new(atom),
                    min: 0,
                    max: None,
                })
            }
            Some(Tok::Op('?')) => {
                self.lexer.next_tok()?;
                Ok(Expr::Repeat {
                    inner: Box::new(atom),
                    min: 0,
                    max: Some(1),
                })
            }
            Some(Tok::Tilde) => {
                self.lexer.next_tok()?;
                let min = match self.lexer.next_tok()? {
                    Some(Tok::Number(n)) => n as usize,
                    other => {
                        return Err(self.err(format!("Expected number after ~, got {:?}", other)))
                    }
                };
                let max = if let Some(Tok::DotDot) = self.lexer.peek_tok()? {
                    self.lexer.next_tok()?;
                    match self.lexer.next_tok()? {
                        Some(Tok::Number(n)) => Some(n as usize),
                        other => {
                            return Err(
                                self.err(format!("Expected number after .., got {:?}", other))
                            )
                        }
                    }
                } else {
                    Some(min)
                };
                // A `~n..m` range with n > m matches nothing; Python Lark rejects it
                // at construction, so we do too (rather than build a dead rule).
                if let Some(m) = max {
                    if min > m {
                        return Err(self.err(format!(
                            "Repetition range is empty: min {min} exceeds max {m}"
                        )));
                    }
                }
                Ok(Expr::Repeat {
                    inner: Box::new(atom),
                    min,
                    max,
                })
            }
            _ => Ok(atom),
        }
    }

    fn parse_atom(&mut self) -> Result<Expr, GrammarError> {
        match self.lexer.peek_tok()? {
            Some(Tok::LPar) => {
                self.lexer.next_tok()?;
                let expansions = self.parse_expansions()?;
                self.expect(&Tok::RPar)?;
                Ok(Expr::Group(expansions))
            }
            Some(Tok::LBra) => {
                self.lexer.next_tok()?;
                let expansions = self.parse_expansions()?;
                self.expect(&Tok::RBra)?;
                Ok(Expr::Maybe(expansions))
            }
            _ => Ok(Expr::Value(self.parse_value()?)),
        }
    }

    fn parse_value(&mut self) -> Result<Value, GrammarError> {
        match self.lexer.next_tok()? {
            Some(Tok::Terminal(name)) => Ok(Value::Terminal(name)),
            Some(Tok::Rule(name)) => {
                // Check for template_usage: name { args }
                if let Some(Tok::LBrace) = self.lexer.peek_tok()? {
                    self.lexer.next_tok()?;
                    let args = self.parse_template_args()?;
                    self.expect(&Tok::RBrace)?;
                    Ok(Value::TemplateUsage { name, args })
                } else {
                    Ok(Value::Rule(name))
                }
            }
            Some(Tok::String(s, _ci)) => {
                // Check for range: "a".."z"
                if let Some(Tok::DotDot) = self.lexer.peek_tok()? {
                    self.lexer.next_tok()?;
                    match self.lexer.next_tok()? {
                        Some(Tok::String(s2, _)) => Ok(Value::Range(s, s2)),
                        other => {
                            Err(self.err(format!("Expected string after .., got {:?}", other)))
                        }
                    }
                } else {
                    Ok(Value::Literal(LiteralVal::Str(s, _ci)))
                }
            }
            Some(Tok::Regexp(pat, flags)) => Ok(Value::Literal(LiteralVal::Re(pat, flags))),
            other => Err(self.err(format!("Expected value, got {:?}", other))),
        }
    }

    fn parse_template_args(&mut self) -> Result<Vec<Value>, GrammarError> {
        let mut args = Vec::new();
        loop {
            args.push(self.parse_value()?);
            match self.lexer.peek_tok()? {
                Some(Tok::Comma) => {
                    self.lexer.next_tok()?;
                }
                _ => break,
            }
        }
        Ok(args)
    }

    fn parse_import(&mut self) -> Result<ImportSpec, GrammarError> {
        let relative = if let Some(Tok::Dot) = self.lexer.peek_tok()? {
            self.lexer.next_tok()?;
            true
        } else {
            false
        };

        let mut path = Vec::new();
        loop {
            match self.lexer.next_tok()? {
                Some(Tok::Rule(n)) | Some(Tok::Terminal(n)) => path.push(n),
                other => return Err(self.err(format!("Expected import path, got {:?}", other))),
            }
            if let Some(Tok::Dot) = self.lexer.peek_tok()? {
                self.lexer.next_tok()?;
            } else {
                break;
            }
        }

        // Optional name list
        let names = if let Some(Tok::LPar) = self.lexer.peek_tok()? {
            self.lexer.next_tok()?;
            let names = self.parse_name_list()?;
            self.expect(&Tok::RPar)?;
            Some(names)
        } else {
            None
        };

        // Optional alias
        let alias = if let Some(Tok::Arrow) = self.lexer.peek_tok()? {
            self.lexer.next_tok()?;
            match self.lexer.next_tok()? {
                Some(Tok::Rule(n)) | Some(Tok::Terminal(n)) => Some(n),
                other => return Err(self.err(format!("Expected alias name, got {:?}", other))),
            }
        } else {
            None
        };

        Ok(ImportSpec {
            path,
            relative,
            names,
            alias,
        })
    }

    fn parse_name_list(&mut self) -> Result<Vec<String>, GrammarError> {
        let mut names = Vec::new();
        loop {
            match self.lexer.next_tok()? {
                Some(Tok::Rule(n)) | Some(Tok::Terminal(n)) => names.push(n),
                other => return Err(self.err(format!("Expected name, got {:?}", other))),
            }
            match self.lexer.peek_tok()? {
                Some(Tok::Comma) => {
                    self.lexer.next_tok()?;
                }
                _ => break,
            }
        }
        Ok(names)
    }

    fn parse_declare_args(&mut self) -> Result<Vec<Symbol>, GrammarError> {
        let mut syms = Vec::new();
        loop {
            match self.lexer.peek_tok()? {
                Some(Tok::Terminal(_)) => {
                    if let Some(Tok::Terminal(n)) = self.lexer.next_tok()? {
                        syms.push(Symbol::Terminal(Terminal::new(n)));
                    }
                }
                Some(Tok::Rule(_)) => {
                    if let Some(Tok::Rule(n)) = self.lexer.next_tok()? {
                        syms.push(Symbol::NonTerminal(NonTerminal::new(n)));
                    }
                }
                _ => break,
            }
        }
        Ok(syms)
    }
}
