//! Lowering engine — milestone **M2** of the Lexer DFA / B1 plan
//! (`docs/LEXER_DFA_PLAN.md`).
//!
//! Compiles a lookaround-bearing terminal (an M1 [`Node`] tree) into a linear,
//! backtracking-free matcher that reproduces Python `re`'s leftmost-first semantics
//! — including the *length-changing* case where a trailing assertion forces a greedy
//! quantifier to a **shorter** match (`a+(?!b)` on `"aaab"` → `"aa"`).
//!
//! ## Why a Pike VM
//!
//! A bounded lookaround denotes a regular language (§2 of the plan), so it *can* be
//! matched without backtracking. But a plain "does it match / longest match" NFA
//! simulation is not enough: Python picks the match by greedy/lazy **priority**, not
//! by length alone (`a|ab` on `"ab"` → `"a"`, the first alternative, even though
//! `"ab"` is longer). The technique that reproduces this in linear time is a
//! **Pike-VM-style priority simulation** (Thompson, Pike, Cox): threads ordered by
//! greedy/alternation priority advance in lockstep over the input, and each
//! assertion is a zero-width **gate** that simply *kills* the thread if it fails —
//! no backtracking, so no ReDoS. The first (highest-priority) thread to reach
//! `Match` cuts the lower-priority ones at that position, giving leftmost-first; a
//! later (longer) match only ever comes from a higher-priority greedy thread, so
//! overwriting with it is correct.
//!
//! ## Unicode correctness
//!
//! Character-class membership (`\w`, `[^\W\d]`, `.`, …) is delegated to the `regex`
//! crate (one anchored single-char test per class, cached), so the engine inherits
//! `regex`'s Unicode tables exactly — Python's default. Only the *structure*
//! (quantifier priority, alternation order, assertion gating) is hand-rolled.
//!
//! ## Scope
//!
//! Only **assertion-bearing** terminals are routed here; every plain terminal stays
//! on the combined `regex` scanner. A construct this engine cannot lower (a
//! backreference, a variable-width lookbehind — both rejected by Python too) yields
//! a clear [`GrammarError`] at build time, the §4.3 rejection path.

use std::collections::HashMap;

use regex::Regex;

use super::{Look, Node};
use crate::error::GrammarError;
use crate::grammar::terminal::flags as fbits;

/// Active inline flags (`i`, `m`, `s`, `x`) at a point in the pattern. Scoped groups
/// (`(?i:…)`) and the terminal's own flags / `g_regex_flags` all feed this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Flags {
    i: bool,
    m: bool,
    s: bool,
    x: bool,
}

impl Flags {
    fn from_u32(bits: u32) -> Self {
        Flags {
            i: bits & fbits::IGNORECASE != 0,
            m: bits & fbits::MULTILINE != 0,
            s: bits & fbits::DOTALL != 0,
            x: bits & fbits::VERBOSE != 0,
        }
    }

    /// The canonical flag-letter string for wrapping a sub-pattern, e.g. `"is"`.
    fn letters(self) -> String {
        let mut out = String::new();
        if self.i {
            out.push('i');
        }
        if self.m {
            out.push('m');
        }
        if self.s {
            out.push('s');
        }
        if self.x {
            out.push('x');
        }
        out
    }
}

// ─── Instruction set ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
enum AnchorKind {
    TextStart, // \A
    TextEnd,   // \z / \Z
    LineStart, // ^  (honors MULTILINE)
    LineEnd,   // $  (honors MULTILINE)
    WordB,     // \b
    NonWordB,  // \B
}

#[derive(Debug, Clone, Copy)]
enum Inst {
    /// Consume one char matching class `classes[id]`; advance.
    Char(usize),
    /// Zero-width anchor test.
    Anchor(AnchorKind),
    /// Try `0` first (higher priority), then `1`.
    Split(usize, usize),
    Jmp(usize),
    /// Zero-width lookaround gate `asserts[id]`.
    Assert(usize),
    Match,
}

// ─── Single-char class membership (Unicode-correct, via `regex`) ──────────────

/// Tests whether one `char` is in a class, by an anchored single-char `regex` match.
/// Building the `Regex` once and reusing it keeps this `O(1)` per char.
struct Membership {
    re: Regex,
}

impl Membership {
    fn new(src: &str) -> Result<Self, GrammarError> {
        let re = Regex::new(src).map_err(|e| GrammarError::InvalidRegex {
            pattern: src.to_string(),
            reason: e.to_string(),
        })?;
        Ok(Membership { re })
    }

    #[inline]
    fn matches(&self, ch: char) -> bool {
        let mut buf = [0u8; 4];
        self.re.is_match(ch.encode_utf8(&mut buf))
    }
}

// ─── Compiled program ─────────────────────────────────────────────────────────

struct Assertion {
    look: Look,
    neg: bool,
    body: Program,
    /// Fixed char-width of `body` for a lookbehind (`Behind` ⇒ always `Some`).
    width: Option<usize>,
}

