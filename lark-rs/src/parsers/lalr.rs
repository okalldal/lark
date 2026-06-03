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
    /// Names of terminals filtered from the tree by default (`filter_out`).
    pub filter_out: HashSet<String>,
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
        // O(1) state deduplication + a worklist for iterative construction.
        let mut index: HashMap<ItemSet, usize> = HashMap::new();
        let mut worklist: VecDeque<usize> = VecDeque::new();

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
            let s0 = self.intern_state(kernel, &mut index, &mut worklist);
            start_states.insert(start_name.clone(), s0);
        }

        // Process states iteratively (a worklist, not recursion) so deep GOTO
        // chains — e.g. `"A"~8191` expands to thousands of chained states — do not
        // overflow the stack.
        while let Some(id) = worklist.pop_front() {
            let closed = self.states[id].clone();
            let symbols: HashSet<Symbol> = closed.iter()
                .filter_map(|item| item.expected(self.rules).cloned())
                .collect();
            for sym in symbols {
                let next_state_items = self.goto(&closed, &sym);
                if !next_state_items.is_empty() {
                    let next_id = self.intern_state(next_state_items, &mut index, &mut worklist);
                    self.transitions.insert((id, sym.name().to_string()), next_id);
                }
            }
        }
        start_states
    }

    /// Return the state id for the closure of `kernel`, creating it (and queuing
    /// it for processing) if it does not already exist. O(1) lookup via `index`.
    fn intern_state(
        &mut self,
        kernel: ItemSet,
        index: &mut HashMap<ItemSet, usize>,
        worklist: &mut VecDeque<usize>,
    ) -> usize {
        let closed = self.closure(&kernel);
        if let Some(&id) = index.get(&closed) {
            return id;
        }
        let id = self.states.len();
        self.states.push(closed.clone());
        index.insert(closed, id);
        worklist.push_back(id);
        id
    }
}

// ─── LALR(1) lookahead computation ───────────────────────────────────────────

/// Sentinel lookahead used by the spontaneous-generation / propagation
/// algorithm to mark lookaheads that *propagate* from a kernel item rather than
/// being generated spontaneously. It can never collide with a real terminal
/// name (terminals are identifiers or `$`-prefixed synthetics).
const PROPAGATE_MARK: &str = "#";

/// Computes true LALR(1) lookaheads for every complete (reduce) item using the
/// canonical spontaneous-generation-and-propagation method (Aho/Sethi/Ullman
/// algorithms 4.62–4.63).
///
/// This is strictly more precise than SLR(1) FOLLOW sets: a reduce never picks
/// up a lookahead in a state where that reduction cannot actually be followed
/// by the token. That precision is what avoids spurious conflicts and is what
/// the contextual lexer relies on.
struct LookaheadComputer<'g> {
    rules: &'g [Rule],
    states: &'g [ItemSet],
    transitions: &'g BTreeMap<(usize, String), usize>,
    analysis: &'g GrammarAnalysis,
    rule_index: HashMap<&'g str, Vec<usize>>,
}

impl<'g> LookaheadComputer<'g> {
    fn new(
        rules: &'g [Rule],
        states: &'g [ItemSet],
        transitions: &'g BTreeMap<(usize, String), usize>,
        analysis: &'g GrammarAnalysis,
    ) -> Self {
        let mut rule_index: HashMap<&str, Vec<usize>> = HashMap::new();
        for (i, rule) in rules.iter().enumerate() {
            rule_index.entry(rule.origin.name.as_str()).or_default().push(i);
        }
        LookaheadComputer { rules, states, transitions, analysis, rule_index }
    }

    /// Kernel items are those with the dot past the start, plus the augmented
    /// start items `$root_X → • X` (dot 0).
    fn is_kernel(&self, item: &LR0Item) -> bool {
        item.dot > 0 || self.rules[item.rule_idx].origin.name.starts_with("$root_")
    }

