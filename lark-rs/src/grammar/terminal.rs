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

    /// Maximum number of characters this pattern can match (None = unbounded).
    pub fn max_width(&self) -> Option<usize> {
        match self {
            Pattern::Str(p) => Some(p.value.len()),
            Pattern::Re(_) => None,
        }
    }

    /// A self-contained regex for this pattern, suitable for *inlining* into a
    /// larger pattern (e.g. when terminal `A` references terminal `B`). Any flags
    /// are applied as a *scoped* group `(?flags:…)` so they affect only this
    /// sub-pattern and never leak into the rest of the enclosing regex — unlike
    /// `as_regex_str`, which drops the separately-stored flags entirely.
    pub fn to_inline_regex(&self) -> String {
        match self {
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

impl PartialEq for Pattern {
    fn eq(&self, other: &Self) -> bool {
        self.as_regex_str() == other.as_regex_str()
    }
}
impl Eq for Pattern {}

impl std::hash::Hash for Pattern {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.as_regex_str().hash(state);
    }
}

#[derive(Debug, Clone)]
pub struct PatternStr {
    pub value: String,
    /// regex-escaped form used when building the combined lexer regex
    pub escaped: String,
}

impl PatternStr {
    pub fn new(value: impl Into<String>) -> Self {
        let value = value.into();
        let escaped = regex::escape(&value);
        PatternStr { value, escaped }
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

impl PatternRe {
    pub fn new(pattern: impl Into<String>, flags: u32) -> Result<Self, GrammarError> {
        let pattern = pattern.into();
        let flag_prefix = build_flag_prefix(flags);
        let full = format!("{}{}", flag_prefix, pattern);
        // Validate the regex early to surface grammar errors. A pattern the linear
        // `regex` crate rejects may still be a valid *lookaround* pattern (some
        // bundled grammars use lookahead/lookbehind — issue #40); accept it if
        // `fancy-regex` can compile it, since the lexer routes such terminals to the
        // backtracking engine. Only a pattern neither engine accepts is a real error,
        // reported with the (more familiar) `regex`-crate message.
        if let Err(e) = Regex::new(&full) {
            if fancy_regex::Regex::new(&full).is_err() {
                return Err(GrammarError::InvalidRegex {
                    pattern: pattern.clone(),
                    reason: e.to_string(),
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