/// A self-contained compiled matcher: instructions + their class/assertion tables.
struct Program {
    insts: Vec<Inst>,
    classes: Vec<Membership>,
    asserts: Vec<Assertion>,
    /// `\w` membership, built lazily only when a word-boundary anchor is used.
    word: Option<Membership>,
}

/// A lowered matcher for one lookaround-bearing terminal.
pub struct LoweredMatcher {
    prog: Program,
}

impl LoweredMatcher {
    /// Compile a parsed pattern `node` under `flags` (the terminal's own flags OR'd
    /// with `g_regex_flags`). Returns a clear error for an unlowerable construct.
    pub fn compile(node: &Node, flags: u32) -> Result<Self, GrammarError> {
        let prog = Program::compile(node, Flags::from_u32(flags))?;
        Ok(LoweredMatcher { prog })
    }

    /// End byte offset of the (leftmost-first) match beginning exactly at `pos` in
    /// the full `text`, or `None`. The full text is passed so a lookbehind sees the
    /// bytes before `pos`. An empty match is reported as `None` (terminals are
    /// non-nullable).
    pub fn match_at(&self, text: &str, pos: usize) -> Option<usize> {
        match self.prog.run(text, pos) {
            Some(end) if end > pos => Some(end),
            _ => None,
        }
    }

    /// End offset of a non-empty match anchored at the start of `sub`. Used by the
    /// dynamic lexer's shorter-tokenization exploration, which matches against a
    /// truncated slice with no preceding context — mirroring the existing
    /// `AnyRegex::match_end_in`.
    pub fn match_in(&self, sub: &str) -> Option<usize> {
        match self.prog.run(sub, 0) {
            Some(end) if end > 0 => Some(end),
            _ => None,
        }
    }
}

// ─── Compilation ──────────────────────────────────────────────────────────────

struct Builder {
    insts: Vec<Inst>,
    classes: Vec<Membership>,
    class_cache: HashMap<String, usize>,
    asserts: Vec<Assertion>,
    word: Option<Membership>,
}

impl Builder {
    fn new() -> Self {
        Builder {
            insts: Vec::new(),
            classes: Vec::new(),
            class_cache: HashMap::new(),
            asserts: Vec::new(),
            word: None,
        }
    }

    fn push(&mut self, inst: Inst) -> usize {
        let i = self.insts.len();
        self.insts.push(inst);
        i
    }

    fn len(&self) -> usize {
        self.insts.len()
    }

    /// Intern a single-char class given its membership-regex source (already
    /// flag-wrapped). Identical sources share one compiled `Membership`.
    fn intern_class(&mut self, src: String) -> Result<usize, GrammarError> {
        if let Some(&id) = self.class_cache.get(&src) {
            return Ok(id);
        }
        let id = self.classes.len();
        self.classes.push(Membership::new(&src)?);
        self.class_cache.insert(src, id);
        Ok(id)
    }
}

impl Program {
    fn compile(node: &Node, flags: Flags) -> Result<Program, GrammarError> {
        let mut b = Builder::new();
        compile_node(&mut b, node, flags)?;
        b.push(Inst::Match);
        Ok(Program {
            insts: b.insts,
            classes: b.classes,
            asserts: b.asserts,
            word: b.word,
        })
    }
}

/// Emit instructions for `node` into `b`.
fn compile_node(b: &mut Builder, node: &Node, flags: Flags) -> Result<(), GrammarError> {
    match node {
        Node::Atom(s) => compile_atom(b, s, flags),
        Node::Concat(parts) => {
            for p in parts {
                compile_node(b, p, flags)?;
            }
            Ok(())
        }
        Node::Alt(branches) => compile_alt(b, branches, flags),
        Node::Group { open, body, quant } => {
            let child_flags = group_flags(open, flags)?;
            let q = Quant::parse(quant)?;
            quantify(b, q, |bb| compile_node(bb, body, child_flags))
        }
        Node::Assertion {
            neg,
            look,
            body,
            quant,
        } => {
            let body_prog = Program::compile(body, flags)?;
            let width =
                match look {
                    Look::Behind => Some(fixed_width(body, flags).ok_or_else(|| {
                        GrammarError::InvalidRegex {
                            pattern: body.to_source(),
                            reason: "look-behind requires a fixed-width pattern".to_string(),
                        }
                    })?),
                    Look::Ahead => None,
                };
            let aid = b.asserts.len();
            b.asserts.push(Assertion {
                look: *look,
                neg: *neg,
                body: body_prog,
                width,
            });
            // A bare assertion is the norm; tolerate an optional `?` quantifier.
            // `*`/`+` on a zero-width assertion is degenerate (infinite loop) and
            // unsupported.
            let q = Quant::parse(quant)?;
            match q.kind {
                QuantKind::One => {
                    b.push(Inst::Assert(aid));
                    Ok(())
                }
                QuantKind::Opt => {
                    let split = b.push(Inst::Split(0, 0));
                    let bstart = b.len();
                    b.push(Inst::Assert(aid));
                    let end = b.len();
                    b.insts[split] = if q.lazy {
                        Inst::Split(end, bstart)
                    } else {
                        Inst::Split(bstart, end)
                    };
                    Ok(())
                }
                _ => Err(GrammarError::InvalidRegex {
                    pattern: node.to_source(),
                    reason: "a repeated zero-width assertion is not supported".to_string(),
                }),
            }
        }
    }
}