    /// LR(1) closure: given kernel items each carrying a set of lookahead
    /// terminals, propagate lookaheads to every reachable closure item.
    /// For an item `A → α • B β` with lookahead set `L`, each production
    /// `B → γ` gains `[B → • γ]` with lookaheads `FIRST(β)` (plus `L` when `β`
    /// is nullable). Iterates to a fixpoint.
    fn lr1_closure(
        &self,
        kernel: &HashMap<LR0Item, HashSet<String>>,
    ) -> HashMap<LR0Item, HashSet<String>> {
        let mut result: HashMap<LR0Item, HashSet<String>> = kernel.clone();
        let mut changed = true;
        while changed {
            changed = false;
            let snapshot: Vec<(LR0Item, HashSet<String>)> =
                result.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            for (item, la) in snapshot {
                if let Some(Symbol::NonTerminal(nt)) = item.expected(self.rules) {
                    let rule = &self.rules[item.rule_idx];
                    let beta: Vec<Symbol> = rule.expansion[item.dot + 1..].to_vec();
                    let (first_beta, beta_nullable) = self.analysis.first_of_seq(&beta);
                    let mut seed: HashSet<String> =
                        first_beta.iter().map(|t| t.name.clone()).collect();
                    if beta_nullable {
                        seed.extend(la.iter().cloned());
                    }
                    if let Some(prods) = self.rule_index.get(nt.name.as_str()) {
                        for &ri in prods {
                            let it = LR0Item::new(ri, 0);
                            let entry = result.entry(it).or_default();
                            for s in &seed {
                                if entry.insert(s.clone()) {
                                    changed = true;
                                }
                            }
                        }
                    }
                }
            }
        }
        result
    }

