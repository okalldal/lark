//! Standalone parser generation (issue #42, Phase 3).
//!
//! Emits a *self-contained* Rust source file that parses a fixed grammar without
//! any dependency on lark-rs at parse time — only the `regex` crate and the Rust
//! standard library. This mirrors Python Lark's `lark.tools.standalone`, which
//! bakes a grammar into a single importable module.
//!
//! ## What is baked
//!
//! The generator runs the *normal* lark-rs pipeline once at build time:
//!
//! ```text
//! .lark text → load_grammar → lower → build_lalr_table  (ParseTable)
//!                                   → basic_lexer_conf + scanner_plan (lexer)
//! ```
//!
//! and then serializes the results as `const`/`static` Rust data:
//!
//!   * the dense LALR ACTION/GOTO tables (emitted sparsely, one row per state),
//!   * every [`CompiledRule`]'s tree-shaping flags (filter mask, `transparent`,
//!     `expand1`, alias, `keep_all_tokens`, placeholder count),
//!   * the symbol-name side table (for token types and diagnostics), and
//!   * the [`ScannerPlan`](crate::lexer::ScannerPlan): the ordered alternation
//!     members (each terminal's inline regex), the `unless` keyword-retype map,
//!     the `%ignore` set and the global-flag prefix.
//!
//! A fixed runtime shim (the [`RUNTIME`] template) is appended: a `BasicLexer`,
//! the LALR driver, and the tree-shaping logic — trimmed copies of `lexer.rs` /
//! `lalr.rs` / `tree_builder.rs` that read the baked tables. Because the scanner
//! data comes from the same [`scanner_plan`](crate::lexer::scanner_plan) the
//! in-process lexer uses, and the driver applies the same reduce/shape rules, a
//! generated parser produces byte-identical trees to lark-rs (pinned by
//! `tests/test_standalone.rs`).
//!
//! ## Limitations (documented parity gaps)
//!
//!   * **LALR only** — the baked artifact is a `ParseTable`; Earley/CYK are not
//!     supported (the generator returns an error).
//!   * **Basic lexer only** — the standalone lexer is the combined-regex
//!     `BasicLexer`, not the contextual lexer. Grammars that *require* the
//!     contextual lexer to resolve terminal collisions are rejected by Python
//!     Lark's standalone tool too; here they will simply fail to lex at runtime.
//!   * **No postlex** — `%declare` + an `Indenter` postlex hook is not baked
//!     (the generator returns an error if one is configured).
//!
//! [`CompiledRule`]: crate::grammar::intern::CompiledRule

use std::fmt::Write as _;

use crate::error::{GrammarError, LarkError};
use crate::grammar::load_grammar_with_base;
use crate::lexer::scanner_plan;
use crate::parsers::basic_lexer_conf;
use crate::parsers::lalr::{build_lalr_table, Action};
use crate::{LarkOptions, ParserAlgorithm};

/// Generate self-contained Rust source for a standalone parser of `grammar_src`.
///
/// The returned string is a complete `.rs` file: write it next to a crate that
/// depends on `regex` and call the generated `Parser::new().parse(text)`.
///
/// Errors if the grammar fails to load/build, or if the requested configuration
/// is not supported by the standalone backend (non-LALR parser, or a postlex
/// hook — see the module docs).
pub fn generate(grammar_src: &str, options: &LarkOptions) -> Result<String, LarkError> {
    if options.parser != ParserAlgorithm::Lalr {
        return Err(LarkError::Grammar(GrammarError::Other {
            msg: "standalone generation supports only parser='lalr'".to_string(),
        }));
    }
    if options.postlex.is_some() {
        return Err(LarkError::Grammar(GrammarError::Other {
            msg: "standalone generation does not support a postlex (Indenter) hook".to_string(),
        }));
    }

    let grammar = load_grammar_with_base(
        grammar_src,
        &options.start,
        options.maybe_placeholders,
        options.keep_all_tokens,
        options.base_path.clone(),
    )?;
    let cg = crate::grammar::lower(&grammar);
    let table = build_lalr_table(&cg, options.strict)?;
    let lexer_conf = basic_lexer_conf(&cg, options.g_regex_flags);

    // Reuse the in-process scanner recipe so the baked lexer is byte-identical.
    let term_refs: Vec<_> = lexer_conf
        .terminals
        .iter()
        .map(|(id, t)| (*id, t))
        .collect();
    let plan = scanner_plan(&term_refs, lexer_conf.global_flags)?;

    let mut out = String::new();
    emit_header(&mut out, grammar_src);
    emit_data(&mut out, &table, &plan, &lexer_conf.ignore, options);
    out.push_str(RUNTIME);
    // Close the wrapping `mod parser` opened by the header and re-export its public
    // surface, so the file works both compiled directly (crate root) and `include!`d
    // into another module.
    out.push_str(
        "}\n\n#[allow(unused_imports)]\npub use parser::{Child, ParseTree, Parser, Token, Tree};\n",
    );
    Ok(out)
}