/// Emit a priority-ordered alternation: branch order *is* the priority (leftmost
/// first), matching Python `re`.
fn compile_alt(b: &mut Builder, branches: &[Node], flags: Flags) -> Result<(), GrammarError> {
    let mut jmp_ends = Vec::new();
    let n = branches.len();
    for (i, br) in branches.iter().enumerate() {
        if i < n - 1 {
            let split = b.push(Inst::Split(0, 0));
            let bstart = b.len();
            compile_node(b, br, flags)?;
            jmp_ends.push(b.push(Inst::Jmp(0)));
            let next = b.len();
            b.insts[split] = Inst::Split(bstart, next);
        } else {
            compile_node(b, br, flags)?;
        }
    }
    let end = b.len();
    for j in jmp_ends {
        b.insts[j] = Inst::Jmp(end);
    }
    Ok(())
}

// ─── Quantifiers ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QuantKind {
    One,
    Opt,
    Star,
    Plus,
    Range(usize, Option<usize>),
}

#[derive(Debug, Clone, Copy)]
struct Quant {
    kind: QuantKind,
    lazy: bool,
}

impl Quant {
    fn one() -> Self {
        Quant {
            kind: QuantKind::One,
            lazy: false,
        }
    }

    /// Parse a quantifier suffix string (`""`, `"*"`, `"+?"`, `"{1,3}"`, …).
    fn parse(s: &str) -> Result<Quant, GrammarError> {
        if s.is_empty() {
            return Ok(Quant::one());
        }
        let chars: Vec<char> = s.chars().collect();
        let (kind, mut idx) = match chars[0] {
            '*' => (QuantKind::Star, 1),
            '+' => (QuantKind::Plus, 1),
            '?' => (QuantKind::Opt, 1),
            '{' => {
                let close = chars.iter().position(|&c| c == '}').ok_or_else(|| {
                    GrammarError::InvalidRegex {
                        pattern: s.to_string(),
                        reason: "unterminated '{' quantifier".to_string(),
                    }
                })?;
                let inner: String = chars[1..close].iter().collect();
                let kind = parse_brace(&inner).ok_or_else(|| GrammarError::InvalidRegex {
                    pattern: s.to_string(),
                    reason: "malformed '{m,n}' quantifier".to_string(),
                })?;
                (kind, close + 1)
            }
            other => {
                return Err(GrammarError::InvalidRegex {
                    pattern: s.to_string(),
                    reason: format!("unexpected quantifier char {other:?}"),
                })
            }
        };
        let lazy = matches!(chars.get(idx), Some('?'));
        if lazy || matches!(chars.get(idx), Some('+')) {
            idx += 1; // consume lazy '?' or possessive '+' (treated as greedy)
        }
        if idx != chars.len() {
            return Err(GrammarError::InvalidRegex {
                pattern: s.to_string(),
                reason: "trailing characters after quantifier".to_string(),
            });
        }
        Ok(Quant { kind, lazy })
    }
}

fn parse_brace(inner: &str) -> Option<QuantKind> {
    if let Some((a, bpart)) = inner.split_once(',') {
        let m: usize = if a.is_empty() { 0 } else { a.parse().ok()? };
        let n = if bpart.is_empty() {
            None
        } else {
            Some(bpart.parse().ok()?)
        };
        if let Some(n) = n {
            if n < m {
                return None;
            }
        }
        Some(QuantKind::Range(m, n))
    } else {
        let m: usize = inner.parse().ok()?;
        Some(QuantKind::Range(m, Some(m)))
    }
}

