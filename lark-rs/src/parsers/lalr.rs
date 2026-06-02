//! LALR(1) parser: table construction and execution.
//!
//! The pipeline is:
//! 1. `GrammarAnalysis` computes FIRST/FOLLOW/NULLABLE.
//! 2. `LR0Builder` constructs LR(0) item sets (states).
//! 3. `LookaheadComputer` propagates LALR(1) lookaheads.
//! 4. `build_lalr_table` assembles the ACTION/GOTO tables.
//! 5. `LalrParser` drives the state machine against a token stream.

use std::collections::{HashMap, HashSet, BTreeMap, BTreeSet, VecDeque};
use crate::grammar::{Grammar, symbol::*, rule::Rule, analysis::{GrammarAnalysis, END_TERMINAL}};
use crate::error::{GrammarError, ParseError};
use crate::tree::{Tree, Token, Child};
use crate::lexer::{ContextualLexer, LexerState};

// ─── Parse table ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum Action {
    Shift(usize),          // shift and go to state N
    Reduce(usize),         // reduce using rule index N
    Accept,
}

/// Immutable parse tables produced by LALR analysis.
#[derive(Debug)]
pub struct ParseTable {
    /// action[state][terminal_name] → Action
    pub action: Vec<HashMap<String, Action>>,
    /// goto[state][nonterminal_name] → next_state
    pub goto: Vec<HashMap<String, usize>>,
    /// Start state per start symbol.
    pub start_states: HashMap<String, usize>,
    /// Accept states per start symbol.
    pub end_states: HashMap<String, usize>,
    /// Compiled rules (indexed by rule index).
    pub rules: Vec<Rule>,
}

// ─── LR(0) item ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct LR0Item {
    rule_idx: usize,
    dot: usize, // position of the dot in the expansion
}

impl LR0Item {
    fn new(rule_idx: usize, dot: usize) -> Self {
        LR0Item { rule_idx, dot }
    }

    fn expected<'a>(&self, rules: &'a [Rule]) -> Option<&'a Symbol> {
        rules[self.rule_idx].expansion.get(self.dot)
    }

    fn advance(&self) -> Self {
        LR0Item { rule_idx: self.rule_idx, dot: self.dot + 1 }
    }

    fn is_complete(&self, rules: &[Rule]) -> bool {
        self.dot >= rules[self.rule_idx].expansion.len()
    }
}

type ItemSet = BTreeSet<LR0Item>;

// ─── LR(0) state machine builder ─────────────────────────────────────────────

struct LR0Builder<'g> {
    rules: &'g [Rule],
    /// Maps (nonterminal name) → [rule indices]
    rule_index: HashMap<&'g str, Vec<usize>>,
    /// States: index → ItemSet
    states: Vec<ItemSet>,
    /// Transitions: (state_id, Symbol) → state_id
    transitions: BTreeMap<(usize, String), usize>,
}

impl<'g> LR0Builder<'g> {
    fn new(rules: &'g [Rule]) -> Self {
        let mut rule_index: HashMap<&str, Vec<usize>> = HashMap::new();
        for (i, rule) in rules.iter().enumerate() {
            rule_index.entry(rule.origin.name.as_str()).or_default().push(i);
        }
        LR0Builder { rules, rule_index, states: Vec::new(), transitions: BTreeMap::new() }
    }

    /// Compute the epsilon-closure of a set of LR(0) items.
    fn closure(&self, kernel: &ItemSet) -> ItemSet {
        let mut result = kernel.clone();
        let mut worklist: VecDeque<LR0Item> = kernel.iter().cloned().collect();
        while let Some(item) = worklist.pop_front() {
            if let Some(Symbol::NonTerminal(nt)) = item.expected(self.rules) {
                for &rule_idx in self.rule_index.get(nt.name.as_str()).unwrap_or(&vec![]) {
                    let new_item = LR0Item::new(rule_idx, 0);
                    if result.insert(new_item.clone()) {
                        worklist.push_back(new_item);
                    }
                }
            }
        }
        result
    }

