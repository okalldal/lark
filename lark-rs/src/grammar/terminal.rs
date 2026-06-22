use crate::error::GrammarError;
use regex::Regex;

/// Pattern for matching a terminal — either a fixed string or a regex.
#[derive(Debug, Clone)]
pub enum Pattern {
    Str(PatternStr),
    Re(PatternRe),
}

impl Pattern {
    pub fn as_regex_str(&self) -> &str {
        match self {
            Pattern::Str(p) => &p.escaped,
            Pattern::Re(p) => &p.pattern,
        }
    }

    /// Maximum number of **characters** this pattern can match (`None` = unbounded),
    /// mirroring Python Lark's `Pattern.max_width` (`sre_parse.getwidth()[1]`, which
    /// is `MAXWIDTH`/∞ for an unbounded pattern). This is the load-bearing second key
    /// of the terminal-ordering sort (`lark/lexer.py:583`,
    /// `(-priority, -max_width, -len(value), name)`): a finite regex must sort
    /// *behind* a genuinely-unbounded one, so a maximal greedy match wins (#268, RC5).
    ///
    /// For a regex we parse its source to a `regex-syntax` HIR and walk it counting
    /// characters; a pattern the parser rejects (lookaround/backref idioms — Python
    /// `re` constructs the linear engine doesn't model) falls back to `None`
    /// (unbounded), the conservative "sort first" default and the same outcome
    /// Python's own `MAXWIDTH` fallback produces for a pattern `sre_parse` can't size.
    pub fn max_width(&self) -> Option<usize> {
        match self {
            Pattern::Str(p) => Some(p.value.chars().count()),
            Pattern::Re(p) => regex_syntax::parse(&p.pattern)
                .ok()
                .and_then(|hir| hir_max_width_chars(&hir)),
        }
    }

    /// The raw pattern length Python's terminal-ordering tiebreak uses
    /// (`len(pattern.value)` — the source *without* any flag wrapper, since Python
    /// stores flags separately on the `Pattern`). lark-rs's loader bakes a terminal's
    /// flags into the stored regex string as a scoped group (`(?i:aa)`), so a naive
    /// `pattern.len()` would count the wrapper and give a flagged terminal a phantom
    /// rank boost (#268, N2). Stripping the whole-pattern flag wrapper first restores
    /// parity: `/aa/` and `/aa/i` both report a raw length of 2 and the tiebreak falls
    /// through to the name sort, exactly as in Python.
    pub fn raw_value_len(&self) -> usize {
        match self {
            // A `PatternStr`'s value is the literal text; its `i` flag is stored on
            // the struct, never in `value` — so `chars().count()` is `len(value)`.
            Pattern::Str(p) => p.value.chars().count(),
            Pattern::Re(p) => {
                let (raw, _) = crate::lexer::strip_whole_pattern_flag_wrapper(&p.pattern, p.flags);
                raw.chars().count()
            }
        }
    }

    /// A self-contained regex for this pattern, suitable for *inlining* into a
    /// larger pattern (e.g. when terminal `A` references terminal `B`). Any flags
    /// are applied as a *scoped* group `(?flags:…)` so they affect only this
    /// sub-pattern and never leak into the rest of the enclosing regex — unlike
    /// `as_regex_str`, which drops the separately-stored flags entirely.
    pub fn to_inline_regex(&self) -> String {
        match self {
            Pattern::Str(p) if p.ci => format!("(?i:{})", p.escaped),
            Pattern::Str(p) => p.escaped.clone(),
            Pattern::Re(p) => {
                let letters = flag_letters(p.flags);
                if letters.is_empty() {
                    p.pattern.clone()
                } else {
                    format!("(?{letters}:{})", p.pattern)
                }
            }
        }
    }
}