/// Wrap the instructions emitted by `emit` in `quant`'s loop/branch structure,
/// re-emitting the body as many times as a bounded repetition needs.
fn quantify(
    b: &mut Builder,
    quant: Quant,
    mut emit: impl FnMut(&mut Builder) -> Result<(), GrammarError>,
) -> Result<(), GrammarError> {
    match quant.kind {
        QuantKind::One => emit(b),
        QuantKind::Opt => {
            let split = b.push(Inst::Split(0, 0));
            let bstart = b.len();
            emit(b)?;
            let end = b.len();
            b.insts[split] = order(quant.lazy, bstart, end);
            Ok(())
        }
        QuantKind::Star => {
            let l1 = b.push(Inst::Split(0, 0));
            let bstart = b.len();
            emit(b)?;
            b.push(Inst::Jmp(l1));
            let end = b.len();
            b.insts[l1] = order(quant.lazy, bstart, end);
            Ok(())
        }
        QuantKind::Plus => {
            let bstart = b.len();
            emit(b)?;
            let split = b.push(Inst::Split(0, 0));
            let end = b.len();
            b.insts[split] = order(quant.lazy, bstart, end);
            Ok(())
        }
        QuantKind::Range(m, n) => {
            for _ in 0..m {
                emit(b)?;
            }
            match n {
                None => {
                    let l1 = b.push(Inst::Split(0, 0));
                    let bstart = b.len();
                    emit(b)?;
                    b.push(Inst::Jmp(l1));
                    let end = b.len();
                    b.insts[l1] = order(quant.lazy, bstart, end);
                }
                Some(n) => {
                    let mut splits = Vec::new();
                    for _ in m..n {
                        let split = b.push(Inst::Split(0, 0));
                        let bstart = b.len();
                        emit(b)?;
                        splits.push((split, bstart));
                    }
                    let end = b.len();
                    for (split, bstart) in splits {
                        b.insts[split] = order(quant.lazy, bstart, end);
                    }
                }
            }
            Ok(())
        }
    }
}

/// A `Split` preferring the body (greedy) or the exit (lazy).
#[inline]
fn order(lazy: bool, body: usize, exit: usize) -> Inst {
    if lazy {
        Inst::Split(exit, body)
    } else {
        Inst::Split(body, exit)
    }
}

// ─── Atom (assertion-free run) sub-parser ─────────────────────────────────────

/// One single-char-matching unit, or a zero-width anchor.
enum Unit {
    /// A literal character (regex-escaped when building its membership test).
    Lit(char),
    /// A source fragment that already matches exactly one char (`\d`, `\.`, `.`,
    /// `[...]`, `\xHH`, …) — used verbatim.
    Verbatim(String),
    Anchor(AnchorKind),
}

/// Compile an assertion-free atom string into instructions. Because M1 already
/// pulled groups, alternation and assertions out into [`Node`]s, an atom is a flat
/// run of single-char units and anchors, each optionally quantified.
fn compile_atom(b: &mut Builder, src: &str, flags: Flags) -> Result<(), GrammarError> {
    let chars: Vec<char> = src.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        // VERBOSE: skip unescaped whitespace and `#…` comments between units.
        if flags.x && (c.is_whitespace() || c == '#') {
            if c == '#' {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            } else {
                i += 1;
            }
            continue;
        }

        let unit = parse_unit(&chars, &mut i, src)?;
        // A quantifier may follow (anchors do not take one in practice; if one
        // appears after an anchor we treat the anchor as un-quantified and let the
        // quantifier bind to nothing — but valid patterns never do this).
        match unit {
            Unit::Anchor(kind) => {
                if let AnchorKind::WordB | AnchorKind::NonWordB = kind {
                    ensure_word(b, flags)?;
                }
                b.push(Inst::Anchor(kind));
            }
            char_unit => {
                let q = parse_quant_at(&chars, &mut i, src)?;
                let mem_src = membership_src(&char_unit, flags);
                let cid = b.intern_class(mem_src)?;
                quantify(b, q, |bb| {
                    bb.push(Inst::Char(cid));
                    Ok(())
                })?;
            }
        }
    }
    Ok(())
}

/// Parse one unit starting at `chars[*i]`, advancing `*i` past it.
fn parse_unit(chars: &[char], i: &mut usize, src: &str) -> Result<Unit, GrammarError> {
    let c = chars[*i];
    match c {
        '\\' => parse_escape(chars, i, src),
        '[' => {
            let class = consume_class(chars, i, src)?;
            Ok(Unit::Verbatim(class))
        }
        '.' => {
            *i += 1;
            Ok(Unit::Verbatim(".".to_string()))
        }
        '^' => {
            *i += 1;
            Ok(Unit::Anchor(AnchorKind::LineStart))
        }
        '$' => {
            *i += 1;
            Ok(Unit::Anchor(AnchorKind::LineEnd))
        }
        _ => {
            *i += 1;
            Ok(Unit::Lit(c))
        }
    }
}

