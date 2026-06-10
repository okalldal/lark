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

use super::{rule::*, symbol::*, terminal::*, Grammar};
use crate::error::GrammarError;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

/// Convert grammar text to a compiled [`Grammar`].
///
/// File imports (`%import .module (...)`) cannot be resolved through this entry
/// point — it carries no base path. Use [`load_grammar_with_base`] when the
/// grammar may import from sibling files.
pub fn load_grammar(
    grammar_text: &str,
    start: &[String],
    maybe_placeholders: bool,
    keep_all_tokens: bool,
) -> Result<Grammar, GrammarError> {
    load_grammar_with_base(
        grammar_text,
        start,
        maybe_placeholders,
        keep_all_tokens,
        None,
    )
}

/// Like [`load_grammar`], but `base_path` is the directory that relative file
/// imports (`%import .module (...)`) resolve against — the directory of the
/// importing grammar's own file, mirroring Python Lark's `GrammarLoader`.
pub fn load_grammar_with_base(
    grammar_text: &str,
    start: &[String],
    maybe_placeholders: bool,
    keep_all_tokens: bool,
    base_path: Option<PathBuf>,
) -> Result<Grammar, GrammarError> {
    let mut parser = GrammarParser::new(grammar_text);
    let items = parser.parse_start()?;

    let mut compiler = GrammarCompiler::new(
        start.to_vec(),
        maybe_placeholders,
        keep_all_tokens,
        base_path,
    );
    compiler.process_items(items)?;
    compiler.compile()
}

/// Synthetic start rule appended to an imported file so the requested terminals
/// survive dead-terminal pruning while the file is compiled. Never copied out.
const IMPORT_PROBE_RULE: &str = "__lark_import_probe";

// ─── Tokenizer ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Rule(String),
    Terminal(String),
    String(String, bool), // value, case_insensitive
    Regexp(String, u32),  // pattern, flags
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
    Op(char),              // + * ?
    Arrow,                 // ->
    RuleModifiers(String), // !, !?, ?!, ?
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
        Lexer {
            src,
            pos: 0,
            line: 1,
            col: 1,
            peeked: None,
        }
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
        let n = self
            .rest()
            .bytes()
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
                    let n = rest
                        .bytes()
                        .take_while(|&b| b == b'\n' || b == b'\r' || b == b' ' || b == b'\t')
                        .count();
                    Dispatch::Newline(n)
                } else if rest.starts_with("//") || rest.starts_with('#') {
                    Dispatch::Comment(rest.find('\n').unwrap_or(rest.len()))
                } else if rest.starts_with("%ignore")
                    && rest[7..]
                        .chars()
                        .next()
                        .map_or(true, |c| !c.is_alphanumeric() && c != '_')
                {
                    Dispatch::Directive("ignore", 7)
                } else if rest.starts_with("%import")
                    && rest[7..]
                        .chars()
                        .next()
                        .map_or(true, |c| !c.is_alphanumeric() && c != '_')
                {
                    Dispatch::Directive("import", 7)
                } else if rest.starts_with("%declare")
                    && rest[8..]
                        .chars()
                        .next()
                        .map_or(true, |c| !c.is_alphanumeric() && c != '_')
                {
                    Dispatch::Directive("declare", 8)
                } else if rest.starts_with("%override")
                    && rest[9..]
                        .chars()
                        .next()
                        .map_or(true, |c| !c.is_alphanumeric() && c != '_')
                {
                    Dispatch::Directive("override", 9)
                } else if rest.starts_with("%extend")
                    && rest[7..]
                        .chars()
                        .next()
                        .map_or(true, |c| !c.is_alphanumeric() && c != '_')
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
                Dispatch::LineContinuation(n) => {
                    self.advance(n);
                    continue;
                }
                Dispatch::Newline(n) => {
                    self.advance(n);
                    // Comment-only lines collapse into the surrounding newline
                    // run: Python Lark's COMMENT terminal starts with `\s*`, so
                    // it swallows the newline+indent *before* the comment and
                    // the whole region lexes as one `_NL`. Without this, a
                    // `//`-comment line between the `|` alternatives of a
                    // multi-line rule emits two Newline tokens and the parser
                    // drops the continuation (wild bank: dotmotif).
                    loop {
                        let rest = self.rest();
                        if rest.starts_with("//") || rest.starts_with('#') {
                            let line_end = rest.find('\n').unwrap_or(rest.len());
                            self.advance(line_end);
                        } else {
                            break;
                        }
                        let run = self
                            .rest()
                            .bytes()
                            .take_while(|&b| b == b'\n' || b == b'\r' || b == b' ' || b == b'\t')
                            .count();
                        self.advance(run);
                    }
                    return Ok(Some(Tok::Newline));
                }
                Dispatch::Comment(n) => {
                    self.advance(n);
                    continue;
                }
                Dispatch::Directive(name, n) => {
                    self.advance(n);
                    return Ok(Some(match name {
                        "ignore" => Tok::Ignore,
                        "import" => Tok::Import,
                        "declare" => Tok::Declare,
                        "override" => Tok::Override,
                        "extend" => Tok::Extend,
                        _ => unreachable!(),
                    }));
                }
                Dispatch::Arrow => {
                    self.advance(2);
                    return Ok(Some(Tok::Arrow));
                }
                Dispatch::DotDot => {
                    self.advance(2);
                    return Ok(Some(Tok::DotDot));
                }
                Dispatch::Dot => {
                    self.advance(1);
                    return Ok(Some(Tok::Dot));
                }
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
                        _ => {
                            return Err(GrammarError::SyntaxError {
                                line: self.line,
                                col: self.col,
                                msg: format!("Unexpected character: {:?}", ch),
                            })
                        }
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
                        _ => {
                            return Err(GrammarError::SyntaxError {
                                line: self.line,
                                col: self.col,
                                msg: format!("Unexpected character: {:?}", ch),
                            })
                        }
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
                        && !src[i + 1..].starts_with(|c: char| c.is_alphanumeric() || c == '_');
                    if ci {
                        i += 1;
                    }
                    let raw = &src[1..i - if ci { 2 } else { 1 }];
                    let value = unescape_string(raw);
                    self.advance(i);
                    return Ok(Some(Tok::String(value, ci)));
                }
                _ => i += 1,
            }
        }
        Err(GrammarError::SyntaxError {
            line: self.line,
            col: self.col,
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
            line: self.line,
            col: self.col,
            msg: "Unterminated regex literal".to_string(),
        })
    }

    fn try_lex_number(&mut self) -> Option<Tok> {
        let rest = self.rest();
        let sign = if rest.starts_with('-') || rest.starts_with('+') {
            1
        } else {
            0
        };
        let after_sign = &rest[sign..];
        if !after_sign.starts_with(|c: char| c.is_ascii_digit()) {
            return None;
        }
        let len = sign
            + after_sign
                .bytes()
                .take_while(|b| b.is_ascii_digit())
                .count();
        let digits = &rest[..len];
        // Python Lark priorities are arbitrary-precision ints; we store i32 and
        // saturate, so a huge (negative) priority like `A.-99999999999999999999999`
        // clamps to the extreme rather than failing to lex.
        let n: i32 = match digits.parse::<i128>() {
            Ok(v) => v.clamp(i32::MIN as i128, i32::MAX as i128) as i32,
            Err(_) => {
                if digits.starts_with('-') {
                    i32::MIN
                } else {
                    i32::MAX
                }
            }
        };
        self.advance(len);
        Some(Tok::Number(n))
    }

    fn lex_rule(&mut self) -> Result<Option<Tok>, GrammarError> {
        let rest = self.rest();
        let len = rest
            .bytes()
            .take_while(|&b| {
                b.is_ascii_lowercase() || b.is_ascii_uppercase() || b == b'_' || b.is_ascii_digit()
            })
            .count();
        let name = rest[..len].to_string();
        self.advance(len);
        Ok(Some(Tok::Rule(name)))
    }

    fn lex_terminal(&mut self) -> Result<Option<Tok>, GrammarError> {
        let rest = self.rest();
        let len = rest
            .bytes()
            .take_while(|&b| b.is_ascii_uppercase() || b == b'_' || b.is_ascii_digit())
            .count();
        let name = rest[..len].to_string();
        self.advance(len);
        Ok(Some(Tok::Terminal(name)))
    }
}

