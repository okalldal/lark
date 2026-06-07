//! Lookaround-aware regex front-end — milestone **M1** of the Lexer DFA / B1 plan
//! (`docs/LEXER_DFA_PLAN.md`).
//!
//! The `regex` crate (and its `regex-automata` layer) cannot parse lookaround
//! assertions — they reject `(?=…)`, `(?!…)`, `(?<=…)`, `(?<!…)` exactly as the
//! `regex` crate does. The B1 strategy retires `fancy-regex` by *lowering* every
//! bounded assertion into a finite automaton, which first requires a parser that
//! can **see** the assertions in a terminal pattern. That is this module.
//!
//! It parses a terminal's regex source into a [`Node`] tree whose only structurally
//! interesting variants are concatenation, alternation, groups, and — crucially —
//! [`Node::Assertion`]. Every other construct (literals, escapes, character
//! classes, anchors, quantifiers) is preserved **verbatim** inside [`Node::Atom`]
//! runs, so the tree round-trips to byte-identical source via [`Node::to_source`].
//!
//! Two properties matter, and both are unit-tested against the real corpus
//! terminals (`STRING` / `LONG_STRING` / `REGEXP` / `DEC_NUMBER` / `OP`, plus
//! `verilog.lark`'s `MULTILINE_COMMENT`):
//!
//!   1. **Faithful round-trip.** `parse(p).to_source() == p` for any pattern the
//!      `regex` crate or `fancy-regex` accepts. This is what lets M2 hand every
//!      assertion-free fragment straight to `regex-automata` by re-emitting its
//!      source — the front-end never has to *understand* a character class, only
//!      to not be confused by the `(`, `)`, `|` that may hide inside one.
//!   2. **Assertion exposure with position.** [`Node::assertions`] enumerates every
//!      assertion in left-to-right order together with its enclosing context, so M2
//!      (the general regular lowering, §2 / §4 Amendment) can splice each one into
//!      the terminal automaton *at its position* — boundary or internal alike. The
//!      §4 Amendment census shows the bundled `STRING`/`LONG_STRING`/`REGEXP`
//!      assertions are *internal*, so a position-blind "strip + peek at the token
//!      edge" front-end would be wrong; this one records where each assertion lives.
//!
//! This module performs **no lowering** — it is purely the parse step. The lowering
//! engine ([`matcher`], milestone M2) consumes the tree it produces.

use crate::error::GrammarError;

pub mod matcher;

/// Which direction a zero-width assertion looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Look {
    /// `(?=…)` / `(?!…)` — constrains the text *after* the position.
    Ahead,
    /// `(?<=…)` / `(?<!…)` — constrains the text *before* the position.
    Behind,
}

/// A parsed regex with its lookaround assertions exposed.
///
/// All non-assertion syntax is kept verbatim in [`Node::Atom`] so the tree
/// reconstructs the exact input via [`Node::to_source`]; the only nodes the rest
/// of the pipeline introspects are [`Node::Concat`], [`Node::Alt`],
/// [`Node::Group`] and [`Node::Assertion`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    /// A run of assertion-free regex source, kept exactly as written (literals,
    /// escapes, character classes, anchors, *and* the quantifiers that bind to
    /// them). Empty atoms are legal and occur, e.g., for an empty alternation
    /// branch (`a|`) or an empty group (`()`).
    Atom(String),
    /// A sequence of sub-nodes, concatenated. A bare pattern parses to a `Concat`
    /// (possibly of length 1) so an assertion's left/right siblings are visible.
    Concat(Vec<Node>),
    /// `a|b|…` at one nesting level. Always has ≥ 2 branches (a single branch is a
    /// plain `Concat`).
    Alt(Vec<Node>),
    /// A parenthesised group. `open` is the exact opening delimiter as written
    /// (`(`, `(?:`, `(?i:`, `(?P<name>`, `(?<name>`, …) and the closing `)` is
    /// implicit, so re-emission is exact. `quant` carries any quantifier that
    /// immediately follows the group (`*`, `+?`, `{1,3}`, …), or is empty.
    Group {
        open: String,
        body: Box<Node>,
        quant: String,
    },
    /// A zero-width lookaround assertion. `body` is the parsed assertion sub-pattern
    /// (the regex inside the assertion group). A trailing quantifier on an assertion
    /// is syntactically legal but degenerate; `quant` preserves it for round-trip.
    Assertion {
        neg: bool,
        look: Look,
        body: Box<Node>,
        quant: String,
    },
}

