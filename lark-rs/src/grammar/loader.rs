//! Parses `.lark` EBNF grammar text into a compiled [`Grammar`].
//!
//! The `.lark` format:
//! - Lowercase names are rules; UPPERCASE names are terminals
//! - Rule modifiers: `!rule` (keep all tokens), `?rule` (inline if single child)
//! - EBNF operators: `+`, `*`, `?`, `|`
//! - Repetition: `expr~n` (exactly n), `expr~n..m` (n to m)
//! - Optional groups: `[...]`
//! - Inline rules: `(...)` group as anonymous rule
//! - Range: `"a".."z"`
//! - Aliases: `expansion -> alias_name`
//! - Directives: `%ignore`, `%import`, `%declare`, `%override`, `%extend`

use std::collections::HashMap;
use super::{Grammar, symbol::*, rule::*, terminal::*};
use crate::error::GrammarError;

/// Convert grammar text to a compiled [`Grammar`].
pub fn load_grammar(
    grammar_text: &str,
    start: &[String],
    maybe_placeholders: bool,
) -> Result<Grammar, GrammarError> {
    let mut parser = GrammarParser::new(grammar_text);
    let items = parser.parse_start()?;

    let mut compiler = GrammarCompiler::new(start.to_vec(), maybe_placeholders);
    compiler.process_items(items)?;
    compiler.compile()
}

// ─── Tokenizer ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Rule(String),
    Terminal(String),
    String(String, bool),   // value, case_insensitive
    Regexp(String, u32),    // pattern, flags
    Number(i32),
    LPar,
    RPar,
    LBra,
    RBra,
    LBrace,
    RBrace,
    Colon,
    Comma,
    Or,
    Dot,
    DotDot,
    Tilde,
    Op(char),               // + * ?
    Arrow,                  // ->
    RuleModifiers(String),  // !, !?, ?!, ?
    Ignore,
    Import,
    Declare,
    Override,
    Extend,
    Newline,
    Comment,
}

struct Lexer<'a> {
    src: &'a str,
    pos: usize,
    line: usize,
    col: usize,
    peeked: Option<(Tok, usize, usize)>,
}

impl<'a> Lexer<'a> {
    fn new(src: &'a str) -> Self {
        Lexer { src, pos: 0, line: 1, col: 1, peeked: None }
    }

    fn rest(&self) -> &str {
        &self.src[self.pos..]
    }

    fn advance(&mut self, n: usize) {
        for ch in self.src[self.pos..self.pos + n].chars() {
            if ch == '\n' {
                self.line += 1;
                self.col = 1;
            } else {
                self.col += 1;
            }
        }
        self.pos += n;
    }

    fn skip_ws_inline(&mut self) {
        let n = self.rest().bytes()
            .take_while(|&b| b == b' ' || b == b'\t')
            .count();
        self.advance(n);
    }

    fn next_tok(&mut self) -> Result<Option<Tok>, GrammarError> {
        if let Some(peeked) = self.peeked.take() {
            self.line = peeked.1;
            self.col = peeked.2;
            return Ok(Some(peeked.0));
        }
        self.next_tok_inner()
    }

    fn peek_tok(&mut self) -> Result<Option<&Tok>, GrammarError> {
        if self.peeked.is_none() {
            let save_line = self.line;
            let save_col = self.col;
            if let Some(tok) = self.next_tok_inner()? {
                self.peeked = Some((tok, self.line, self.col));
                // restore position metadata to before the peek
                self.line = save_line;
                self.col = save_col;
            }
        }
        Ok(self.peeked.as_ref().map(|(t, _, _)| t))
    }