/// A Rust string literal for `s` (`{:?}` produces valid, fully-escaped source).
fn lit(s: &str) -> String {
    format!("{s:?}")
}

fn emit_header(out: &mut String, grammar_src: &str) {
    out.push_str(
        "// @generated by `lark-rs generate-parser` — DO NOT EDIT.\n\
         //\n\
         // A self-contained LALR parser. Depends only on the `regex` crate and the\n\
         // Rust standard library — not on lark-rs. Drop it into any crate that has\n\
         // `regex` as a dependency and call `Parser::new().parse(text)`.\n\
         //\n\
         // Source grammar:\n",
    );
    for line in grammar_src.lines() {
        out.push_str("//   ");
        out.push_str(line);
        out.push('\n');
    }
    // Everything lives in an inner module carrying an *outer* `#[allow]` — an
    // inner `#![allow]` would be rejected when the file is `include!`d into another
    // module (macro-expanded inner attributes are not permitted there).
    out.push_str(
        "\n#[allow(dead_code, unused_parens, clippy::all)]\npub mod parser {\nuse regex::Regex;\nuse std::collections::{HashMap, HashSet};\nuse std::fmt;\n\n",
    );
}

fn emit_data(
    out: &mut String,
    table: &crate::parsers::lalr::ParseTable,
    plan: &crate::lexer::ScannerPlan,
    ignore: &[crate::grammar::SymbolId],
    options: &LarkOptions,
) {
    let _ = writeln!(out, "static N_TERMINALS: u32 = {};\n", table.n_terminals);

    // Symbol names, indexed by id.
    out.push_str("static SYMBOL_NAMES: &[&str] = &[\n");
    for i in 0..table.symbols.len() {
        let name = table.symbols.name(crate::grammar::SymbolId(i as u32));
        let _ = writeln!(out, "    {},", lit(name));
    }
    out.push_str("];\n\n");

    // Rules.
    out.push_str("static RULES: &[RuleData] = &[\n");
    for r in &table.rules {
        let filter: String = r
            .filter_pos
            .iter()
            .map(|b| b.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(
            out,
            "    RuleData {{ origin: {}, len: {}, tree_name: {}, transparent: {}, expand1: {}, has_alias: {}, keep_all: {}, filter_pos: &[{}], placeholder_count: {}, is_start: {} }},",
            r.origin.0,
            r.expansion.len(),
            lit(&r.tree_name),
            r.transparent,
            r.options.expand1,
            r.alias.is_some(),
            r.options.keep_all_tokens,
            filter,
            r.options.placeholder_count,
            r.is_start,
        );
    }
    out.push_str("];\n\n");

    // ACTION table — one sparse row per state, terminals ascending.
    out.push_str("static ACTION: &[&[(u32, Action)]] = &[\n");
    for row in &table.action {
        out.push_str("    &[");
        let mut first = true;
        for (term, cell) in row.iter().enumerate() {
            let Some(action) = cell else { continue };
            if !first {
                out.push_str(", ");
            }
            first = false;
            let a = match action {
                Action::Shift(s) => format!("Action::Shift({s})"),
                Action::Reduce(r) => format!("Action::Reduce({r})"),
                Action::Accept => "Action::Accept".to_string(),
            };
            let _ = write!(out, "({term}, {a})");
        }
        out.push_str("],\n");
    }
    out.push_str("];\n\n");

    // GOTO table — sparse (nonterminal index, next state) per state.
    out.push_str("static GOTO: &[&[(u32, u32)]] = &[\n");
    for row in &table.goto {
        out.push_str("    &[");
        let mut first = true;
        for (nt, cell) in row.iter().enumerate() {
            let Some(next) = cell else { continue };
            if !first {
                out.push_str(", ");
            }
            first = false;
            let _ = write!(out, "({nt}, {next})");
        }
        out.push_str("],\n");
    }
    out.push_str("];\n\n");

    // Start states (name → state), sorted by name for deterministic output.
    let mut starts: Vec<(String, usize)> = table
        .start_states
        .iter()
        .map(|(id, st)| (table.symbols.name(*id).to_string(), *st))
        .collect();
    starts.sort();
    out.push_str("static START_STATES: &[(&str, u32)] = &[\n");
    for (name, st) in &starts {
        let _ = writeln!(out, "    ({}, {}),", lit(name), st);
    }
    out.push_str("];\n\n");
    let default_start = options
        .start
        .first()
        .cloned()
        .unwrap_or_else(|| "start".to_string());
    let _ = writeln!(
        out,
        "static START_DEFAULT: &str = {};\n",
        lit(&default_start)
    );

    // Lexer: global prefix, scanner alternation, unless map, ignore set.
    let _ = writeln!(
        out,
        "static GLOBAL_PREFIX: &str = {};\n",
        lit(&plan.global_prefix)
    );

    out.push_str("static SCAN_GROUPS: &[(u32, &str)] = &[\n");
    for (id, rx) in &plan.groups {
        let _ = writeln!(out, "    ({}, {}),", id.0, lit(rx));
    }
    out.push_str("];\n\n");

    // unless: sorted by regex id, inner by matched value, for determinism.
    let mut unless: Vec<(u32, Vec<(String, u32)>)> = plan
        .unless
        .iter()
        .map(|(re_id, m)| {
            let mut entries: Vec<(String, u32)> =
                m.iter().map(|(v, kw)| (v.clone(), kw.0)).collect();
            entries.sort();
            (re_id.0, entries)
        })
        .collect();
    unless.sort();
    out.push_str("static UNLESS: &[(u32, &[(&str, u32)])] = &[\n");
    for (re_id, entries) in &unless {
        out.push_str("    (");
        let _ = write!(out, "{re_id}, &[");
        let mut first = true;
        for (v, kw) in entries {
            if !first {
                out.push_str(", ");
            }
            first = false;
            let _ = write!(out, "({}, {})", lit(v), kw);
        }
        out.push_str("]),\n");
    }
    out.push_str("];\n\n");

    let mut ig: Vec<u32> = ignore.iter().map(|s| s.0).collect();
    ig.sort_unstable();
    let ig_list: String = ig
        .iter()
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let _ = writeln!(out, "static IGNORE: &[u32] = &[{ig_list}];\n");
}

/// The fixed runtime shim appended after the baked data. It defines the public
/// `Parser`, the `Tree`/`Token`/`ParseTree` types (with `Display` impls matching
/// lark-rs), the basic lexer, and the LALR driver. Trimmed copies of the
/// in-process modules; kept correct by the round-trip oracle test.
const RUNTIME: &str = r###"// ───────────────────────────── runtime shim ─────────────────────────────

#[derive(Clone, Copy)]
pub enum Action {
    Shift(u32),
    Reduce(u32),
    Accept,
}

struct RuleData {
    origin: u32,
    len: u32,
    tree_name: &'static str,
    transparent: bool,
    expand1: bool,
    has_alias: bool,
    keep_all: bool,
    filter_pos: &'static [bool],
    placeholder_count: u32,
    is_start: bool,
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

fn name_of(id: u32) -> &'static str {
    SYMBOL_NAMES.get(id as usize).copied().unwrap_or("")
}

// ───────────────────────────── lexer ─────────────────────────────

struct Scanner {
    re: Regex,
    /// (terminal id, capture-group index), in alternation order.
    groups: Vec<(u32, usize)>,
    unless: HashMap<u32, HashMap<String, u32>>,
}

impl Scanner {
    fn new() -> Scanner {
        let mut parts: Vec<String> = Vec::with_capacity(SCAN_GROUPS.len());
        for (id, rx) in SCAN_GROUPS {
            parts.push(format!("(?P<g{}>{})", id, rx));
        }
        let pattern = format!("{}{}", GLOBAL_PREFIX, parts.join("|"));
        let re = Regex::new(&pattern).expect("baked scanner regex is valid");
        // Resolve each group's name to its capture index (a terminal pattern can
        // itself contain groups, so the index is not the alternation position).
        let name_to_idx: HashMap<String, usize> = re
            .capture_names()
            .enumerate()
            .filter_map(|(i, n)| n.map(|n| (n.to_string(), i)))
            .collect();
        let groups = SCAN_GROUPS
            .iter()
            .map(|(id, _)| (*id, name_to_idx[&format!("g{}", id)]))
            .collect();
        let mut unless: HashMap<u32, HashMap<String, u32>> = HashMap::new();
        for (re_id, entries) in UNLESS {
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

fn lex(scanner: &Scanner, text: &str) -> Result<Vec<Token>, String> {
    let ignore: HashSet<u32> = IGNORE.iter().copied().collect();
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
                        type_: name_of(id).to_string(),
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
        type_: name_of(0).to_string(),
        value: String::new(),
        line,
        column: col,
    });
    Ok(tokens)
}

// ───────────────────────────── parser ─────────────────────────────

fn action_at(state: usize, term: u32) -> Option<Action> {
    for (t, a) in ACTION[state] {
        if *t == term {
            return Some(*a);
        }
    }
    None
}

fn goto_at(state: usize, nt_index: u32) -> Option<u32> {
    for (n, s) in GOTO[state] {
        if *n == nt_index {
            return Some(*s);
        }
    }
    None
}

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

fn assemble(rule_idx: usize, child_values: Vec<NodeValue>) -> NodeValue {
    let rule = &RULES[rule_idx];
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

fn run(tokens: &[Token], start_state: usize) -> Result<ParseTree, String> {
    let mut state_stack: Vec<usize> = vec![start_state];
    let mut value_stack: Vec<NodeValue> = Vec::new();
    let mut i = 0usize;

    loop {
        let state = *state_stack.last().unwrap();
        let token = &tokens[i];
        match action_at(state, token.type_id) {
            Some(Action::Shift(ns)) => {
                i += 1;
                state_stack.push(ns as usize);
                value_stack.push(NodeValue::Token(token.clone()));
            }
            Some(Action::Reduce(r)) => {
                let rule = &RULES[r as usize];
                let len = rule.len as usize;
                let child_values: Vec<NodeValue> =
                    value_stack.split_off(value_stack.len() - len);
                for _ in 0..len {
                    state_stack.pop();
                }
                let value = assemble(r as usize, child_values);
                let top = *state_stack.last().unwrap();
                let nt_index = rule.origin - N_TERMINALS;
                let next = goto_at(top, nt_index).expect("missing goto entry");
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
                let expected: Vec<&str> =
                    ACTION[state].iter().map(|(t, _)| name_of(*t)).collect();
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

fn start_state_for(start: Option<&str>) -> Result<usize, String> {
    let name = start.unwrap_or(START_DEFAULT);
    for (n, s) in START_STATES {
        if *n == name {
            return Ok(*s as usize);
        }
    }
    Err(format!("unknown start symbol {:?}", name))
}

/// A self-contained parser for the baked grammar.
pub struct Parser {
    scanner: Scanner,
}

impl Parser {
    /// Build the parser (compiles the combined lexer regex once).
    pub fn new() -> Parser {
        Parser {
            scanner: Scanner::new(),
        }
    }

    /// Parse `text` from the default start symbol.
    pub fn parse(&self, text: &str) -> Result<ParseTree, String> {
        self.parse_from(text, None)
    }

    /// Parse `text` from the named start symbol.
    pub fn parse_from(&self, text: &str, start: Option<&str>) -> Result<ParseTree, String> {
        let tokens = lex(&self.scanner, text)?;
        let start_state = start_state_for(start)?;
        run(&tokens, start_state)
    }
}

impl Default for Parser {
    fn default() -> Self {
        Parser::new()
    }
}
"###;
