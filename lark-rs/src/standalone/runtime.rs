//! The runtime shim embedded verbatim into every generated standalone parser.
//!
//! This module exists to kill the "opaque string literal" smell: rather than the
//! driver living as an un-type-checked text blob, it is a *real* module compiled
//! and type-checked as part of lark-rs, exercised by the round-trip fixtures
//! (`tests/test_standalone.rs`) and a direct unit test, **and** emitted into
//! generated parsers via `include_str!` (see the parent module). One source, two
//! consumers — the same discipline `scanner_plan` already applies to the lexer.
//!
//! Everything here is generic over a [`GrammarData`] value: the generated file
//! bakes one as a `static` and wires it into `Parser::new`, while the in-crate
//! unit test builds a tiny one by hand. The module depends only on `regex` + std
//! (no lark-rs items), so the identical source compiles in both places.
//!
//! **This driver lexes a baked `regex` `ScannerPlan` (a combined alternation compiled
//! at load), not a serialized DFA, and it has no `fancy-regex`** — so it cannot run a
//! grammar with lookaround terminals. Replacing this with a baked serialized **DFA
//! scanner bundle** (`docs/LEXER_DFA_PLAN.md`, L5) is the bakeability payoff that makes
//! the bundled `python`/`lark` grammars standalone-able; it is blocked on L4.

use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::fmt;

/// A parse-table cell: shift to a state, reduce by a rule, or accept.
#[derive(Clone, Copy)]
pub enum Action {
    Shift(u32),
    Reduce(u32),
    Accept,
}

/// A rule's tree-shaping metadata (everything the reducer needs, precomputed).
pub struct RuleData {
    pub origin: u32,
    pub len: u32,
    pub tree_name: &'static str,
    pub transparent: bool,
    pub expand1: bool,
    pub has_alias: bool,
    pub keep_all: bool,
    pub filter_pos: &'static [bool],
    pub placeholder_count: u32,
    pub is_start: bool,
}

/// Every per-grammar table a generated parser bakes. The generated file emits a
/// single `static DATA: GrammarData = …;` of this shape; the runtime is otherwise
/// grammar-agnostic.
pub struct GrammarData {
    /// Terminal ids occupy `[0, n_terminals)`; non-terminal GOTO index is
    /// `origin - n_terminals`.
    pub n_terminals: u32,
    /// Symbol name by id (token types + tree-node fallback + diagnostics).
    pub symbol_names: &'static [&'static str],
    pub rules: &'static [RuleData],
    /// `action[state]` is a sparse `(terminal id, action)` row.
    pub action: &'static [&'static [(u32, Action)]],
    /// `goto[state]` is a sparse `(nonterminal index, next state)` row.
    pub goto: &'static [&'static [(u32, u32)]],
    /// Start symbol name → initial state.
    pub start_states: &'static [(&'static str, u32)],
    pub start_default: &'static str,
    /// Leading inline-flag group for `g_regex_flags` (e.g. `(?i)`), or empty.
    pub global_prefix: &'static str,
    /// `(terminal id, inline regex)` in alternation order — the combined scanner.
    pub scan_groups: &'static [(u32, &'static str)],
    /// `unless` keyword retype: regex-terminal id → `(matched value, keyword id)`.
    pub unless: &'static [(u32, &'static [(&'static str, u32)])],
    /// `%ignore` terminal ids, discarded after matching.
    pub ignore: &'static [u32],
}

impl GrammarData {
    fn name_of(&self, id: u32) -> &'static str {
        self.symbol_names.get(id as usize).copied().unwrap_or("")
    }

    fn action_at(&self, state: usize, term: u32) -> Option<Action> {
        self.action[state]
            .iter()
            .find(|(t, _)| *t == term)
            .map(|(_, a)| *a)
    }

    fn goto_at(&self, state: usize, nt_index: u32) -> Option<u32> {
        self.goto[state]
            .iter()
            .find(|(n, _)| *n == nt_index)
            .map(|(_, s)| *s)
    }
}

/// A positioned token from the lexer.
#[derive(Clone, Debug)]
pub struct Token {
    pub type_id: u32,
    pub type_: String,
    pub value: String,
    pub line: usize,
    pub column: usize,
}

/// A child of a `Tree` node — a subtree, a leaf token, or a `None` placeholder.
#[derive(Clone, Debug)]
pub enum Child {
    Tree(Tree),
    Token(Token),
    None,
}

/// A node in the parse tree. `data` is the rule name (or alias) that built it.
#[derive(Clone, Debug)]
pub struct Tree {
    pub data: String,
    pub children: Vec<Child>,
}

