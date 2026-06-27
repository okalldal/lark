//! Lookaround-aware regex front-end — the assertion parser the L2 bounded-lookaround
//! lowering builds on (`docs/LEXER_DFA_PLAN.md`).
//!
//! Resurrected from the closed [PR #110](https://github.com/okalldal/lark/pull/110),
//! whose `src/lookaround/` front-end is **not** on `master`. Per the DFA plan's
//! salvage map this module is re-landed *without* PR #110's `matcher.rs` (the runtime
//! Pike-VM): the lowering is a **DFA**, not a runtime lookaround executor, so the
//! Pike-VM is dropped and only the parser/classifier survives. The classifier that
//! decides which assertions are lowerable lives in [`classify`] and builds directly on
//! the [`Node`] tree this module produces.
//!
//! The `regex` crate (and its `regex-automata` layer) cannot parse lookaround
//! assertions — they reject `(?=…)`, `(?!…)`, `(?<=…)`, `(?<!…)` exactly as the
//! `regex` crate does. The L2 lowering retires `fancy-regex` from the *runtime* by
//! lowering every bounded assertion into a finite automaton, which first requires a
//! parser that can **see** the assertions in a terminal pattern. That is this module.
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
//!      `regex` crate or `fancy-regex` accepts. This is what lets the lowering hand
//!      every assertion-free fragment straight to the automaton builder by re-emitting
//!      its source — the front-end never has to *understand* a character class, only
//!      to not be confused by the `(`, `)`, `|` that may hide inside one.
//!   2. **Assertion exposure with position.** [`Node::assertions`] enumerates every
//!      assertion in left-to-right order together with its enclosing context
//!      (leading / trailing / internal), so the classifier can pick the lowering path
//!      — boundary fast-path vs. the general internal case — at each assertion's
//!      position.
//!
//! This module performs **no lowering** — it is purely the parse step. The classifier
//! ([`classify`]) consumes the tree it produces.

use crate::error::GrammarError;

pub mod classify;
pub mod lower;

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
/// reject (unbalanced `(`/`)`, an unterminated character class). Every pattern the
/// `regex` crate or `fancy-regex` accepts parses here, **except** a *named*
/// backreference (`(?P=name)` / `\k<name>`), which takes the named-group path and
/// errors — the safe direction (reject), since a backref is not a regular language
/// the lowering can accept anyway.
///
/// **Nesting cap (#455).** Group/alternation nesting is bounded at [`NEST_LIMIT`]
/// (mirroring `regex_syntax`'s default `nest_limit`); a pattern nested deeper returns
/// an `InvalidRegex` error rather than recursing the parser to a stack overflow. The
/// `regex` crate would itself reject such a pattern (its own `nest_limit`), so this
/// only refuses what the engines already refuse. Every caller of [`parse`] maps the
/// `Err` to a graceful fallback — `pattern_max_width` / `pattern_min_width_is_zero`
/// return `None`, and the classifier turns it into a categorized
/// `GrammarError::LookaroundScope` build error — so a pathological terminal fails the
/// grammar build gracefully instead of aborting the process.
///
/// [`PatternRe`]: crate::grammar::terminal::PatternRe
pub fn parse(pattern: &str) -> Result<Node, GrammarError> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut p = Parser {
        src: pattern,
        chars,
        pos: 0,
        depth: 0,
    };
    let node = p.parse_alternation()?;
    if p.pos != p.chars.len() {
        // A `)` with no matching `(` is the only way to stop early.
        return Err(p.err("unbalanced ')' in regex"));
    }
    Ok(node)
}

/// Whether `pattern`'s regexp can derive the empty string — i.e. its **minimum**
/// match width is zero. This is the lark-rs equivalent of Python Lark's
/// `get_regexp_width(regexp)[0] == 0` (`lark/utils.py`), the test both the basic
/// lexer (`Pattern.min_width == 0`) and the dynamic Earley lexer
/// (`parser_frontends.py::EarleyRegexpMatcher`) use to reject zero-width terminals.
///
/// Unlike a `Regex::new(src).is_match("")` probe it (a) sees lookaround/boundary
/// assertions the `regex` crate cannot even compile — `(?=…)`, `(?<=…)` — and (b)
/// agrees with Python on bare word boundaries: `\b` has `min_width == 0` in Python
/// (an `is_match("")` probe returns *false* for it, since the empty string has no
/// word boundary). Computed by parsing the pattern into the shared assertion-aware
/// [`Node`] tree and taking `width_range(...).0`, the single min/max-width routine
/// the whole `lookaround` module shares. A pattern this front-end cannot parse
/// (e.g. a genuine backreference) returns `None` — the caller then falls back to
/// its own probe rather than over-rejecting.
pub(crate) fn pattern_min_width_is_zero(pattern: &str) -> Option<bool> {
    let node = parse(pattern).ok()?;
    Some(lower::width_range(&node).0 == 0)
}