/// One assertion together with the position context M2 needs to lower it. Yielded
/// by [`Node::assertions`] in left-to-right (source) order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssertionRef<'a> {
    pub neg: bool,
    pub look: Look,
    /// The assertion's sub-pattern (the regex between the `(?=` … `)`).
    pub body: &'a Node,
    /// Whether this assertion is the *first* element of its concatenation — i.e. it
    /// sits at the **leading** token boundary (a lookbehind here, like a hypothetical
    /// `(?<!\\)STRING`, is the §4.2 fast-path case).
    pub at_concat_start: bool,
    /// Whether this assertion is the *last* element of its concatenation — i.e. it
    /// sits at the **trailing** token boundary (`DEC_NUMBER`'s `(?![1-9])`,
    /// `OP`'s `(?![a-z])` — the other §4.2 fast-path case).
    pub at_concat_end: bool,
}

impl Node {
    /// Reconstruct the exact regex source this node was parsed from.
    pub fn to_source(&self) -> String {
        let mut out = String::new();
        self.write_source(&mut out);
        out
    }

    fn write_source(&self, out: &mut String) {
        match self {
            Node::Atom(s) => out.push_str(s),
            Node::Concat(parts) => {
                for p in parts {
                    p.write_source(out);
                }
            }
            Node::Alt(branches) => {
                for (i, b) in branches.iter().enumerate() {
                    if i > 0 {
                        out.push('|');
                    }
                    b.write_source(out);
                }
            }
            Node::Group { open, body, quant } => {
                out.push_str(open);
                body.write_source(out);
                out.push(')');
                out.push_str(quant);
            }
            Node::Assertion {
                neg,
                look,
                body,
                quant,
            } => {
                out.push_str(match (look, neg) {
                    (Look::Ahead, false) => "(?=",
                    (Look::Ahead, true) => "(?!",
                    (Look::Behind, false) => "(?<=",
                    (Look::Behind, true) => "(?<!",
                });
                body.write_source(out);
                out.push(')');
                out.push_str(quant);
            }
        }
    }

    /// Whether this node (or any descendant) contains a lookaround assertion. A
    /// terminal whose tree returns `false` is a plain `regex`-crate pattern and
    /// needs no lowering at all.
    pub fn has_assertion(&self) -> bool {
        match self {
            Node::Atom(_) => false,
            Node::Assertion { .. } => true,
            Node::Concat(parts) | Node::Alt(parts) => parts.iter().any(Node::has_assertion),
            Node::Group { body, .. } => body.has_assertion(),
        }
    }

    /// Enumerate every assertion in the tree, left-to-right, each tagged with the
    /// boundary context M2 uses to pick the lowering path (§4.2 boundary fast-path
    /// vs. §4.3 general internal lowering).
    pub fn assertions(&self) -> Vec<AssertionRef<'_>> {
        let mut out = Vec::new();
        self.collect_assertions(&mut out);
        out
    }

    fn collect_assertions<'a>(&'a self, out: &mut Vec<AssertionRef<'a>>) {
        match self {
            Node::Atom(_) => {}
            Node::Assertion {
                neg, look, body, ..
            } => {
                // A bare assertion not inside a Concat is treated as both ends.
                out.push(AssertionRef {
                    neg: *neg,
                    look: *look,
                    body,
                    at_concat_start: true,
                    at_concat_end: true,
                });
                body.collect_assertions(out);
            }
            Node::Concat(parts) => {
                let n = parts.len();
                for (i, p) in parts.iter().enumerate() {
                    if let Node::Assertion {
                        neg, look, body, ..
                    } = p
                    {
                        out.push(AssertionRef {
                            neg: *neg,
                            look: *look,
                            body,
                            at_concat_start: i == 0,
                            at_concat_end: i == n - 1,
                        });
                        body.collect_assertions(out);
                    } else {
                        p.collect_assertions(out);
                    }
                }
            }
            Node::Alt(branches) => {
                for b in branches {
                    b.collect_assertions(out);
                }
            }
            Node::Group { body, .. } => body.collect_assertions(out),
        }
    }
}