/// Maximum match width of a `regex-syntax` HIR, counted in **characters**
/// (`None` = unbounded). Mirrors Python's `sre_parse.getwidth()[1]`: a `+`/`*`/open
/// `{n,}` repetition is unbounded; a literal counts its code points (so a multibyte
/// literal is *one* char, not its UTF-8 byte length — the HIR's own `maximum_len`
/// reports bytes, which would diverge from Python on non-ASCII); a class is one char;
/// concatenation sums, alternation takes the max, and a lookaround assertion is
/// zero-width.
fn hir_max_width_chars(hir: &regex_syntax::hir::Hir) -> Option<usize> {
    use regex_syntax::hir::HirKind;
    match hir.kind() {
        HirKind::Empty | HirKind::Look(_) => Some(0),
        HirKind::Literal(lit) => Some(
            // HIR literals are UTF-8 bytes; count code points for char-width parity.
            std::str::from_utf8(&lit.0)
                .map(|s| s.chars().count())
                .unwrap_or(lit.0.len()),
        ),
        HirKind::Class(_) => Some(1),
        HirKind::Repetition(r) => match r.max {
            None => None, // unbounded (`+`, `*`, `{n,}`)
            Some(max) => hir_max_width_chars(&r.sub).map(|w| w.saturating_mul(max as usize)),
        },
        HirKind::Capture(c) => hir_max_width_chars(&c.sub),
        HirKind::Concat(subs) => subs
            .iter()
            .map(hir_max_width_chars)
            .try_fold(0usize, |acc, w| w.map(|w| acc.saturating_add(w))),
        HirKind::Alternation(subs) => subs
            .iter()
            .map(hir_max_width_chars)
            .try_fold(0usize, |acc, w| w.map(|w| acc.max(w))),
    }
}

impl PartialEq for Pattern {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            // `"a"` and `"a"i` share an escaped form but are distinct patterns.
            (Pattern::Str(a), Pattern::Str(b)) => a.value == b.value && a.ci == b.ci,
            _ => self.as_regex_str() == other.as_regex_str(),
        }
    }
}
impl Eq for Pattern {}

impl std::hash::Hash for Pattern {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.as_regex_str().hash(state);
    }
}

/// A string-literal pattern — Python Lark's `PatternStr`, including the
/// case-insensitive form (`"literal"i`), which Python keeps as a `PatternStr`
/// with the `i` flag attached rather than converting to a regex. Keeping the
/// type here too is what lets a `"keyword"i` literal participate in the
/// lexer's `unless` keyword retyping and sort with string-pattern width
/// semantics, exactly like its case-sensitive sibling.
#[derive(Debug, Clone)]
pub struct PatternStr {
    pub value: String,
    /// regex-escaped form used when building the combined lexer regex
    pub escaped: String,
    /// case-insensitive (`"..."i`): inlined as `(?i:escaped)`.
    pub ci: bool,
}

impl PatternStr {
    pub fn new(value: impl Into<String>) -> Self {
        let value = value.into();
        let escaped = regex::escape(&value);
        PatternStr {
            value,
            escaped,
            ci: false,
        }
    }

    /// A case-insensitive string literal (`"..."i`).
    pub fn new_ci(value: impl Into<String>) -> Self {
        PatternStr {
            ci: true,
            ..Self::new(value)
        }
    }
}

/// Regex flags (bit-field matching Python's re module flags subset).
pub mod flags {
    pub const IGNORECASE: u32 = 1;
    pub const MULTILINE: u32 = 2;
    pub const DOTALL: u32 = 4;
    pub const VERBOSE: u32 = 8;
}

#[derive(Debug, Clone)]
pub struct PatternRe {
    pub pattern: String,
    pub flags: u32,
}

/// Normalize the **`\<` and `\>` escapes** from the Python `re` dialect to the
/// `regex` crate's. Python treats an escaped punctuation char as that literal
/// everywhere, so `\<` / `\>` mean `<` / `>`; the `regex` crate instead reserves
/// them as **word-boundary escapes** — outside a character class `\<\>` is two
/// zero-width assertions that match *nothing* where Python matches `"<>"` (a silent
/// mis-lex), and inside a class they are rejected outright ("invalid escape sequence
/// found in character class" — the wild-bank dotmotif `OPERATOR`'s `[\!=\>\<]` and
/// `\<\>`). Rewriting exactly those two escapes to the bare char is
/// semantics-preserving in *both* dialects (`<` and `>` are ordinary literals bare,
/// in and out of classes, in Python and in the regex crate); every other escape —
/// including class-special ones like `\]` and idiom-pinned ones like `[^\/]` (the
/// bundled `lark.REGEXP` shape) — is left byte-exact.
fn normalize_python_escapes(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len());
    let chars: Vec<char> = pattern.chars().collect();
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' {
            match chars.get(i + 1).copied() {
                Some(n @ ('<' | '>')) => out.push(n), // drop the divergent escape
                Some(n) => {
                    out.push('\\');
                    out.push(n);
                }
                None => out.push('\\'),
            }
            i += 2;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