/// The **maximum** match width of `pattern` in characters — the lark-rs equivalent of
/// Python Lark's `get_regexp_width(regexp)[1]` (`sre_parse.getwidth()[1]`), the second
/// key of the terminal-ordering sort (`lark/lexer.py`). The outer `Option` reports
/// *parseability* (`None` = this front-end cannot parse the pattern, e.g. a genuine
/// backreference — the caller then falls back to its own sizing); the inner `Option`
/// is the width itself (`None` = unbounded, the `MAXWIDTH`/∞ Python reports for a `*` /
/// `+` / `{m,}`).
///
/// This is the assertion-aware counterpart of [`pattern_min_width_is_zero`]: it sees
/// lookaround/boundary assertions the `regex` crate cannot even compile (`(?=…)`,
/// `(?<=…)`, `\b`) and sizes them at their finite consumed width (assertions are
/// zero-width), exactly as `sre_parse` does — so a lowerable-lookaround terminal like
/// `/a(?=b)/` sizes to `1`, not unbounded. Computed via the same shared
/// [`width_range`](lower::width_range) walk, so the min and max sides can never drift.
pub(crate) fn pattern_max_width(pattern: &str) -> Option<Option<usize>> {
    let node = parse(pattern).ok()?;
    Some(lower::width_range(&node).1)
}

/// Maximum group/alternation nesting depth the front-end parser accepts before it
/// refuses with an `InvalidRegex` error (#455). Mirrors `regex_syntax`'s default
/// `nest_limit` of 250: a terminal regex nested deeper than this is already rejected
/// by the `regex` crate, so capping here only refuses what the engines refuse — and it
/// keeps the recursive descent (`parse_paren` → `parse_alternation`) from overflowing
/// the stack on an adversarial deeply-nested terminal.
pub(crate) const NEST_LIMIT: u32 = 250;