/// The result of a parse — usually a `Tree`, but a `?start` rule that collapses
/// via expand1 to a single token yields that bare `Token`.
#[derive(Clone, Debug)]
pub enum ParseTree {
    Tree(Tree),
    Token(Token),
}

enum NodeValue {
    Token(Token),
    Tree(Tree),
    Inline(Vec<Child>),
}

impl fmt::Display for Tree {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let children: Vec<String> = self
            .children
            .iter()
            .map(|c| match c {
                Child::Tree(t) => t.to_string(),
                Child::Token(tok) => format!("Token({}, {:?})", tok.type_, tok.value),
                Child::None => "None".to_string(),
            })
            .collect();
        write!(f, "Tree({}, [{}])", self.data, children.join(", "))
    }
}

impl fmt::Display for ParseTree {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseTree::Tree(t) => write!(f, "{}", t),
            ParseTree::Token(tok) => write!(f, "Token({}, {:?})", tok.type_, tok.value),
        }
    }
}

// ───────────────────────────── lexer ─────────────────────────────

struct Scanner {
    re: Regex,
    /// (terminal id, capture-group index), in alternation order.
    groups: Vec<(u32, usize)>,
    unless: HashMap<u32, HashMap<String, u32>>,
}

impl Scanner {
    fn new(data: &GrammarData) -> Scanner {
        let mut parts: Vec<String> = Vec::with_capacity(data.scan_groups.len());
        for (id, rx) in data.scan_groups {
            parts.push(format!("(?P<g{}>{})", id, rx));
        }
        let pattern = format!("{}{}", data.global_prefix, parts.join("|"));
        let re = Regex::new(&pattern).expect("baked scanner regex is valid");
        // Resolve each group's name to its capture index (a terminal pattern can
        // itself contain groups, so the index is not the alternation position).
        let name_to_idx: HashMap<String, usize> = re
            .capture_names()
            .enumerate()
            .filter_map(|(i, n)| n.map(|n| (n.to_string(), i)))
            .collect();
        let groups = data
            .scan_groups
            .iter()
            .map(|(id, _)| (*id, name_to_idx[&format!("g{}", id)]))
            .collect();
        let mut unless: HashMap<u32, HashMap<String, u32>> = HashMap::new();
        for (re_id, entries) in data.unless {
            let m = unless.entry(*re_id).or_default();
            for (value, kw_id) in *entries {
                m.insert(value.to_string(), *kw_id);
            }
        }
        Scanner { re, groups, unless }
    }

    /// Match a single token starting exactly at `pos`. `None` = nothing here.
    fn match_at<'t>(&self, text: &'t str, pos: usize) -> Option<(u32, &'t str)> {
        let caps = self.re.captures_at(text, pos)?;
        let m0 = caps.get(0)?;
        // Accept only a non-empty match beginning exactly at `pos`.
        if m0.start() != pos || m0.end() == pos {
            return None;
        }
        let value = m0.as_str();
        for (id, idx) in &self.groups {
            if caps.get(*idx).is_some() {
                let ty = self
                    .unless
                    .get(id)
                    .and_then(|m| m.get(value))
                    .copied()
                    .unwrap_or(*id);
                return Some((ty, value));
            }
        }
        None
    }
}

fn lex(data: &GrammarData, scanner: &Scanner, text: &str) -> Result<Vec<Token>, String> {
    let ignore: HashSet<u32> = data.ignore.iter().copied().collect();
    let mut tokens = Vec::new();
    let mut pos = 0usize;
    let mut line = 1usize;
    let mut col = 1usize;

    while pos < text.len() {
        match scanner.match_at(text, pos) {
            Some((id, value)) => {
                let start_line = line;
                let start_col = col;
                for ch in value.chars() {
                    if ch == '\n' {
                        line += 1;
                        col = 1;
                    } else {
                        col += 1;
                    }
                }
                pos += value.len();
                if !ignore.contains(&id) {
                    tokens.push(Token {
                        type_id: id,
                        type_: data.name_of(id).to_string(),
                        value: value.to_string(),
                        line: start_line,
                        column: start_col,
                    });
                }
            }
            None => {
                let ch = text[pos..].chars().next().unwrap();
                return Err(format!(
                    "Unexpected character {:?} at line {}, column {}",
                    ch, line, col
                ));
            }
        }
    }

    // End-of-input sentinel (terminal id 0 = $END).
    tokens.push(Token {
        type_id: 0,
        type_: data.name_of(0).to_string(),
        value: String::new(),
        line,
        column: col,
    });
    Ok(tokens)
}

// ───────────────────────────── parser ─────────────────────────────