/// Find the first **global (bodiless) inline flag group** — `(?i)`, `(?ms)`, `(?i-s)`,
/// `(?-i)`, … — anywhere in a terminal's regex source, returning its exact `(?flags)`
/// text. This is the `(?flags)` form that sets flags for the rest of the enclosing
/// expression, as opposed to the *scoped* `(?flags:…)` form (which has a body and a
/// `:`). Python Lark rejects every terminal carrying one: it combines all terminals
/// into one regex, wrapping each pattern, which demotes the flag off position 0 — so
/// `re` raises either `global flags not at the start of the expression` (a leading
/// group) or `Cannot compile token` (a mid-pattern group). Either way the terminal is
/// unusable; lark-rs matches that rejection at build (N3, bounty H2). The scoped
/// `(?flags:…)` form — accepted by both engines — is left untouched.
///
/// The scan honors backslash escapes (a literal `\(` is not a group) and character
/// classes (`[(?i)]` is a class, not a flag group).
fn find_global_inline_flag_group(pattern: &str) -> Option<String> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut i = 0usize;
    while i < chars.len() {
        match chars[i] {
            '\\' => {
                i += 2; // skip the escape pair (a literal metachar, never structure)
            }
            '[' => {
                // Skip a character class verbatim, honoring `\]`, `[^…]`, and a
                // literal `]` as the first member.
                i += 1;
                if chars.get(i) == Some(&'^') {
                    i += 1;
                }
                if chars.get(i) == Some(&']') {
                    i += 1;
                }
                while i < chars.len() && chars[i] != ']' {
                    i += if chars[i] == '\\' { 2 } else { 1 };
                }
                i += 1; // past the closing ']' (or end of input)
            }
            '(' if chars.get(i + 1) == Some(&'?') => {
                // Read flag letters / `-` after "(?". A bodiless flag group ends in
                // ')' with no ':' body; a scoped `(?flags:…)` has a ':' and is fine,
                // and an assertion (`(?=`, `(?!`, `(?<`) or a named group (`(?P<`,
                // `(?<name>`) is not flags-only either (none reach the ')' below).
                let mut j = i + 2;
                let mut saw_flag = false;
                while let Some(&c) = chars.get(j) {
                    if c.is_ascii_alphabetic() || c == '-' {
                        saw_flag = true;
                        j += 1;
                    } else {
                        break;
                    }
                }
                if saw_flag && chars.get(j) == Some(&')') {
                    return Some(chars[i..=j].iter().collect());
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
    None
}

/// Reject a terminal's *user-authored* regex source that carries a **global (bodiless)
/// inline flag group** — the N3 (bounty H2) parity gate. Called by the grammar loader
/// on each `/…/` regex literal a user writes, *before* it composes into a pattern; the
/// internally-generated `(?i)`-prefixed case-insensitive string-literal bake (`"x"i`)
/// is a `LiteralVal::Str`, never a `LiteralVal::Re`, so it never reaches this gate (it
/// is a Python-supported feature, not the user-authored global flag we reject). See
/// [`find_global_inline_flag_group`].
pub(crate) fn reject_global_inline_flags(pattern: &str) -> Result<(), GrammarError> {
    if let Some(group) = find_global_inline_flag_group(pattern) {
        return Err(GrammarError::InvalidRegex {
            pattern: pattern.to_string(),
            reason: format!(
                "global inline flag group `{}` is not supported — Python Lark rejects it \
                 (the combined-regex wrapper moves it off the start of the expression, so \
                 `re` raises \"global flags not at the start\"). Use a scoped flag group \
                 `(?flags:…)` or a terminal-level flag (`/…/i`) instead.",
                group
            ),
        });
    }
    Ok(())
}

impl PatternRe {
    pub fn new(pattern: impl Into<String>, flags: u32) -> Result<Self, GrammarError> {
        let pattern = normalize_python_escapes(&pattern.into());
        let flag_prefix = build_flag_prefix(flags);
        let full = format!("{}{}", flag_prefix, pattern);
        // Validate the regex early to surface grammar errors. A pattern the linear
        // `regex` crate rejects may still be a valid *lookaround* pattern (some
        // bundled grammars use lookahead/lookbehind — issue #40); accept it if the
        // lookaround analyzer can parse it, and defer the verdict to the lexer-build
        // routing, which either lowers it into the DFA or refuses it with the
        // categorized scope error (`docs/LOOKAROUND_SCOPE.md`). A pattern neither
        // accepts is a real error, reported with the (more familiar) `regex`-crate
        // message plus a backtracking-syntax hint. Deliberately engine-independent:
        // grammar-load outcomes are identical with and without the `fancy-oracle`
        // test feature.
        if let Err(e) = Regex::new(&full) {
            // Parse the raw pattern (not `full`): the analyzer models the loader's
            // baked flag wrapper via the same parse the routing strip uses.
            // Also accept fence-idiom patterns (named backreferences): the lookaround
            // analyzer correctly cannot parse them, but they are handled by the
            // two-phase `FenceMatcher` at lexer-build time.
            if crate::lookaround::parse(&pattern).is_err()
                && crate::lookaround::lower::recognize_fence_idiom(&pattern).is_none()
            {
                return Err(GrammarError::InvalidRegex {
                    pattern: pattern.clone(),
                    reason: format!(
                        "{e} (and the lookaround analyzer cannot parse it either; \
                         note that backtracking-only syntax is not supported — see \
                         docs/LOOKAROUND_SCOPE.md)"
                    ),
                });
            }
        }
        Ok(PatternRe { pattern, flags })
    }
}

/// The inline-flag letters (`imsx`) for a flag bitset, in canonical order.
/// Empty when no flags are set.
pub fn flag_letters(flags: u32) -> String {
    let mut s = String::new();
    if flags & flags::IGNORECASE != 0 {
        s.push('i');
    }
    if flags & flags::MULTILINE != 0 {
        s.push('m');
    }
    if flags & flags::DOTALL != 0 {
        s.push('s');
    }
    if flags & flags::VERBOSE != 0 {
        s.push('x');
    }
    s
}

fn build_flag_prefix(flags: u32) -> String {
    let mut s = String::from("(?");
    if flags & flags::IGNORECASE != 0 {
        s.push('i');
    }
    if flags & flags::MULTILINE != 0 {
        s.push('m');
    }
    if flags & flags::DOTALL != 0 {
        s.push('s');
    }
    if flags & flags::VERBOSE != 0 {
        s.push('x');
    }
    if s == "(?)" || s == "(?" {
        return String::new();
    }
    s.push(')');
    s
}

/// A fully-resolved terminal definition.
///
/// Note there is no `filter_out` here: whether a token is dropped from the tree
/// is a property of each *rule-symbol occurrence*, not of the terminal (Python
/// Lark's model). The same terminal can be kept at one rule position and dropped
/// at another — e.g. `start: "a" A` with `A: "a"`, where both lex to `A` but the
/// literal occurrence is filtered and the `A` reference is kept. The per-occurrence
/// flag lives on [`Symbol::Terminal`](super::symbol::Terminal) and is lowered into
/// each rule's keep mask.
#[derive(Debug, Clone)]
pub struct TerminalDef {
    pub name: String,
    pub pattern: Pattern,
    /// Higher priority terminals are tried first in the lexer.
    pub priority: i32,
    /// A `%declare`d terminal: it has *no* pattern of its own and is never lexed.
    /// It is interned as a terminal (so rules can reference it and the parse table
    /// reserves a column) but excluded from every scanner; a postlex hook (e.g. an
    /// [`Indenter`](crate::postlex::Indenter)) injects its tokens into the stream.
    /// The `pattern` field carries a never-used placeholder for these.
    pub declared: bool,
    /// Whether Python Lark would represent this terminal as a `PatternStr` (a plain
    /// string literal, `pattern.type == "str"`) rather than a `PatternRE`. lark-rs
    /// compiles *every* named terminal to a regex `Pattern`, so this flag preserves
    /// the distinction Python keeps. It matters for the strict-mode regex-collision
    /// check (issue #35), which — exactly like Python's `_check_regex_collisions` —
    /// only ever compares the regex terminals (`pattern.type == "re"`); string
    /// terminals are disambiguated by the lexer's `unless` retyping, not flagged.
    pub string_type: bool,
}

impl TerminalDef {
    pub fn new(name: impl Into<String>, pattern: Pattern, priority: i32) -> Self {
        TerminalDef {
            name: name.into(),
            pattern,
            priority,
            declared: false,
            string_type: false,
        }
    }

    /// Builder-style setter for [`string_type`](Self::string_type).
    pub fn with_string_type(mut self, string_type: bool) -> Self {
        self.string_type = string_type;
        self
    }

    /// A pattern-less `%declare`d terminal (see [`declared`](Self::declared)). The
    /// placeholder pattern never reaches a lexer — `declared` terminals are filtered
    /// out before any scanner is built.
    pub fn declared(name: impl Into<String>) -> Self {
        TerminalDef {
            name: name.into(),
            pattern: Pattern::Str(PatternStr::new("")),
            priority: 0,
            declared: true,
            string_type: false,
        }
    }
}

impl PartialEq for TerminalDef {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
    }
}
impl Eq for TerminalDef {}

impl std::hash::Hash for TerminalDef {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.name.hash(state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// N3: a *global* (bodiless) inline flag group is detected anywhere — leading or
    /// mid-pattern — and `reject_global_inline_flags` (the loader gate on user regex
    /// literals) rejects it. A scoped `(?flags:…)` group, an assertion, a named group,
    /// an escaped `\(`, and a flag-looking char class are all left alone (no false
    /// positive). `PatternRe::new` itself does NOT gate — it serves internally-composed
    /// patterns (e.g. the `(?i)foo` case-insensitive string-literal bake) too.
    #[test]
    fn detects_only_bodiless_inline_flag_groups() {
        // Rejected: bodiless flag groups (the global form Python rejects).
        for p in [
            "(?i)abc", "(?ms)x", "(?i-s)y", "(?-i)z", "a(?i)b", "x(?im)y",
        ] {
            assert!(
                find_global_inline_flag_group(p).is_some(),
                "{p:?} should be flagged as a global inline flag group"
            );
            assert!(
                reject_global_inline_flags(p).is_err(),
                "the loader gate must reject {p:?}"
            );
        }
        // Accepted: scoped flag groups, assertions, named groups, escaped parens, and a
        // char class whose contents merely look like a flag group.
        for p in [
            "(?i:abc)",
            "(?-i:abc)",
            "x(?i:y)z",
            "(?=ab)cd",
            "(?!ab)cd",
            "(?P<name>x)",
            "(?<name>x)",
            r"\(?i\)abc", // escaped — not a group at all
            "[(?i)]",     // a character class of literal chars
            "[a-z]+",
        ] {
            assert!(
                find_global_inline_flag_group(p).is_none(),
                "{p:?} must NOT be flagged as a global inline flag group"
            );
            assert!(
                reject_global_inline_flags(p).is_ok(),
                "the loader gate must accept {p:?}"
            );
        }
    }

    /// The N3 gate lives in the loader (on user `/…/` literals), NOT in `PatternRe::new`,
    /// so the internal case-insensitive string-literal bake — `(?i)foo` paired with the
    /// `IGNORECASE` bitset, whose leading `(?i)` is load-bearing because `as_regex_str`
    /// drops the separate flag bitset when the literal is *composed* into a larger regex
    /// — still constructs cleanly through `PatternRe::new`.
    #[test]
    fn pattern_re_new_does_not_gate_the_internal_ci_bake() {
        let p = PatternRe::new("(?i)foo", flags::IGNORECASE).expect("ci bake constructs");
        assert_eq!(
            p.pattern, "(?i)foo",
            "the prefix must survive for as_regex_str composition"
        );
    }
}