struct Parser<'a> {
    src: &'a str,
    chars: Vec<char>,
    pos: usize,
    /// Current group/assertion nesting depth; capped at [`NEST_LIMIT`] in
    /// [`Parser::parse_paren`] so the recursion cannot overflow the stack (#455).
    depth: u32,
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
    ///
    /// Every nested construct re-enters the parser through here, so this is the single
    /// place the [`NEST_LIMIT`] depth cap is enforced (#455): a group/assertion nested
    /// deeper than the limit returns an `InvalidRegex` error instead of recursing into
    /// `parse_alternation` and overflowing the stack. The depth is restored on the way
    /// out (success or error) so a wide-but-shallow pattern is unaffected.
    fn parse_paren(&mut self) -> Result<Node, GrammarError> {
        self.depth += 1;
        if self.depth > NEST_LIMIT {
            self.depth -= 1;
            return Err(self.err("regex nesting depth exceeds the limit"));
        }
        let result = self.parse_paren_inner();
        self.depth -= 1;
        result
    }

    fn parse_paren_inner(&mut self) -> Result<Node, GrammarError> {
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

        // Named backreference `(?P=name)` (N4) — Python's `re` spelling of a
        // backreference, keyed by group name. The `regex` crate rejects it (backrefs
        // are not a regular language), and so does this front-end's named-*group* path
        // (`consume_named_group_open` expects `<`, not `=`), which used to surface a raw
        // `InvalidRegex` parse error and let `PatternRe::new` leak an *uncategorized*
        // refusal. Keep it verbatim as a zero-width [`Node::Atom`] — exactly as the
        // escape-spelled `\k<name>` backref stays a verbatim atom — so the pattern
        // *parses*, reaches the lexer-build routing seam, and is refused with the
        // **categorized** `LookaroundScope::Backref` error (`BacktrackingOnlySyntax`,
        // OutOfScope), the same "not supported (by design) … a backreference" message
        // `\1`/`\k`/`\g` produce. General backreferences remain out of scope — this
        // categorizes the refusal, it does not promote them to support.
        if self.at(1) == Some('?') && self.at(2) == Some('P') && self.at(3) == Some('=') {
            if let Some(text) = self.try_named_backref() {
                return Ok(Node::Atom(text));
            }
        }

        // Bodiless inline-flag group `(?imsx)` / `(?i-s)` — the `regex` crate accepts
        // it; it sets flags for the rest of the *enclosing* group and has no body of
        // its own. Keep it verbatim as a zero-width [`Node::Atom`] so the tree still
        // round-trips and carries no assertion. (A flag-*scoped* `(?flags:…)` has a
        // body and stays on the group path below.)
        if self.at(1) == Some('?') {
            if let Some(text) = self.try_bodiless_flag_group() {
                return Ok(Node::Atom(text));
            }
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

    /// If the upcoming construct is a *bodiless* inline-flag group `(?<flags>)` —
    /// flag letters (and `-`) followed immediately by `)`, no `:` and no body —
    /// consume it and return its verbatim source. Otherwise consume nothing and
    /// return `None` (so `(?:`, `(?flags:…)`, `(?P<name>`, `(?<name>`, the
    /// assertions, and a named backref `(?P=name)` all stay on their own paths).
    fn try_bodiless_flag_group(&mut self) -> Option<String> {
        // self.peek() == '(' and self.at(1) == '?'
        let start = self.pos;
        let mut i = self.pos + 2; // past "(?"
        let mut saw_flag = false;
        while let Some(c) = self.chars.get(i).copied() {
            if c.is_ascii_alphabetic() || c == '-' {
                saw_flag = true;
                i += 1;
            } else {
                break;
            }
        }
        if saw_flag && self.chars.get(i).copied() == Some(')') {
            let text: String = self.chars[start..=i].iter().collect();
            self.pos = i + 1; // past the ')'
            Some(text)
        } else {
            None
        }
    }

    /// If the upcoming construct is a *named backreference* `(?P=name)` — `(?P=`
    /// followed by a non-empty group name and a closing `)` — consume it and return
    /// its verbatim source. Otherwise consume nothing and return `None` (so a genuine
    /// `(?P<name>…` named *group* stays on its own path). The name is kept verbatim;
    /// the backref's only consumer downstream is the categorized refusal, which never
    /// inspects the name.
    fn try_named_backref(&mut self) -> Option<String> {
        // self.peek() == '(', at(1) == '?', at(2) == 'P', at(3) == '='
        let start = self.pos;
        let mut i = self.pos + 4; // past "(?P="
        let mut saw_name = false;
        while let Some(c) = self.chars.get(i).copied() {
            if c == ')' {
                break;
            }
            saw_name = true;
            i += 1;
        }
        if saw_name && self.chars.get(i).copied() == Some(')') {
            let text: String = self.chars[start..=i].iter().collect();
            self.pos = i + 1; // past the ')'
            Some(text)
        } else {
            None
        }
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
            // Non-capturing or flag-scoped `(?flags:…)`: copy through the ':'. A
            // *bodiless* `(?flags)` never reaches here — `parse_paren` routes it to
            // the atom path via `try_bodiless_flag_group` first — so hitting `)`
            // before a ':' is a genuinely malformed/unsupported group.
            _ => loop {
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
            },
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
            "(?i)abc",      // bodiless inline-flag group (regex-crate accepts it)
            "(?ms)x",       // multiple flags, bodiless
            "(?i-s)y",      // flag set + clear, bodiless
            "a(?i)b(?-i)c", // flag toggles mid-pattern
            "(?P<x>ab)",
            "(?<name>ab)",
            "(?P<x>a)(?P=x)", // N4: a named backref round-trips as a verbatim atom
            "(?P=x)",
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

    #[test]
    fn bodiless_inline_flag_group_parses_and_has_no_assertion() {
        // Regression for the front-end contract: a bodiless `(?flags)` is accepted by
        // the `regex` crate, so it must parse here (not error like a malformed group)
        // and round-trip — it carries no assertion, so it is a plain terminal.
        for p in ["(?i)abc", "(?ms)x", "(?i-s)y", "a(?i)b(?-i)c"] {
            let node = parse(p).unwrap_or_else(|e| panic!("parse {p:?} failed: {e:?}"));
            assert_eq!(node.to_source(), p, "round-trip mismatch for {p:?}");
            assert!(!node.has_assertion(), "{p:?} has no assertion");
        }
        // A flag-*scoped* group still has a body and is preserved as a group.
        assert!(parse("(?i:abc)").unwrap().to_source() == "(?i:abc)");
    }

    /// N4: a Python named backreference `(?P=name)` parses (it used to error on the
    /// named-group path), round-trips verbatim, and — like the escape-spelled `\k<n>`
    /// — carries no *assertion* (it is a plain backref atom, refused downstream by the
    /// categorized backtracking-only route, not by the lookaround classifier). A
    /// genuine named *group* `(?P<name>…)` stays a group, not a backref.
    #[test]
    fn named_backref_parses_as_a_verbatim_atom() {
        for p in ["(?P=x)", "(?P<x>a)(?P=x)", "a(?P=name)b"] {
            let node = parse(p).unwrap_or_else(|e| panic!("parse {p:?} failed: {e:?}"));
            assert_eq!(node.to_source(), p, "round-trip mismatch for {p:?}");
            assert!(
                !node.has_assertion(),
                "{p:?} is a backref, not an assertion"
            );
        }
        // The named *group* definition is unaffected (still a group, no backref).
        assert_eq!(parse("(?P<x>ab)").unwrap().to_source(), "(?P<x>ab)");
    }

    // ─── #455: nesting depth cap (mirrors regex_syntax's nest_limit) ────────────
    //
    // The front-end's recursive descent (`parse_paren` → `parse_alternation`) used to
    // recurse on the regex nesting depth with no bound, so a terminal with thousands of
    // nested groups — even plain, non-lookaround ones — overflowed the stack and
    // *aborted the process* during a routine lexer build (`Pattern::max_width` /
    // `pattern_min_width_is_zero` call `parse` on every terminal). The cap turns that
    // process abort into a graceful, categorized refusal.

    /// Build a pattern of `depth` nested capturing groups around a literal `a`, e.g.
    /// `depth == 2` → `"((a))"`. This is the pathological shape #455 names.
    fn nested(depth: usize) -> String {
        format!("{}a{}", "(".repeat(depth), ")".repeat(depth))
    }

    #[test]
    fn deep_but_under_limit_still_parses() {
        // A terminal nested right up to the cap must still build (round-trips exactly).
        let p = nested(NEST_LIMIT as usize);
        let node = parse(&p).unwrap_or_else(|e| panic!("under-limit parse failed: {e:?}"));
        assert_eq!(node.to_source(), p, "under-limit pattern must round-trip");
    }

    #[test]
    fn over_limit_nesting_returns_categorized_error_not_overflow() {
        // One level past the cap is refused with an `InvalidRegex` error instead of
        // recursing — this is the categorized `GrammarError` the Done-when asks for.
        let p = nested(NEST_LIMIT as usize + 1);
        match parse(&p) {
            Err(GrammarError::InvalidRegex { reason, .. }) => {
                assert!(
                    reason.contains("nesting depth"),
                    "reason should name the nesting-depth cap, got: {reason}"
                );
            }
            other => panic!("expected InvalidRegex nesting-depth error, got {other:?}"),
        }
    }

    #[test]
    fn pathological_depth_does_not_abort_and_callers_fall_back_to_none() {
        // The headline #455 case: a *deeply* nested terminal (thousands of groups) that
        // previously overflowed the stack / aborted the process. With the cap, `parse`
        // returns an `Err` (it never recurses past the limit), and the two width
        // callers that route through it (`pattern_max_width`,
        // `pattern_min_width_is_zero` — called on every lexer build) fall back to `None`
        // instead of aborting. Reaching these asserts at all is the evidence the build
        // no longer aborts.
        let p = nested(50_000);
        assert!(
            parse(&p).is_err(),
            "pathological depth must error, not abort"
        );
        assert_eq!(
            pattern_max_width(&p),
            None,
            "max-width caller falls back to None on an unparseable terminal"
        );
        assert_eq!(
            pattern_min_width_is_zero(&p),
            None,
            "min-width caller falls back to None on an unparseable terminal"
        );
    }

    #[test]
    fn deep_nesting_inside_a_lookaround_assertion_is_capped() {
        // The cap counts assertion bodies too (the lookaround front-end's reason for
        // existing), so a deeply-nested assertion body is refused just like a plain
        // group nest — not overflowed.
        let p = format!("(?={})", nested(50_000));
        assert!(
            matches!(parse(&p), Err(GrammarError::InvalidRegex { .. })),
            "deep nesting inside an assertion must be capped"
        );
    }
}