    /// Compute lookaheads for all reduce items in all states.
    /// Returns: `reduce_la[state_id][rule_idx]` → lookahead terminals, for every
    /// complete item in the state (including ε-rule items).
    fn compute(&self) -> HashMap<usize, HashMap<usize, HashSet<String>>> {
        // Kernel-item lookahead sets, keyed (state, item).
        let mut kla: HashMap<(usize, LR0Item), HashSet<String>> = HashMap::new();
        // Propagation links: (from_state, from_item) → (to_state, to_item).
        let mut links: Vec<((usize, LR0Item), (usize, LR0Item))> = Vec::new();

        // Seed all kernel items; the augmented start kernels get $END.
        for (state_id, state) in self.states.iter().enumerate() {
            for item in state {
                if self.is_kernel(item) {
                    let set = kla.entry((state_id, item.clone())).or_default();
                    if self.rules[item.rule_idx].origin.name.starts_with("$root_") {
                        set.insert(END_TERMINAL.to_string());
                    }
                }
            }
        }

        // Discover spontaneous lookaheads and propagation links by closing each
        // kernel item against the dummy lookahead `#`.
        for (state_id, state) in self.states.iter().enumerate() {
            for k in state {
                if !self.is_kernel(k) {
                    continue;
                }
                let mut seed = HashMap::new();
                seed.insert(
                    k.clone(),
                    std::iter::once(PROPAGATE_MARK.to_string()).collect(),
                );
                let closed = self.lr1_closure(&seed);
                for (item, la_set) in &closed {
                    let sym = match item.expected(self.rules) {
                        Some(s) => s,
                        None => continue,
                    };
                    let goto_state = match self.transitions.get(&(state_id, sym.name().to_string())) {
                        Some(&g) => g,
                        None => continue,
                    };
                    let advanced = item.advance();
                    for a in la_set {
                        if a == PROPAGATE_MARK {
                            links.push(((state_id, k.clone()), (goto_state, advanced.clone())));
                        } else {
                            kla.entry((goto_state, advanced.clone()))
                                .or_default()
                                .insert(a.clone());
                        }
                    }
                }
            }
        }

        // Propagate lookaheads along the links until a fixpoint.
        let mut changed = true;
        while changed {
            changed = false;
            for (from, to) in &links {
                let src: Vec<String> = kla
                    .get(from)
                    .map(|s| s.iter().cloned().collect())
                    .unwrap_or_default();
                if src.is_empty() {
                    continue;
                }
                let dst = kla.entry(to.clone()).or_default();
                for t in src {
                    if dst.insert(t) {
                        changed = true;
                    }
                }
            }
        }

        // For each state, run a final LR(1) closure on the kernel lookaheads so
        // that ε-rule reduce items (which are closure items, not kernels) also
        // receive their lookaheads, then collect per reduce rule.
        let mut reduce_la: HashMap<usize, HashMap<usize, HashSet<String>>> = HashMap::new();
        for (state_id, state) in self.states.iter().enumerate() {
            let mut kernel: HashMap<LR0Item, HashSet<String>> = HashMap::new();
            for item in state {
                if self.is_kernel(item) {
                    let set = kla
                        .get(&(state_id, item.clone()))
                        .cloned()
                        .unwrap_or_default();
                    kernel.insert(item.clone(), set);
                }
            }
            let closed = self.lr1_closure(&kernel);
            for (item, la_set) in closed {
                if item.is_complete(self.rules) {
                    reduce_la
                        .entry(state_id)
                        .or_default()
                        .entry(item.rule_idx)
                        .or_default()
                        .extend(la_set);
                }
            }
        }
        reduce_la
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

    // Terminals filtered from the tree by default (filter_out), used by
    // apply_rule_options instead of a name-prefix heuristic.
    let filter_out: HashSet<String> = grammar.terminals.iter()
        .filter(|t| t.filter_out)
        .map(|t| t.name.clone())
        .collect();

    // LR(0) state construction
    let mut builder = LR0Builder::new(&rules);
    let start_states = builder.build(&grammar.start);
    let (states, transitions) = (builder.states, builder.transitions);

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

    // True LALR(1) lookaheads for every reduce item.
    let reduce_la = LookaheadComputer::new(&rules, &states, &transitions, &analysis).compute();

    // Fill REDUCE / ACCEPT actions, detecting and resolving conflicts exactly
    // the way Python Lark does (lark/parsers/lalr_analysis.py):
    //   * shift/reduce  → resolve as shift; no error in the default (non-strict) mode
    //   * reduce/reduce → resolve by rule priority; a tie for the top priority is
    //                     an unrepresentable grammar and raises a hard error
    let mut conflicts: Vec<String> = Vec::new();
    for (state_id, state) in states.iter().enumerate() {
        // Augmented start items reduce to ACCEPT on $END.
        for item in state {
            if item.is_complete(&rules)
                && rules[item.rule_idx].origin.name.starts_with("$root_")
            {
                action[state_id].insert(END_TERMINAL.to_string(), Action::Accept);
            }
        }

        let Some(rule_la) = reduce_la.get(&state_id) else { continue };
        // For each lookahead terminal, the set of rules that reduce on it.
        let mut reduces_by_la: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (&rule_idx, la_set) in rule_la {
            if rules[rule_idx].origin.name.starts_with("$root_") {
                continue; // handled as ACCEPT above
            }
            for la in la_set {
                reduces_by_la.entry(la.clone()).or_default().push(rule_idx);
            }
        }

        for (la, mut candidates) in reduces_by_la {
            candidates.sort_unstable();
            candidates.dedup();

            // Resolve reduce/reduce collisions by priority (higher wins; tie = error).
            let winner = if candidates.len() > 1 {
                let mut by_prio = candidates.clone();
                by_prio.sort_by_key(|&ri| std::cmp::Reverse(rules[ri].options.priority));
                let best = rules[by_prio[0]].options.priority;
                let second = rules[by_prio[1]].options.priority;
                if best > second {
                    by_prio[0]
                } else {
                    let rule_list: String = candidates
                        .iter()
                        .map(|&ri| format!("\n\t- {}", rules[ri]))
                        .collect();
                    conflicts.push(format!(
                        "Reduce/Reduce collision in state {} for terminal {}:{}",
                        state_id, la, rule_list
                    ));
                    continue;
                }
            } else {
                candidates[0]
            };

            // Shift/reduce: an existing shift (or accept) wins (Lark default).
            match action[state_id].get(&la) {
                Some(Action::Shift(_)) | Some(Action::Accept) => { /* shift/accept wins */ }
                _ => {
                    action[state_id].insert(la, Action::Reduce(winner));
                }
            }
        }
    }

    if !conflicts.is_empty() {
        return Err(GrammarError::Conflict {
            report: conflicts.join("\n\n"),
        });
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
        filter_out,
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

    // ─── Shared LALR driver helpers ──────────────────────────────────────────
    //
    // `parse` and `parse_contextual` differ only in how they source the next token
    // (a pre-lexed iterator vs. the contextual lexer). The state-machine core —
    // start resolution, REDUCE, ACCEPT, and the unexpected-token error — is shared
    // through the helpers below so the two drivers stay thin.

    /// Resolve the start symbol to its initial state.
    fn initial_state(&self, start: Option<&str>) -> Result<usize, ParseError> {
        let start_name = start.unwrap_or_else(|| {
            self.table.start_states.keys().next().map(String::as_str).unwrap_or("start")
        });
        self.table.start_states.get(start_name).copied().ok_or_else(|| {
            ParseError::UnexpectedEof {
                line: 0, col: 0,
                expected: vec![format!("start symbol '{}'", start_name)],
            }
        })
    }

    /// Valid terminals (the ACTION keys) for a state — used to build error reports.
    fn expected_at(&self, state: usize) -> Vec<String> {
        self.table.action.get(state).map(|m| m.keys().cloned().collect()).unwrap_or_default()
    }

    /// Apply a REDUCE: pop the rule's children, build its node (applying rule
    /// options), and follow GOTO. `at` supplies the position for the (effectively
    /// unreachable) missing-GOTO error.
    fn reduce(
        &self,
        rule_idx: usize,
        state_stack: &mut Vec<usize>,
        value_stack: &mut Vec<StackValue>,
        at: &Token,
    ) -> Result<(), ParseError> {
        let rule = &self.table.rules[rule_idx];
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
        let child = apply_rule_options(tree_name, children, rule, &self.table.filter_out);
        let top_state = *state_stack.last().unwrap();
        let next_state = self.table.goto[top_state]
            .get(&rule.origin.name)
            .copied()
            .ok_or_else(|| ParseError::UnexpectedToken {
                token: at.value.clone(),
                token_type: rule.origin.name.clone(),
                line: at.line, col: at.column,
                expected: vec![rule.origin.name.clone()],
            })?;
        state_stack.push(next_state);
        value_stack.push(match child {
            Child::Tree(t) => StackValue::Tree(t),
            Child::Token(t) => StackValue::Token(t),
            Child::None => unreachable!("placeholder cannot be a standalone rule value"),
        });
        Ok(())
    }

    /// ACCEPT: the final value on the stack is the parse result.
    fn accept(value_stack: &mut Vec<StackValue>) -> Result<Tree, ParseError> {
        value_stack.pop().map(|sv| match sv {
            StackValue::Tree(t) => t,
            StackValue::Token(tok) => Tree::new(tok.type_.clone(), vec![Child::Token(tok)]),
        }).ok_or(ParseError::UnexpectedEof { line: 0, col: 0, expected: vec![] })
    }

    /// Build the error for a token with no action in the current state.
    fn unexpected(&self, state: usize, token: &Token) -> ParseError {
        let expected = self.expected_at(state);
        if token.type_ == "$END" {
            ParseError::UnexpectedEof { line: token.line, col: token.column, expected }
        } else {
            ParseError::UnexpectedToken {
                token: token.value.clone(),
                token_type: token.type_.clone(),
                line: token.line, col: token.column,
                expected,
            }
        }
    }

    /// Parse a pre-tokenized sequence.
    pub fn parse(
        &self,
        tokens: Vec<Token>,
        start: Option<&str>,
    ) -> Result<Tree, ParseError> {
        let mut state_stack: Vec<usize> = vec![self.initial_state(start)?];
        let mut value_stack: Vec<StackValue> = Vec::new();
        let mut token_iter = tokens.into_iter().peekable();

        loop {
            let current_state = *state_stack.last().unwrap();
            let token = token_iter.peek().cloned().unwrap_or_else(|| Token::new("$END", ""));

            match self.table.action.get(current_state).and_then(|m| m.get(&token.type_)) {
                Some(Action::Shift(next_state)) => {
                    let tok = token_iter.next().unwrap();
                    state_stack.push(*next_state);
                    value_stack.push(StackValue::Token(tok));
                }
                Some(Action::Reduce(rule_idx)) => {
                    self.reduce(*rule_idx, &mut state_stack, &mut value_stack, &token)?;
                }
                Some(Action::Accept) => return Self::accept(&mut value_stack),
                None => return Err(self.unexpected(current_state, &token)),
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
        let mut state_stack: Vec<usize> = vec![self.initial_state(start)?];
        let mut value_stack: Vec<StackValue> = Vec::new();
        let mut lex_state = LexerState::new(text);
        let mut current_token: Option<Token> = None;

        loop {
            let current_state = *state_stack.last().unwrap();

            // Lex the next token for this state if we don't already hold one.
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
                                // Consume the ignored token and loop for the next real one.
                                lex_state.advance_by_lines(tok.value.len(), &tok.value);
                                continue;
                            }
                            current_token = Some(tok);
                            break;
                        }
                        None => {
                            // No terminal valid here matched at the current position.
                            let ch: String = lex_state.text[lex_state.pos..].chars().take(1).collect();
                            return Err(ParseError::UnexpectedToken {
                                token: ch,
                                token_type: String::new(),
                                line: lex_state.line,
                                col: lex_state.col,
                                expected: self.expected_at(current_state),
                            });
                        }
                    }
                }
            }
            let token = current_token.as_ref().unwrap().clone();

            match self.table.action.get(current_state).and_then(|m| m.get(&token.type_)) {
                Some(Action::Shift(next_state)) => {
                    state_stack.push(*next_state);
                    value_stack.push(StackValue::Token(token.clone()));
                    lex_state.advance_by(token.value.len());
                    current_token = None;
                }
                Some(Action::Reduce(rule_idx)) => {
                    // Don't advance the lexer — the same token may be consumed next.
                    self.reduce(*rule_idx, &mut state_stack, &mut value_stack, &token)?;
                }
                Some(Action::Accept) => return Self::accept(&mut value_stack),
                None => return Err(self.unexpected(current_state, &token)),
            }
        }
    }
}