fn parse_escape(chars: &[char], i: &mut usize, src: &str) -> Result<Unit, GrammarError> {
    // chars[*i] == '\\'
    let next = *chars
        .get(*i + 1)
        .ok_or_else(|| GrammarError::InvalidRegex {
            pattern: src.to_string(),
            reason: "trailing backslash".to_string(),
        })?;
    match next {
        'A' => {
            *i += 2;
            Ok(Unit::Anchor(AnchorKind::TextStart))
        }
        'Z' | 'z' => {
            *i += 2;
            Ok(Unit::Anchor(AnchorKind::TextEnd))
        }
        'b' => {
            *i += 2;
            Ok(Unit::Anchor(AnchorKind::WordB))
        }
        'B' => {
            *i += 2;
            Ok(Unit::Anchor(AnchorKind::NonWordB))
        }
        '1'..='9' => Err(GrammarError::InvalidRegex {
            pattern: src.to_string(),
            reason: "backreferences are not supported".to_string(),
        }),
        'x' | 'u' | 'U' => {
            // Keep the whole hex escape (`\xHH`, `\x{…}`, `\uHHHH`, `\U…`) verbatim.
            let mut s = String::from("\\");
            s.push(next);
            *i += 2;
            if chars.get(*i) == Some(&'{') {
                while *i < chars.len() {
                    s.push(chars[*i]);
                    let done = chars[*i] == '}';
                    *i += 1;
                    if done {
                        break;
                    }
                }
            } else {
                let n = if next == 'x' {
                    2
                } else if next == 'u' {
                    4
                } else {
                    8
                };
                for _ in 0..n {
                    if let Some(&h) = chars.get(*i) {
                        if h.is_ascii_hexdigit() {
                            s.push(h);
                            *i += 1;
                        }
                    }
                }
            }
            Ok(Unit::Verbatim(s))
        }
        _ => {
            // `\d \w \s \D \W \S` and ordinary escaped literals (`\.`, `\\`, `\/`,
            // `\n`, …) all match one char — keep verbatim.
            *i += 2;
            Ok(Unit::Verbatim(format!("\\{next}")))
        }
    }
}

/// Copy a `[...]` character class verbatim (including brackets), honoring escapes
/// and the literal-`]`-after-`[`/`[^` rule.
fn consume_class(chars: &[char], i: &mut usize, src: &str) -> Result<String, GrammarError> {
    let mut out = String::from("[");
    *i += 1;
    if chars.get(*i) == Some(&'^') {
        out.push('^');
        *i += 1;
    }
    if chars.get(*i) == Some(&']') {
        out.push(']');
        *i += 1;
    }
    loop {
        match chars.get(*i) {
            None => {
                return Err(GrammarError::InvalidRegex {
                    pattern: src.to_string(),
                    reason: "unterminated character class".to_string(),
                })
            }
            Some('\\') => {
                out.push('\\');
                *i += 1;
                if let Some(&n) = chars.get(*i) {
                    out.push(n);
                    *i += 1;
                }
            }
            Some(']') => {
                out.push(']');
                *i += 1;
                return Ok(out);
            }
            Some(&ch) => {
                out.push(ch);
                *i += 1;
            }
        }
    }
}

/// Parse an optional quantifier at `chars[*i]`, advancing past it. Returns
/// [`Quant::one`] when none is present.
fn parse_quant_at(chars: &[char], i: &mut usize, src: &str) -> Result<Quant, GrammarError> {
    let Some(&c) = chars.get(*i) else {
        return Ok(Quant::one());
    };
    let mut q = String::new();
    match c {
        '*' | '+' | '?' => {
            q.push(c);
            *i += 1;
        }
        '{' => {
            // Only a well-formed `{m,n}` is a quantifier; a lone `{` is a literal,
            // already consumed as a Lit unit (so here we just look ahead).
            if let Some(close) = chars[*i..].iter().position(|&c| c == '}') {
                let inner: String = chars[*i + 1..*i + close].iter().collect();
                if parse_brace(&inner).is_some() {
                    for &qc in &chars[*i..=*i + close] {
                        q.push(qc);
                    }
                    *i += close + 1;
                } else {
                    return Ok(Quant::one());
                }
            } else {
                return Ok(Quant::one());
            }
        }
        _ => return Ok(Quant::one()),
    }
    if matches!(chars.get(*i), Some('?') | Some('+')) {
        q.push(chars[*i]);
        *i += 1;
    }
    Quant::parse(&q).map_err(|_| GrammarError::InvalidRegex {
        pattern: src.to_string(),
        reason: "malformed quantifier".to_string(),
    })
}

/// The anchored single-char membership-regex source for a unit, under `flags`.
fn membership_src(unit: &Unit, flags: Flags) -> String {
    let inner = match unit {
        Unit::Lit(c) => regex::escape(&c.to_string()),
        Unit::Verbatim(s) => s.clone(),
        Unit::Anchor(_) => unreachable!("anchors are not class units"),
    };
    let letters = flags.letters();
    if letters.is_empty() {
        format!("^(?:{inner})$")
    } else {
        format!("^(?:(?{letters}:{inner}))$")
    }
}

/// Build the `\w` membership the word-boundary anchors need (lazily, once).
fn ensure_word(b: &mut Builder, flags: Flags) -> Result<(), GrammarError> {
    if b.word.is_none() {
        let letters = flags.letters();
        let src = if letters.is_empty() {
            "^(?:\\w)$".to_string()
        } else {
            format!("^(?:(?{letters}:\\w))$")
        };
        b.word = Some(Membership::new(&src)?);
    }
    Ok(())
}