    fn next_tok_inner(&mut self) -> Result<Option<Tok>, GrammarError> {
        loop {
            self.skip_ws_inline();

            // Extract all needed info in a scoped borrow so we can call &mut self below.
            enum Dispatch {
                Empty,
                LineContinuation(usize),
                Newline(usize),
                Comment(usize),
                Directive(&'static str, usize),
                Arrow,
                DotDot,
                Dot,
                RuleModifier(String, usize),
                Str,
                Re,
                NumberCandidate,
                Terminal,
                Rule,
                SingleChar(char, usize),
            }

            let dispatch = {
                let rest = self.rest();
                if rest.is_empty() {
                    Dispatch::Empty
                } else if rest.starts_with("\\\n") || rest.starts_with("\\ \n") {
                    Dispatch::LineContinuation(rest.find('\n').unwrap() + 1)
                } else if rest.starts_with('\n') || rest.starts_with('\r') {
                    let n = rest.bytes()
                        .take_while(|&b| b == b'\n' || b == b'\r' || b == b' ' || b == b'\t')
                        .count();
                    Dispatch::Newline(n)
                } else if rest.starts_with("//") || rest.starts_with('#') {
                    Dispatch::Comment(rest.find('\n').unwrap_or(rest.len()))
                } else if rest.starts_with("%ignore")
                    && rest[7..].chars().next().map_or(true, |c| !c.is_alphanumeric() && c != '_')
                {
                    Dispatch::Directive("ignore", 7)
                } else if rest.starts_with("%import")
                    && rest[7..].chars().next().map_or(true, |c| !c.is_alphanumeric() && c != '_')
                {
                    Dispatch::Directive("import", 7)
                } else if rest.starts_with("%declare")
                    && rest[8..].chars().next().map_or(true, |c| !c.is_alphanumeric() && c != '_')
                {
                    Dispatch::Directive("declare", 8)
                } else if rest.starts_with("%override")
                    && rest[9..].chars().next().map_or(true, |c| !c.is_alphanumeric() && c != '_')
                {
                    Dispatch::Directive("override", 9)
                } else if rest.starts_with("%extend")
                    && rest[7..].chars().next().map_or(true, |c| !c.is_alphanumeric() && c != '_')
                {
                    Dispatch::Directive("extend", 7)
                } else if rest.starts_with("->") {
                    Dispatch::Arrow
                } else if rest.starts_with("..") {
                    Dispatch::DotDot
                } else if rest.starts_with('.') {
                    Dispatch::Dot
                } else if (rest.starts_with("!?") || rest.starts_with("?!"))
                    && rest[2..].starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
                {
                    Dispatch::RuleModifier(rest[..2].to_string(), 2)
                } else if rest.starts_with('!')
                    && rest[1..].starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
                {
                    Dispatch::RuleModifier("!".to_string(), 1)
                } else if rest.starts_with('?')
                    && rest[1..].starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
                {
                    Dispatch::RuleModifier("?".to_string(), 1)
                } else if rest.starts_with('"') {
                    Dispatch::Str
                } else if rest.starts_with('/') {
                    Dispatch::Re
                } else if rest.starts_with(|c: char| c.is_ascii_digit() || c == '-' || c == '+') {
                    Dispatch::NumberCandidate
                } else if rest.starts_with(|c: char| c.is_ascii_uppercase())
                    || (rest.starts_with('_')
                        && rest[1..].starts_with(|c: char| c.is_ascii_uppercase()))
                {
                    Dispatch::Terminal
                } else if rest.starts_with(|c: char| c.is_ascii_lowercase() || c == '_') {
                    Dispatch::Rule
                } else {
                    let ch = rest.chars().next().unwrap();
                    Dispatch::SingleChar(ch, ch.len_utf8())
                }
            };

            match dispatch {
                Dispatch::Empty => return Ok(None),
                Dispatch::LineContinuation(n) => { self.advance(n); continue; }
                Dispatch::Newline(n) => {
                    self.advance(n);
                    return Ok(Some(Tok::Newline));
                }
                Dispatch::Comment(n) => { self.advance(n); continue; }
                Dispatch::Directive(name, n) => {
                    self.advance(n);
                    return Ok(Some(match name {
                        "ignore"   => Tok::Ignore,
                        "import"   => Tok::Import,
                        "declare"  => Tok::Declare,
                        "override" => Tok::Override,
                        "extend"   => Tok::Extend,
                        _          => unreachable!(),
                    }));
                }
                Dispatch::Arrow => { self.advance(2); return Ok(Some(Tok::Arrow)); }
                Dispatch::DotDot => { self.advance(2); return Ok(Some(Tok::DotDot)); }
                Dispatch::Dot => { self.advance(1); return Ok(Some(Tok::Dot)); }
                Dispatch::RuleModifier(s, n) => {
                    self.advance(n);
                    return Ok(Some(Tok::RuleModifiers(s)));
                }
                Dispatch::Str => return self.lex_string(),
                Dispatch::Re => return self.lex_regexp(),
                Dispatch::NumberCandidate => {
                    if let Some(tok) = self.try_lex_number() {
                        return Ok(Some(tok));
                    }
                    // Lone + or - without a following digit → treat as operator
                    let ch = self.src[self.pos..].chars().next().unwrap();
                    let n = ch.len_utf8();
                    self.advance(n);
                    return Ok(Some(match ch {
                        '+' | '*' => Tok::Op(ch),
                        _ => return Err(GrammarError::SyntaxError {
                            line: self.line, col: self.col,
                            msg: format!("Unexpected character: {:?}", ch),
                        }),
                    }));
                }
                Dispatch::Terminal => return self.lex_terminal(),
                Dispatch::Rule => return self.lex_rule(),
                Dispatch::SingleChar(ch, n) => {
                    self.advance(n);
                    return Ok(Some(match ch {
                        '(' => Tok::LPar,
                        ')' => Tok::RPar,
                        '[' => Tok::LBra,
                        ']' => Tok::RBra,
                        '{' => Tok::LBrace,
                        '}' => Tok::RBrace,
                        ':' => Tok::Colon,
                        ',' => Tok::Comma,
                        '|' => Tok::Or,
                        '~' => Tok::Tilde,
                        '+' | '*' => Tok::Op(ch),
                        '?' => Tok::Op('?'),
                        _ => return Err(GrammarError::SyntaxError {
                            line: self.line,
                            col: self.col,
                            msg: format!("Unexpected character: {:?}", ch),
                        }),
                    }));
                }
            } // match dispatch
        } // loop
    } // fn next_tok_inner

    fn lex_string(&mut self) -> Result<Option<Tok>, GrammarError> {
        // Extract owned data so we don't hold a borrow across self.advance()
        let src = self.src[self.pos..].to_string();
        let mut i = 1; // skip opening "
        while i < src.len() {
            match src.as_bytes()[i] {
                b'\\' => i += 2,
                b'"' => {
                    i += 1;
                    let ci = src[i..].starts_with('i')
                        && !src[i+1..].starts_with(|c: char| c.is_alphanumeric() || c == '_');
                    if ci { i += 1; }
                    let raw = &src[1..i - if ci { 2 } else { 1 }];
                    let value = unescape_string(raw);
                    self.advance(i);
                    return Ok(Some(Tok::String(value, ci)));
                }
                _ => i += 1,
            }
        }
        Err(GrammarError::SyntaxError {
            line: self.line, col: self.col,
            msg: "Unterminated string literal".to_string(),
        })
    }

    fn lex_regexp(&mut self) -> Result<Option<Tok>, GrammarError> {
        let src = self.src[self.pos..].to_string();
        let mut i = 1; // skip opening /
        while i < src.len() {
            match src.as_bytes()[i] {
                b'\\' => i += 2,
                b'/' => {
                    i += 1;
                    let flag_start = i;
                    while i < src.len() && b"imslux".contains(&src.as_bytes()[i]) {
                        i += 1;
                    }
                    let flag_str = &src[flag_start..i];
                    let pattern = src[1..i - 1 - flag_str.len()].to_string();
                    let flags = parse_re_flags(flag_str);
                    self.advance(i);
                    return Ok(Some(Tok::Regexp(pattern, flags)));
                }
                _ => i += 1,
            }
        }
        Err(GrammarError::SyntaxError {
            line: self.line, col: self.col,
            msg: "Unterminated regex literal".to_string(),
        })
    }

    fn try_lex_number(&mut self) -> Option<Tok> {
        let rest = self.rest();
        let sign = if rest.starts_with('-') || rest.starts_with('+') { 1 } else { 0 };
        let after_sign = &rest[sign..];
        if !after_sign.starts_with(|c: char| c.is_ascii_digit()) {
            return None;
        }
        let len = sign + after_sign.bytes().take_while(|b| b.is_ascii_digit()).count();
        let n: i32 = rest[..len].parse().ok()?;
        self.advance(len);
        Some(Tok::Number(n))
    }

    fn lex_rule(&mut self) -> Result<Option<Tok>, GrammarError> {
        let rest = self.rest();
        let len = rest.bytes()
            .take_while(|&b| b.is_ascii_lowercase() || b.is_ascii_uppercase() || b == b'_' || b.is_ascii_digit())
            .count();
        let name = rest[..len].to_string();
        self.advance(len);
        Ok(Some(Tok::Rule(name)))
    }

    fn lex_terminal(&mut self) -> Result<Option<Tok>, GrammarError> {
        let rest = self.rest();
        let len = rest.bytes()
            .take_while(|&b| b.is_ascii_uppercase() || b == b'_' || b.is_ascii_digit())
            .count();
        let name = rest[..len].to_string();
        self.advance(len);
        Ok(Some(Tok::Terminal(name)))
    }
}

fn unescape_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('\\') => out.push('\\'),
                Some('"') => out.push('"'),
                Some('\'') => out.push('\''),
                Some(c) => { out.push('\\'); out.push(c); }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn parse_re_flags(s: &str) -> u32 {
    let mut flags = 0u32;
    for c in s.chars() {
        flags |= match c {
            'i' => flags::IGNORECASE,
            'm' => flags::MULTILINE,
            's' => flags::DOTALL,
            'x' => flags::VERBOSE,
            _ => 0,
        };
    }
    flags
}

// ─── AST nodes ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
enum Item {
    RuleItem(RawRule),
    TermItem(RawTerm),
    /// Each element is one `%ignore` expansion (list of Exprs).
    IgnoreItem(Vec<Vec<Expr>>),
    ImportItem(ImportSpec),
    DeclareItem(Vec<Symbol>),
}

#[derive(Debug, Clone)]
struct RawRule {
    name: String,
    modifiers: String,
    params: Vec<String>,
    priority: i32,
    expansions: Vec<AliasedExpansion>,
}

#[derive(Debug, Clone)]
struct AliasedExpansion {
    expansion: Vec<Expr>,
    alias: Option<String>,
}

#[derive(Debug, Clone)]
enum Expr {
    Value(Value),
    Repeat { inner: Box<Expr>, min: usize, max: Option<usize> },
    Group(Vec<AliasedExpansion>),
    Maybe(Vec<AliasedExpansion>),
}

#[derive(Debug, Clone)]
enum Value {
    Terminal(String),
    Rule(String),
    Literal(LiteralVal),
    Range(String, String),
    TemplateUsage { name: String, args: Vec<Value> },
}

#[derive(Debug, Clone)]
enum LiteralVal {
    Str(String, bool),         // value, case-insensitive
    Re(String, u32),           // pattern, flags
}

#[derive(Debug, Clone)]
struct RawTerm {
    name: String,
    priority: i32,
    expansions: Vec<AliasedExpansion>,
}

#[derive(Debug, Clone)]
struct ImportSpec {
    path: Vec<String>,   // e.g. ["common"] or [".", "mylib"]
    relative: bool,
    names: Option<Vec<String>>,
    alias: Option<String>,
}

// ─── Recursive-descent grammar parser ────────────────────────────────────────

struct GrammarParser<'a> {
    lexer: Lexer<'a>,
}

