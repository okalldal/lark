use regex::Regex;
use crate::error::GrammarError;

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
        // Validate the regex early to surface grammar errors.
        Regex::new(&full).map_err(|e| GrammarError::InvalidRegex {
            pattern: pattern.clone(),
            reason: e.to_string(),
        })?;
        Ok(PatternRe { pattern, flags })
    }
}

fn build_flag_prefix(flags: u32) -> String {
    let mut s = String::from("(?");
    if flags & flags::IGNORECASE != 0 { s.push('i'); }
    if flags & flags::MULTILINE != 0 { s.push('m'); }
    if flags & flags::DOTALL != 0 { s.push('s'); }
    if flags & flags::VERBOSE != 0 { s.push('x'); }
    if s == "(?)" || s == "(?" {
        return String::new();
    }
    s.push(')');
    s
}

/// A fully-resolved terminal definition.
#[derive(Debug, Clone)]
pub struct TerminalDef {
    pub name: String,
    pub pattern: Pattern,
    /// Higher priority terminals are tried first in the lexer.
    pub priority: i32,
    /// When true, tokens of this terminal are dropped from the tree unless the
    /// rule keeps all tokens. Set for terminals auto-created from string/regex
    /// literals and for user terminals named with a leading `_`. Decouples
    /// filtering from the terminal's name so anonymous literals can be named
    /// cleanly (e.g. `A`, `PLUS`) like Python Lark.
    pub filter_out: bool,
}

impl TerminalDef {
    pub fn new(name: impl Into<String>, pattern: Pattern, priority: i32) -> Self {
        TerminalDef { name: name.into(), pattern, priority, filter_out: false }
    }

    pub fn with_filter_out(mut self, filter_out: bool) -> Self {
        self.filter_out = filter_out;
        self
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