    /// Compute the GOTO transition from a state on a symbol.
    fn goto(&self, state: &ItemSet, sym: &Symbol) -> ItemSet {
        let moved: ItemSet = state.iter()
            .filter(|item| item.expected(self.rules) == Some(sym))
            .map(|item| item.advance())
            .collect();
        self.closure(&moved)
    }

    /// Build all LR(0) states starting from augmented start rules.
    fn build(&mut self, start_names: &[String]) -> HashMap<String, usize> {
        let mut start_states = HashMap::new();

        for start_name in start_names {
            // Find the augmented start rule: $root_<name> -> <name> $END
            // We synthesize a kernel item for it.
            let aug_name = format!("$root_{}", start_name);
            let aug_idx = self.rules.iter().position(|r| r.origin.name == aug_name);
            let aug_idx = match aug_idx {
                Some(i) => i,
                None => {
                    // No augmented rule — use first rule for this start symbol
                    match self.rules.iter().position(|r| r.origin.name == *start_name) {
                        Some(i) => i,
                        None => continue,
                    }
                }
            };

            let kernel: ItemSet = std::iter::once(LR0Item::new(aug_idx, 0)).collect();
            let s0 = self.add_state(kernel);
            start_states.insert(start_name.clone(), s0);
        }
        start_states
    }

    fn add_state(&mut self, kernel: ItemSet) -> usize {
        let closed = self.closure(&kernel);
        if let Some(pos) = self.states.iter().position(|s| s == &closed) {
            return pos;
        }
        let id = self.states.len();
        self.states.push(closed.clone());

        // Collect symbols that can be shifted from this state
        let symbols: HashSet<Symbol> = closed.iter()
            .filter_map(|item| item.expected(self.rules).cloned())
            .collect();

        for sym in symbols {
            let next_state_items = self.goto(&closed, &sym);
            if !next_state_items.is_empty() {
                let next_id = self.add_state(next_state_items);
                self.transitions.insert((id, sym.name().to_string()), next_id);
            }
        }
        id
    }
}

// ─── LALR(1) lookahead computation ───────────────────────────────────────────

/// Propagates lookahead tokens to build LALR(1) from LR(0).
struct LookaheadComputer<'g> {
    rules: &'g [Rule],
    states: &'g [ItemSet],
    transitions: &'g BTreeMap<(usize, String), usize>,
    analysis: &'g GrammarAnalysis,
}