/// Decode escape sequences in a string literal, mirroring Python Lark's
/// `eval_escaping` (which defers to `ast.literal_eval`). The numeric escapes
/// `\xHH`, `\uHHHH`, and `\UHHHHHHHH` decode to the corresponding `char`;
/// `\n \t \r \f \v \0` map to their control characters; `\\ \" \'` are literal.
/// An unrecognized escape (e.g. `\w`, `\d`) keeps its backslash so regex
/// metacharacters embedded in a string survive — matching Lark, which prepends a
/// backslash for any escape outside `Uuxnftr`.
fn unescape_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('f') => out.push('\u{0C}'),
            Some('v') => out.push('\u{0B}'),
            Some('0') => out.push('\0'),
            Some('\\') => out.push('\\'),
            Some('"') => out.push('"'),
            Some('\'') => out.push('\''),
            Some('x') => push_hex_escape(&mut out, &mut chars, 2, "\\x"),
            Some('u') => push_hex_escape(&mut out, &mut chars, 4, "\\u"),
            Some('U') => push_hex_escape(&mut out, &mut chars, 8, "\\U"),
            // Unknown escape: keep the backslash (regex escapes like `\w`, `\d`).
            Some(c) => {
                out.push('\\');
                out.push(c);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// Consume exactly `n` hex digits and push the decoded `char`. If the digits are
/// missing or do not form a valid scalar value, emit the escape verbatim so a
/// malformed escape never silently changes meaning.
fn push_hex_escape(
    out: &mut String,
    chars: &mut std::iter::Peekable<std::str::Chars>,
    n: usize,
    prefix: &str,
) {
    let mut hex = String::with_capacity(n);
    for _ in 0..n {
        match chars.peek() {
            Some(c) if c.is_ascii_hexdigit() => {
                hex.push(*c);
                chars.next();
            }
            _ => break,
        }
    }
    match (hex.len() == n)
        .then(|| u32::from_str_radix(&hex, 16).ok())
        .flatten()
        .and_then(char::from_u32)
    {
        Some(ch) => out.push(ch),
        None => {
            out.push_str(prefix);
            out.push_str(&hex);
        }
    }
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
    Repeat {
        inner: Box<Expr>,
        min: usize,
        max: Option<usize>,
    },
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
    Str(String, bool), // value, case-insensitive
    Re(String, u32),   // pattern, flags
}

#[derive(Debug, Clone)]
struct RawTerm {
    name: String,
    priority: i32,
    expansions: Vec<AliasedExpansion>,
}

#[derive(Debug, Clone)]
struct ImportSpec {
    path: Vec<String>, // e.g. ["common"] or [".", "mylib"]
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

        Ok(RawTerm {
            name,
            priority,
            expansions,
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

// ─── Grammar Compiler: AST → BNF ─────────────────────────────────────────────

/// The flavour of anonymous EBNF helper a structural cache key describes. Two
/// helpers share a generated rule only when they agree on *both* their kind and
/// their compiled alternatives, so a `(",", X)` group never collapses into a
/// `(",", X)?` optional even though their alternatives coincide.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum HelperKind {
    /// `(...)` — a plain spliced group.
    Group,
    /// `(...)?` / a placeholder-less `[...]` — a group plus an empty alternative.
    GroupOptional,
    /// `[...]` under `maybe_placeholders` — empty case emits `None` placeholders.
    Maybe,
    /// `x?` — the single-symbol optional wrapper (`P: x | ε`).
    Opt,
    /// `x*` — the nullable wrapper around the shared `+`-recurse helper.
    Star,
}

/// One alternative of a compiled expansion: its symbol sequence plus the
/// per-gap `None`-placeholder counts a distributed absent `[...]` left behind
/// (`gaps[i]` Nones go before symbol `i`; `gaps[len]` trail). The gap vector is
/// always `syms.len() + 1` long during compilation; it is stored on the rule
/// only when some entry is nonzero.
type CompiledAlt = (Vec<Symbol>, Vec<usize>);

/// One compiled position of an expansion (see `compile_slot`): either a fixed
/// symbol sequence, or a distributable leading nullable contributing several
/// present-form alternatives that fan out across the parent's alternatives.
enum Slot {
    /// Contributes this exact symbol sequence at its position (usually one
    /// symbol). Covers every non-distributed position, including a *trailing*
    /// nullable's shared `__anon_*` helper.
    Fixed(Vec<Symbol>),
    /// A leading nullable distributed into the parent: these are the non-empty
    /// ("present") alternatives; the absent alternative is added during the
    /// cartesian product in `compile_expansion`, contributing `absent_nones`
    /// `None` placeholders (nonzero only for a `maybe_placeholders` `[...]`,
    /// mirroring Python Lark's `_EMPTY` markers → `empty_indices`).
    Nullable {
        present: Vec<CompiledAlt>,
        absent_nones: usize,
    },
    /// A plain `(a|b)` group distributed into the parent: one alternative per
    /// arm, with **no** absent/ε arm (the group is not nullable). Python Lark
    /// never materializes a helper rule for an inline group —
    /// `SimplifyRule_Visitor.expansion` cartesian-products it into the parent
    /// at *every* position — and the helper form is not behaviour-preserving
    /// under LALR: a helper arm that duplicates another rule's RHS (e.g.
    /// `(atom_expr | list)` next to `?atom: ... | list`) makes two unit rules
    /// over one symbol, which collide as an unresolvable reduce/reduce where
    /// Python sees only a silently-resolved shift/reduce (wild bank: vyper).
    Choices(Vec<CompiledAlt>),
}

/// Structural identity of an anonymous EBNF helper: its kind, the enclosing
/// `keep_all_tokens` context, and the ordered, compiled `(symbols, gaps, alias)`
/// of each alternative. Identical keys reuse one generated rule — Python Lark's
/// `rules_cache`. Caching the *compiled* symbols (not the AST) means the sharing
/// composes bottom-up: a repeated `(",", X)*` shares its inner group, which lets
/// its `+`-recurse helper and `*` wrapper share in turn, collapsing what would
/// otherwise be duplicate nullable helpers that LALR cannot disambiguate.
type HelperKey = (HelperKind, bool, Vec<(CompiledAlt, Option<String>)>);

/// Converts the parsed AST into flat BNF rules and terminal definitions.
struct GrammarCompiler {
    start: Vec<String>,
    rules: Vec<Rule>,
    terminals: Vec<TerminalDef>,
    /// Raw terminal definitions, collected before any are compiled so a terminal
    /// body may reference another terminal defined later (`C: "C" | D`).
    raw_terms: Vec<RawTerm>,
    ignore_patterns: Vec<Pattern>,
    /// Counter for generating unique anonymous rule names.
    anon_counter: usize,
    /// Counter for generating unique terminal names for literals.
    term_counter: usize,
    /// Cache: literal string/regex → auto-generated terminal name.
    literal_cache: HashMap<String, String>,
    /// Template definitions: name → (params, expansions, modifiers, priority).
    /// The modifiers (`!` keep-all, `?` expand1) and priority are kept so each
    /// instantiation inherits the template's rule options, exactly as Python Lark
    /// deep-copies the template's `RuleOptions` onto every instance.
    templates: HashMap<String, (Vec<String>, Vec<AliasedExpansion>, String, i32)>,
    /// Memo of template instantiations: canonical `name<args>` key → instance rule
    /// name. Lets a self-recursive template (`_sep{x,d}: x | _sep{x,d} d x`) resolve
    /// its own reference to the rule already being built instead of recursing
    /// forever (mirrors Python Lark, which memoizes instantiations).
    template_instances: HashMap<String, String>,
    /// Whether absent `[...]` groups emit `None` placeholders (Lark parity).
    maybe_placeholders: bool,
    /// The grammar-wide `keep_all_tokens` option: when set, every rule keeps its
    /// tokens, exactly as if each carried the `!` modifier.
    global_keep_all: bool,
    /// `keep_all_tokens` of the rule currently being compiled — needed to count
    /// kept symbols for placeholder generation.
    current_keep_all: bool,
    /// Inlined "rule size" of each anonymous EBNF helper (maybe / optional /
    /// group), mirroring Python Lark's `FindRuleSize`. An absent `[...]` emits one
    /// `None` per unit of this size, and a *nested* maybe/group inside a `[...]`
    /// must contribute its own size (not 0) so placeholders compose recursively.
    /// `*` / `+` / `~` helpers and transparent `_rules` are deliberately absent
    /// (size 0), exactly as Lark treats `_`-prefixed symbols as removed.
    helper_sizes: HashMap<String, usize>,
    /// Cache of the shared `+`-recurse helper (`P: inner | P inner`) keyed by its
    /// inner symbol and the keep-all context. Identical `x+`/`x*` occurrences reuse
    /// one rule — Python Lark's `rules_cache`. This sharing is what keeps grammars
    /// like `a+ b | a+` and `a* b | a+` LALR-parseable: with separate recurse rules
    /// the duplicated `… -> "a"` reductions are an unresolvable reduce/reduce.
    recurse_cache: HashMap<(Symbol, bool), String>,
    /// Cache of every other anonymous EBNF helper — groups, optionals, `?`/`*`
    /// wrappers — keyed by its [`HelperKey`] structural identity. Extends the
    /// single-symbol `recurse_cache` sharing to grouped repetition: Python Lark's
    /// `rules_cache`. Without it, each `(",", X)*` occurrence gets a fresh helper,
    /// so structurally-identical nullable rules collide as unresolvable
    /// reduce/reduce (e.g. `python.lark`'s many `(",", param)*` patterns).
    helper_cache: HashMap<HelperKey, String>,
    /// Anon helper rules that already derive ε (the `?`/`*` helpers). A `?` applied
    /// to one of these is redundant — `(X?)?` is just `X?` — so it is collapsed
    /// rather than stacked, which is what Python Lark's distribute+dedup achieves
    /// and what keeps `("A"?)?` from building two ambiguous empty rules.
    nullable_opts: std::collections::HashSet<String>,
    /// Directory that relative file imports resolve against (the importing
    /// grammar's directory). `None` when the grammar was built from a string with
    /// no source location, in which case only `%import common.*` resolves.
    base_path: Option<PathBuf>,
}

impl GrammarCompiler {
    fn new(
        start: Vec<String>,
        maybe_placeholders: bool,
        keep_all_tokens: bool,
        base_path: Option<PathBuf>,
    ) -> Self {
        GrammarCompiler {
            start,
            rules: Vec::new(),
            terminals: Vec::new(),
            raw_terms: Vec::new(),
            ignore_patterns: Vec::new(),
            anon_counter: 0,
            term_counter: 0,
            literal_cache: HashMap::new(),
            templates: HashMap::new(),
            template_instances: HashMap::new(),
            maybe_placeholders,
            global_keep_all: keep_all_tokens,
            current_keep_all: keep_all_tokens,
            helper_sizes: HashMap::new(),
            recurse_cache: HashMap::new(),
            helper_cache: HashMap::new(),
            nullable_opts: std::collections::HashSet::new(),
            base_path,
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
        RuleOptions {
            keep_all_tokens: self.current_keep_all,
            ..RuleOptions::default()
        }
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
                    self.templates.insert(
                        r.name.clone(),
                        (
                            r.params.clone(),
                            r.expansions.clone(),
                            r.modifiers.clone(),
                            r.priority,
                        ),
                    );
                }
            }
        }

        // Staged compilation. Terminals are resolved as a whole *before* rule bodies
        // so that (a) a string literal in a rule can unify with an already-known
        // terminal and (b) a terminal body may reference any other terminal,
        // regardless of definition order. Imports/declares run first so terminal
        // bodies can reference imported terminals.
        let mut rule_items = Vec::new();
        let mut ignore_items = Vec::new();
        for item in items {
            match item {
                Item::ImportItem(spec) => self.resolve_import(spec)?,
                Item::DeclareItem(syms) => self.declare_terminals(syms),
                Item::TermItem(t) => self.raw_terms.push(t),
                Item::RuleItem(r) if !r.params.is_empty() => { /* template — used on demand */ }
                Item::RuleItem(r) => rule_items.push(r),
                Item::IgnoreItem(expansions) => ignore_items.push(expansions),
            }
        }

        // Resolve all terminals (inlining terminal-to-terminal references).
        self.resolve_terminals()?;

        // Rule bodies, then `%ignore` expansions (which may reference terminals).
        for r in rule_items {
            self.compile_rule(r)?;
        }
        for expansions in ignore_items {
            for expansion in expansions {
                let pat = self.expansion_to_pattern(&expansion)?;
                self.ignore_patterns.push(pat);
            }
        }
        Ok(())
    }

    fn compile_rule(&mut self, raw: RawRule) -> Result<(), GrammarError> {
        let keep_all = raw.modifiers.contains('!') || self.global_keep_all;
        let expand1 = raw.modifiers.contains('?');
        let origin = NonTerminal::new(&raw.name);
        // Make keep_all visible to placeholder counting while this rule's body
        // (and the anonymous rules it expands into) is compiled.
        self.current_keep_all = keep_all;

        // Each source alternative may distribute into several BNF alternatives
        // (a leading nullable fanned out), so `order` runs over the flattened
        // result rather than the raw alternatives — after the cross-alternative
        // dedup + collision check (Python numbers post-dedup too).
        let mut compiled: Vec<(CompiledAlt, Option<String>)> = Vec::new();
        for alt in raw.expansions.into_iter() {
            let alias = alt.alias.clone();
            for alt_c in self.compile_expansion(alt.expansion, &origin.name, true)? {
                compiled.push((alt_c, alias.clone()));
            }
        }
        let compiled = Self::dedup_and_check_alts(&origin.name, compiled)?;
        for (order, ((expansion_syms, gaps), alias)) in compiled.into_iter().enumerate() {
            let options = RuleOptions {
                expand1,
                keep_all_tokens: keep_all,
                priority: raw.priority,
                nones_before: Self::stored_gaps(gaps),
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

    /// Python Lark's two-stage duplicate handling for one origin's compiled
    /// alternatives (`load_grammar.py`). Stage 1, `SimplifyRule_Visitor.expansions`:
    /// alternatives that are identical *trees* — here, identical
    /// `(symbols, gaps, alias)`, since `_EMPTY` markers and alias nodes are part of
    /// Python's tree — are silently deduped, so `a: X | X` and the coinciding
    /// absent arms of `a: [A] C | [B] C` collapse instead of colliding as
    /// reduce/reduce under LALR. Stage 2, the final `Rule` compile: surviving
    /// duplicates of `(origin, expansion)` — `Rule.__eq__` ignores alias and
    /// options — raise "Rules defined twice", which is how a colliding expansion
    /// of optionals (`a: [A] [A] B`, whose two `A B` arms differ only in
    /// placeholder positions) or a same-expansion alias pair (`a: X -> p | X -> q`)
    /// is rejected *at load*, on every parser backend, instead of surfacing as an
    /// LALR-only conflict or being silently resolved by Earley. Duplicate *empty*
    /// expansions are tolerated, as in Python.
    fn dedup_and_check_alts(
        origin: &str,
        alts: Vec<(CompiledAlt, Option<String>)>,
    ) -> Result<Vec<(CompiledAlt, Option<String>)>, GrammarError> {
        let mut seen: HashSet<(CompiledAlt, Option<String>)> = HashSet::new();
        let mut out: Vec<(CompiledAlt, Option<String>)> = Vec::with_capacity(alts.len());
        let mut seen_syms: HashSet<Vec<Symbol>> = HashSet::new();
        for alt in alts {
            if !seen.insert(alt.clone()) {
                continue; // exact duplicate — Python's AST-level dedup_list
            }
            let syms = &alt.0 .0;
            if !syms.is_empty() && !seen_syms.insert(syms.clone()) {
                let rhs: Vec<&str> = syms.iter().map(|s| s.name()).collect();
                return Err(GrammarError::Other {
                    msg: format!(
                        "Rules defined twice: {origin} -> {} \
                         (Might happen due to colliding expansion of optionals: [] or ?)",
                        rhs.join(" ")
                    ),
                });
            }
            out.push(alt);
        }
        Ok(out)
    }

    /// Gap vectors are stored on the rule only when they carry placeholders;
    /// the all-zero common case stays an empty `Vec` so ordinary rules pay
    /// nothing.
    fn stored_gaps(gaps: Vec<usize>) -> Vec<usize> {
        if gaps.iter().any(|&g| g > 0) {
            gaps
        } else {
            Vec::new()
        }
    }

    /// Compile a list of `Expr` nodes into one or more alternative symbol
    /// sequences, creating auxiliary rules as needed for EBNF operators.
    ///
    /// A single source expansion can lower to **several** BNF alternatives:
    /// a *leading nullable* EBNF helper (`X?`, `X*`, or `[X]`) that is not the
    /// last symbol of the expansion is **distributed** into the parent's
    /// alternatives — `a: X? Y` becomes `a: X Y | Y` — exactly as Python Lark's
    /// `SimplifyRule_Visitor` does. This is required for correctness: a named
    /// nullable helper before further symbols hides those symbols from the
    /// textbook LR(0) closure (the dot never advances past the helper until it
    /// ε-reduces), so the LALR automaton mispredicts and a shift/reduce conflict
    /// against the hidden path silently drops it (#97). Under
    /// `maybe_placeholders`, a distributed `[X]`'s absent alternative records
    /// its `None` placeholders positionally on the rule
    /// (`RuleOptions::nones_before`, Python's `_EMPTY` markers →
    /// `empty_indices`; #106). A *trailing* nullable causes no such hiding, so
    /// it keeps its shared helper (the lower-churn variant of the fix — Python
    /// distributes those too, but the helper form is conflict-free and
    /// byte-identical in the tree).
    ///
    /// `tail_ctx` is whether this expansion's *own* last position is genuinely
    /// final in the rule it will land in. It is `false` when compiling the
    /// present forms of a nullable being distributed (`distributable_alternatives`):
    /// those symbols are spliced inline into the parent's alternatives mid-rule,
    /// so a "trailing" nullable inside them is not actually trailing — left as a
    /// helper it would re-create the LR(0) dot-hiding this distribution exists to
    /// remove (e.g. `python.lark`'s `["," SLASH ("," paramvalue)*]`, whose inner
    /// `*` lands before the `["," [starparams|kwparams]]` branch).
    fn compile_expansion(
        &mut self,
        exprs: Vec<Expr>,
        parent: &str,
        tail_ctx: bool,
    ) -> Result<Vec<CompiledAlt>, GrammarError> {
        let n = exprs.len();
        // Cartesian product of each position's choices, building present-form
        // alternatives before the empty one (Python's distribution order). Each
        // accumulated alternative carries its gap vector (`gaps.len() == syms.len()
        // + 1`), threading distributed-absent `None` placeholders positionally.
        let mut acc: Vec<CompiledAlt> = vec![(Vec::new(), vec![0])];
        for (i, expr) in exprs.into_iter().enumerate() {
            let is_last = (i + 1 == n) && tail_ctx;
            let choices: Vec<CompiledAlt> = match self.compile_slot(expr, parent, is_last)? {
                Slot::Fixed(syms) => {
                    let gaps = vec![0; syms.len() + 1];
                    vec![(syms, gaps)]
                }
                Slot::Nullable {
                    mut present,
                    absent_nones,
                } => {
                    // present-forms first, then the absent alternative (which
                    // contributes only its placeholder count).
                    present.push((Vec::new(), vec![absent_nones]));
                    present
                }
                // A distributed plain group: its arms fan out as-is, no ε arm.
                Slot::Choices(arms) => arms,
            };
            let mut next = Vec::with_capacity(acc.len() * choices.len());
            for (psyms, pgaps) in &acc {
                for (csyms, cgaps) in &choices {
                    let mut syms = psyms.clone();
                    syms.extend_from_slice(csyms);
                    // Merge gap vectors: the seam gap is the sum of the prefix's
                    // trailing gap and the choice's leading gap.
                    let mut gaps = pgaps[..pgaps.len() - 1].to_vec();
                    gaps.push(pgaps[pgaps.len() - 1] + cgaps[0]);
                    gaps.extend_from_slice(&cgaps[1..]);
                    next.push((syms, gaps));
                }
            }
            acc = next;
        }
        // Distributing two optionals can coincide (`X? X?` → `X X | X | X | ε`);
        // identical alternatives would reduce/reduce on the same item, so keep the
        // first occurrence of each (Python's grammar dedups identical rules too).
        let mut seen = std::collections::HashSet::new();
        acc.retain(|a| seen.insert(a.clone()));
        Ok(acc)
    }

    /// Compile one position of an expansion into either a single fixed symbol
    /// sequence (the common case) or, for a distributable leading nullable, the
    /// set of present-form alternatives to fan out across the parent (see
    /// [`compile_expansion`]). `is_last` suppresses distribution for a *trailing*
    /// nullable, which keeps its shared `__anon_*` helper.
    fn compile_slot(
        &mut self,
        expr: Expr,
        parent: &str,
        is_last: bool,
    ) -> Result<Slot, GrammarError> {
        // A plain `(a|b)` group distributes into the parent at *every* position
        // (Python never gives an inline group a helper rule — see
        // `Slot::Choices`) unless it carries an alias (an alias names a subtree
        // that inline distribution would lose, so those fall back to the helper
        // form).
        if let Expr::Group(alts) = &expr {
            if !Self::expr_contains_alias(&expr) {
                if let Some(arms) = self.distributable_alternatives(alts.clone(), parent)? {
                    return Ok(Slot::Choices(arms));
                }
            }
        }
        // Only a *leading* (non-final) nullable distributes, and only when it
        // carries no alias. `try_distribute` never compiles anything on its
        // `None` path, so the fall-through `compile_expr` below compiles the
        // position exactly once.
        if !is_last && !Self::expr_contains_alias(&expr) {
            if let Some(slot) = self.try_distribute(&expr, parent)? {
                return Ok(slot);
            }
        }
        Ok(Slot::Fixed(vec![self.compile_expr(expr, parent)?]))
    }

    /// If `expr` is a distributable leading nullable (`X?`, `X*`, or a `[X]`),
    /// return its distribution slot (present-form alternatives + the absent
    /// case's `None` count); otherwise `None`. The `None` paths bail *before*
    /// compiling anything, so the caller may compile the expr afresh without
    /// emitting duplicate helper rules.
    fn try_distribute(&mut self, expr: &Expr, parent: &str) -> Result<Option<Slot>, GrammarError> {
        match expr {
            // `X?` / `(...)?` → present forms of the inner.
            Expr::Repeat {
                inner,
                min: 0,
                max: Some(1),
            } => Ok(self
                .present_forms((**inner).clone(), parent)?
                .map(|present| Slot::Nullable {
                    present,
                    absent_nones: 0,
                })),
            // `X*` → the shared one-or-more recurse helper.
            Expr::Repeat {
                inner,
                min: 0,
                max: None,
            } => {
                let inner_sym = self.compile_expr((**inner).clone(), parent)?;
                let plus = self.plus_helper(inner_sym);
                Ok(Some(Slot::Nullable {
                    present: vec![(vec![plus], vec![0, 0])],
                    absent_nones: 0,
                }))
            }
            // `[X]`: distributed like Python's `maybe()` → `expansions(X, _EMPTY*n)`.
            // Under `maybe_placeholders` the absent alternative contributes the
            // widest present form's kept-slot count as positional `None`
            // placeholders (Python's `_EMPTY` markers → `empty_indices`); without
            // placeholders it contributes nothing.
            Expr::Maybe(alts) => {
                let present = match self.distributable_alternatives(alts.clone(), parent)? {
                    Some(p) => p,
                    None => return Ok(None),
                };
                let absent_nones = if self.maybe_placeholders {
                    // A present alternative's size is its kept symbols plus any
                    // `None`s its own nested absent maybes left inline, so sizes
                    // compose through nesting exactly as Lark's `FindRuleSize`.
                    present
                        .iter()
                        .map(|(syms, gaps)| {
                            syms.iter().map(|s| self.symbol_size(s)).sum::<usize>()
                                + gaps.iter().sum::<usize>()
                        })
                        .max()
                        .unwrap_or(0)
                } else {
                    0
                };
                Ok(Some(Slot::Nullable {
                    present,
                    absent_nones,
                }))
            }
            _ => Ok(None),
        }
    }

    /// The non-empty ("present") derivations of an expr, used when distributing a
    /// leading nullable. Returns `None` when the expr cannot be safely distributed
    /// — a `maybe_placeholders` `[X]` *nested under another nullable wrapper*
    /// (e.g. `([X])?`), whose absent-with-placeholders middle alternative this
    /// present/absent split cannot represent — so the caller keeps the helper.
    /// (A `[X]` standing directly at a rule position distributes via
    /// `try_distribute`'s own `Maybe` arm, placeholders and all.)
    fn present_forms(
        &mut self,
        expr: Expr,
        parent: &str,
    ) -> Result<Option<Vec<CompiledAlt>>, GrammarError> {
        let single = |sym: Symbol| Some(vec![(vec![sym], vec![0, 0])]);
        match expr {
            Expr::Value(v) => Ok(single(self.compile_value(v, parent)?)),
            Expr::Group(alts) => self.distributable_alternatives(alts, parent),
            // `[X]` without placeholders is a plain optional group; with
            // placeholders this nested position cannot carry the absent case's
            // `None`s (see the doc comment), so keep the helper.
            Expr::Maybe(_) if self.maybe_placeholders => Ok(None),
            Expr::Maybe(alts) => self.distributable_alternatives(alts, parent),
            // A nested `?` collapses: `(X?)?` ≡ `X?`, so drop the inner optionality
            // and let the outer distribution re-add the single ε.
            Expr::Repeat {
                inner,
                min: 0,
                max: Some(1),
            } => self.present_forms(*inner, parent),
            // `X*` / `X+` present form is the shared one-or-more recurse helper.
            Expr::Repeat {
                inner,
                min: 0,
                max: None,
            }
            | Expr::Repeat {
                inner,
                min: 1,
                max: None,
            } => {
                let inner_sym = self.compile_expr(*inner, parent)?;
                let plus = self.plus_helper(inner_sym);
                Ok(single(plus))
            }
            // Exact / bounded repetition: a single helper symbol.
            other => Ok(single(self.compile_expr(other, parent)?)),
        }
    }

    /// Lower each alternative of a group/`[...]` into distributed present-form
    /// sequences, flattened into one alternative list. Returns `None` if any
    /// alternative carries an alias (inline distribution would lose the named
    /// subtree), so the caller falls back to the helper form.
    fn distributable_alternatives(
        &mut self,
        alts: Vec<AliasedExpansion>,
        parent: &str,
    ) -> Result<Option<Vec<CompiledAlt>>, GrammarError> {
        if alts.iter().any(|a| a.alias.is_some()) {
            return Ok(None);
        }
        let mut out = Vec::new();
        for alt in alts {
            // `tail_ctx: false` — these symbols are spliced mid-rule into the
            // parent (the distributed nullable is never final), so a trailing
            // nullable here is not actually trailing and must distribute too.
            let subs = self.compile_expansion(alt.expansion, parent, false)?;
            out.extend(subs);
        }
        Ok(Some(out))
    }

    /// Whether an expr (recursively) carries a `->` alias on any of its grouped
    /// alternatives. A distributable nullable wrapping an alias is kept as a
    /// helper instead, so the alias's named subtree survives.
    fn expr_contains_alias(expr: &Expr) -> bool {
        match expr {
            Expr::Value(_) => false,
            Expr::Repeat { inner, .. } => Self::expr_contains_alias(inner),
            Expr::Group(alts) | Expr::Maybe(alts) => alts
                .iter()
                .any(|a| a.alias.is_some() || a.expansion.iter().any(Self::expr_contains_alias)),
        }
    }

    fn compile_expr(&mut self, expr: Expr, parent: &str) -> Result<Symbol, GrammarError> {
        match expr {
            Expr::Value(v) => self.compile_value(v, parent),
            Expr::Group(alts) => self.compile_group(alts, parent, false),
            Expr::Maybe(alts) => self.compile_maybe(alts, parent),
            Expr::Repeat { inner, min, max } => self.compile_repeat(*inner, min, max, parent),
        }
    }

    fn compile_value(&mut self, v: Value, parent: &str) -> Result<Symbol, GrammarError> {
        match v {
            // A named terminal reference is filtered iff `_`-prefixed (Lark's
            // `Terminal(s, filter_out=s.startswith('_'))`).
            Value::Terminal(name) => {
                let filter_out = name.starts_with('_');
                Ok(Symbol::Terminal(Terminal { name, filter_out }))
            }
            Value::Rule(name) => Ok(Symbol::NonTerminal(NonTerminal::new(name))),
            Value::Literal(lit) => {
                // An anonymous *string* literal is filtered out of the tree
                // (keyword-like punctuation); an anonymous *regex* literal is kept,
                // matching Python Lark. This is a property of the *occurrence*, not
                // the terminal — the same terminal may be kept elsewhere.
                let filter_out = matches!(lit, LiteralVal::Str(..));
                let term_name = self.get_or_create_terminal(lit)?;
                Ok(Symbol::Terminal(Terminal {
                    name: term_name,
                    filter_out,
                }))
            }
            Value::Range(from, to) => {
                let pat_str = format!("[{}-{}]", regex::escape(&from), regex::escape(&to));
                let pat = Pattern::Re(PatternRe::new(&pat_str, 0)?);
                // A char-range terminal is a regex literal — kept, like `/[a-z]/`.
                let name = self.intern_anon_pattern(pat, None, false);
                Ok(Symbol::Terminal(Terminal {
                    name,
                    filter_out: false,
                }))
            }
            Value::TemplateUsage { name, args } => self.instantiate_template(&name, args, parent),
        }
    }

    fn get_or_create_terminal(&mut self, lit: LiteralVal) -> Result<String, GrammarError> {
        let key = format!("{:?}", lit);
        if let Some(name) = self.literal_cache.get(&key) {
            return Ok(name.clone());
        }
        // `string_type` mirrors Python's `pattern.type`: a string literal is a
        // `PatternStr` even when case-insensitive (only the flag is attached), while
        // a `/regex/` literal is a `PatternRE`. It gates the strict-mode collision
        // check (issue #35).
        let (pat, name_hint, string_type) = match &lit {
            LiteralVal::Str(s, ci) => {
                // Case-insensitive literals stay `PatternStr` (Python attaches
                // the flag without changing the pattern type), so they keep
                // string-pattern ordering and join `unless` keyword retyping.
                let pat = if *ci {
                    Pattern::Str(PatternStr::new_ci(s.as_str()))
                } else {
                    Pattern::Str(PatternStr::new(s.as_str()))
                };
                // Try to create a human-readable name from the string content
                let hint = terminal_name_hint(s);
                (pat, hint, true)
            }
            LiteralVal::Re(pattern, flags) => {
                let pat = Pattern::Re(PatternRe::new(pattern.as_str(), *flags)?);
                (pat, None, false)
            }
        };
        let name = self.intern_anon_pattern(pat, name_hint, string_type);
        self.literal_cache.insert(key, name.clone());
        Ok(name)
    }

    /// Intern an anonymous literal/range pattern, returning the terminal name to
    /// reference it by. Unifies with an existing same-pattern terminal — named or
    /// anonymous — by adopting its name, exactly as Python Lark's
    /// `PrepareAnonTerminals` reuses the user terminal's name (so `"a"` lexes as
    /// `A` when `A: "a"` exists, and an inline `/a/` reuses `A` from `A: /a/`).
    /// Filtering is *not* keyed on this terminal — each occurrence carries its own
    /// `filter_out` — so unifying for lexing never changes a token's keep/drop fate.
    fn intern_anon_pattern(
        &mut self,
        pat: Pattern,
        name_hint: Option<String>,
        string_type: bool,
    ) -> String {
        if let Some(existing) = self
            .terminals
            .iter()
            .find(|td| patterns_equivalent(&td.pattern, &pat))
        {
            return existing.name.clone();
        }
        // Use the clean hint when it is a fresh, valid identifier; otherwise fall
        // back to a generated `__ANON_N` name (always a valid regex group name).
        let name = match name_hint {
            Some(h) if !self.terminals.iter().any(|t| t.name == h) => h,
            _ => self.fresh_terminal(),
        };
        self.terminals
            .push(TerminalDef::new(&name, pat, 0).with_string_type(string_type));
        name
    }

    fn compile_group(
        &mut self,
        alts: Vec<AliasedExpansion>,
        parent: &str,
        optional: bool,
    ) -> Result<Symbol, GrammarError> {
        // Lower every alternative up front so the structural cache key is built
        // from the compiled symbols, then share or emit one helper for it. A
        // single source alternative may itself distribute into several (a leading
        // nullable fanned out), so each contributes one *or more* compiled
        // alternatives. (The `parent` name is inert below the top level — only
        // template usage reads it, and that path ignores it — so lowering before
        // the helper is named is behaviourally identical to the old numbering.)
        let mut compiled: Vec<(CompiledAlt, Option<String>)> = Vec::with_capacity(alts.len());
        for alt in alts {
            let alias = alt.alias.clone();
            for alt_c in self.compile_expansion(alt.expansion, parent, true)? {
                compiled.push((alt_c, alias.clone()));
            }
        }
        // Same dedup + collision check as a named rule's alternatives: Python
        // inlines groups into the parent, where its `expansions` dedup and
        // "Rules defined twice" check run — so `(X | X)` collapses (and then
        // takes the single-symbol shortcut below, like Python's inlined `X`),
        // and `([A] [A] B)` is rejected at load.
        let compiled = Self::dedup_and_check_alts(parent, compiled)?;
        // A plain single non-aliased alternative that compiles to exactly one
        // symbol *is* that symbol — skip the wrapper rule. Besides dropping a
        // redundant transparent node, this lets `(X)+` share `X`'s recurse helper
        // and stops `("A"?)?` from stacking a second nullable rule.
        if !optional
            && compiled.len() == 1
            && compiled[0].1.is_none()
            && compiled[0].0 .0.len() == 1
            && compiled[0].0 .1.iter().all(|&g| g == 0)
        {
            return Ok(compiled
                .into_iter()
                .next()
                .unwrap()
                .0
                 .0
                .into_iter()
                .next()
                .unwrap());
        }
        let kind = if optional {
            HelperKind::GroupOptional
        } else {
            HelperKind::Group
        };
        Ok(self.intern_helper(kind, compiled))
    }

    /// Share or emit the anonymous helper rule(s) for `kind` over its already
    /// lowered alternatives. On a structural cache hit the existing helper
    /// non-terminal is returned and nothing is emitted; otherwise a fresh
    /// `__anon_*` rule set is generated, its inlined size recorded (Lark's
    /// `FindRuleSize`), and the name cached under its [`HelperKey`]. This is the
    /// single choke point that extends Python Lark's `rules_cache` to every EBNF
    /// helper, so repeated `(",", X)*`-style patterns collapse to one rule
    /// instead of colliding as duplicate nullable helpers under LALR.
    fn intern_helper(
        &mut self,
        kind: HelperKind,
        alts: Vec<(CompiledAlt, Option<String>)>,
    ) -> Symbol {
        // What to share is anchored to Python Lark's `rules_cache`, but with one
        // structural caveat worth stating precisely. Python caches only the
        // *non-nullable* recurse core (`_c: _c c | c`, keyed on the inner
        // expression) — shared by both `+` and `*` — and has *no* nullable `*`
        // rule at all: `SimplifyRule_Visitor` distributes `c*`'s empty case into
        // each parent (`a: b c* d` → `_c: _c c | c` + `a: b _c d | b d`). lark-rs
        // instead lowers `x*` to a nullable wrapper `__star: __plus | ε` over that
        // same core, so what we cache is not a verbatim mirror of `rules_cache`:
        //
        //   * `Group` / `Star` — share. Sharing the `(",", X)` group lets the
        //     pre-existing `recurse_cache` share its `+`-recurse `__plus` in turn
        //     (keyed on that one inner symbol). That makes every `(",", X)*`
        //     wrapper *byte-identical* (`__plus | ε`), and two identical nullable
        //     wrappers collide as an unresolvable reduce/reduce the moment two of
        //     them reduce on the *same* lookahead in a common state (witnessed on
        //     `python.lark`: state 716, `__anon_star_102 -> ε` vs
        //     `__anon_star_106 -> ε` on COMMA). Sharing the wrapper *resolves* that
        //     collision by recognizing the two rules are one rule — it is forced by
        //     the shared core, not a free choice. It does not over-narrow: the
        //     collision is the proof the parser already cannot tell the wrappers
        //     apart (they merge via the shared `__plus`, exactly as Python's shared
        //     `_c` merges its parents' contexts), so unifying them widens no state's
        //     contextual scanner. Pinned against the oracle by
        //     `test_shared_star_wrapper_matches_oracle`: a grammar whose two
        //     `(NAME ";")*` wrappers *do* collide without sharing parses, rejects,
        //     and narrows byte-for-byte like Python Lark.
        //   * `Opt` / `Maybe` / `GroupOptional` — do *not* share. These are the
        //     `?`/`[...]` helpers Python inlines into parents. Unlike the `*`
        //     wrapper there is no pre-shared core forcing their states together, so
        //     sharing one *forces* a merge LALR would otherwise keep separate —
        //     unioning two parents' follow-sets into a contextual scanner that LALR
        //     never actually merges, silently widening it (it made `csv.lark`'s
        //     `header` start trying `row`'s terminals, picking the higher-priority
        //     `NON_SEPARATOR_STRING` over `WORD`). Leaving them per-parent keeps
        //     lark-rs byte-identical to the oracle, which never shares them either.
        //
        // #97 took the principled convergence *partway*: a *leading* (non-final)
        // `*`/`?`/`[...]` is now distributed into its parent's alternatives by
        // `compile_expansion`, exactly as Python's `SimplifyRule` does, so it never
        // reaches `intern_helper` as a `Star`/`Opt`/`GroupOptional` at all. What
        // still flows here is the *trailing* nullable — which causes no LR(0)
        // closure hiding and so keeps its shared helper (Python distributes those
        // too, but the helper form is conflict-free and tree-identical). The
        // forced-identical trailing `*` wrapper is still shared for the same R/R
        // reason above; the leading case that motivated the workaround is gone.
        let cacheable = matches!(kind, HelperKind::Group | HelperKind::Star);
        let key: HelperKey = (kind.clone(), self.current_keep_all, alts.clone());
        if cacheable {
            if let Some(name) = self.helper_cache.get(&key) {
                return Symbol::NonTerminal(NonTerminal::new(name));
            }
        }
        let tag = match kind {
            HelperKind::Group | HelperKind::GroupOptional => "group",
            HelperKind::Maybe => "maybe",
            HelperKind::Opt => "opt",
            HelperKind::Star => "star",
        };
        let name = self.fresh_anon_rule(tag);
        let origin = NonTerminal::new(&name);
        let mut max_size = 0;
        for (order, ((syms, gaps), alias)) in alts.iter().enumerate() {
            // An alternative's inlined size counts its kept symbols plus any
            // `None`s its distributed nested maybes left inline, so nested
            // placeholders compose (Lark's `FindRuleSize`).
            let size: usize = syms.iter().map(|s| self.symbol_size(s)).sum::<usize>()
                + gaps.iter().sum::<usize>();
            max_size = max_size.max(size);
            let options = RuleOptions {
                nones_before: Self::stored_gaps(gaps.clone()),
                ..self.anon_opts()
            };
            self.rules.push(Rule::new(
                origin.clone(),
                syms.clone(),
                alias.clone(),
                options,
                order,
            ));
        }
        // `*` helpers stay size 0 (transparent, inlined away) — `symbol_size` of
        // their lone `+`-recurse child is already 0, so recording `max_size` here
        // is a no-op for them and keeps the bookkeeping uniform.
        self.helper_sizes.insert(name.clone(), max_size);
        match kind {
            // `(...)` is spliced inline with no empty arm.
            HelperKind::Group => {}
            // A placeholder-less optional group: just an empty alternative.
            HelperKind::GroupOptional => {
                self.rules.push(Rule::new(
                    origin.clone(),
                    vec![],
                    None,
                    self.anon_opts(),
                    100,
                ));
            }
            // `[...]` under maybe_placeholders: the empty case emits one `None`
            // per kept slot of the widest alternative.
            HelperKind::Maybe => {
                let empty_opts = RuleOptions {
                    placeholder_count: max_size,
                    ..self.anon_opts()
                };
                self.rules
                    .push(Rule::new(origin.clone(), vec![], None, empty_opts, 100));
            }
            // `x?` / `x*`: a single-arm nullable wrapper `P: inner | ε`.
            HelperKind::Opt | HelperKind::Star => {
                self.nullable_opts.insert(name.clone());
                self.rules
                    .push(Rule::new(origin.clone(), vec![], None, self.anon_opts(), 1));
            }
        }
        if cacheable {
            self.helper_cache.insert(key, name);
        }
        Symbol::NonTerminal(origin)
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
        // A kept slot is a kept token *or* the inlined size of a nested maybe/group,
        // so nested optionals compose (Lark `FindRuleSize`); `intern_helper` records
        // the widest alternative's size and threads it into the empty production.
        let mut compiled: Vec<(CompiledAlt, Option<String>)> = Vec::with_capacity(alts.len());
        for alt in alts {
            let alias = alt.alias.clone();
            for alt_c in self.compile_expansion(alt.expansion, parent, true)? {
                compiled.push((alt_c, alias.clone()));
            }
        }
        // Same dedup + collision check as a named rule's alternatives (Python
        // distributes `[...]` into the parent, where they run; see
        // `dedup_and_check_alts`).
        let compiled = Self::dedup_and_check_alts(parent, compiled)?;
        Ok(self.intern_helper(HelperKind::Maybe, compiled))
    }

    /// Number of tree children a symbol contributes to an absent `[...]`'s `None`
    /// placeholder count — Python Lark's `FindRuleSize`. A kept token is 1, a
    /// filtered token 0; a named rule is 1, a transparent `_rule` / `*` / `+` / `~`
    /// helper is 0 (inlined-away, like Lark's `_`-prefixed symbols); a nested
    /// maybe / optional / group contributes its own recorded inlined size, so
    /// placeholders compose through arbitrary nesting.
    fn symbol_size(&self, s: &Symbol) -> usize {
        match s {
            Symbol::Terminal(t) => {
                if self.current_keep_all {
                    1
                } else if t.filter_out {
                    0
                } else {
                    1
                }
            }
            Symbol::NonTerminal(nt) => {
                if let Some(&size) = self.helper_sizes.get(&nt.name) {
                    size
                } else if nt.name.starts_with('_') {
                    0
                } else {
                    1
                }
            }
        }
    }

    /// The shared one-or-more recurse helper `P: inner | P inner` for `inner`,
    /// cached by `(inner, keep_all)` so identical `x+`/`x*` occurrences reuse one
    /// rule (Python Lark's `rules_cache`). Sharing collapses what would otherwise be
    /// duplicate, conflicting recurse rules into one, keeping `a+ b | a+` LALR.
    fn plus_helper(&mut self, inner_sym: Symbol) -> Symbol {
        let key = (inner_sym.clone(), self.current_keep_all);
        if let Some(name) = self.recurse_cache.get(&key) {
            return Symbol::NonTerminal(NonTerminal::new(name));
        }
        let name = self.fresh_anon_rule("plus");
        let nt = NonTerminal::new(&name);
        self.rules.push(Rule::new(
            nt.clone(),
            vec![inner_sym.clone()],
            None,
            self.anon_opts(),
            0,
        ));
        self.rules.push(Rule::new(
            nt.clone(),
            vec![Symbol::NonTerminal(nt.clone()), inner_sym],
            None,
            self.anon_opts(),
            1,
        ));
        self.recurse_cache.insert(key, name);
        Symbol::NonTerminal(nt)
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
                // inner? → optional rule. `?` adds no placeholders of its own, but
                // when nested inside a `[...]` it contributes its inner size to the
                // outer maybe's count (Lark's `FindRuleSize` takes the present arm).
                // If `inner` is *already* a nullable `?`/`*` helper, the extra `?` is
                // redundant — collapse it so `(X?)?` is just `X?`.
                if let Symbol::NonTerminal(nt) = &inner_sym {
                    if self.nullable_opts.contains(&nt.name) {
                        return Ok(inner_sym);
                    }
                }
                Ok(
                    self.intern_helper(
                        HelperKind::Opt,
                        vec![((vec![inner_sym], vec![0, 0]), None)],
                    ),
                )
            }
            (1, None) => {
                // inner+ → one-or-more, via the shared recurse helper.
                Ok(self.plus_helper(inner_sym))
            }
            (0, None) => {
                // inner* → optional wrapper around the *same* shared recurse helper,
                // so `x*` and `x+` reuse one `P: inner | P inner` rule (Lark's model).
                // The wrapper itself is shared too, so repeated `x*` collapse to one
                // nullable helper instead of colliding under LALR.
                let plus = self.plus_helper(inner_sym);
                Ok(self.intern_helper(HelperKind::Star, vec![((vec![plus], vec![0, 0]), None)]))
            }
            (n, Some(m)) if n == m => {
                // exact repetition: inline n copies
                let name = self.fresh_anon_rule("rep");
                let nt = NonTerminal::new(&name);
                let syms: Vec<Symbol> = std::iter::repeat(inner_sym).take(n).collect();
                self.rules
                    .push(Rule::new(nt.clone(), syms, None, self.anon_opts(), 0));
                Ok(Symbol::NonTerminal(nt))
            }
            (n, max_opt) => {
                // Range: generate rules for n..m repetitions
                let max_count = max_opt.unwrap_or(n + 10); // cap at n+10 for unbounded
                let name = self.fresh_anon_rule("rep_range");
                let nt = NonTerminal::new(&name);
                for count in n..=max_count {
                    let syms: Vec<Symbol> =
                        std::iter::repeat(inner_sym.clone()).take(count).collect();
                    self.rules
                        .push(Rule::new(nt.clone(), syms, None, self.anon_opts(), count));
                }
                Ok(Symbol::NonTerminal(nt))
            }
        }
    }

    /// Register each `%declare`d name as a pattern-less terminal. A declared
    /// terminal is never lexed — it is interned (so rules can reference it and the
    /// parse table reserves a column) and injected into the token stream by a
    /// postlex hook, e.g. an [`Indenter`](crate::postlex::Indenter)'s `_INDENT` /
    /// `_DEDENT`. Already-defined names are left untouched (an explicit definition
    /// or import wins, matching how imports are kept in `resolve_terminals`).
    fn declare_terminals(&mut self, syms: Vec<Symbol>) {
        for sym in syms {
            if let Symbol::Terminal(t) = sym {
                if !self.terminals.iter().any(|td| td.name == t.name) {
                    self.terminals.push(TerminalDef::declared(&t.name));
                }
            }
        }
    }

    /// Compile every user terminal to a regex, inlining terminal-to-terminal
    /// references (`C: "C" | D`). Resolution is order-independent and memoized;
    /// mutually-recursive terminals are rejected (a terminal denotes a *regular*
    /// language, so it cannot reference itself). Each terminal is then registered
    /// as a `Pattern::Re`, **except** one that reduces to a single case-sensitive
    /// string literal, which is registered as a `Pattern::Str` — like an inline
    /// `"literal"` and like Python Lark's `PatternStr`, so a named keyword terminal
    /// participates in the contextual lexer's `unless` keyword retyping.
    fn resolve_terminals(&mut self) -> Result<(), GrammarError> {
        let raw_terms = std::mem::take(&mut self.raw_terms);
        let by_name: HashMap<&str, &RawTerm> =
            raw_terms.iter().map(|t| (t.name.as_str(), t)).collect();
        // Terminals already known (imports, declares) as inline-ready regex — a
        // terminal body may reference these too.
        let imported: HashMap<String, String> = self
            .terminals
            .iter()
            .map(|t| (t.name.clone(), t.pattern.to_inline_regex()))
            .collect();

        let mut memo: HashMap<String, String> = HashMap::new();
        for t in &raw_terms {
            Self::resolve_term_regex(&t.name, &by_name, &imported, &mut memo, &mut Vec::new())?;
        }

        // Classify each terminal as Python would: `pattern.type == "str"` (a plain
        // string literal) vs `"re"`. lark-rs compiles everything to a regex, so we
        // recover the distinction structurally here — it gates the strict-mode
        // collision check (issue #35), which only compares the regex terminals.
        let imported_str: HashMap<&str, bool> = self
            .terminals
            .iter()
            .map(|t| (t.name.as_str(), t.string_type))
            .collect();
        let mut str_memo: HashMap<String, bool> = HashMap::new();
        for t in &raw_terms {
            Self::term_is_str(&t.name, &by_name, &imported_str, &mut str_memo);
        }

        // The recoverable literal value (and case-insensitivity) of each
        // already-known string terminal, so a reference to an imported
        // `PatternStr` resolves to a `PatternStr` too.
        let imported_val: HashMap<String, (String, bool)> = self
            .terminals
            .iter()
            .filter_map(|t| match &t.pattern {
                Pattern::Str(p) => Some((t.name.clone(), (p.value.clone(), p.ci))),
                _ => None,
            })
            .collect();

        // Register in source order so terminal ordering stays stable. A terminal
        // already defined via `%import` is not redefined (import wins).
        //
        // A terminal that reduces to a single string literal — case-sensitive or
        // `"..."i` — is compiled to `Pattern::Str`, exactly like an inline
        // `"literal"` and like Python Lark's `PatternStr` (which keeps the type
        // for case-insensitive literals, only attaching the flag). This is what
        // lets a named keyword terminal (`ASYNC: "async"`) join the keyword
        // `unless` retyping in the contextual lexer — otherwise it is a
        // `Pattern::Re` that ties with, and loses to, an overlapping identifier
        // regex (`NAME`), so `async` would lex as `NAME`. Everything else
        // (regex, concatenation, alternation, range, repetition) stays
        // `Pattern::Re`.
        let mut strval_memo: HashMap<String, Option<(String, bool)>> = HashMap::new();
        for t in &raw_terms {
            if self.terminals.iter().any(|td| td.name == t.name) {
                continue;
            }
            let string_type = str_memo.get(&t.name).copied().unwrap_or(false);
            let pat = match Self::term_str_value(&t.name, &by_name, &imported_val, &mut strval_memo)
            {
                Some((value, false)) => Pattern::Str(PatternStr::new(&value)),
                Some((value, true)) => Pattern::Str(PatternStr::new_ci(&value)),
                None => Pattern::Re(PatternRe::new(memo[&t.name].as_str(), 0)?),
            };
            self.terminals
                .push(TerminalDef::new(&t.name, pat, t.priority).with_string_type(string_type));
        }
        Ok(())
    }

    /// The string value (and case-insensitivity) iff this terminal compiles to
    /// a `PatternStr` whose value lark-rs can recover — a single string literal
    /// (case-sensitive or `"..."i`), possibly through a single-alternative group
    /// or a reference to another such terminal. Returns `None` for anything else
    /// (regex, concatenation, alternation, range, repetition). Parallels
    /// [`term_is_str`]; memoized; assumes the acyclic grammar the regex pass
    /// already validated.
    fn term_str_value(
        name: &str,
        by_name: &HashMap<&str, &RawTerm>,
        imported_val: &HashMap<String, (String, bool)>,
        memo: &mut HashMap<String, Option<(String, bool)>>,
    ) -> Option<(String, bool)> {
        if let Some(v) = memo.get(name) {
            return v.clone();
        }
        if let Some(raw) = by_name.get(name) {
            memo.insert(name.to_string(), None); // cycle guard
            let result = Self::alts_str_value(&raw.expansions, by_name, imported_val, memo);
            memo.insert(name.to_string(), result.clone());
            return result;
        }
        // An imported / declared terminal: recoverable only if it is itself a
        // `PatternStr`. Common-library terminals are regex-typed → `None`.
        imported_val.get(name).cloned()
    }

    /// Value of a parenthesised/whole-terminal `expansions` node: present only when
    /// there is a single alternative that is itself a recoverable string.
    fn alts_str_value(
        alts: &[AliasedExpansion],
        by_name: &HashMap<&str, &RawTerm>,
        imported_val: &HashMap<String, (String, bool)>,
        memo: &mut HashMap<String, Option<(String, bool)>>,
    ) -> Option<(String, bool)> {
        if alts.len() != 1 {
            return None;
        }
        let expansion = &alts[0].expansion;
        match expansion.len() {
            0 => Some((String::new(), false)), // empty PatternStr('')
            1 => Self::expr_str_value(&expansion[0], by_name, imported_val, memo),
            _ => None, // concatenation → joined PatternRe
        }
    }

    /// Value of a single `Expr` in a terminal body (see [`term_str_value`]).
    fn expr_str_value(
        expr: &Expr,
        by_name: &HashMap<&str, &RawTerm>,
        imported_val: &HashMap<String, (String, bool)>,
        memo: &mut HashMap<String, Option<(String, bool)>>,
    ) -> Option<(String, bool)> {
        match expr {
            Expr::Value(Value::Literal(LiteralVal::Str(s, ci))) => Some((s.clone(), *ci)),
            Expr::Value(Value::Terminal(referenced)) => {
                Self::term_str_value(referenced, by_name, imported_val, memo)
            }
            Expr::Group(alts) => Self::alts_str_value(alts, by_name, imported_val, memo),
            _ => None,
        }
    }

    /// Does this terminal reduce to a single string literal (Python's `PatternStr`,
    /// `pattern.type == "str"`)? Mirrors `TerminalTreeToPattern`: an alternation, a
    /// concatenation of >1 part, a repetition, a range, or a regex literal all make
    /// it a `PatternRE`; only a lone string literal (possibly through a single-alt
    /// group or a reference to another string terminal) stays a `PatternStr`.
    /// Memoized; assumes the grammar is acyclic (the regex pass already rejected
    /// cycles).
    fn term_is_str(
        name: &str,
        by_name: &HashMap<&str, &RawTerm>,
        imported_str: &HashMap<&str, bool>,
        memo: &mut HashMap<String, bool>,
    ) -> bool {
        if let Some(b) = memo.get(name) {
            return *b;
        }
        // A reference to an already-resolved (imported/declared) terminal, or a
        // common-library terminal (all of which are regex-typed).
        if let Some(b) = imported_str.get(name) {
            return *b;
        }
        let Some(raw) = by_name.get(name) else {
            return false; // common-library or unknown → regex-typed
        };
        // Guard against the cyclic case the regex pass would already have rejected.
        memo.insert(name.to_string(), false);
        let result = Self::alts_are_str(&raw.expansions, by_name, imported_str, memo);
        memo.insert(name.to_string(), result);
        result
    }

    /// Type of a parenthesised/whole-terminal `expansions` node: `str` only when
    /// there is a single alternative that is itself `str`.
    fn alts_are_str(
        alts: &[AliasedExpansion],
        by_name: &HashMap<&str, &RawTerm>,
        imported_str: &HashMap<&str, bool>,
        memo: &mut HashMap<String, bool>,
    ) -> bool {
        if alts.len() != 1 {
            return false;
        }
        let expansion = &alts[0].expansion;
        match expansion.len() {
            0 => true, // empty PatternStr('')
            1 => Self::expr_is_str(&expansion[0], by_name, imported_str, memo),
            _ => false, // concatenation → joined PatternRE
        }
    }

    /// Type of a single `Expr` in a terminal body.
    fn expr_is_str(
        expr: &Expr,
        by_name: &HashMap<&str, &RawTerm>,
        imported_str: &HashMap<&str, bool>,
        memo: &mut HashMap<String, bool>,
    ) -> bool {
        match expr {
            // A string literal is a PatternStr even when case-insensitive (Python
            // keeps the type, only attaching the flag).
            Expr::Value(Value::Literal(LiteralVal::Str(_, _))) => true,
            Expr::Value(Value::Terminal(referenced)) => {
                Self::term_is_str(referenced, by_name, imported_str, memo)
            }
            // A single-alternative group collapses to its inner pattern's type.
            Expr::Group(alts) => Self::alts_are_str(alts, by_name, imported_str, memo),
            // Regex literal, range, repetition, `?`, rule/template ref → PatternRE.
            _ => false,
        }
    }

    /// Resolve one terminal to its combined regex string, recursing into any
    /// referenced terminals. Memoized; `stack` carries the active resolution chain
    /// for cycle detection.
    fn resolve_term_regex(
        name: &str,
        by_name: &HashMap<&str, &RawTerm>,
        imported: &HashMap<String, String>,
        memo: &mut HashMap<String, String>,
        stack: &mut Vec<String>,
    ) -> Result<String, GrammarError> {
        if let Some(r) = memo.get(name) {
            return Ok(r.clone());
        }
        // Reference to an imported/declared terminal, or a common-library terminal.
        if let Some(r) = imported.get(name) {
            return Ok(r.clone());
        }
        let Some(raw) = by_name.get(name) else {
            if let Some(src) = common_terminals().get(name) {
                return Ok(src.clone());
            }
            return Err(GrammarError::UndefinedTerminal {
                name: name.to_string(),
            });
        };
        if stack.iter().any(|n| n == name) {
            stack.push(name.to_string());
            return Err(GrammarError::Other {
                msg: format!("Cyclic terminal definition: {}", stack.join(" -> ")),
            });
        }
        stack.push(name.to_string());

        // Build one regex per alternative, then join longest-first (mirroring
        // Python Lark) so a more specific alternative beats its own prefix.
        let mut alts = Vec::with_capacity(raw.expansions.len());
        for alt in &raw.expansions {
            let mut parts = String::new();
            for expr in &alt.expansion {
                parts.push_str(&Self::term_expr_regex(
                    expr, by_name, imported, memo, stack,
                )?);
            }
            alts.push(parts);
        }
        stack.pop();

        let combined = if alts.len() == 1 {
            alts.pop().unwrap()
        } else {
            alts.sort_by(|a, b| b.len().cmp(&a.len()));
            alts.into_iter()
                .map(|p| format!("(?:{p})"))
                .collect::<Vec<_>>()
                .join("|")
        };
        memo.insert(name.to_string(), combined.clone());
        Ok(combined)
    }

    /// Regex for a single `Expr` appearing in a *terminal* body. Unlike
    /// `expr_to_pattern`, a terminal reference is resolved (and inlined) rather
    /// than looked up after the fact, and flags are applied as scoped groups.
    fn term_expr_regex(
        expr: &Expr,
        by_name: &HashMap<&str, &RawTerm>,
        imported: &HashMap<String, String>,
        memo: &mut HashMap<String, String>,
        stack: &mut Vec<String>,
    ) -> Result<String, GrammarError> {
        let regex = match expr {
            Expr::Value(Value::Literal(LiteralVal::Str(s, ci))) => {
                let escaped = regex::escape(s);
                if *ci {
                    format!("(?i:{escaped})")
                } else {
                    escaped
                }
            }
            Expr::Value(Value::Literal(LiteralVal::Re(pattern, flags))) => {
                // Validate and apply any flags as a scoped group.
                Pattern::Re(PatternRe::new(pattern.as_str(), *flags)?).to_inline_regex()
            }
            Expr::Value(Value::Range(from, to)) => {
                if from.chars().count() != 1 || to.chars().count() != 1 {
                    return Err(GrammarError::Other {
                        msg: "Range requires single characters".to_string(),
                    });
                }
                format!("[{}-{}]", regex::escape(from), regex::escape(to))
            }
            Expr::Value(Value::Terminal(referenced)) => {
                let inner = Self::resolve_term_regex(referenced, by_name, imported, memo, stack)?;
                format!("(?:{inner})")
            }
            Expr::Repeat { inner, min, max } => {
                let inner_re = Self::term_expr_regex(inner, by_name, imported, memo, stack)?;
                let quantifier = match (*min, *max) {
                    (0, Some(1)) => "?".to_string(),
                    (1, None) => "+".to_string(),
                    (0, None) => "*".to_string(),
                    (n, Some(m)) if n == m => format!("{{{n}}}"),
                    (n, Some(m)) => format!("{{{n},{m}}}"),
                    (n, None) => format!("{{{n},}}"),
                };
                format!("(?:{inner_re}){quantifier}")
            }
            Expr::Group(alts) => {
                let parts = Self::term_alts_regex(alts, by_name, imported, memo, stack)?;
                format!("(?:{})", parts.join("|"))
            }
            Expr::Maybe(alts) => {
                let parts = Self::term_alts_regex(alts, by_name, imported, memo, stack)?;
                format!("(?:{})?", parts.join("|"))
            }
            Expr::Value(Value::Rule(name)) | Expr::Value(Value::TemplateUsage { name, .. }) => {
                return Err(GrammarError::Other {
                    msg: format!("Terminal definition cannot reference rule {name:?}"),
                });
            }
        };
        Ok(regex)
    }

    /// Regex strings for each alternative of a parenthesised group inside a
    /// terminal body (concatenating each alternative's exprs).
    fn term_alts_regex(
        alts: &[AliasedExpansion],
        by_name: &HashMap<&str, &RawTerm>,
        imported: &HashMap<String, String>,
        memo: &mut HashMap<String, String>,
        stack: &mut Vec<String>,
    ) -> Result<Vec<String>, GrammarError> {
        let mut out = Vec::with_capacity(alts.len());
        for alt in alts {
            let mut parts = String::new();
            for expr in &alt.expansion {
                parts.push_str(&Self::term_expr_regex(
                    expr, by_name, imported, memo, stack,
                )?);
            }
            out.push(parts);
        }
        Ok(out)
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
            let combined = parts
                .iter()
                .map(|p| p.as_regex_str())
                .collect::<Vec<_>>()
                .join("");
            Ok(Pattern::Re(PatternRe::new(&combined, 0)?))
        }
    }

    fn expr_to_pattern(&self, expr: &Expr) -> Result<Pattern, GrammarError> {
        match expr {
            Expr::Value(Value::Literal(LiteralVal::Str(s, ci))) => {
                if *ci {
                    Ok(Pattern::Re(PatternRe::new(
                        &format!("(?i){}", regex::escape(s)),
                        flags::IGNORECASE,
                    )?))
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
                    return Err(GrammarError::Other {
                        msg: "Range requires single characters".to_string(),
                    });
                }
                Ok(Pattern::Re(PatternRe::new(
                    &format!("[{}-{}]", regex::escape(from), regex::escape(to)),
                    0,
                )?))
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
                let parts: Vec<String> = alts
                    .iter()
                    .map(|a| {
                        let parts: Vec<Result<Pattern, GrammarError>> = a
                            .expansion
                            .iter()
                            .map(|e| self.expr_to_pattern(e))
                            .collect();
                        parts.into_iter().collect::<Result<Vec<_>, _>>().map(|ps| {
                            ps.iter()
                                .map(|p| p.as_regex_str().to_string())
                                .collect::<Vec<_>>()
                                .join("")
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(Pattern::Re(PatternRe::new(
                    &format!("(?:{})", parts.join("|")),
                    0,
                )?))
            }
            Expr::Maybe(alts) => {
                let inner_pat = self.expansion_to_pattern(&alts[0].expansion)?;
                Ok(Pattern::Re(PatternRe::new(
                    &format!("(?:{})?", inner_pat.as_regex_str()),
                    0,
                )?))
            }
            // Terminal reference in %ignore — look up the terminal's pattern
            Expr::Value(Value::Terminal(name)) => {
                if let Some(td) = self.terminals.iter().find(|t| &t.name == name) {
                    Ok(td.pattern.clone())
                } else if let Some(pat_str) = common_terminals().get(name) {
                    Ok(Pattern::Re(PatternRe::new(pat_str, 0)?))
                } else {
                    Err(GrammarError::UndefinedTerminal { name: name.clone() })
                }
            }
            _ => Err(GrammarError::Other {
                msg: format!("Cannot convert {:?} to pattern", expr),
            }),
        }
    }

    fn instantiate_template(
        &mut self,
        name: &str,
        args: Vec<Value>,
        _parent: &str,
    ) -> Result<Symbol, GrammarError> {
        let (params, expansions, modifiers, priority) = self
            .templates
            .get(name)
            .ok_or_else(|| GrammarError::UndefinedRule {
                name: name.to_string(),
            })?
            .clone();

        if params.len() != args.len() {
            return Err(GrammarError::Other {
                msg: format!(
                    "Template {} expects {} args, got {}",
                    name,
                    params.len(),
                    args.len()
                ),
            });
        }

        // Memoize by (name, args): a repeat request for the same instantiation —
        // including the self-reference inside a recursive template — resolves to the
        // rule already being built rather than instantiating (and recursing) again.
        let key = format!("{}::{:?}", name, args);
        if let Some(existing) = self.template_instances.get(&key) {
            return Ok(Symbol::NonTerminal(NonTerminal::new(existing)));
        }

        // Name the instance `base{N}`: the `{` marks it as a template instance whose
        // *tree label* is the base name (Lark's `template_source`), and leaving the
        // base prefix intact means a `_`-prefixed template (`_expr`) instantiates to
        // a transparent rule while `expr` does not. The counter keeps distinct
        // arg-sets distinct. Registered *before* compiling the body so a
        // self-reference resolves to the rule being built.
        let inst_name = format!("{}{{{}}}", name, self.anon_counter);
        self.anon_counter += 1;
        self.template_instances.insert(key, inst_name.clone());

        // Build substitution map
        let subst: HashMap<String, Value> = params.into_iter().zip(args).collect();

        // Each instance inherits the template's own rule options (keep-all / expand1
        // / priority), not the anon-helper defaults — so `!expr{t}` keeps its tokens.
        let keep_all = modifiers.contains('!') || self.global_keep_all;
        let inst_opts = RuleOptions {
            expand1: modifiers.contains('?'),
            keep_all_tokens: keep_all,
            priority,
            ..RuleOptions::default()
        };
        // Make keep-all visible to placeholder counting while this body compiles,
        // then restore the caller's context.
        let saved_keep_all = self.current_keep_all;
        self.current_keep_all = keep_all;

        // Substitute template params in expansions
        let expansions = Self::substitute_template(&expansions, &subst);
        let origin = NonTerminal::new(&inst_name);
        let mut compiled: Vec<(CompiledAlt, Option<String>)> = Vec::new();
        for alt in expansions.into_iter() {
            let alias = alt.alias.clone();
            for alt_c in self.compile_expansion(alt.expansion, &inst_name, true)? {
                compiled.push((alt_c, alias.clone()));
            }
        }
        let compiled = Self::dedup_and_check_alts(&inst_name, compiled)?;
        for (order, ((syms, gaps), alias)) in compiled.into_iter().enumerate() {
            let options = RuleOptions {
                nones_before: Self::stored_gaps(gaps),
                ..inst_opts.clone()
            };
            self.rules
                .push(Rule::new(origin.clone(), syms, alias, options, order));
        }
        self.current_keep_all = saved_keep_all;
        Ok(Symbol::NonTerminal(origin))
    }

    fn substitute_template(
        expansions: &[AliasedExpansion],
        subst: &HashMap<String, Value>,
    ) -> Vec<AliasedExpansion> {
        expansions
            .iter()
            .map(|alt| AliasedExpansion {
                expansion: alt
                    .expansion
                    .iter()
                    .map(|e| Self::subst_expr(e, subst))
                    .collect(),
                alias: alt.alias.clone(),
            })
            .collect()
    }

    fn subst_expr(expr: &Expr, subst: &HashMap<String, Value>) -> Expr {
        match expr {
            Expr::Value(v) => Expr::Value(Self::subst_value(v, subst)),
            Expr::Repeat { inner, min, max } => Expr::Repeat {
                inner: Box::new(Self::subst_expr(inner, subst)),
                min: *min,
                max: *max,
            },
            Expr::Group(alts) => Expr::Group(Self::substitute_template(alts, subst)),
            Expr::Maybe(alts) => Expr::Maybe(Self::substitute_template(alts, subst)),
        }
    }

    /// Substitute template params inside a `Value`. Crucially this recurses into a
    /// nested template usage's arguments, so `_sep{item, delim}` inside a `_sep`
    /// body becomes `_sep{NUMBER, ","}` — the self-instantiation the memo then
    /// collapses, rather than a reference to undefined `item`/`delim` rules.
    fn subst_value(v: &Value, subst: &HashMap<String, Value>) -> Value {
        match v {
            Value::Rule(name) | Value::Terminal(name) => {
                subst.get(name).cloned().unwrap_or_else(|| v.clone())
            }
            // Higher-order templates: a parameter can itself be a template applied
            // as `t{…}`. Substitute the *usage's name* too (`t` → `b`), so
            // `a{t}: t{"a"}` with `a{b}` instantiates `b{"a"}`, not undefined `t`.
            Value::TemplateUsage { name, args } => {
                let name = match subst.get(name) {
                    Some(Value::Rule(n)) | Some(Value::Terminal(n)) => n.clone(),
                    _ => name.clone(),
                };
                Value::TemplateUsage {
                    name,
                    args: args.iter().map(|a| Self::subst_value(a, subst)).collect(),
                }
            }
            other => other.clone(),
        }
    }

    fn resolve_import(&mut self, spec: ImportSpec) -> Result<(), GrammarError> {
        // Split the directive into the module path (which file/library to load
        // from) and the list of `(name, alias)` symbols to import. Three forms:
        //   %import common.WORD              → module=["common"],  import WORD
        //   %import common.WS -> _WS         → module=["common"],  import WS as _WS
        //   %import common (WORD, INT, ...)  → module=["common"],  import each
        //   %import .tokens (NUMBER, NAME)   → module=["tokens"] (relative file)
        let (module_path, names_to_import): (Vec<String>, Vec<(String, Option<String>)>) =
            if let Some(names) = spec.names {
                // Name-list form: a multi-import cannot carry per-name aliases.
                (
                    spec.path.clone(),
                    names.into_iter().map(|n| (n, None)).collect(),
                )
            } else if spec.path.len() > 1 {
                // Single import: the last path element is the symbol; the leading
                // elements are the module. An alias may rename it.
                let original = spec.path.last().cloned().unwrap_or_default();
                let module = spec.path[..spec.path.len() - 1].to_vec();
                (module, vec![(original, spec.alias)])
            } else {
                return Ok(()); // nothing to import
            };

        // Bundled grammar libraries (shipped with lark-rs, mirroring the grammars
        // Python Lark ships under `lark/grammars/`) are resolved from embedded
        // sources, not the filesystem. Everything else is a file import resolved
        // relative to the importing grammar's directory. A *relative*
        // `%import .common ...` (leading dot) is a file, not the library.
        let is_library = !spec.relative && module_path.len() == 1;

        // `common` keeps its dedicated terminal-table path (terminals only, no
        // rules) — it is the hot, heavily-pinned library and copies inline regexes
        // directly.
        if is_library && module_path[0] == "common" {
            for (name, alias) in &names_to_import {
                if let Some(regex) = common_terminals().get(name) {
                    let registered_name = alias.as_deref().unwrap_or(name.as_str());
                    let pat = Pattern::Re(PatternRe::new(regex, 0)?);
                    if !self.terminals.iter().any(|t| t.name == registered_name) {
                        self.terminals
                            .push(TerminalDef::new(registered_name, pat, 0));
                    }
                }
                // Rules from common (e.g., %import common.list) are silently skipped for now.
            }
            return Ok(());
        }

        // Other bundled libraries (`python`, `unicode`, `lark`, …) carry rules as
        // well as terminals, so they route through the same source-parse +
        // closure-copy path as a file import — just with the source embedded in the
        // binary instead of read from disk.
        if is_library {
            if let Some(src) = bundled_grammar_source(&module_path[0]) {
                return self.import_from_source(src, None, &module_path, &names_to_import);
            }
        }

        self.resolve_file_import(&module_path, &names_to_import)
    }

    /// Resolve a file import: load and parse a sibling `.lark` file through
    /// `load_grammar`, then copy the requested terminals/rules (and, for a rule,
    /// its dependency closure) into this grammar — mirroring Python Lark's
    /// `GrammarLoader.do_import` + `_remove_unused`.
    fn resolve_file_import(
        &mut self,
        module_path: &[String],
        names_to_import: &[(String, Option<String>)],
    ) -> Result<(), GrammarError> {
        let dotted = module_path.join(".");
        // Resolve `a.b.c` → `<base>/a/b/c.lark`. Without a base path (grammar built
        // from a bare string) a file import is unresolvable, exactly as Python Lark
        // cannot find a relative import with no source location.
        let base = self
            .base_path
            .as_ref()
            .ok_or_else(|| GrammarError::ImportNotFound {
                path: dotted.clone(),
            })?;
        let mut file = base.clone();
        for comp in module_path {
            file.push(comp);
        }
        file.set_extension("lark");
        let text = std::fs::read_to_string(&file).map_err(|_| GrammarError::ImportNotFound {
            path: dotted.clone(),
        })?;

        // The imported grammar's own relative imports resolve against *its*
        // directory, so nested file imports compose.
        let sub_base = file.parent().map(PathBuf::from);
        self.import_from_source(&text, sub_base, module_path, names_to_import)
    }

    /// Parse a grammar `text` (read from a sibling file or embedded as a bundled
    /// library) and copy the requested terminals/rules — and, for a rule, its
    /// dependency closure — into this grammar. `sub_base` is the directory the
    /// imported grammar's *own* relative imports resolve against (`None` for an
    /// embedded library, which can only re-import other libraries, never files).
    fn import_from_source(
        &mut self,
        text: &str,
        sub_base: Option<PathBuf>,
        module_path: &[String],
        names_to_import: &[(String, Option<String>)],
    ) -> Result<(), GrammarError> {
        let dotted = module_path.join(".");
        // A pure-terminal source (e.g. `tokens.lark`, `unicode.lark`) has no rule
        // referencing its terminals, so dead-terminal pruning would drop them.
        // Append a probe rule that references every requested name so they survive
        // compilation — the same trick `common_terminals()` uses. The probe is
        // never copied out.
        let probe_body = names_to_import
            .iter()
            .map(|(n, _)| n.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        let probe = format!("{text}\n{IMPORT_PROBE_RULE}: {probe_body}\n");
        let imported = load_grammar_with_base(
            &probe,
            &[IMPORT_PROBE_RULE.to_string()],
            self.maybe_placeholders,
            self.global_keep_all,
            sub_base,
        )?;

        // Dependency names are namespaced under the module path so an imported
        // rule's private helpers/terminals never collide with the importing
        // grammar's. Requested names keep their (aliased) name. Matches Python
        // Lark's `_get_mangle('__'.join(dotted_path), aliases, ...)`.
        let prefix = module_path.join("__");
        for (name, alias) in names_to_import {
            let final_name = alias.clone().unwrap_or_else(|| name.clone());
            if imported.terminals.iter().any(|t| &t.name == name) {
                self.import_terminal(&imported, name, &final_name);
            } else if imported.rules.iter().any(|r| &r.origin.name == name) {
                self.import_rule_closure(&imported, name, &final_name, &prefix);
            } else {
                return Err(GrammarError::ImportNotFound {
                    path: format!("{dotted}.{name}"),
                });
            }
        }
        Ok(())
    }

    /// Copy a single compiled terminal from an imported grammar under `final_name`.
    fn import_terminal(&mut self, imported: &Grammar, name: &str, final_name: &str) {
        if self.terminals.iter().any(|t| t.name == final_name) {
            return; // already defined locally — don't shadow it
        }
        if let Some(td) = imported.terminals.iter().find(|t| t.name == name) {
            let mut copy = td.clone();
            copy.name = final_name.to_string();
            self.terminals.push(copy);
        }
    }

    /// Copy an imported rule plus every rule/terminal it transitively references.
    /// The requested rule keeps `final_name`; all dependencies are mangled under
    /// `prefix` (underscore-preserving, so transparent `_rules` stay transparent)
    /// to avoid colliding with the importing grammar's own symbols.
    fn import_rule_closure(
        &mut self,
        imported: &Grammar,
        name: &str,
        final_name: &str,
        prefix: &str,
    ) {
        // Reachable rule origins (BFS from `name`) and the terminals they touch.
        let mut rule_names: std::collections::HashSet<String> =
            std::collections::HashSet::from([name.to_string()]);
        let mut term_names: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut worklist = vec![name.to_string()];
        while let Some(rn) = worklist.pop() {
            for rule in imported.rules.iter().filter(|r| r.origin.name == rn) {
                for sym in &rule.expansion {
                    match sym {
                        Symbol::Terminal(t) => {
                            term_names.insert(t.name.clone());
                        }
                        Symbol::NonTerminal(nt) => {
                            if rule_names.insert(nt.name.clone()) {
                                worklist.push(nt.name.clone());
                            }
                        }
                    }
                }
            }
        }

        // Don't re-import a rule already defined locally (Python raises; we keep the
        // existing definition rather than duplicate the origin).
        if self.rules.iter().any(|r| r.origin.name == final_name) {
            return;
        }

        // Name map: requested symbol → final name; everything else → mangled.
        let rename = |n: &str| -> String {
            if n == name {
                final_name.to_string()
            } else if let Some(rest) = n.strip_prefix('_') {
                format!("_{prefix}__{rest}")
            } else {
                format!("{prefix}__{n}")
            }
        };

        for rule in imported
            .rules
            .iter()
            .filter(|r| rule_names.contains(&r.origin.name))
        {
            let origin = NonTerminal::new(rename(&rule.origin.name));
            let expansion = rule
                .expansion
                .iter()
                .map(|sym| match sym {
                    Symbol::Terminal(t) => Symbol::Terminal(Terminal {
                        name: rename(&t.name),
                        filter_out: t.filter_out,
                    }),
                    Symbol::NonTerminal(nt) => {
                        Symbol::NonTerminal(NonTerminal::new(rename(&nt.name)))
                    }
                })
                .collect();
            // An alias (`-> name`) names the tree node this rule produces; Python
            // Lark mangles it under the module prefix just like a rule origin, so an
            // imported `-> literal` surfaces as `<module>__literal`. Mangle it here
            // too, otherwise the imported grammar's aliased nodes would collide with
            // (or leak into) the importing grammar's namespace.
            let alias = rule.alias.as_deref().map(rename);
            self.rules.push(Rule::new(
                origin,
                expansion,
                alias,
                rule.options.clone(),
                rule.order,
            ));
        }
        for td in imported
            .terminals
            .iter()
            .filter(|t| term_names.contains(&t.name))
        {
            let new_name = rename(&td.name);
            if !self.terminals.iter().any(|t| t.name == new_name) {
                let mut copy = td.clone();
                copy.name = new_name;
                self.terminals.push(copy);
            }
        }
    }

    fn compile(mut self) -> Result<Grammar, GrammarError> {
        // Add $END terminal
        if !self.terminals.iter().any(|t| t.name == "$END") {
            // $END is synthetic and handled by the parser, not the lexer.
        }

        // Add ignore terminals (one terminal per ignore pattern)
        let n_ignore = self.ignore_patterns.len();
        let ignore_names: Vec<String> = (0..n_ignore).map(|i| format!("__IGNORE_{}", i)).collect();
        for (i, pat) in self.ignore_patterns.into_iter().enumerate() {
            let name = format!("__IGNORE_{}", i);
            // `%ignore` tokens never reach the tree (the parse loop skips them), so
            // they need no per-occurrence filter — they appear in no rule body.
            self.terminals.push(TerminalDef::new(&name, pat, 0));
        }

        // Reject use-before-definition: a rule body that references a symbol which
        // is neither a defined rule nor a defined terminal is a grammar error, as in
        // Python Lark (`GrammarError("Rule 'X' used but not defined")`). We check
        // *before* pruning so the full terminal set is visible. Template parameters
        // never reach here — templates are instantiated on demand and only their
        // (fully substituted) instances live in `self.rules` — and anonymous literal
        // terminals are interned as they are compiled, so they are always defined.
        let defined_rules: std::collections::HashSet<&str> =
            self.rules.iter().map(|r| r.origin.name.as_str()).collect();
        let defined_terms: std::collections::HashSet<&str> =
            self.terminals.iter().map(|t| t.name.as_str()).collect();
        for rule in &self.rules {
            for sym in &rule.expansion {
                match sym {
                    Symbol::NonTerminal(nt) if !defined_rules.contains(nt.name.as_str()) => {
                        return Err(GrammarError::UndefinedRule {
                            name: nt.name.clone(),
                        });
                    }
                    Symbol::Terminal(t) if !defined_terms.contains(t.name.as_str()) => {
                        return Err(GrammarError::UndefinedTerminal {
                            name: t.name.clone(),
                        });
                    }
                    _ => {}
                }
            }
        }

        // Prune terminals that no rule (or `%ignore`) references. A terminal used
        // only inside another terminal (`C: "C" | D` — `D` is inlined into `C`)
        // has no token of its own, exactly as Python Lark drops it. Terminals
        // referenced by a rule body, and the synthetic `%ignore` terminals, stay.
        let mut used: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for rule in &self.rules {
            for sym in &rule.expansion {
                if let Symbol::Terminal(t) = sym {
                    used.insert(t.name.as_str());
                }
            }
        }
        for name in &ignore_names {
            used.insert(name.as_str());
        }
        self.terminals.retain(|t| used.contains(t.name.as_str()));

        // Sort terminals by (priority desc, max_width desc, name asc)
        self.terminals.sort_by(|a, b| {
            b.priority
                .cmp(&a.priority)
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
    if let Some(&name) = TERMINAL_NAMES
        .iter()
        .find(|(ch, _)| ch == &s)
        .map(|(_, n)| n)
    {
        return Some(name.to_string());
    }
    // Keyword-like strings become their uppercase form, but only when that is a
    // valid regex named-capture identifier (must not start with a digit).
    let first_ok = s
        .chars()
        .next()
        .is_some_and(|c| c.is_alphabetic() || c == '_');
    if first_ok && s.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return Some(s.to_uppercase());
    }
    None
}

/// Two patterns are equivalent for terminal unification when they match the same
/// language: identical regex source *and* identical flags. Python Lark keys its
/// `term_reverse` map on `Pattern` equality (and raises on a flag mismatch for the
/// same source); we treat differing flags as simply distinct, so unification never
/// merges, say, `"a"` with `"a"i`.
fn patterns_equivalent(a: &Pattern, b: &Pattern) -> bool {
    fn flags_of(p: &Pattern) -> u32 {
        match p {
            Pattern::Str(s) if s.ci => flags::IGNORECASE,
            Pattern::Str(_) => 0,
            Pattern::Re(r) => r.flags,
        }
    }
    a.as_regex_str() == b.as_regex_str() && flags_of(a) == flags_of(b)
}

/// Standard terminal names for common punctuation/operators.
static TERMINAL_NAMES: &[(&str, &str)] = &[
    (".", "DOT"),
    (",", "COMMA"),
    (":", "COLON"),
    (";", "SEMICOLON"),
    ("+", "PLUS"),
    ("-", "MINUS"),
    ("*", "STAR"),
    ("/", "SLASH"),
    ("|", "VBAR"),
    ("?", "QMARK"),
    ("!", "BANG"),
    ("@", "AT"),
    ("#", "HASH"),
    ("$", "DOLLAR"),
    ("%", "PERCENT"),
    ("^", "CIRCUMFLEX"),
    ("&", "AMPERSAND"),
    ("_", "UNDERSCORE"),
    ("<", "LESSTHAN"),
    (">", "MORETHAN"),
    ("=", "EQUAL"),
    ("\"", "DBLQUOTE"),
    ("'", "QUOTE"),
    ("`", "BACKQUOTE"),
    ("~", "TILDE"),
    ("(", "LPAR"),
    (")", "RPAR"),
    ("{", "LBRACE"),
    ("}", "RBRACE"),
    ("[", "LSQB"),
    ("]", "RSQB"),
    ("\n", "NEWLINE"),
    ("\t", "TAB"),
    (" ", "SPACE"),
];

/// Embedded source of a bundled grammar library (the equivalents of the grammars
/// Python Lark ships under `lark/grammars/`), keyed by its `%import` module name.
///
/// `common` is handled separately (its dedicated terminal-table fast path); the
/// libraries here carry rules as well as terminals and are imported through the
/// same source-parse + closure-copy path as a sibling-file import. The files are
/// verbatim copies of Python Lark's grammars — a handful of their terminals use
/// lookaround (the `regex` crate has no lookahead/lookbehind), which the lexer
/// **lowers into its DFA** (`docs/LEXER_DFA_PLAN.md`; every bundled lookaround
/// terminal is in scope, `docs/LOOKAROUND_SCOPE.md`), so the grammar text needs no
/// hand-edits. Pinned by `tests/test_stdlib.rs`.
fn bundled_grammar_source(module: &str) -> Option<&'static str> {
    match module {
        "python" => Some(include_str!("../grammars/python.lark")),
        "unicode" => Some(include_str!("../grammars/unicode.lark")),
        "lark" => Some(include_str!("../grammars/lark.lark")),
        _ => None,
    }
}

/// Lark's `common.lark`, bundled and compiled once into a `name → inline-regex`
/// map for `%import common.X` resolution.
///
/// Rather than maintain a hand-transcribed regex table (which silently drifts from
/// Python Lark), we parse our own bundled copy of `common.lark` through the *same*
/// terminal-algebra path lark-rs uses for user grammars: each terminal's regex is
/// the loader's own compiled output, so a common terminal cannot lex differently
/// from the way the same definition would in a user grammar. The pinned fidelity
/// net is `tests/test_common.rs` (oracles in `fixtures/oracles/common/`).
///
/// The bundled copy carries one documented adaptation (the lookbehind in Lark's
/// escaped-string helpers, which the `regex` crate cannot compile) — see the
/// header of `src/grammars/common.lark`.
fn common_terminals() -> &'static HashMap<String, String> {
    use std::sync::OnceLock;
    static MAP: OnceLock<HashMap<String, String>> = OnceLock::new();
    MAP.get_or_init(|| {
        const COMMON_LARK: &str = include_str!("../grammars/common.lark");
        // Collect every terminal name so a probe rule keeps them all alive through
        // dead-terminal pruning (a terminal only referenced by another terminal is
        // otherwise inlined away and would not be importable).
        let names: Vec<&str> = COMMON_LARK
            .lines()
            .filter_map(|line| {
                let line = line.trim_start();
                let name = line.split_once(':')?.0.trim();
                let is_term_name = !name.is_empty()
                    && name.starts_with(|c: char| c == '_' || c.is_ascii_uppercase())
                    && name
                        .chars()
                        .all(|c| c == '_' || c.is_ascii_uppercase() || c.is_ascii_digit());
                is_term_name.then_some(name)
            })
            .collect();
        let probe = format!("{COMMON_LARK}\n__common_probe: {}\n", names.join(" "));
        let grammar = load_grammar(&probe, &["__common_probe".to_string()], false, false)
            .expect("bundled common.lark must compile");
        grammar
            .terminals
            .into_iter()
            .map(|t| (t.name, t.pattern.to_inline_regex()))
            .collect()
    })
}