// ─── Group flag scoping ───────────────────────────────────────────────────────

/// Compute the flags inside a group, given its opening delimiter. Only a
/// flag-scoped group `(?flags:` (optionally with `-neg`) changes them.
fn group_flags(open: &str, parent: Flags) -> Result<Flags, GrammarError> {
    // Forms: "(", "(?:", "(?P<name>", "(?<name>", "(?flags:", "(?flags-flags:".
    if !open.starts_with("(?") || !open.ends_with(':') {
        return Ok(parent); // capturing / named / non-flag group
    }
    let mid = &open[2..open.len() - 1]; // between "(?" and ":"
    if mid.is_empty() {
        return Ok(parent); // "(?:"
    }
    let (pos, neg) = match mid.split_once('-') {
        Some((p, n)) => (p, n),
        None => (mid, ""),
    };
    let mut f = parent;
    for (part, on) in [(pos, true), (neg, false)] {
        for ch in part.chars() {
            match ch {
                'i' => f.i = on,
                'm' => f.m = on,
                's' => f.s = on,
                'x' => f.x = on,
                'a' | 'u' | 'L' => {} // unicode/ascii mode toggles — ignore here
                other => {
                    return Err(GrammarError::InvalidRegex {
                        pattern: open.to_string(),
                        reason: format!("unsupported inline flag {other:?}"),
                    })
                }
            }
        }
    }
    Ok(f)
}

// ─── Fixed-width computation (for look-behind) ────────────────────────────────

/// The fixed char-width of `node`, or `None` if it can match differing widths.
/// Python requires look-behind bodies to be fixed-width, so this gates them.
fn fixed_width(node: &Node, flags: Flags) -> Option<usize> {
    match node {
        Node::Atom(s) => atom_width(s, flags),
        Node::Concat(parts) => {
            let mut total = 0;
            for p in parts {
                total += fixed_width(p, flags)?;
            }
            Some(total)
        }
        Node::Alt(branches) => {
            let mut w = None;
            for br in branches {
                let bw = fixed_width(br, flags)?;
                match w {
                    None => w = Some(bw),
                    Some(x) if x == bw => {}
                    Some(_) => return None,
                }
            }
            w
        }
        Node::Group { open, body, quant } => {
            let child_flags = group_flags(open, flags).ok()?;
            let bw = fixed_width(body, child_flags)?;
            apply_quant_width(bw, quant)
        }
        Node::Assertion { .. } => Some(0), // zero-width
    }
}

/// Fixed width of an atom run (each char unit is width 1; anchors width 0; a
/// quantifier is fixed only if `min == max`).
fn atom_width(src: &str, flags: Flags) -> Option<usize> {
    let chars: Vec<char> = src.chars().collect();
    let mut i = 0;
    let mut total = 0;
    while i < chars.len() {
        let c = chars[i];
        if flags.x && (c.is_whitespace() || c == '#') {
            if c == '#' {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            } else {
                i += 1;
            }
            continue;
        }
        let unit = parse_unit(&chars, &mut i, src).ok()?;
        match unit {
            Unit::Anchor(_) => {} // zero-width, no quantifier
            _ => {
                let q = parse_quant_at(&chars, &mut i, src).ok()?;
                total += apply_quant_width(1, quant_to_str(&q).as_str())?;
            }
        }
    }
    Some(total)
}

/// Width of `base` repeated per `quant` source — fixed only when `min == max`.
fn apply_quant_width(base: usize, quant: &str) -> Option<usize> {
    let q = Quant::parse(quant).ok()?;
    match q.kind {
        QuantKind::One => Some(base),
        QuantKind::Opt | QuantKind::Star | QuantKind::Plus => None,
        QuantKind::Range(m, Some(n)) if m == n => Some(base * m),
        QuantKind::Range(..) => None,
    }
}

/// Re-serialize a parsed [`Quant`] to source (only used to reuse
/// [`apply_quant_width`] from the atom path).
fn quant_to_str(q: &Quant) -> String {
    let base = match q.kind {
        QuantKind::One => return String::new(),
        QuantKind::Opt => "?".to_string(),
        QuantKind::Star => "*".to_string(),
        QuantKind::Plus => "+".to_string(),
        QuantKind::Range(m, None) => format!("{{{m},}}"),
        QuantKind::Range(m, Some(n)) => format!("{{{m},{n}}}"),
    };
    if q.lazy {
        format!("{base}?")
    } else {
        base
    }
}

// ─── Pike-VM execution ────────────────────────────────────────────────────────

/// A priority-ordered set of program counters at one input position. `seen`
/// deduplicates within a position (and so terminates ε-loops).
struct ThreadList {
    dense: Vec<usize>,
    seen: Vec<bool>,
}