impl<'a> GrammarParser<'a> {
    fn new(src: &'a str) -> Self {
        GrammarParser { lexer: Lexer::new(src) }
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

    fn parse_start(&mut self) -> Result<Vec<Item>, GrammarError> {
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
                Ok(Some(Item::IgnoreItem(expansions.into_iter().map(|a| a.expansion).collect())))
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
                self.lexer.next_tok()?; // consume modifier; treat same as normal for now
                self.parse_item()
            }
            Some(Tok::RuleModifiers(_)) => {
                let rule = self.parse_rule()?;
                Ok(Some(Item::RuleItem(rule)))
            }
            Some(Tok::Rule(_)) => {
                let rule = self.parse_rule()?;
                Ok(Some(Item::RuleItem(rule)))
            }
            Some(Tok::Terminal(_)) => {
                let term = self.parse_term()?;
                Ok(Some(Item::TermItem(term)))
            }
            Some(other) => {
                let msg = format!("Unexpected token at top level: {:?}", other);
                let (line, col) = (self.lexer.line, self.lexer.col);
                Err(GrammarError::SyntaxError { line, col, msg })
            }
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

    fn parse_rule(&mut self) -> Result<RawRule, GrammarError> {
        // rule_modifiers?
        let modifiers = if let Some(Tok::RuleModifiers(_)) = self.lexer.peek_tok()? {
            if let Some(Tok::RuleModifiers(m)) = self.lexer.next_tok()? {
                m
            } else { String::new() }
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

        Ok(RawRule { name, modifiers, params, priority, expansions })
    }

    fn parse_template_params(&mut self) -> Result<Vec<String>, GrammarError> {
        let mut params = Vec::new();
        loop {
            match self.lexer.next_tok()? {
                Some(Tok::Rule(n)) => params.push(n),
                other => return Err(self.err(format!("Expected template param name, got {:?}", other))),
            }
            match self.lexer.peek_tok()? {
                Some(Tok::Comma) => { self.lexer.next_tok()?; }
                _ => break,
            }
        }
        Ok(params)
    }

    fn parse_term(&mut self) -> Result<RawTerm, GrammarError> {
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

        Ok(RawTerm { name, priority, expansions })
    }

    fn parse_expansions(&mut self) -> Result<Vec<AliasedExpansion>, GrammarError> {
        let mut alts = Vec::new();
        alts.push(self.parse_alias()?);
        loop {
            match self.lexer.peek_tok()? {
                Some(Tok::Or) => {
                    self.lexer.next_tok()?;
                    // Allow newline after |
                    if let Some(Tok::Newline) = self.lexer.peek_tok()? {
                        self.lexer.next_tok()?;
                    }
                    alts.push(self.parse_alias()?);
                }
                Some(Tok::Newline) => {
                    // Continuation: newline followed by | on the next line.
                    // Consume the newline speculatively; if the next token is
                    // not Or, break and leave the cursor after the newline
                    // (consume_newline in the caller becomes a no-op).
                    self.lexer.next_tok()?;
                    if let Some(Tok::Or) = self.lexer.peek_tok()? {
                        self.lexer.next_tok()?; // consume |
                        // Allow another newline immediately after |
                        if let Some(Tok::Newline) = self.lexer.peek_tok()? {
                            self.lexer.next_tok()?;
                        }
                        alts.push(self.parse_alias()?);
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
                None
                | Some(Tok::Newline)
                | Some(Tok::Or)
                | Some(Tok::RPar)
                | Some(Tok::RBra)
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
                Ok(Expr::Repeat { inner: Box::new(atom), min: 1, max: None })
            }
            Some(Tok::Op('*')) => {
                self.lexer.next_tok()?;
                Ok(Expr::Repeat { inner: Box::new(atom), min: 0, max: None })
            }
            Some(Tok::Op('?')) => {
                self.lexer.next_tok()?;
                Ok(Expr::Repeat { inner: Box::new(atom), min: 0, max: Some(1) })
            }
            Some(Tok::Tilde) => {
                self.lexer.next_tok()?;
                let min = match self.lexer.next_tok()? {
                    Some(Tok::Number(n)) => n as usize,
                    other => return Err(self.err(format!("Expected number after ~, got {:?}", other))),
                };
                let max = if let Some(Tok::DotDot) = self.lexer.peek_tok()? {
                    self.lexer.next_tok()?;
                    match self.lexer.next_tok()? {
                        Some(Tok::Number(n)) => Some(n as usize),
                        other => return Err(self.err(format!("Expected number after .., got {:?}", other))),
                    }
                } else {
                    Some(min)
                };
                Ok(Expr::Repeat { inner: Box::new(atom), min, max })
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
                        other => Err(self.err(format!("Expected string after .., got {:?}", other))),
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
                Some(Tok::Comma) => { self.lexer.next_tok()?; }
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

        Ok(ImportSpec { path, relative, names, alias })
    }

    fn parse_name_list(&mut self) -> Result<Vec<String>, GrammarError> {
        let mut names = Vec::new();
        loop {
            match self.lexer.next_tok()? {
                Some(Tok::Rule(n)) | Some(Tok::Terminal(n)) => names.push(n),
                other => return Err(self.err(format!("Expected name, got {:?}", other))),
            }
            match self.lexer.peek_tok()? {
                Some(Tok::Comma) => { self.lexer.next_tok()?; }
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

// ─── Grammar Compiler: AST → BNF ─────────────────────────────────────────────

/// Converts the parsed AST into flat BNF rules and terminal definitions.
struct GrammarCompiler {
    start: Vec<String>,
    rules: Vec<Rule>,
    terminals: Vec<TerminalDef>,
    ignore_patterns: Vec<Pattern>,
    /// Counter for generating unique anonymous rule names.
    anon_counter: usize,
    /// Counter for generating unique terminal names for literals.
    term_counter: usize,
    /// Cache: literal string/regex → auto-generated terminal name.
    literal_cache: HashMap<String, String>,
    /// Template definitions: name → (params, expansions).
    templates: HashMap<String, (Vec<String>, Vec<AliasedExpansion>)>,
    /// Whether absent `[...]` groups emit `None` placeholders (Lark parity).
    maybe_placeholders: bool,
    /// `keep_all_tokens` of the rule currently being compiled — needed to count
    /// kept symbols for placeholder generation.
    current_keep_all: bool,
}

impl GrammarCompiler {
    fn new(start: Vec<String>, maybe_placeholders: bool) -> Self {
        GrammarCompiler {
            start,
            rules: Vec::new(),
            terminals: Vec::new(),
            ignore_patterns: Vec::new(),
            anon_counter: 0,
            term_counter: 0,
            literal_cache: HashMap::new(),
            templates: HashMap::new(),
            maybe_placeholders,
            current_keep_all: false,
        }
    }

    fn fresh_anon_rule(&mut self, tag: &str) -> String {
        let name = format!("__anon_{}_{}", tag, self.anon_counter);
        self.anon_counter += 1;
        name
    }

    /// Options for anonymous EBNF helper rules (groups, optionals, repetition).
    /// `keep_all_tokens` propagates from the enclosing rule so that `!rule` keeps
    /// tokens inside its `[...]`, `(...)`, `*`, `+` sub-expressions too.
    fn anon_opts(&self) -> RuleOptions {
        RuleOptions { keep_all_tokens: self.current_keep_all, ..RuleOptions::default() }
    }

    fn fresh_terminal(&mut self) -> String {
        let name = format!("__ANON_{}", self.term_counter);
        self.term_counter += 1;
        name
    }

    fn process_items(&mut self, items: Vec<Item>) -> Result<(), GrammarError> {
        // First pass: register templates
        for item in &items {
            if let Item::RuleItem(r) = item {
                if !r.params.is_empty() {
                    self.templates.insert(r.name.clone(), (r.params.clone(), r.expansions.clone()));
                }
            }
        }

        // Two passes: define terminals (and imports) before compiling rule bodies,
        // so a string literal in a rule can unify with an already-known terminal
        // of identical pattern (and avoid duplicate-name collisions).
        let (term_items, rule_items): (Vec<Item>, Vec<Item>) = items.into_iter().partition(
            |item| matches!(item, Item::TermItem(_) | Item::ImportItem(_) | Item::DeclareItem(_)),
        );
        for item in term_items {
            match item {
                Item::TermItem(t) => self.compile_term(t)?,
                Item::ImportItem(spec) => self.resolve_import(spec)?,
                Item::DeclareItem(_) => { /* forward declarations — no-op for now */ }
                _ => unreachable!(),
            }
        }
        for item in rule_items {
            match item {
                Item::RuleItem(r) if !r.params.is_empty() => { /* template — used on demand */ }
                Item::RuleItem(r) => self.compile_rule(r)?,
                Item::IgnoreItem(expansions) => {
                    for expansion in expansions {
                        let pat = self.expansion_to_pattern(&expansion)?;
                        self.ignore_patterns.push(pat);
                    }
                }
                _ => unreachable!(),
            }
        }
        Ok(())
    }

    fn compile_rule(&mut self, raw: RawRule) -> Result<(), GrammarError> {
        let keep_all = raw.modifiers.contains('!');
        let expand1 = raw.modifiers.contains('?');
        let origin = NonTerminal::new(&raw.name);
        // Make keep_all visible to placeholder counting while this rule's body
        // (and the anonymous rules it expands into) is compiled.
        self.current_keep_all = keep_all;

        for (order, alt) in raw.expansions.into_iter().enumerate() {
            let alias = alt.alias.clone();
            let expansion_syms = self.compile_expansion(alt.expansion, &origin.name)?;
            let options = RuleOptions {
                expand1,
                keep_all_tokens: keep_all,
                priority: raw.priority,
                empty_indices: Vec::new(),
                placeholder_count: 0,
            };
            self.rules.push(Rule::new(
                origin.clone(),
                expansion_syms,
                alias,
                options,
                order,
            ));
        }
        Ok(())
    }

    /// Compile a list of `Expr` nodes into a `Vec<Symbol>`, creating auxiliary
    /// rules as needed for EBNF operators.
    fn compile_expansion(
        &mut self,
        exprs: Vec<Expr>,
        parent: &str,
    ) -> Result<Vec<Symbol>, GrammarError> {
        let mut syms = Vec::new();
        for expr in exprs {
            let sym = self.compile_expr(expr, parent)?;
            syms.push(sym);
        }
        Ok(syms)
    }

    fn compile_expr(&mut self, expr: Expr, parent: &str) -> Result<Symbol, GrammarError> {
        match expr {
            Expr::Value(v) => self.compile_value(v, parent),
            Expr::Group(alts) => self.compile_group(alts, parent, false),
            Expr::Maybe(alts) => self.compile_maybe(alts, parent),
            Expr::Repeat { inner, min, max } => {
                self.compile_repeat(*inner, min, max, parent)
            }
        }
    }

    fn compile_value(&mut self, v: Value, parent: &str) -> Result<Symbol, GrammarError> {
        match v {
            Value::Terminal(name) => Ok(Symbol::Terminal(Terminal::new(name))),
            Value::Rule(name) => Ok(Symbol::NonTerminal(NonTerminal::new(name))),
            Value::Literal(lit) => {
                let term_name = self.get_or_create_terminal(lit)?;
                Ok(Symbol::Terminal(Terminal::new(term_name)))
            }
            Value::Range(from, to) => {
                let pat_str = format!("[{}-{}]",
                    regex::escape(&from), regex::escape(&to));
                let pat = Pattern::Re(PatternRe::new(&pat_str, 0)?);
                let name = self.fresh_terminal();
                self.terminals.push(TerminalDef::new(&name, pat, 0).with_filter_out(true));
                Ok(Symbol::Terminal(Terminal::new(name)))
            }
            Value::TemplateUsage { name, args } => {
                self.instantiate_template(&name, args, parent)
            }
        }
    }

    fn get_or_create_terminal(&mut self, lit: LiteralVal) -> Result<String, GrammarError> {
        let key = format!("{:?}", lit);
        if let Some(name) = self.literal_cache.get(&key) {
            return Ok(name.clone());
        }
        let (pat, name_hint) = match &lit {
            LiteralVal::Str(s, ci) => {
                let mut flags = 0;
                if *ci { flags |= flags::IGNORECASE; }
                let pat = if *ci {
                    Pattern::Re(PatternRe::new(&regex::escape(s), flags)?)
                } else {
                    Pattern::Str(PatternStr::new(s.as_str()))
                };
                // Try to create a human-readable name from the string content
                let hint = terminal_name_hint(s);
                (pat, hint)
            }
            LiteralVal::Re(pattern, flags) => {
                let pat = Pattern::Re(PatternRe::new(pattern.as_str(), *flags)?);
                (pat, None)
            }
        };
        // Use the clean hint when it is a fresh, valid identifier; otherwise fall
        // back to a generated `__ANON_N` name (always a valid regex group name).
        // We deliberately do NOT unify with a same-pattern user terminal: a
        // literal usage is always filter_out, whereas the named terminal may be
        // kept, so they must stay distinct.
        let name = match name_hint {
            Some(h) if !self.terminals.iter().any(|t| t.name == h) => h,
            _ => self.fresh_terminal(),
        };
        // Terminals created from a literal in a rule body are filtered by default.
        self.terminals.push(TerminalDef::new(&name, pat, 0).with_filter_out(true));
        self.literal_cache.insert(key, name.clone());
        Ok(name)
    }

    fn compile_group(
        &mut self,
        alts: Vec<AliasedExpansion>,
        _parent: &str,
        optional: bool,
    ) -> Result<Symbol, GrammarError> {
        if alts.len() == 1 && alts[0].alias.is_none() {
            // Inline single-alternative groups by flattening (handled at call site)
            // But we still need a symbol, so create a named rule
        }
        let name = self.fresh_anon_rule("group");
        let origin = NonTerminal::new(&name);
        for (order, alt) in alts.into_iter().enumerate() {
            let alias = alt.alias.clone();
            let syms = self.compile_expansion(alt.expansion, &name)?;
            self.rules.push(Rule::new(
                origin.clone(),
                syms,
                alias,
                self.anon_opts(),
                order,
            ));
        }
        if optional {
            // Add empty alternative
            self.rules.push(Rule::new(
                origin.clone(),
                vec![],
                None,
                self.anon_opts(),
                100,
            ));
        }
        Ok(Symbol::NonTerminal(origin))
    }

    fn compile_maybe(
        &mut self,
        alts: Vec<AliasedExpansion>,
        parent: &str,
    ) -> Result<Symbol, GrammarError> {
        // Without maybe_placeholders, `[x]` is just an optional group.
        if !self.maybe_placeholders {
            return self.compile_group(alts, parent, true);
        }
        // With maybe_placeholders, the empty case emits one `None` per kept symbol,
        // using the widest alternative (Python Lark inserts max-width placeholders).
        let name = self.fresh_anon_rule("maybe");
        let origin = NonTerminal::new(&name);
        let mut max_kept = 0;
        for (order, alt) in alts.into_iter().enumerate() {
            let alias = alt.alias.clone();
            let syms = self.compile_expansion(alt.expansion, &name)?;
            let kept = syms.iter().filter(|s| self.is_kept_symbol(s)).count();
            max_kept = max_kept.max(kept);
            self.rules.push(Rule::new(origin.clone(), syms, alias, self.anon_opts(), order));
        }
        let empty_opts = RuleOptions { placeholder_count: max_kept, ..self.anon_opts() };
        self.rules.push(Rule::new(origin.clone(), vec![], None, empty_opts, 100));
        Ok(Symbol::NonTerminal(origin))
    }

    /// Whether a symbol survives tree filtering (so it counts toward the number of
    /// `None` placeholders an absent `[...]` must emit). Mirrors the filter applied
    /// in `apply_rule_options`: tokens are dropped if `_`-prefixed (unless the rule
    /// keeps all tokens); `_`-prefixed nonterminals are inlined away.
    fn is_kept_symbol(&self, s: &Symbol) -> bool {
        match s {
            Symbol::Terminal(t) => {
                if self.current_keep_all {
                    return true;
                }
                // Kept iff the terminal is not filter_out.
                !self.terminals.iter().any(|td| td.name == t.name && td.filter_out)
            }
            Symbol::NonTerminal(nt) => !nt.name.starts_with('_'),
        }
    }

    fn compile_repeat(
        &mut self,
        inner: Expr,
        min: usize,
        max: Option<usize>,
        parent: &str,
    ) -> Result<Symbol, GrammarError> {
        let inner_sym = self.compile_expr(inner, parent)?;

        match (min, max) {
            (0, Some(1)) => {
                // inner? → optional rule
                let name = self.fresh_anon_rule("opt");
                let nt = NonTerminal::new(&name);
                self.rules.push(Rule::new(nt.clone(), vec![inner_sym], None, self.anon_opts(), 0));
                self.rules.push(Rule::new(nt.clone(), vec![], None, self.anon_opts(), 1));
                Ok(Symbol::NonTerminal(nt))
            }
            (1, None) => {
                // inner+ → one-or-more
                let name = self.fresh_anon_rule("plus");
                let nt = NonTerminal::new(&name);
                // name : inner | name inner
                self.rules.push(Rule::new(nt.clone(), vec![inner_sym.clone()], None, self.anon_opts(), 0));
                self.rules.push(Rule::new(nt.clone(), vec![Symbol::NonTerminal(nt.clone()), inner_sym], None, self.anon_opts(), 1));
                Ok(Symbol::NonTerminal(nt))
            }
            (0, None) => {
                // inner* → zero-or-more via helper + optional
                let name = self.fresh_anon_rule("star");
                let nt = NonTerminal::new(&name);
                // name : ε | name inner
                self.rules.push(Rule::new(nt.clone(), vec![], None, self.anon_opts(), 0));
                self.rules.push(Rule::new(nt.clone(), vec![Symbol::NonTerminal(nt.clone()), inner_sym], None, self.anon_opts(), 1));
                Ok(Symbol::NonTerminal(nt))
            }
            (n, Some(m)) if n == m => {
                // exact repetition: inline n copies
                let name = self.fresh_anon_rule("rep");
                let nt = NonTerminal::new(&name);
                let syms: Vec<Symbol> = std::iter::repeat(inner_sym).take(n).collect();
                self.rules.push(Rule::new(nt.clone(), syms, None, self.anon_opts(), 0));
                Ok(Symbol::NonTerminal(nt))
            }
            (n, max_opt) => {
                // Range: generate rules for n..m repetitions
                let max_count = max_opt.unwrap_or(n + 10); // cap at n+10 for unbounded
                let name = self.fresh_anon_rule("rep_range");
                let nt = NonTerminal::new(&name);
                for count in n..=max_count {
                    let syms: Vec<Symbol> = std::iter::repeat(inner_sym.clone()).take(count).collect();
                    self.rules.push(Rule::new(nt.clone(), syms, None, self.anon_opts(), count));
                }
                Ok(Symbol::NonTerminal(nt))
            }
        }
    }

    fn compile_term(&mut self, raw: RawTerm) -> Result<(), GrammarError> {
        // A terminal's expansion must resolve to a single pattern (or alternative of patterns).
        // Sort alternatives longest-first to mirror Python Lark's ordering so that
        // more-specific patterns (e.g. decimal+exponent) beat their prefixes.
        let mut patterns = Vec::new();
        for alt in raw.expansions {
            let pat = self.expansion_to_pattern(&alt.expansion)?;
            patterns.push(pat.as_regex_str().to_string());
        }
        let combined = if patterns.len() == 1 {
            patterns.remove(0)
        } else {
            patterns.sort_by(|a, b| b.len().cmp(&a.len()));
            patterns.into_iter().map(|p| format!("(?:{})", p)).collect::<Vec<_>>().join("|")
        };
        let pat = Pattern::Re(PatternRe::new(&combined, 0)?);
        // A user terminal already defined (e.g. via %import) is not redefined.
        if self.terminals.iter().any(|t| t.name == raw.name) {
            return Ok(());
        }
        let filter_out = raw.name.starts_with('_');
        self.terminals.push(
            TerminalDef::new(&raw.name, pat, raw.priority).with_filter_out(filter_out),
        );
        Ok(())
    }

    fn expansion_to_pattern(&self, exprs: &[Expr]) -> Result<Pattern, GrammarError> {
        // For terminal expansions, build a regex from literals/ranges.
        let mut parts = Vec::new();
        for expr in exprs {
            let p = self.expr_to_pattern(expr)?;
            parts.push(p);
        }
        if parts.len() == 1 {
            Ok(parts.remove(0))
        } else {
            let combined = parts.iter().map(|p| p.as_regex_str()).collect::<Vec<_>>().join("");
            Ok(Pattern::Re(PatternRe::new(&combined, 0)?))
        }
    }

    fn expr_to_pattern(&self, expr: &Expr) -> Result<Pattern, GrammarError> {
        match expr {
            Expr::Value(Value::Literal(LiteralVal::Str(s, ci))) => {
                if *ci {
                    Ok(Pattern::Re(PatternRe::new(&format!("(?i){}", regex::escape(s)), flags::IGNORECASE)?))
                } else {
                    Ok(Pattern::Str(PatternStr::new(s.as_str())))
                }
            }
            Expr::Value(Value::Literal(LiteralVal::Re(p, f))) => {
                Ok(Pattern::Re(PatternRe::new(p.as_str(), *f)?))
            }
            Expr::Value(Value::Range(from, to)) => {
                let chars: Vec<char> = from.chars().collect();
                let chare: Vec<char> = to.chars().collect();
                if chars.len() != 1 || chare.len() != 1 {
                    return Err(GrammarError::Other { msg: "Range requires single characters".to_string() });
                }
                Ok(Pattern::Re(PatternRe::new(&format!("[{}-{}]", regex::escape(from), regex::escape(to)), 0)?))
            }
            Expr::Repeat { inner, min, max } => {
                let inner_pat = self.expr_to_pattern(inner)?;
                // Inside a terminal, repetition becomes a regex quantifier.
                // Bounded forms (`~n`, `~n..m`) must emit `{n}` / `{n,m}` / `{n,}`;
                // previously they fell through to "" and silently dropped the count.
                let quantifier = match (*min, *max) {
                    (0, Some(1)) => "?".to_string(),
                    (1, None) => "+".to_string(),
                    (0, None) => "*".to_string(),
                    (n, Some(m)) if n == m => format!("{{{n}}}"),
                    (n, Some(m)) => format!("{{{n},{m}}}"),
                    (n, None) => format!("{{{n},}}"),
                };
                Ok(Pattern::Re(PatternRe::new(
                    &format!("(?:{}){}", inner_pat.as_regex_str(), quantifier),
                    0,
                )?))
            }
            Expr::Group(alts) => {
                let parts: Vec<String> = alts.iter()
                    .map(|a| {
                        let parts: Vec<Result<Pattern, GrammarError>> = a.expansion.iter()
                            .map(|e| self.expr_to_pattern(e))
                            .collect();
                        parts.into_iter()
                            .collect::<Result<Vec<_>, _>>()
                            .map(|ps| ps.iter().map(|p| p.as_regex_str().to_string()).collect::<Vec<_>>().join(""))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(Pattern::Re(PatternRe::new(&format!("(?:{})", parts.join("|")), 0)?))
            }
            Expr::Maybe(alts) => {
                let inner_pat = self.expansion_to_pattern(&alts[0].expansion)?;
                Ok(Pattern::Re(PatternRe::new(&format!("(?:{})?", inner_pat.as_regex_str()), 0)?))
            }
            // Terminal reference in %ignore — look up the terminal's pattern
            Expr::Value(Value::Terminal(name)) => {
                if let Some(td) = self.terminals.iter().find(|t| &t.name == name) {
                    Ok(td.pattern.clone())
                } else if let Some(&(_, pat_str)) = COMMON_TERMINALS.iter().find(|(n, _)| *n == name.as_str()) {
                    Ok(Pattern::Re(PatternRe::new(pat_str, 0)?))
                } else {
                    Err(GrammarError::Other { msg: format!("Unknown terminal in ignore: {name}") })
                }
            }
            _ => Err(GrammarError::Other { msg: format!("Cannot convert {:?} to pattern", expr) }),
        }
    }

    fn instantiate_template(
        &mut self,
        name: &str,
        args: Vec<Value>,
        parent: &str,
    ) -> Result<Symbol, GrammarError> {
        let (params, expansions) = self.templates.get(name)
            .ok_or_else(|| GrammarError::UndefinedRule { name: name.to_string() })?
            .clone();

        if params.len() != args.len() {
            return Err(GrammarError::Other {
                msg: format!("Template {} expects {} args, got {}", name, params.len(), args.len()),
            });
        }

        // Create a name for this instantiation
        let inst_name = format!("__{}_{}_{}", name, parent, self.anon_counter);
        self.anon_counter += 1;

        // Build substitution map
        let subst: HashMap<String, Value> = params.into_iter().zip(args).collect();

        // Substitute template params in expansions
        let expansions = Self::substitute_template(&expansions, &subst);
        let origin = NonTerminal::new(&inst_name);
        for (order, alt) in expansions.into_iter().enumerate() {
            let alias = alt.alias.clone();
            let syms = self.compile_expansion(alt.expansion, &inst_name)?;
            self.rules.push(Rule::new(origin.clone(), syms, alias, self.anon_opts(), order));
        }
        Ok(Symbol::NonTerminal(origin))
    }

    fn substitute_template(
        expansions: &[AliasedExpansion],
        subst: &HashMap<String, Value>,
    ) -> Vec<AliasedExpansion> {
        expansions.iter().map(|alt| AliasedExpansion {
            expansion: alt.expansion.iter().map(|e| Self::subst_expr(e, subst)).collect(),
            alias: alt.alias.clone(),
        }).collect()
    }

    fn subst_expr(expr: &Expr, subst: &HashMap<String, Value>) -> Expr {
        match expr {
            Expr::Value(Value::Rule(name)) => {
                if let Some(val) = subst.get(name) {
                    Expr::Value(val.clone())
                } else {
                    expr.clone()
                }
            }
            Expr::Value(Value::Terminal(name)) => {
                if let Some(val) = subst.get(name) {
                    Expr::Value(val.clone())
                } else {
                    expr.clone()
                }
            }
            Expr::Repeat { inner, min, max } => Expr::Repeat {
                inner: Box::new(Self::subst_expr(inner, subst)),
                min: *min,
                max: *max,
            },
            Expr::Group(alts) => Expr::Group(Self::substitute_template(alts, subst)),
            Expr::Maybe(alts) => Expr::Maybe(Self::substitute_template(alts, subst)),
            other => other.clone(),
        }
    }

    fn resolve_import(&mut self, spec: ImportSpec) -> Result<(), GrammarError> {
        // Determine what to import and from which module.
        //
        // Two forms:
        //   %import common.WORD              → path=["common","WORD"], no name list
        //   %import common.WS -> _WS         → path=["common","WS"], alias=Some("_WS")
        //   %import common (WORD, INT, ...)  → path=["common"], names=[...]
        let names_to_import: Vec<(String, Option<String>)> = if let Some(names) = spec.names {
            // Name list form: no per-name aliases
            names.into_iter().map(|n| (n, None)).collect()
        } else if spec.path.len() > 1 {
            // Single import: last path element is the symbol name; alias may override it.
            let original = spec.path.last().cloned().unwrap_or_default();
            vec![(original, spec.alias)]
        } else {
            return Ok(()); // nothing to import
        };

        let is_common = spec.path.first().map(String::as_str) == Some("common");
        if is_common {
            for (name, alias) in &names_to_import {
                if let Some(td) = COMMON_TERMINALS.iter().find(|(n, _)| *n == name.as_str()) {
                    let registered_name = alias.as_deref().unwrap_or(name.as_str());
                    let pat = Pattern::Re(PatternRe::new(td.1, 0)?);
                    if !self.terminals.iter().any(|t| t.name == registered_name) {
                        self.terminals.push(
                            TerminalDef::new(registered_name, pat, 0)
                                .with_filter_out(registered_name.starts_with('_')),
                        );
                    }
                }
                // Rules from common (e.g., %import common.list) are silently skipped for now.
            }
        }
        Ok(())
    }

    fn compile(mut self) -> Result<Grammar, GrammarError> {
        // Add $END terminal
        if !self.terminals.iter().any(|t| t.name == "$END") {
            // $END is synthetic and handled by the parser, not the lexer.
        }

        // Add ignore terminals (one terminal per ignore pattern)
        let n_ignore = self.ignore_patterns.len();
        let ignore_names: Vec<String> = (0..n_ignore)
            .map(|i| format!("__IGNORE_{}", i))
            .collect();
        for (i, pat) in self.ignore_patterns.into_iter().enumerate() {
            let name = format!("__IGNORE_{}", i);
            self.terminals.push(TerminalDef::new(&name, pat, 0).with_filter_out(true));
        }

        // Sort terminals by (priority desc, max_width desc, name asc)
        self.terminals.sort_by(|a, b| {
            b.priority.cmp(&a.priority)
                .then_with(|| {
                    let bw = b.pattern.max_width().unwrap_or(usize::MAX);
                    let aw = a.pattern.max_width().unwrap_or(usize::MAX);
                    bw.cmp(&aw)
                })
                .then_with(|| a.name.cmp(&b.name))
        });

        Ok(Grammar {
            rules: self.rules,
            terminals: self.terminals,
            ignore: ignore_names,
            start: self.start,
        })
    }
}

/// Attempt to produce a human-readable terminal name for a literal string.
///
/// Returns `None` when the literal has no safe identifier form (e.g. it contains
/// backslashes, tabs, or other characters that are not valid in a regex named
/// capture group); the caller then assigns a fresh `__ANON_N` name. Embedding
/// raw/escaped pattern characters in the name produces invalid group names like
/// `(?P<__ANON_\>…)` and crashes regex compilation.
fn terminal_name_hint(s: &str) -> Option<String> {
    // Common punctuation uses Python Lark's names (e.g. "," -> COMMA, "(" -> LPAR).
    // Filtering is handled by `filter_out`, not a name prefix, so names are clean.
    if let Some(&name) = TERMINAL_NAMES.iter().find(|(ch, _)| ch == &s).map(|(_, n)| n) {
        return Some(name.to_string());
    }
    // Keyword-like strings become their uppercase form, but only when that is a
    // valid regex named-capture identifier (must not start with a digit).
    let first_ok = s.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_');
    if first_ok && s.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return Some(s.to_uppercase());
    }
    None
}

/// Standard terminal names for common punctuation/operators.
static TERMINAL_NAMES: &[(&str, &str)] = &[
    (".", "DOT"), (",", "COMMA"), (":", "COLON"), (";", "SEMICOLON"),
    ("+", "PLUS"), ("-", "MINUS"), ("*", "STAR"), ("/", "SLASH"),
    ("|", "VBAR"), ("?", "QMARK"), ("!", "BANG"), ("@", "AT"),
    ("#", "HASH"), ("$", "DOLLAR"), ("%", "PERCENT"), ("^", "CIRCUMFLEX"),
    ("&", "AMPERSAND"), ("_", "UNDERSCORE"), ("<", "LESSTHAN"),
    (">", "MORETHAN"), ("=", "EQUAL"), ("\"", "DBLQUOTE"), ("'", "QUOTE"),
    ("`", "BACKQUOTE"), ("~", "TILDE"), ("(", "LPAR"), (")", "RPAR"),
    ("{", "LBRACE"), ("}", "RBRACE"), ("[", "LSQB"), ("]", "RSQB"),
    ("\n", "NEWLINE"), ("\t", "TAB"), (" ", "SPACE"),
];

/// Subset of `common.lark` terminals inlined for %import resolution.
static COMMON_TERMINALS: &[(&str, &str)] = &[
    ("DIGIT",           r"[0-9]"),
    ("HEXDIGIT",        r"[0-9a-fA-F]"),
    ("INT",             r"[0-9]+"),
    ("SIGNED_INT",      r"[+-]?[0-9]+"),
    ("DECIMAL",         r"[0-9]+\.[0-9]*"),
    ("FLOAT",           r"(?i)(\d+e[+-]?\d+|\d+\.\d*e[+-]?\d+|\d*\.\d+e[+-]?\d+|\d+\.\d*|\d*\.\d+)"),
    ("SIGNED_FLOAT",    r"[+-]?(\d+e[+-]?\d+|\d+\.\d*e[+-]?\d+|\d*\.\d+e[+-]?\d+|\d+\.\d*|\d*\.\d+)"),
    ("NUMBER",          r"(\d+\.?\d*|\.\d+)([Ee][+-]?\d+)?"),
    ("SIGNED_NUMBER",   r"[+-]?([0-9]+\.?[0-9]*|\.[0-9]+)([Ee][+-]?[0-9]+)?"),
    ("LETTER",          r"[a-zA-Z]"),
    ("WORD",            r"[a-zA-Z]+"),
    ("CNAME",           r"[_a-zA-Z][_a-zA-Z0-9]*"),
    ("WS_INLINE",       r"[ \t]+"),
    ("WS",              r"[ \t\f\r\n]+"),
    ("NEWLINE",         r"(\r?\n)+"),
    ("SH_COMMENT",      r"#[^\n]*"),
    ("CPP_COMMENT",     r"//[^\n]*"),
    ("C_COMMENT",       r"/\*.*?\*/"),
    ("STRING",          r#""([^"\\\n\r]|\\.)*?""#),
    ("ESCAPED_STRING",  r#""([^"\\\n\r]|\\.)*""#),
    ("LCASE_LETTER",    r"[a-z]"),
    ("UCASE_LETTER",    r"[A-Z]"),
];
