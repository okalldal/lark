//! Phase 1 — the `.lark` tokenizer: grammar text → [`Tok`] stream.

use crate::error::GrammarError;
use crate::grammar::terminal::flags;

#[derive(Debug, Clone, PartialEq)]
pub(super) enum Tok {
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

pub(super) struct Lexer<'a> {
    src: &'a str,
    pos: usize,
    pub(super) line: usize,
    pub(super) col: usize,
    peeked: Option<(Tok, usize, usize)>,
}

impl<'a> Lexer<'a> {
    pub(super) fn new(src: &'a str) -> Self {
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

    pub(super) fn next_tok(&mut self) -> Result<Option<Tok>, GrammarError> {
        if let Some(peeked) = self.peeked.take() {
            self.line = peeked.1;
            self.col = peeked.2;
            return Ok(Some(peeked.0));
        }
        self.next_tok_inner()
    }

    pub(super) fn peek_tok(&mut self) -> Result<Option<&Tok>, GrammarError> {
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
                    // A full-line comment swallows its *leading* newline run, exactly
                    // as Python Lark's grammar-of-grammars does: lark.lark's
                    // `COMMENT: /\s*/ "//" /[^\n]*/` out-lengths `_NL` at the newline,
                    // so a comment line between the `|` alternatives of a multi-line
                    // rule never terminates the rule (the wild-bank dotmotif shape).
                    // The comment's own trailing newline is left for the next
                    // dispatch, exactly like Python's.
                    if rest[n..].starts_with("//") || rest[n..].starts_with('#') {
                        Dispatch::Comment(rest[n..].find('\n').map_or(rest.len(), |k| n + k))
                    } else {
                        Dispatch::Newline(n)
                    }
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
/// `eval_escaping` (which defers to `ast.literal_eval`). Python decodes **only**
/// the `Uuxnftr` set plus `\\` and `\"`: the numeric escapes `\xHH`, `\uHHHH`,
/// and `\UHHHHHHHH` decode to the corresponding `char`; `\n \t \r \f` map to
/// their control characters; `\\` is a literal backslash and `\"` is a literal
/// quote. **Every other** escape — including `\v`, `\0`, `\'`, and regex
/// metacharacters like `\w`/`\d` — keeps its backslash, because `eval_escaping`
/// prepends a backslash for any escape outside `Uuxnftr` (so `\v` is the literal
/// two chars backslash+`v`, not U+000B; `\0` is not NUL; `\'` is not `'`). This
/// keeps the `PatternStr` value byte-identical to Python's (#344).
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
            Some('\\') => out.push('\\'),
            Some('"') => out.push('"'),
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