impl ThreadList {
    fn new(n: usize) -> Self {
        ThreadList {
            dense: Vec::with_capacity(n),
            seen: vec![false; n],
        }
    }

    fn clear(&mut self) {
        self.dense.clear();
        self.seen.iter_mut().for_each(|x| *x = false);
    }
}

impl Program {
    /// Run the leftmost-first simulation anchored at byte `start`; return the byte
    /// end of the (possibly empty) match, or `None`.
    fn run(&self, text: &str, start: usize) -> Option<usize> {
        let mut clist = ThreadList::new(self.insts.len());
        let mut nlist = ThreadList::new(self.insts.len());
        let mut matched: Option<usize> = None;

        self.add_thread(&mut clist, 0, text, start);
        let mut pos = start;
        loop {
            let cur = text[pos..].chars().next();
            nlist.clear();
            for idx in 0..clist.dense.len() {
                let pc = clist.dense[idx];
                match self.insts[pc] {
                    Inst::Char(cid) => {
                        if let Some(ch) = cur {
                            if self.classes[cid].matches(ch) {
                                self.add_thread(&mut nlist, pc + 1, text, pos + ch.len_utf8());
                            }
                        }
                    }
                    Inst::Match => {
                        matched = Some(pos);
                        break; // cut lower-priority threads → leftmost-first
                    }
                    _ => {} // Split/Jmp/Assert/Anchor resolved in add_thread
                }
            }
            match cur {
                Some(ch) => {
                    std::mem::swap(&mut clist, &mut nlist);
                    pos += ch.len_utf8();
                }
                None => break,
            }
            if clist.dense.is_empty() {
                break;
            }
        }
        matched
    }

    /// Add `pc` and follow zero-width instructions (ε-closure) in priority order.
    fn add_thread(&self, list: &mut ThreadList, pc: usize, text: &str, pos: usize) {
        if list.seen[pc] {
            return;
        }
        list.seen[pc] = true;
        match self.insts[pc] {
            Inst::Jmp(t) => self.add_thread(list, t, text, pos),
            Inst::Split(a, b) => {
                self.add_thread(list, a, text, pos);
                self.add_thread(list, b, text, pos);
            }
            Inst::Assert(aid) => {
                if self.eval_assert(aid, text, pos) {
                    self.add_thread(list, pc + 1, text, pos);
                }
            }
            Inst::Anchor(kind) => {
                if self.eval_anchor(kind, text, pos) {
                    self.add_thread(list, pc + 1, text, pos);
                }
            }
            Inst::Char(_) | Inst::Match => list.dense.push(pc),
        }
    }

    fn eval_assert(&self, aid: usize, text: &str, pos: usize) -> bool {
        let a = &self.asserts[aid];
        let holds = match a.look {
            Look::Ahead => a.body.run(text, pos).is_some(),
            Look::Behind => {
                let width = a.width.expect("look-behind has a fixed width");
                // Byte offset `width` chars before `pos`.
                match nth_char_back(text, pos, width) {
                    Some(bstart) => {
                        // Body is fixed-width, so a full match of the window consumes
                        // exactly to `pos`.
                        a.body.run(&text[..pos], bstart) == Some(pos)
                    }
                    None => false, // not enough preceding chars
                }
            }
        };
        holds ^ a.neg
    }

    fn eval_anchor(&self, kind: AnchorKind, text: &str, pos: usize) -> bool {
        let prev = text[..pos].chars().next_back();
        let next = text[pos..].chars().next();
        let is_word = |c: Option<char>| match (c, &self.word) {
            (Some(ch), Some(w)) => w.matches(ch),
            _ => false,
        };
        match kind {
            AnchorKind::TextStart => pos == 0,
            AnchorKind::TextEnd => pos == text.len(),
            AnchorKind::LineStart => pos == 0 || prev == Some('\n'),
            AnchorKind::LineEnd => pos == text.len() || next == Some('\n'),
            AnchorKind::WordB => is_word(prev) != is_word(next),
            AnchorKind::NonWordB => is_word(prev) == is_word(next),
        }
    }
}