fn keep_token(rule: &RuleData, pos: usize) -> bool {
    rule.keep_all || !rule.filter_pos.get(pos).copied().unwrap_or(false)
}

fn shape(rule: &RuleData, mut children: Vec<Child>) -> NodeValue {
    for _ in 0..rule.placeholder_count {
        children.push(Child::None);
    }
    if rule.transparent {
        NodeValue::Inline(children)
    } else if rule.expand1
        && !rule.has_alias
        && children.len() == 1
        && !matches!(children[0], Child::None)
    {
        match children.pop().unwrap() {
            Child::Tree(t) => NodeValue::Tree(t),
            Child::Token(t) => NodeValue::Token(t),
            Child::None => unreachable!(),
        }
    } else {
        NodeValue::Tree(Tree {
            data: rule.tree_name.to_string(),
            children,
        })
    }
}

fn assemble(data: &GrammarData, rule_idx: usize, child_values: Vec<NodeValue>) -> NodeValue {
    let rule = &data.rules[rule_idx];
    let mut children: Vec<Child> = Vec::new();
    for (i, value) in child_values.into_iter().enumerate() {
        match value {
            NodeValue::Token(t) => {
                if keep_token(rule, i) {
                    children.push(Child::Token(t));
                }
            }
            NodeValue::Tree(t) => children.push(Child::Tree(t)),
            NodeValue::Inline(cs) => children.extend(cs),
        }
    }
    shape(rule, children)
}

fn run(data: &GrammarData, tokens: &[Token], start_state: usize) -> Result<ParseTree, String> {
    let mut state_stack: Vec<usize> = vec![start_state];
    let mut value_stack: Vec<NodeValue> = Vec::new();
    let mut i = 0usize;

    loop {
        let state = *state_stack.last().unwrap();
        let token = &tokens[i];
        match data.action_at(state, token.type_id) {
            Some(Action::Shift(ns)) => {
                i += 1;
                state_stack.push(ns as usize);
                value_stack.push(NodeValue::Token(token.clone()));
            }
            Some(Action::Reduce(r)) => {
                let rule = &data.rules[r as usize];
                let len = rule.len as usize;
                let child_values: Vec<NodeValue> = value_stack.split_off(value_stack.len() - len);
                for _ in 0..len {
                    state_stack.pop();
                }
                let value = assemble(data, r as usize, child_values);
                let top = *state_stack.last().unwrap();
                let nt_index = rule.origin - data.n_terminals;
                let next = data.goto_at(top, nt_index).expect("missing goto entry");
                state_stack.push(next as usize);
                value_stack.push(value);
            }
            Some(Action::Accept) => {
                return match value_stack.pop() {
                    Some(NodeValue::Tree(t)) => Ok(ParseTree::Tree(t)),
                    Some(NodeValue::Token(t)) => Ok(ParseTree::Token(t)),
                    _ => Err("accept with empty value stack".to_string()),
                };
            }
            None => {
                let expected: Vec<&str> = data.action[state]
                    .iter()
                    .map(|(t, _)| data.name_of(*t))
                    .collect();
                if token.type_id == 0 {
                    return Err(format!(
                        "Unexpected end of input at line {}, column {}. Expected one of: {:?}",
                        token.line, token.column, expected
                    ));
                }
                return Err(format!(
                    "Unexpected token {:?} ({}) at line {}, column {}. Expected one of: {:?}",
                    token.value, token.type_, token.line, token.column, expected
                ));
            }
        }
    }
}

fn start_state_for(data: &GrammarData, start: Option<&str>) -> Result<usize, String> {
    let name = start.unwrap_or(data.start_default);
    for (n, s) in data.start_states {
        if *n == name {
            return Ok(*s as usize);
        }
    }
    Err(format!("unknown start symbol {:?}", name))
}

/// A self-contained parser over a baked [`GrammarData`].
pub struct Parser {
    data: &'static GrammarData,
    scanner: Scanner,
}

impl Parser {
    /// Build a parser for the given baked grammar (compiles the lexer regex once).
    /// The generated file adds a `Parser::new()` that calls this with its `&DATA`.
    pub fn from_data(data: &'static GrammarData) -> Parser {
        let scanner = Scanner::new(data);
        Parser { data, scanner }
    }

    /// Parse `text` from the default start symbol.
    pub fn parse(&self, text: &str) -> Result<ParseTree, String> {
        self.parse_from(text, None)
    }

    /// Parse `text` from the named start symbol.
    pub fn parse_from(&self, text: &str, start: Option<&str>) -> Result<ParseTree, String> {
        let tokens = lex(self.data, &self.scanner, text)?;
        let start_state = start_state_for(self.data, start)?;
        run(self.data, &tokens, start_state)
    }
}