impl<'g> LookaheadComputer<'g> {
    /// Compute lookaheads for all reduce items in all states.
    /// Returns: lookahead[state_id][item] → set of terminals
    fn compute(&self) -> HashMap<usize, HashMap<LR0Item, HashSet<String>>> {
        let mut lookaheads: HashMap<usize, HashMap<LR0Item, HashSet<String>>> = HashMap::new();

        // Initialise: start items get $END
        for (state_id, state) in self.states.iter().enumerate() {
            for item in state {
                if item.is_complete(self.rules) {
                    lookaheads.entry(state_id).or_default()
                        .entry(item.clone()).or_default();
                }
            }
        }

        // Propagate: for each item A → α • B β in state S:
        //   FIRST(β FOLLOW(A)) contributes to lookaheads of B items in GOTO(S, B).
        let mut changed = true;
        while changed {
            changed = false;
            for (state_id, state) in self.states.iter().enumerate() {
                for item in state {
                    if let Some(sym) = item.expected(self.rules) {
                        // The "after-dot" suffix
                        let rule = &self.rules[item.rule_idx];
                        let beta: Vec<Symbol> = rule.expansion[item.dot + 1..].to_vec();

                        let (first_beta, beta_nullable) = self.analysis.first_of_seq(&beta);

                        // Find GOTO(state, sym)
                        if let Some(&next_state) = self.transitions.get(&(state_id, sym.name().to_string())) {
                            let advanced = item.advance();
                            if advanced.is_complete(self.rules) {
                                let la_set = lookaheads.entry(next_state).or_default()
                                    .entry(advanced.clone()).or_default();
                                for t in &first_beta {
                                    la_set.insert(t.name.clone());
                                }
                                if beta_nullable {
                                    if let Some(current_la) = lookaheads.get(&state_id)
                                        .and_then(|m| m.get(item))
                                    {
                                        let extra: Vec<String> = current_la.iter().cloned().collect();
                                        let la_set2 = lookaheads.entry(next_state).or_default()
                                            .entry(advanced).or_default();
                                        for t in extra {
                                            la_set2.insert(t);
                                        }
                                    }
                                }
                            }
                        }

                        // Also add FOLLOW(B) to reduce items reachable via B
                        if let Symbol::NonTerminal(nt) = sym {
                            if let Some(follow_nt) = self.analysis.follow.get(nt) {
                                // These propagate to reduce items in goto state
                                if let Some(&next_state) = self.transitions.get(&(state_id, nt.name.clone())) {
                                    for b_item in &self.states[next_state] {
                                        if b_item.is_complete(self.rules)
                                            && self.rules[b_item.rule_idx].origin == *nt
                                        {
                                            let la = lookaheads.entry(next_state).or_default()
                                                .entry(b_item.clone()).or_default();
                                            let before = la.len();
                                            for t in follow_nt {
                                                la.insert(t.name.clone());
                                            }
                                            if la.len() > before { changed = true; }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        lookaheads
    }
}

// ─── Table construction ───────────────────────────────────────────────────────

pub fn build_lalr_table(grammar: &Grammar) -> Result<ParseTable, GrammarError> {
    // Augment grammar: add $root_<start> → <start> rules
    let mut rules = grammar.rules.clone();

    for start in &grammar.start {
        let aug_rule = Rule::simple(
            NonTerminal::new(format!("$root_{}", start)),
            vec![Symbol::NonTerminal(NonTerminal::new(start.clone()))],
        );
        rules.push(aug_rule);
    }

    // Grammar analysis
    let aug_grammar = Grammar {
        rules: rules.clone(),
        terminals: grammar.terminals.clone(),
        ignore: grammar.ignore.clone(),
        start: grammar.start.clone(),
    };
    let analysis = GrammarAnalysis::compute(&aug_grammar);

    // Build a lookup set of terminal names for correct SHIFT vs GOTO classification.
    // We cannot rely on naming conventions (e.g. anonymous non-terminals use "__anon_"
    // which clashes with the "__RSQB" naming of literal terminals).
    let terminal_names: HashSet<&str> = grammar.terminals.iter()
        .map(|t| t.name.as_str())
        .collect();

    // LR(0) state construction
    let mut builder = LR0Builder::new(&rules);
    let start_states = builder.build(&grammar.start);
    let (states, transitions) = (builder.states, builder.transitions);

    // LALR(1) lookahead computation — simplified: use FOLLOW sets
    let n_states = states.len();
    let mut action: Vec<HashMap<String, Action>> = vec![HashMap::new(); n_states];
    let mut goto: Vec<HashMap<String, usize>> = vec![HashMap::new(); n_states];

    // Fill SHIFT and GOTO from transitions
    for ((state_id, sym_name), &next_state) in &transitions {
        // Use the grammar's terminal list to determine SHIFT vs GOTO.
        // Augmented start rules ($root_X) and $END are always terminals.
        let is_terminal = terminal_names.contains(sym_name.as_str())
            || sym_name.starts_with('$');
        if is_terminal {
            action[*state_id].insert(sym_name.clone(), Action::Shift(next_state));
        } else {
            goto[*state_id].insert(sym_name.clone(), next_state);
        }
    }

    // Fill REDUCE from complete items, using FOLLOW sets as lookaheads
    for (state_id, state) in states.iter().enumerate() {
        for item in state {
            if item.is_complete(&rules) {
                let rule = &rules[item.rule_idx];
                // Is this an augmented start rule?
                if rule.origin.name.starts_with("$root_") {
                    let _start_sym = rule.origin.name.strip_prefix("$root_").unwrap();
                    action[state_id].insert(END_TERMINAL.to_string(), Action::Accept);
                    continue;
                }
                // Use FOLLOW(origin) as lookaheads
                let lookaheads: Vec<String> = analysis.follow
                    .get(&rule.origin)
                    .map(|f| f.iter().map(|t| t.name.clone()).collect())
                    .unwrap_or_default();

                for la in lookaheads {
                    // Conflict resolution: shift wins over reduce (Lark default)
                    if action[state_id].contains_key(&la) {
                        // Prefer shift; only override if reduce isn't already set
                        match action[state_id][&la] {
                            Action::Shift(_) => { /* shift wins */ }
                            _ => {
                                action[state_id].insert(la, Action::Reduce(item.rule_idx));
                            }
                        }
                    } else {
                        action[state_id].insert(la, Action::Reduce(item.rule_idx));
                    }
                }
            }
        }
    }

    // Determine end states (state where we just shifted the start nonterminal)
    let mut end_states = HashMap::new();
    for start in &grammar.start {
        // End state: from start_state, follow goto[start]
        if let Some(&s0) = start_states.get(start) {
            if let Some(&end) = goto[s0].get(start) {
                end_states.insert(start.clone(), end);
            }
        }
    }

    Ok(ParseTable {
        action,
        goto,
        start_states,
        end_states,
        rules,
    })
}

// ─── LALR parser execution ────────────────────────────────────────────────────

pub struct LalrParser {
    pub table: ParseTable,
}

impl LalrParser {
    pub fn new(table: ParseTable) -> Self {
        LalrParser { table }
    }

    /// Return the set of valid terminals per state (for the contextual lexer).
    pub fn state_terminals(&self) -> HashMap<usize, Vec<String>> {
        self.table.action.iter().enumerate()
            .map(|(state, actions)| (state, actions.keys().cloned().collect()))
            .collect()
    }

    /// Parse a pre-tokenized sequence.
    pub fn parse(
        &self,
        tokens: Vec<Token>,
        start: Option<&str>,
    ) -> Result<Tree, ParseError> {
        let start_name = start.unwrap_or_else(|| {
            self.table.start_states.keys().next().map(String::as_str).unwrap_or("start")
        });

        let initial_state = *self.table.start_states.get(start_name)
            .ok_or_else(|| ParseError::UnexpectedEof {
                line: 0, col: 0,
                expected: vec![format!("start symbol '{}'", start_name)],
            })?;

        let mut state_stack: Vec<usize> = vec![initial_state];
        let mut value_stack: Vec<StackValue> = Vec::new();
        let mut token_iter = tokens.into_iter().peekable();

        loop {
            let current_state = *state_stack.last().unwrap();
            let token = match token_iter.peek() {
                Some(t) => t.clone(),
                None => Token::new("$END", ""),
            };

            let action = self.table.action.get(current_state)
                .and_then(|m| m.get(&token.type_));

            match action {
                Some(Action::Shift(next_state)) => {
                    let tok = token_iter.next().unwrap();
                    state_stack.push(*next_state);
                    value_stack.push(StackValue::Token(tok));
                }
                Some(Action::Reduce(rule_idx)) => {
                    let rule = &self.table.rules[*rule_idx];
                    let len = rule.expansion.len();
                    let children: Vec<Child> = value_stack
                        .drain(value_stack.len() - len..)
                        .map(|sv| match sv {
                            StackValue::Token(t) => Child::Token(t),
                            StackValue::Tree(tr) => Child::Tree(tr),
                        })
                        .collect();
                    for _ in 0..len { state_stack.pop(); }

                    let tree_name = rule.tree_name().to_string();
                    let child = apply_rule_options(tree_name, children, rule);
                    let top_state = *state_stack.last().unwrap();
                    let next_state = self.table.goto[top_state]
                        .get(&rule.origin.name)
                        .copied()
                        .ok_or_else(|| ParseError::UnexpectedToken {
                            token: format!("{:?}", token.value),
                            token_type: rule.origin.name.clone(),
                            line: token.line, col: token.column,
                            expected: vec![rule.origin.name.clone()],
                        })?;
                    state_stack.push(next_state);
                    value_stack.push(match child {
                        Child::Tree(t) => StackValue::Tree(t),
                        Child::Token(t) => StackValue::Token(t),
                    });
                }
                Some(Action::Accept) => {
                    return value_stack.pop().map(|sv| match sv {
                        StackValue::Tree(t) => t,
                        StackValue::Token(tok) => Tree::new(tok.type_.clone(), vec![Child::Token(tok)]),
                    }).ok_or_else(|| ParseError::UnexpectedEof {
                        line: 0, col: 0, expected: vec![],
                    });
                }
                None => {
                    let expected: Vec<String> = self.table.action
                        .get(current_state)
                        .map(|m| m.keys().cloned().collect())
                        .unwrap_or_default();
                    if token.type_ == "$END" {
                        return Err(ParseError::UnexpectedEof {
                            line: token.line, col: token.column, expected,
                        });
                    }
                    return Err(ParseError::UnexpectedToken {
                        token: token.value.clone(),
                        token_type: token.type_.clone(),
                        line: token.line, col: token.column,
                        expected,
                    });
                }
            }
        }
    }

    /// Parse using the contextual lexer — lex one token at a time, feeding
    /// the current parser state to the lexer so it only tries valid terminals.
    pub fn parse_contextual(
        &self,
        text: &str,
        lexer: &ContextualLexer,
        start: Option<&str>,
    ) -> Result<Tree, ParseError> {
        let start_name = start.unwrap_or_else(|| {
            self.table.start_states.keys().next().map(String::as_str).unwrap_or("start")
        });

        let initial_state = *self.table.start_states.get(start_name)
            .ok_or_else(|| ParseError::UnexpectedEof {
                line: 0, col: 0,
                expected: vec![format!("start symbol '{}'", start_name)],
            })?;

        let mut state_stack: Vec<usize> = vec![initial_state];
        let mut value_stack: Vec<StackValue> = Vec::new();
        let mut lex_state = LexerState::new(text);
        let mut current_token: Option<Token> = None;

        loop {
            let current_state = *state_stack.last().unwrap();

            // Get next token from contextual lexer if we don't have one.
            // Ignored terminals (whitespace etc.) are transparently consumed.
            if current_token.is_none() {
                loop {
                    if lex_state.is_done() {
                        current_token = Some(Token::new("$END", "").with_position(
                            lex_state.line, lex_state.col, lex_state.pos, lex_state.pos,
                        ));
                        break;
                    }
                    match lexer.next_token(lex_state.text, lex_state.pos, current_state, lex_state.line, lex_state.col)? {
                        Some(tok) => {
                            if lexer.ignore().contains(&tok.type_) {
                                // Consume the ignored token and loop to get the next real one
                                lex_state.advance_by_lines(tok.value.len(), &tok.value);
                                continue;
                            }
                            current_token = Some(tok);
                            break;
                        }
                        None => {
                            // No token matched at current position; nothing valid here.
                            let expected: Vec<String> = self.table.action.get(current_state)
                                .map(|m| m.keys().cloned().collect())
                                .unwrap_or_default();
                            let ch: String = lex_state.text[lex_state.pos..].chars().take(1).collect();
                            return Err(ParseError::UnexpectedToken {
                                token: ch,
                                token_type: String::new(),
                                line: lex_state.line,
                                col: lex_state.col,
                                expected,
                            });
                        }
                    }
                }
            }
            let token = current_token.as_ref().unwrap().clone();

            let action = self.table.action.get(current_state)
                .and_then(|m| m.get(&token.type_));

            match action {
                Some(Action::Shift(next_state)) => {
                    state_stack.push(*next_state);
                    value_stack.push(StackValue::Token(token.clone()));
                    // Advance lexer position
                    lex_state.advance_by(token.value.len());
                    current_token = None;
                }
                Some(Action::Reduce(rule_idx)) => {
                    let rule = &self.table.rules[*rule_idx];
                    let len = rule.expansion.len();
                    let children: Vec<Child> = value_stack
                        .drain(value_stack.len() - len..)
                        .map(|sv| match sv {
                            StackValue::Token(t) => Child::Token(t),
                            StackValue::Tree(tr) => Child::Tree(tr),
                        })
                        .collect();
                    for _ in 0..len { state_stack.pop(); }

                    let tree_name = rule.tree_name().to_string();
                    let child = apply_rule_options(tree_name, children, rule);
                    let top_state = *state_stack.last().unwrap();
                    let next_state = self.table.goto[top_state]
                        .get(&rule.origin.name)
                        .copied()
                        .ok_or_else(|| ParseError::UnexpectedToken {
                            token: token.value.clone(),
                            token_type: rule.origin.name.clone(),
                            line: token.line, col: token.column,
                            expected: vec![rule.origin.name.clone()],
                        })?;
                    state_stack.push(next_state);
                    value_stack.push(match child {
                        Child::Tree(t) => StackValue::Tree(t),
                        Child::Token(t) => StackValue::Token(t),
                    });
                    // Don't advance lexer — same token may be consumed next
                }
                Some(Action::Accept) => {
                    return value_stack.pop().map(|sv| match sv {
                        StackValue::Tree(t) => t,
                        StackValue::Token(tok) => Tree::new(tok.type_.clone(), vec![Child::Token(tok)]),
                    }).ok_or_else(|| ParseError::UnexpectedEof {
                        line: 0, col: 0, expected: vec![],
                    });
                }
                None => {
                    let expected: Vec<String> = self.table.action
                        .get(current_state)
                        .map(|m| m.keys().cloned().collect())
                        .unwrap_or_default();
                    if token.type_ == "$END" {
                        return Err(ParseError::UnexpectedEof {
                            line: token.line, col: token.column, expected,
                        });
                    }
                    return Err(ParseError::UnexpectedToken {
                        token: token.value.clone(),
                        token_type: token.type_.clone(),
                        line: token.line, col: token.column,
                        expected,
                    });
                }
            }
        }
    }
}

// ─── Tree construction helpers ────────────────────────────────────────────────

enum StackValue {
    Token(Token),
    Tree(Tree),
}

fn apply_rule_options(name: String, mut children: Vec<Child>, rule: &Rule) -> Child {
    // Filter punctuation (unnamed/anonymous terminals) unless keep_all_tokens
    if !rule.options.keep_all_tokens {
        children.retain(|c| match c {
            Child::Token(t) => !t.type_.starts_with("__") && !t.type_.starts_with("_"),
            Child::Tree(_) => true,
        });
    }

    // Inline children of anonymous/transparent rules (those whose name starts
    // with "__anon_" or is a _private_rule starting with "_").
    // These are EBNF expansion helpers that should be invisible in the final tree.
    let children = inline_anonymous_trees(children);

    // expand1: if exactly one child and no alias, return that child directly.
    // This handles both Tree and Token children — a ?rule matching a single
    // terminal should yield the token itself, not a tree wrapping it.
    let has_alias = rule.alias.is_some();
    if rule.options.expand1 && !has_alias && children.len() == 1 {
        return children.into_iter().next().unwrap();
    }

    Child::Tree(Tree::new(name, children))
}

/// Recursively flatten anonymous rule trees into their parent's child list.
/// Anonymous rules are those named `__anon_*` (EBNF helpers) or `_name` (transparent rules).
fn inline_anonymous_trees(children: Vec<Child>) -> Vec<Child> {
    let mut result = Vec::with_capacity(children.len());
    for child in children {
        match child {
            Child::Tree(ref t) if is_anonymous_rule(&t.data) => {
                // Inline this node's children (already processed when the node was built)
                if let Child::Tree(t) = child {
                    result.extend(t.children);
                }
            }
            other => result.push(other),
        }
    }
    result
}

fn is_anonymous_rule(name: &str) -> bool {
    name.starts_with("__anon_")
}