// ─── Tree construction helpers ────────────────────────────────────────────────

enum StackValue {
    Token(Token),
    Tree(Tree),
}

fn apply_rule_options(
    name: String,
    mut children: Vec<Child>,
    rule: &Rule,
    filter_out: &HashSet<String>,
) -> Child {
    // Drop filter_out terminals (anonymous literals, `_`-prefixed) unless the rule
    // keeps all tokens. `None` placeholders are never filtered.
    if !rule.options.keep_all_tokens {
        children.retain(|c| match c {
            Child::Token(t) => !filter_out.contains(&t.type_),
            Child::Tree(_) => true,
            Child::None => true,
        });
    }

    // Inline children of anonymous/transparent rules (those whose name starts
    // with "__anon_" or is a _private_rule starting with "_").
    // These are EBNF expansion helpers that should be invisible in the final tree.
    let mut children = inline_anonymous_trees(children);

    // maybe_placeholders: an empty `[...]` production emits one `None` per kept
    // symbol of its widest alternative. These inline into the parent's children.
    for _ in 0..rule.options.placeholder_count {
        children.push(Child::None);
    }

    // expand1: if exactly one child and no alias, return that child directly.
    // This handles both Tree and Token children — a ?rule matching a single
    // terminal should yield the token itself, not a tree wrapping it. A lone
    // `None` placeholder is not collapsed (it must stay inside a tree, since the
    // value stack only holds tokens and trees).
    let has_alias = rule.alias.is_some();
    if rule.options.expand1 && !has_alias && children.len() == 1
        && !matches!(children[0], Child::None)
    {
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

/// A tree node is spliced into its parent (rather than kept as a child) when its
/// rule is "transparent". Two cases, both matching Python Lark:
///   * `__anon_*` — EBNF expansion helpers (`*`, `+`, `?`, groups).
///   * `_name`    — user-declared transparent rules (single leading underscore).
/// Aliased rules are exempt: an alias overrides transparency, and the node's name
/// is already the alias (which does not start with `_`), so it is not matched here.
fn is_anonymous_rule(name: &str) -> bool {
    name.starts_with('_')
}