/// Byte offset `n` chars before `pos` in `text`, or `None` if fewer than `n`
/// precede it.
fn nth_char_back(text: &str, pos: usize, n: usize) -> Option<usize> {
    let mut off = pos;
    for _ in 0..n {
        let ch = text[..off].chars().next_back()?;
        off -= ch.len_utf8();
    }
    Some(off)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lookaround::parse;

    fn matcher(pattern: &str) -> LoweredMatcher {
        matcher_flags(pattern, 0)
    }

    fn matcher_flags(pattern: &str, flags: u32) -> LoweredMatcher {
        let node = parse(pattern).unwrap_or_else(|e| panic!("parse {pattern:?}: {e:?}"));
        LoweredMatcher::compile(&node, flags)
            .unwrap_or_else(|e| panic!("compile {pattern:?}: {e:?}"))
    }

    /// The matched prefix of `text` (anchored at 0), or `None`.
    fn m<'t>(pattern: &str, text: &'t str) -> Option<&'t str> {
        matcher(pattern).match_at(text, 0).map(|e| &text[..e])
    }

    #[test]
    fn literals_and_classes() {
        assert_eq!(m("abc", "abcd"), Some("abc"));
        assert_eq!(m("abc", "abx"), None);
        assert_eq!(m("[a-z]+", "abc1"), Some("abc"));
        assert_eq!(m("[^\\W\\d]\\w*", "foo_9 bar"), Some("foo_9"));
        assert_eq!(m("\\d{2,4}", "12345"), Some("1234")); // greedy, capped at 4
        assert_eq!(m("a.c", "axc"), Some("axc"));
        assert_eq!(m("a.c", "a\nc"), None); // '.' excludes newline w/o DOTALL
    }

    #[test]
    fn greedy_lazy_and_alternation_priority() {
        assert_eq!(m("a*", "aaa"), Some("aaa")); // greedy
        assert_eq!(m("a*?b", "aaab"), Some("aaab")); // lazy still must reach b
        assert_eq!(m("a|ab", "ab"), Some("a")); // leftmost-first: first alt wins
        assert_eq!(m("ab|a", "ab"), Some("ab")); // first alt wins (longer here)
    }

    #[test]
    fn trailing_negative_lookahead_is_length_changing() {
        // a+(?!b): greedy a+ forced to a SHORTER match so (?!b) can hold.
        let mm = matcher("a+(?!b)");
        assert_eq!(mm.match_at("aaab", 0).map(|e| &"aaab"[..e]), Some("aa"));
        assert_eq!(mm.match_at("aaa", 0).map(|e| &"aaa"[..e]), Some("aaa"));
        assert_eq!(mm.match_at("ab", 0), None); // can't shrink below one 'a'
    }

    #[test]
    fn trailing_positive_lookahead() {
        let mm = matcher("a+(?=b)");
        assert_eq!(mm.match_at("aaab", 0).map(|e| &"aaab"[..e]), Some("aaa"));
        assert_eq!(mm.match_at("aaa", 0), None);
    }

    #[test]
    fn lookbehind_fixed_width() {
        // (?<=@)[a-z]+ at the position after '@'.
        let mm = matcher("(?<=@)[a-z]+");
        assert_eq!(mm.match_at("@abc", 1).map(|e| &"@abc"[1..e]), Some("abc"));
        assert_eq!(mm.match_at("xabc", 1), None); // preceding char is not '@'
                                                  // Negative look-behind: even-backslash STRING-guard shape.
        let q = matcher("(?<!\\\\)(?:\\\\\\\\)*\"");
        assert_eq!(q.match_at("\"", 0), Some(1)); // zero (even) backslashes
        assert_eq!(q.match_at("\\\\\"", 0), Some(3)); // two backslashes then quote
        assert_eq!(q.match_at("\\\"", 1), None); // one backslash escapes the quote
    }

    #[test]
    fn internal_assertion_inside_repetition() {
        // verilog MULTILINE_COMMENT body shape: `\*(?!\/)` inside a star.
        let mm = matcher("/\\*(\\*(?!/)|[^*])*\\*/");
        assert_eq!(m_str(&mm, "/* a */"), Some("/* a */"));
        assert_eq!(m_str(&mm, "/* a * b */"), Some("/* a * b */"));
        assert_eq!(m_str(&mm, "/**/"), Some("/**/"));
    }

    fn m_str<'t>(mm: &LoweredMatcher, text: &'t str) -> Option<&'t str> {
        mm.match_at(text, 0).map(|e| &text[..e])
    }

    #[test]
    fn inline_and_global_flags_reach_assertions() {
        // (?i:END) inside a look-ahead body.
        let mm = matcher("a+(?=(?i:END))");
        assert_eq!(
            mm.match_at("aaaEND", 0).map(|e| &"aaaEND"[..e]),
            Some("aaa")
        );
        assert_eq!(
            mm.match_at("aaaend", 0).map(|e| &"aaaend"[..e]),
            Some("aaa")
        );
        assert_eq!(mm.match_at("aaaxyz", 0), None);

        // g_regex_flags=IGNORECASE reaches both the body and the assertion.
        let g = matcher_flags("a+(?!b)", fbits::IGNORECASE);
        assert_eq!(g.match_at("AAAB", 0).map(|e| &"AAAB"[..e]), Some("AA"));
    }

    #[test]
    fn rejects_unsupported_constructs() {
        let node = parse("(.)\\1").unwrap();
        assert!(LoweredMatcher::compile(&node, 0).is_err()); // backreference
        let node = parse("(?<=a+)b").unwrap();
        assert!(LoweredMatcher::compile(&node, 0).is_err()); // variable-width behind
    }
}
