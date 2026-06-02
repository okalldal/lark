/// A grammar symbol — either a terminal or a non-terminal.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Symbol {
    Terminal(Terminal),
    NonTerminal(NonTerminal),
}

impl Symbol {
    pub fn name(&self) -> &str {
        match self {
            Symbol::Terminal(t) => &t.name,
            Symbol::NonTerminal(nt) => &nt.name,
        }
    }

    pub fn is_term(&self) -> bool {
        matches!(self, Symbol::Terminal(_))
    }

    pub fn as_terminal(&self) -> Option<&Terminal> {
        match self {
            Symbol::Terminal(t) => Some(t),
            Symbol::NonTerminal(_) => None,
        }
    }

    pub fn as_nonterminal(&self) -> Option<&NonTerminal> {
        match self {
            Symbol::NonTerminal(nt) => Some(nt),
            Symbol::Terminal(_) => None,
        }
    }
}

impl From<Terminal> for Symbol {
    fn from(t: Terminal) -> Self {
        Symbol::Terminal(t)
    }
}

impl From<NonTerminal> for Symbol {
    fn from(nt: NonTerminal) -> Self {
        Symbol::NonTerminal(nt)
    }
}

impl std::fmt::Display for Symbol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Terminal {
    pub name: String,
    /// True when the terminal should not appear in the parse tree
    pub filter_out: bool,
}

impl Terminal {
    pub fn new(name: impl Into<String>) -> Self {
        Terminal { name: name.into(), filter_out: false }
    }

    pub fn filtered(name: impl Into<String>) -> Self {
        Terminal { name: name.into(), filter_out: true }
    }
}

impl std::fmt::Display for Terminal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NonTerminal {
    pub name: String,
}

impl NonTerminal {
    pub fn new(name: impl Into<String>) -> Self {
        NonTerminal { name: name.into() }
    }
}

impl std::fmt::Display for NonTerminal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name)
    }
}
