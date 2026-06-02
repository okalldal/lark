//! Parse tree types: `Tree` and `Token`.

use std::fmt;

/// A positioned token from the lexer.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Token {
    /// Terminal type name (e.g. "WORD", "NUMBER").
    pub type_: String,
    /// The matched text.
    pub value: String,
    pub line: usize,
    pub column: usize,
    pub end_line: usize,
    pub end_column: usize,
    pub start_pos: usize,
    pub end_pos: usize,
}

impl Token {
    pub fn new(type_: impl Into<String>, value: impl Into<String>) -> Self {
        Token {
            type_: type_.into(),
            value: value.into(),
            line: 0, column: 0, end_line: 0, end_column: 0,
            start_pos: 0, end_pos: 0,
        }
    }

    pub fn with_position(mut self, line: usize, col: usize, start: usize, end: usize) -> Self {
        self.line = line;
        self.column = col;
        self.end_line = line;  // updated by lexer for multi-line tokens
        self.end_column = col + (end - start);
        self.start_pos = start;
        self.end_pos = end;
        self
    }
}

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.value)
    }
}

/// Source position metadata attached to a `Tree` node.
#[derive(Debug, Clone, Default)]
pub struct Meta {
    pub line: Option<usize>,
    pub column: Option<usize>,
    pub end_line: Option<usize>,
    pub end_column: Option<usize>,
    pub start_pos: Option<usize>,
    pub end_pos: Option<usize>,
    /// True when the rule produced zero tokens (empty match).
    pub empty: bool,
}

impl Meta {
    pub fn from_children(children: &[Child]) -> Self {
        let mut meta = Meta::default();
        // Propagate position from first/last tokens
        for child in children {
            if let (None, Some(line)) = (meta.line, child_line(child)) {
                meta.line = Some(line);
                meta.column = child_column(child);
                meta.start_pos = child_start(child);
            }
        }
        for child in children.iter().rev() {
            if let Some(line) = child_end_line(child) {
                meta.end_line = Some(line);
                meta.end_column = child_end_column(child);
                meta.end_pos = child_end(child);
                break;
            }
        }
        if children.is_empty() {
            meta.empty = true;
        }
        meta
    }
}

fn child_line(c: &Child) -> Option<usize> {
    match c {
        Child::Token(t) if t.line > 0 => Some(t.line),
        Child::Tree(t) => t.meta.line,
        _ => None,
    }
}
fn child_column(c: &Child) -> Option<usize> {
    match c {
        Child::Token(t) if t.column > 0 => Some(t.column),
        Child::Tree(t) => t.meta.column,
        _ => None,
    }
}
fn child_start(c: &Child) -> Option<usize> {
    match c {
        Child::Token(t) => Some(t.start_pos),
        Child::Tree(t) => t.meta.start_pos,
        Child::None => None,
    }
}
fn child_end_line(c: &Child) -> Option<usize> {
    match c {
        Child::Token(t) if t.end_line > 0 => Some(t.end_line),
        Child::Tree(t) => t.meta.end_line,
        _ => None,
    }
}
fn child_end_column(c: &Child) -> Option<usize> {
    match c {
        Child::Token(t) if t.end_column > 0 => Some(t.end_column),
        Child::Tree(t) => t.meta.end_column,
        _ => None,
    }
}
fn child_end(c: &Child) -> Option<usize> {
    match c {
        Child::Token(t) => Some(t.end_pos),
        Child::Tree(t) => t.meta.end_pos,
        Child::None => None,
    }
}

/// A child of a `Tree` node — a subtree, a leaf token, or a `None` placeholder.
///
/// `None` placeholders are inserted for absent `[...]` groups when the parser is
/// built with `maybe_placeholders` (mirroring Python Lark's `None` children).
#[derive(Debug, Clone)]
pub enum Child {
    Tree(Tree),
    Token(Token),
    None,
}

impl Child {
    pub fn as_tree(&self) -> Option<&Tree> {
        match self { Child::Tree(t) => Some(t), _ => None }
    }

    pub fn as_token(&self) -> Option<&Token> {
        match self { Child::Token(t) => Some(t), _ => None }
    }

    pub fn is_tree(&self) -> bool { matches!(self, Child::Tree(_)) }
    pub fn is_token(&self) -> bool { matches!(self, Child::Token(_)) }
}

impl From<Tree> for Child {
    fn from(t: Tree) -> Self { Child::Tree(t) }
}
impl From<Token> for Child {
    fn from(t: Token) -> Self { Child::Token(t) }
}

impl fmt::Display for Child {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Child::Tree(t) => write!(f, "{}", t),
            Child::Token(t) => write!(f, "{}", t.value),
            Child::None => write!(f, "None"),
        }
    }
}

/// A node in the parse tree.
///
/// `data` is the rule name or alias that produced this node.
/// `children` are the sub-nodes or tokens.
#[derive(Debug, Clone)]
pub struct Tree {
    pub data: String,
    pub children: Vec<Child>,
    pub meta: Meta,
}

impl Tree {
    pub fn new(data: impl Into<String>, children: Vec<Child>) -> Self {
        let meta = Meta::from_children(&children);
        Tree { data: data.into(), children, meta }
    }

    /// Iterate all subtrees depth-first (post-order).
    pub fn iter_subtrees(&self) -> impl Iterator<Item = &Tree> {
        IterSubtrees { stack: vec![self], result: Vec::new() }.collect_all()
    }

    /// Iterate all leaf tokens.
    pub fn scan_values(&self) -> impl Iterator<Item = &Token> {
        self.iter_subtrees()
            .flat_map(|t| t.children.iter().filter_map(|c| c.as_token()))
    }

    /// Pretty-print the tree with indentation.
    pub fn pretty(&self, indent: usize) -> String {
        let pad = "  ".repeat(indent);
        let mut out = format!("{}Tree({}", pad, self.data);
        if self.children.is_empty() {
            out.push(')');
            return out;
        }
        out.push('\n');
        for child in &self.children {
            match child {
                Child::Tree(t) => out.push_str(&t.pretty(indent + 1)),
                Child::Token(tok) => out.push_str(&format!("{}  Token({}, {:?})", pad, tok.type_, tok.value)),
                Child::None => out.push_str(&format!("{}  None", pad)),
            }
            out.push('\n');
        }
        out.push_str(&format!("{})", pad));
        out
    }
}

impl fmt::Display for Tree {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let children: Vec<String> = self.children.iter()
            .map(|c| match c {
                Child::Tree(t) => t.to_string(),
                Child::Token(tok) => format!("Token({}, {:?})", tok.type_, tok.value),
                Child::None => "None".to_string(),
            })
            .collect();
        write!(f, "Tree({}, [{}])", self.data, children.join(", "))
    }
}

// Post-order depth-first iterator helper.
struct IterSubtrees<'a> {
    stack: Vec<&'a Tree>,
    result: Vec<&'a Tree>,
}

impl<'a> IterSubtrees<'a> {
    fn collect_all(mut self) -> std::vec::IntoIter<&'a Tree> {
        while let Some(tree) = self.stack.pop() {
            self.result.push(tree);
            for child in &tree.children {
                if let Child::Tree(t) = child {
                    self.stack.push(t);
                }
            }
        }
        self.result.into_iter()
    }
}