/// Parse a terminal regex `pattern` into a [`Node`] tree exposing its lookaround
/// assertions. The pattern is the bare regex source (no `/…/` delimiters, no
/// trailing flags — flags are stored separately on the [`PatternRe`] and applied
/// by the lexer, exactly as today).
///
/// Errors only on structurally malformed input the regex engines would also
/// reject (unbalanced `(`/`)`, an unterminated character class). Everything the
/// `regex` crate or `fancy-regex` accepts parses here.
///
/// [`PatternRe`]: crate::grammar::terminal::PatternRe
pub fn parse(pattern: &str) -> Result<Node, GrammarError> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut p = Parser {
        src: pattern,
        chars,
        pos: 0,
    };
    let node = p.parse_alternation()?;
    if p.pos != p.chars.len() {
        // A `)` with no matching `(` is the only way to stop early.
        return Err(p.err("unbalanced ')' in regex"));
    }
    Ok(node)
}

struct Parser<'a> {
    src: &'a str,
    chars: Vec<char>,
    pos: usize,
}

impl Parser<'_> {
    fn err(&self, msg: &str) -> GrammarError {
        GrammarError::InvalidRegex {
            pattern: self.src.to_string(),
            reason: msg.to_string(),
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn at(&self, offset: usize) -> Option<char> {
        self.chars.get(self.pos + offset).copied()
    }

    /// Parse `a|b|…` until end-of-input or an unmatched `)`. Returns a [`Node::Alt`]
    /// for ≥ 2 branches, otherwise the single branch's [`Node::Concat`].
    fn parse_alternation(&mut self) -> Result<Node, GrammarError> {
        let mut branches = vec![self.parse_concat()?];
        while self.peek() == Some('|') {
            self.pos += 1; // consume '|'
            branches.push(self.parse_concat()?);
        }
        if branches.len() == 1 {
            Ok(branches.pop().unwrap())
        } else {
            Ok(Node::Alt(branches))
        }
    }

    /// Parse a concatenation: a run of atoms, groups and assertions, stopping at a
    /// `|` or `)` (which belong to the caller) or end-of-input.
    fn parse_concat(&mut self) -> Result<Node, GrammarError> {
        let mut parts: Vec<Node> = Vec::new();
        // Accumulates verbatim assertion-free source; flushed as a `Node::Atom`
        // whenever a structural boundary (group / assertion) is hit.
        let mut atom = String::new();

        while let Some(c) = self.peek() {
            match c {
                '|' | ')' => break,
                '\\' => {
                    // Escape: keep the backslash and the next char together so a
                    // `\(` / `\)` / `\|` never reads as structure. (`\x41`, `\u….`,
                    // etc. need only the first char consumed here; the hex digits
                    // that follow are ordinary atom characters.)
                    atom.push('\\');
                    self.pos += 1;
                    if let Some(n) = self.peek() {
                        atom.push(n);
                        self.pos += 1;
                    }
                }
                '[' => {
                    // Character class: copy verbatim up to the closing `]`, honoring
                    // escapes and the literal-`]`-right-after-`[`/`[^` rule.
                    self.consume_char_class(&mut atom)?;
                }
                '(' => {
                    // Flush the pending atom, then parse the parenthesised construct.
                    if !atom.is_empty() {
                        parts.push(Node::Atom(std::mem::take(&mut atom)));
                    }
                    parts.push(self.parse_paren()?);
                }
                _ => {
                    atom.push(c);
                    self.pos += 1;
                }
            }
        }
        if !atom.is_empty() || parts.is_empty() {
            parts.push(Node::Atom(atom));
        }

        if parts.len() == 1 {
            Ok(parts.pop().unwrap())
        } else {
            Ok(Node::Concat(parts))
        }
    }

    /// Copy a `[...]` character class verbatim into `atom`, including the brackets.
    fn consume_char_class(&mut self, atom: &mut String) -> Result<(), GrammarError> {
        atom.push('['); // the '['
        self.pos += 1;
        if self.peek() == Some('^') {
            atom.push('^');
            self.pos += 1;
        }
        // A `]` as the very first class member is a literal, not the terminator.
        if self.peek() == Some(']') {
            atom.push(']');
            self.pos += 1;
        }
        loop {
            match self.peek() {
                None => return Err(self.err("unterminated character class '['")),
                Some('\\') => {
                    atom.push('\\');
                    self.pos += 1;
                    if let Some(n) = self.peek() {
                        atom.push(n);
                        self.pos += 1;
                    }
                }
                Some(']') => {
                    atom.push(']');
                    self.pos += 1;
                    return Ok(());
                }
                Some(c) => {
                    atom.push(c);
                    self.pos += 1;
                }
            }
        }
    }

    /// Parse a construct beginning with `(`: an assertion, or a (capturing,
    /// non-capturing, named, or flag-scoped) group. Assumes `self.peek() == '('`.
    fn parse_paren(&mut self) -> Result<Node, GrammarError> {
        // Classify by the characters right after '('.
        let assertion = match (self.at(1), self.at(2), self.at(3)) {
            (Some('?'), Some('='), _) => Some((false, Look::Ahead, 3)),
            (Some('?'), Some('!'), _) => Some((true, Look::Ahead, 3)),
            (Some('?'), Some('<'), Some('=')) => Some((false, Look::Behind, 4)),
            (Some('?'), Some('<'), Some('!')) => Some((true, Look::Behind, 4)),
            _ => None,
        };

        if let Some((neg, look, open_len)) = assertion {
            self.pos += open_len; // consume the assertion opener
            let body = self.parse_alternation()?;
            self.expect_close()?;
            let quant = self.consume_quantifier();
            return Ok(Node::Assertion {
                neg,
                look,
                body: Box::new(body),
                quant,
            });
        }

        // An ordinary group. Capture the exact opening delimiter so re-emission is
        // byte-identical: `(`, `(?:`, `(?P<name>`, `(?<name>`, `(?flags:`.
        let open = self.consume_group_open()?;
        let body = self.parse_alternation()?;
        self.expect_close()?;
        let quant = self.consume_quantifier();
        Ok(Node::Group {
            open,
            body: Box::new(body),
            quant,
        })
    }

    /// Consume and return a group's opening delimiter (everything from `(` up to and
    /// including the char that begins its body). Handles `(`, `(?:`, `(?P<name>`,
    /// `(?<name>`, and inline-flag-scoped `(?flags:`.
    fn consume_group_open(&mut self) -> Result<String, GrammarError> {
        let mut open = String::from("(");
        self.pos += 1; // '('
        if self.peek() != Some('?') {
            return Ok(open); // plain capturing group
        }
        open.push('?');
        self.pos += 1;
        match self.peek() {
            // Named group: `(?P<name>` or `(?<name>` — copy through the closing '>'.
            Some('P') => {
                open.push('P');
                self.pos += 1;
                self.consume_named_group_open(&mut open)?;
            }
            Some('<') => {
                self.consume_named_group_open(&mut open)?;
            }
            // Non-capturing or flag-scoped: copy through the ':' (or the closing ')'
            // of a bodiless inline-flag group, which `parse_paren`'s caller path does
            // not reach — a `(?flags)` has no body, so handle it as an atom instead).
            _ => {
                // Copy flag letters / ':' until we hit ':' (scoped group) — a
                // bodiless `(?flags)` was already routed to the atom path because it
                // contains no ':' before ')'. Guard against that here.
                loop {
                    match self.peek() {
                        Some(':') => {
                            open.push(':');
                            self.pos += 1;
                            break;
                        }
                        Some(')') | None => {
                            return Err(self.err(
                                "unsupported or bodiless group construct '(?…)'; \
                                 expected a ':' before ')'",
                            ));
                        }
                        Some(c) => {
                            open.push(c);
                            self.pos += 1;
                        }
                    }
                }
            }
        }
        Ok(open)
    }

    /// Copy a named-group opener `<name>` (the leading `(?P` / `(?` already taken)
    /// up to and including the `>`.
    fn consume_named_group_open(&mut self, open: &mut String) -> Result<(), GrammarError> {
        // self.peek() == '<'
        loop {
            match self.peek() {
                Some('>') => {
                    open.push('>');
                    self.pos += 1;
                    return Ok(());
                }
                Some(c) => {
                    open.push(c);
                    self.pos += 1;
                }
                None => return Err(self.err("unterminated named group '(?<…'")),
            }
        }
    }

    /// Consume a `)` that closes the current group/assertion.
    fn expect_close(&mut self) -> Result<(), GrammarError> {
        if self.peek() == Some(')') {
            self.pos += 1;
            Ok(())
        } else {
            Err(self.err("missing ')' in regex"))
        }
    }

    /// Consume a quantifier immediately following a group/assertion, if any:
    /// `*`, `+`, `?`, or `{m}` / `{m,}` / `{m,n}`, plus an optional trailing `?`
    /// (lazy) or `+` (possessive). Returns the consumed text (empty if none).
    fn consume_quantifier(&mut self) -> String {
        let mut q = String::new();
        match self.peek() {
            Some('*') | Some('+') | Some('?') => {
                q.push(self.peek().unwrap());
                self.pos += 1;
            }
            Some('{') => {
                // Only treat `{…}` as a quantifier if it is well-formed `{digits}` /
                // `{digits,}` / `{digits,digits}`; otherwise a literal `{` (left in
                // the following atom).
                if let Some(consumed) = self.try_consume_brace_quantifier() {
                    q.push_str(&consumed);
                } else {
                    return q;
                }
            }
            _ => return q,
        }
        // Optional laziness / possessiveness marker.
        if matches!(self.peek(), Some('?') | Some('+')) {
            q.push(self.peek().unwrap());
            self.pos += 1;
        }
        q
    }

    /// Try to consume a `{m}` / `{m,}` / `{m,n}` brace quantifier. Returns the
    /// consumed text on success and consumes it; on a non-quantifier `{` it consumes
    /// nothing and returns `None`.
    fn try_consume_brace_quantifier(&mut self) -> Option<String> {
        let start = self.pos;
        let mut s = String::from("{");
        let mut i = self.pos + 1;
        let mut saw_digit = false;
        while let Some(c) = self.chars.get(i).copied() {
            if c.is_ascii_digit() {
                saw_digit = true;
                s.push(c);
                i += 1;
            } else {
                break;
            }
        }
        if self.chars.get(i).copied() == Some(',') {
            s.push(',');
            i += 1;
            while let Some(c) = self.chars.get(i).copied() {
                if c.is_ascii_digit() {
                    s.push(c);
                    i += 1;
                } else {
                    break;
                }
            }
        }
        if saw_digit && self.chars.get(i).copied() == Some('}') {
            s.push('}');
            self.pos = i + 1;
            Some(s)
        } else {
            self.pos = start;
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Round-trip is the load-bearing invariant: M2 re-emits assertion-free
    /// fragments straight to `regex-automata`, so the parser must never lose or
    /// alter a single byte.
    fn assert_roundtrip(pattern: &str) {
        let node = parse(pattern).unwrap_or_else(|e| panic!("parse {pattern:?} failed: {e:?}"));
        assert_eq!(
            node.to_source(),
            pattern,
            "round-trip mismatch for {pattern:?}"
        );
    }

    #[test]
    fn roundtrips_ordinary_patterns() {
        for p in [
            "",
            "abc",
            "[a-z]+",
            "[^\\W\\d]\\w*", // python.lark NAME
            "a|b|c",
            "(a|b)*c",
            "(?:ab)+",
            "(?i:foo)",
            "(?P<x>ab)",
            "(?<name>ab)",
            "\\(\\)\\|\\[", // escaped metacharacters
            "a{3}b{2,}c{1,4}",
            "x{not a quant}", // literal braces, not a quantifier
            "[abc{]",         // brace inside a class
            "\\/\\*",         // escaped slashes
        ] {
            assert_roundtrip(p);
        }
    }

    #[test]
    fn roundtrips_all_corpus_lookaround_terminals() {
        // The exact bundled / examples patterns from the §4 Amendment census.
        for p in [
            "(?![1-9])",                                                   // DEC_NUMBER
            "[+*]|[?](?![a-z])",                                           // lark OP
            "\\/(?!\\/)(\\\\\\/|\\\\\\\\|[^\\/])*?\\/[imslux]*",           // lark REGEXP
            "([ubf]?r?|r[ubf])(\"(?!\"\").*?(?<!\\\\)(\\\\\\\\)*?\"|'(?!'').*?(?<!\\\\)(\\\\\\\\)*?')", // STRING
            "([ubf]?r?|r[ubf])(\"\"\".*?(?<!\\\\)(\\\\\\\\)*?\"\"\"|'''.*?(?<!\\\\)(\\\\\\\\)*?''')",   // LONG_STRING
            "\\/\\*(\\*(?!\\/)|[^*])*\\*\\/",                              // verilog MULTILINE_COMMENT
        ] {
            assert_roundtrip(p);
        }
    }

    #[test]
    fn dec_number_trailing_lookahead_is_a_boundary_assertion() {
        let node = parse("(?![1-9])").unwrap();
        let asserts = node.assertions();
        assert_eq!(asserts.len(), 1);
        let a = &asserts[0];
        assert!(a.neg);
        assert_eq!(a.look, Look::Ahead);
        assert_eq!(a.body.to_source(), "[1-9]");
        assert!(
            a.at_concat_start && a.at_concat_end,
            "bare assertion is both ends"
        );
    }

    #[test]
    fn op_trailing_lookahead_sits_at_branch_end() {
        // `[+*]|[?](?![a-z])` — the assertion is the *last* item of the second
        // branch's concat → a trailing boundary assertion.
        let node = parse("[+*]|[?](?![a-z])").unwrap();
        let asserts = node.assertions();
        assert_eq!(asserts.len(), 1);
        assert!(asserts[0].at_concat_end);
        assert!(!asserts[0].at_concat_start);
        assert_eq!(asserts[0].body.to_source(), "[a-z]");
    }

    #[test]
    fn string_guards_are_internal_assertions() {
        // The §4 Amendment's headline correction: STRING's assertions are interior.
        let p = "([ubf]?r?|r[ubf])(\"(?!\"\").*?(?<!\\\\)(\\\\\\\\)*?\"|'(?!'').*?(?<!\\\\)(\\\\\\\\)*?')";
        let node = parse(p).unwrap();
        let asserts = node.assertions();
        // Four assertions: (?!"") , (?<!\\) , (?!'') , (?<!\\) .
        assert_eq!(asserts.len(), 4, "got {asserts:#?}");
        // None of them is at a token boundary — every one is mid-concat.
        for a in &asserts {
            assert!(
                !a.at_concat_start && !a.at_concat_end,
                "STRING assertion should be internal: {a:?}"
            );
        }
        assert_eq!(asserts[0].look, Look::Ahead);
        assert!(asserts[0].neg);
        assert_eq!(asserts[1].look, Look::Behind);
        assert!(asserts[1].neg);
    }

    #[test]
    fn regexp_forbid_slash_is_internal() {
        // lark REGEXP: `\/(?!\/)…` — the assertion follows the opening `\/`, so it
        // is internal (not at the leading boundary).
        let node = parse("\\/(?!\\/)(\\\\\\/|\\\\\\\\|[^\\/])*?\\/[imslux]*").unwrap();
        let asserts = node.assertions();
        assert_eq!(asserts.len(), 1);
        assert!(!asserts[0].at_concat_start, "follows the opening slash");
        assert_eq!(asserts[0].body.to_source(), "\\/");
    }

    #[test]
    fn verilog_assertion_is_nested_inside_a_repetition() {
        // `\/\*(\*(?!\/)|[^*])*\*\/` — the assertion lives inside a `(…)*` group,
        // the deepest "internal" case. It must still be found, and re-emit exactly.
        let node = parse("\\/\\*(\\*(?!\\/)|[^*])*\\*\\/").unwrap();
        assert!(node.has_assertion());
        let asserts = node.assertions();
        assert_eq!(asserts.len(), 1);
        assert_eq!(asserts[0].body.to_source(), "\\/");
    }

    #[test]
    fn plain_pattern_has_no_assertion() {
        let node = parse("[^\\W\\d]\\w*").unwrap();
        assert!(!node.has_assertion());
        assert!(node.assertions().is_empty());
    }

    #[test]
    fn rejects_unbalanced_parens() {
        assert!(parse("(ab").is_err());
        assert!(parse("ab)").is_err());
        assert!(parse("(?=ab").is_err());
    }
}
