//! LALR(1) parser: table construction and execution.
//!
//! The pipeline is:
//! 1. `GrammarAnalysis` computes NULLABLE/FIRST.
//! 2. `LR0Builder` constructs LR(0) item sets (states).
//! 3. `LookaheadComputer` propagates true LALR(1) lookaheads.
//! 4. `build_lalr_table` assembles dense ACTION/GOTO tables.
//! 5. `LalrParser` drives the state machine against a token stream.
//!
//! The grammar is fully interned ([`CompiledGrammar`]): every symbol is a `Copy`
//! [`SymbolId`], terminals occupy id range `[0, n_terminals)` and non-terminals
//! `[n_terminals, len)`. So ACTION is a dense `[state][terminal_id]` matrix and
//! GOTO a dense `[state][nonterminal_index]` matrix — both pure array indexing,
//! no hashing on the hot path. Every tree-shaping decision is a precomputed flag
//! on the rule; the engine never inspects a symbol's name.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};

use crate::error::{GrammarError, ParseError};
use crate::grammar::analysis::GrammarAnalysis;
use crate::grammar::intern::{CompiledGrammar, CompiledRule, SymbolId, SymbolTable};
use crate::lexer::{ContextualLexer, LexerState};
use crate::tree::{Child, Token, Tree};

// ─── Parse table ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum Action {
    Shift(usize),  // shift and go to state N
    Reduce(usize), // reduce using rule index N
    Accept,
}

/// Immutable parse tables produced by LALR analysis. Dense and id-indexed.
#[derive(Debug)]
pub struct ParseTable {
    /// `action[state][terminal_id]` → action (None = error).
    pub action: Vec<Vec<Option<Action>>>,
    /// `goto[state][nonterminal_index]` → next state (None = no transition).
    pub goto: Vec<Vec<Option<u32>>>,
    /// Start state per start symbol.
    pub start_states: HashMap<SymbolId, usize>,
    /// Compiled rules (indexed by rule index).
    pub rules: Vec<CompiledRule>,
    /// `filter_out[terminal_id]` — token dropped from the tree by default.
    pub filter_out: Vec<bool>,
    /// Symbol metadata (names for diagnostics, kind, …).
    pub symbols: SymbolTable,
    /// Size of the terminal id range; non-terminal GOTO index is `id - this`.
    pub n_terminals: usize,
}

impl ParseTable {
    #[inline]
    fn is_filtered(&self, id: SymbolId) -> bool {
        self.filter_out.get(id.index()).copied().unwrap_or(false)
    }

    #[inline]
    fn action_at(&self, state: usize, terminal: SymbolId) -> Option<&Action> {
        self.action.get(state)?.get(terminal.index())?.as_ref()
    }
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

    fn expected(&self, rules: &[CompiledRule]) -> Option<SymbolId> {
        rules[self.rule_idx].expansion.get(self.dot).copied()
    }

    fn advance(&self) -> Self {
        LR0Item { rule_idx: self.rule_idx, dot: self.dot + 1 }
    }

    fn is_complete(&self, rules: &[CompiledRule]) -> bool {
        self.dot >= rules[self.rule_idx].expansion.len()
    }
}

type ItemSet = BTreeSet<LR0Item>;

// ─── LR(0) state machine builder ─────────────────────────────────────────────

struct LR0Builder<'g> {
    rules: &'g [CompiledRule],
    n_terminals: usize,
    /// non-terminal id → rule indices producing it.
    rule_index: HashMap<SymbolId, Vec<usize>>,
    states: Vec<ItemSet>,
    transitions: BTreeMap<(usize, SymbolId), usize>,
}

impl<'g> LR0Builder<'g> {
    fn new(rules: &'g [CompiledRule], n_terminals: usize) -> Self {
        let mut rule_index: HashMap<SymbolId, Vec<usize>> = HashMap::new();
        for (i, rule) in rules.iter().enumerate() {
            rule_index.entry(rule.origin).or_default().push(i);
        }
        LR0Builder { rules, n_terminals, rule_index, states: Vec::new(), transitions: BTreeMap::new() }
    }

    #[inline]
    fn is_nonterminal(&self, id: SymbolId) -> bool {
        id.index() >= self.n_terminals
    }

    /// Epsilon-closure of a set of LR(0) items.
    fn closure(&self, kernel: &ItemSet) -> ItemSet {
        let mut result = kernel.clone();
        let mut worklist: VecDeque<LR0Item> = kernel.iter().cloned().collect();
        while let Some(item) = worklist.pop_front() {
            if let Some(sym) = item.expected(self.rules) {
                if self.is_nonterminal(sym) {
                    if let Some(prods) = self.rule_index.get(&sym) {
                        for &rule_idx in prods {
                            let new_item = LR0Item::new(rule_idx, 0);
                            if result.insert(new_item.clone()) {
                                worklist.push_back(new_item);
                            }
                        }
                    }
                }
            }
        }
        result
    }

    /// GOTO transition from a state on a symbol.
    fn goto(&self, state: &ItemSet, sym: SymbolId) -> ItemSet {
        let moved: ItemSet = state
            .iter()
            .filter(|item| item.expected(self.rules) == Some(sym))
            .map(|item| item.advance())
            .collect();
        self.closure(&moved)
    }

    /// Build all LR(0) states from the augmented start rules.
    ///
    /// `starts`: (start symbol id, its augmented `$root` rule index).
    fn build(&mut self, starts: &[(SymbolId, usize)]) -> HashMap<SymbolId, usize> {
        let mut start_states = HashMap::new();
        let mut index: HashMap<ItemSet, usize> = HashMap::new();
        let mut worklist: VecDeque<usize> = VecDeque::new();

        for &(start_id, aug_idx) in starts {
            let kernel: ItemSet = std::iter::once(LR0Item::new(aug_idx, 0)).collect();
            let s0 = self.intern_state(kernel, &mut index, &mut worklist);
            start_states.insert(start_id, s0);
        }

        // Iterative worklist (not recursion) so deep GOTO chains — e.g. `"A"~8191`
        // expands to thousands of chained states — do not overflow the stack.
        while let Some(id) = worklist.pop_front() {
            let closed = self.states[id].clone();
            let symbols: BTreeSet<SymbolId> =
                closed.iter().filter_map(|item| item.expected(self.rules)).collect();
            for sym in symbols {
                let next_state_items = self.goto(&closed, sym);
                if !next_state_items.is_empty() {
                    let next_id = self.intern_state(next_state_items, &mut index, &mut worklist);
                    self.transitions.insert((id, sym), next_id);
                }
            }
        }
        start_states
    }

    /// State id for the closure of `kernel`, creating + queuing it if new.
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

/// Sentinel lookahead marking lookaheads that *propagate* from a kernel item
/// rather than being generated spontaneously. [`SymbolId::UNSET`] can never
/// collide with a real terminal (terminals live in `[0, n_terminals)`).
const PROPAGATE_MARK: SymbolId = SymbolId::UNSET;

/// Computes true LALR(1) lookaheads for every reduce item via spontaneous
/// generation + propagation (Aho/Sethi/Ullman 4.62–4.63). Strictly more precise
/// than SLR FOLLOW sets, which is what avoids spurious conflicts and what the
/// contextual lexer relies on.
struct LookaheadComputer<'g> {
    rules: &'g [CompiledRule],
    states: &'g [ItemSet],
    transitions: &'g BTreeMap<(usize, SymbolId), usize>,
    analysis: &'g GrammarAnalysis,
}

impl<'g> LookaheadComputer<'g> {
    fn new(
        rules: &'g [CompiledRule],
        states: &'g [ItemSet],
        transitions: &'g BTreeMap<(usize, SymbolId), usize>,
        analysis: &'g GrammarAnalysis,
    ) -> Self {
        LookaheadComputer { rules, states, transitions, analysis }
    }

    /// Kernel items: dot past the start, plus the augmented start items (dot 0).
    fn is_kernel(&self, item: &LR0Item) -> bool {
        item.dot > 0 || self.rules[item.rule_idx].is_start
    }

    fn rule_index(&self) -> HashMap<SymbolId, Vec<usize>> {
        let mut idx: HashMap<SymbolId, Vec<usize>> = HashMap::new();
        for (i, rule) in self.rules.iter().enumerate() {
            idx.entry(rule.origin).or_default().push(i);
        }
        idx
    }

    /// LR(1) closure: propagate lookahead sets from kernel items to every
    /// reachable closure item, to a fixpoint.
    fn lr1_closure(
        &self,
        kernel: &HashMap<LR0Item, HashSet<SymbolId>>,
        rule_index: &HashMap<SymbolId, Vec<usize>>,
    ) -> HashMap<LR0Item, HashSet<SymbolId>> {
        let mut result: HashMap<LR0Item, HashSet<SymbolId>> = kernel.clone();
        let mut changed = true;
        while changed {
            changed = false;
            let snapshot: Vec<(LR0Item, HashSet<SymbolId>)> =
                result.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            for (item, la) in snapshot {
                let Some(sym) = item.expected(self.rules) else { continue };
                let Some(prods) = rule_index.get(&sym) else { continue };
                let rule = &self.rules[item.rule_idx];
                let beta = &rule.expansion[item.dot + 1..];
                let (first_beta, beta_nullable) = self.analysis.first_of_seq(beta);
                let mut seed = first_beta;
                if beta_nullable {
                    seed.extend(la.iter().copied());
                }
                for &ri in prods {
                    let entry = result.entry(LR0Item::new(ri, 0)).or_default();
                    for &s in &seed {
                        if entry.insert(s) {
                            changed = true;
                        }
                    }
                }
            }
        }
        result
    }

    /// Lookaheads for every reduce item in every state.
    /// `reduce_la[state][rule_idx]` → lookahead terminals.
    fn compute(&self) -> HashMap<usize, HashMap<usize, HashSet<SymbolId>>> {
        let rule_index = self.rule_index();
        // Kernel-item lookahead sets, keyed (state, item).
        let mut kla: HashMap<(usize, LR0Item), HashSet<SymbolId>> = HashMap::new();
        // Propagation links: (from_state, from_item) → (to_state, to_item).
        let mut links: Vec<((usize, LR0Item), (usize, LR0Item))> = Vec::new();

        // Seed augmented start kernels with $END.
        for (state_id, state) in self.states.iter().enumerate() {
            for item in state {
                if self.is_kernel(item) {
                    let set = kla.entry((state_id, item.clone())).or_default();
                    if self.rules[item.rule_idx].is_start {
                        set.insert(SymbolId::END);
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
                seed.insert(k.clone(), std::iter::once(PROPAGATE_MARK).collect());
                let closed = self.lr1_closure(&seed, &rule_index);
                for (item, la_set) in &closed {
                    let Some(sym) = item.expected(self.rules) else { continue };
                    let Some(&goto_state) = self.transitions.get(&(state_id, sym)) else { continue };
                    let advanced = item.advance();
                    for &a in la_set {
                        if a == PROPAGATE_MARK {
                            links.push(((state_id, k.clone()), (goto_state, advanced.clone())));
                        } else {
                            kla.entry((goto_state, advanced.clone())).or_default().insert(a);
                        }
                    }
                }
            }
        }

        // Propagate lookaheads along links to a fixpoint.
        let mut changed = true;
        while changed {
            changed = false;
            for (from, to) in &links {
                let src: Vec<SymbolId> = kla.get(from).map(|s| s.iter().copied().collect()).unwrap_or_default();
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

        // Final per-state LR(1) closure so ε-rule reduce items (closure items,
        // not kernels) also receive lookaheads; collect per reduce rule.
        let mut reduce_la: HashMap<usize, HashMap<usize, HashSet<SymbolId>>> = HashMap::new();
        for (state_id, state) in self.states.iter().enumerate() {
            let mut kernel: HashMap<LR0Item, HashSet<SymbolId>> = HashMap::new();
            for item in state {
                if self.is_kernel(item) {
                    let set = kla.get(&(state_id, item.clone())).cloned().unwrap_or_default();
                    kernel.insert(item.clone(), set);
                }
            }
            let closed = self.lr1_closure(&kernel, &rule_index);
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

pub fn build_lalr_table(grammar: &CompiledGrammar) -> Result<ParseTable, GrammarError> {
    let rules = &grammar.rules;
    let n_terminals = grammar.n_terminals();
    let n_nonterminals = grammar.symbols.n_nonterminals();
    let analysis = GrammarAnalysis::compute(grammar);

    // Pair each start symbol with its augmented `$root` rule index.
    let starts: Vec<(SymbolId, usize)> = grammar
        .start
        .iter()
        .filter_map(|&s| {
            rules
                .iter()
                .position(|r| r.is_start && r.expansion == [s])
                .map(|idx| (s, idx))
        })
        .collect();

    // LR(0) state construction.
    let mut builder = LR0Builder::new(rules, n_terminals);
    let start_states = builder.build(&starts);
    let (states, transitions) = (builder.states, builder.transitions);

    let n_states = states.len();
    let mut action: Vec<Vec<Option<Action>>> = vec![vec![None; n_terminals]; n_states];
    let mut goto: Vec<Vec<Option<u32>>> = vec![vec![None; n_nonterminals]; n_states];

    // SHIFT and GOTO from transitions — terminal vs non-terminal is the id range.
    for (&(state_id, sym), &next_state) in &transitions {
        if sym.index() < n_terminals {
            action[state_id][sym.index()] = Some(Action::Shift(next_state));
        } else {
            goto[state_id][sym.index() - n_terminals] = Some(next_state as u32);
        }
    }

    // REDUCE / ACCEPT, resolving conflicts exactly as Python Lark does:
    //   * shift/reduce  → shift wins (no error in default mode)
    //   * reduce/reduce → highest rule priority wins; a tie is a hard error
    let reduce_la = LookaheadComputer::new(rules, &states, &transitions, &analysis).compute();
    let mut conflicts: Vec<String> = Vec::new();

    for (state_id, state) in states.iter().enumerate() {
        // Augmented start items reduce to ACCEPT on $END.
        for item in state {
            if item.is_complete(rules) && rules[item.rule_idx].is_start {
                action[state_id][SymbolId::END.index()] = Some(Action::Accept);
            }
        }

        let Some(rule_la) = reduce_la.get(&state_id) else { continue };
        let mut reduces_by_la: BTreeMap<SymbolId, Vec<usize>> = BTreeMap::new();
        for (&rule_idx, la_set) in rule_la {
            if rules[rule_idx].is_start {
                continue; // handled as ACCEPT
            }
            for &la in la_set {
                reduces_by_la.entry(la).or_default().push(rule_idx);
            }
        }

        for (la, mut candidates) in reduces_by_la {
            candidates.sort_unstable();
            candidates.dedup();

            let winner = if candidates.len() > 1 {
                let mut by_prio = candidates.clone();
                by_prio.sort_by_key(|&ri| std::cmp::Reverse(rules[ri].options.priority));
                let best = rules[by_prio[0]].options.priority;
                let second = rules[by_prio[1]].options.priority;
                if best > second {
                    by_prio[0]
                } else {
                    let rule_list: String =
                        candidates.iter().map(|&ri| format!("\n\t- {}", rules[ri])).collect();
                    conflicts.push(format!(
                        "Reduce/Reduce collision in state {} for terminal {}:{}",
                        state_id,
                        grammar.symbols.name(la),
                        rule_list
                    ));
                    continue;
                }
            } else {
                candidates[0]
            };

            // Shift/accept wins over reduce (Lark default).
            match &action[state_id][la.index()] {
                Some(Action::Shift(_)) | Some(Action::Accept) => {}
                _ => action[state_id][la.index()] = Some(Action::Reduce(winner)),
            }
        }
    }

    if !conflicts.is_empty() {
        return Err(GrammarError::Conflict { report: conflicts.join("\n\n") });
    }

    let filter_out: Vec<bool> =
        (0..n_terminals).map(|t| grammar.symbols.info(SymbolId(t as u32)).filter_out).collect();

    Ok(ParseTable {
        action,
        goto,
        start_states,
        rules: rules.clone(),
        filter_out,
        symbols: grammar.symbols.clone(),
        n_terminals,
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

    /// Valid terminal ids per state — for the contextual lexer.
    pub fn state_terminals(&self) -> HashMap<usize, Vec<SymbolId>> {
        self.table
            .action
            .iter()
            .enumerate()
            .map(|(state, row)| {
                let ids = row
                    .iter()
                    .enumerate()
                    .filter_map(|(t, a)| a.as_ref().map(|_| SymbolId(t as u32)))
                    .collect();
                (state, ids)
            })
            .collect()
    }

    // ─── Shared LALR driver helpers ──────────────────────────────────────────
    //
    // `parse` and `parse_contextual` differ only in how they source the next
    // token (a pre-lexed iterator vs. the contextual lexer). The state-machine
    // core is shared through the helpers below so the two drivers stay thin.

    /// Resolve the start symbol name to its initial state.
    fn initial_state(&self, start: Option<&str>) -> Result<usize, ParseError> {
        let start_id = match start {
            Some(name) => self.table.symbols.id(name),
            None => self.table.start_states.keys().next().copied(),
        };
        start_id.and_then(|id| self.table.start_states.get(&id).copied()).ok_or_else(|| {
            ParseError::UnexpectedEof {
                line: 0,
                col: 0,
                expected: vec![format!("start symbol '{}'", start.unwrap_or("?"))],
            }
        })
    }

    /// Valid terminal names for a state — used to build error reports.
    fn expected_at(&self, state: usize) -> Vec<String> {
        self.table
            .action
            .get(state)
            .map(|row| {
                row.iter()
                    .enumerate()
                    .filter_map(|(t, a)| a.as_ref().map(|_| self.table.symbols.name(SymbolId(t as u32)).to_string()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Apply a REDUCE: pop the rule's children, build its value (applying rule
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

        // Collect children: expand inlined (transparent) sub-rules, and drop
        // filtered tokens unless this rule keeps all tokens. Inlined children were
        // already filtered when their own rule reduced.
        let mut children: Vec<Child> = Vec::new();
        for sv in value_stack.drain(value_stack.len() - len..) {
            match sv {
                StackValue::Token(t) => {
                    if rule.options.keep_all_tokens || !self.table.is_filtered(t.type_id) {
                        children.push(Child::Token(t));
                    }
                }
                StackValue::Tree(t) => children.push(Child::Tree(t)),
                StackValue::Inline(cs) => children.extend(cs),
            }
        }
        for _ in 0..len {
            state_stack.pop();
        }

        // maybe_placeholders: an empty `[...]` production emits one `None` per
        // kept symbol of its widest alternative.
        for _ in 0..rule.options.placeholder_count {
            children.push(Child::None);
        }

        let value = if rule.transparent {
            // `_rule` / `__anon_*`: splice children into the parent.
            StackValue::Inline(children)
        } else if rule.options.expand1
            && rule.alias.is_none()
            && children.len() == 1
            && !matches!(children[0], Child::None)
        {
            // `?rule` with a single child: return that child directly (Token or Tree).
            match children.pop().unwrap() {
                Child::Tree(t) => StackValue::Tree(t),
                Child::Token(t) => StackValue::Token(t),
                Child::None => unreachable!("guarded above"),
            }
        } else {
            StackValue::Tree(Tree::new(rule.tree_name.clone(), children))
        };

        let top_state = *state_stack.last().unwrap();
        let nt_index = rule.origin.index() - self.table.n_terminals;
        let next_state = self.table.goto[top_state]
            .get(nt_index)
            .copied()
            .flatten()
            .ok_or_else(|| ParseError::UnexpectedToken {
                token: at.value.clone(),
                token_type: self.table.symbols.name(rule.origin).to_string(),
                line: at.line,
                col: at.column,
                expected: vec![self.table.symbols.name(rule.origin).to_string()],
            })?;
        state_stack.push(next_state as usize);
        value_stack.push(value);
        Ok(())
    }

    /// ACCEPT: the final value on the stack is the parse result.
    fn accept(value_stack: &mut Vec<StackValue>) -> Result<Tree, ParseError> {
        match value_stack.pop() {
            Some(StackValue::Tree(t)) => Ok(t),
            Some(StackValue::Token(tok)) => Ok(Tree::new(tok.type_.clone(), vec![Child::Token(tok)])),
            // A start rule is never transparent, so its value is never Inline.
            Some(StackValue::Inline(_)) | None => {
                Err(ParseError::UnexpectedEof { line: 0, col: 0, expected: vec![] })
            }
        }
    }

    /// Build the error for a token with no action in the current state.
    fn unexpected(&self, state: usize, token: &Token) -> ParseError {
        let expected = self.expected_at(state);
        if token.type_id == SymbolId::END {
            ParseError::UnexpectedEof { line: token.line, col: token.column, expected }
        } else {
            ParseError::UnexpectedToken {
                token: token.value.clone(),
                token_type: token.type_.clone(),
                line: token.line,
                col: token.column,
                expected,
            }
        }
    }

    /// Parse a pre-tokenized sequence.
    pub fn parse(&self, tokens: Vec<Token>, start: Option<&str>) -> Result<Tree, ParseError> {
        let mut state_stack: Vec<usize> = vec![self.initial_state(start)?];
        let mut value_stack: Vec<StackValue> = Vec::new();
        let mut token_iter = tokens.into_iter().peekable();

        loop {
            let current_state = *state_stack.last().unwrap();
            let token = token_iter.peek().cloned().unwrap_or_else(Token::end);

            match self.table.action_at(current_state, token.type_id) {
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

    /// Parse using the contextual lexer — lex one token at a time, feeding the
    /// current parser state to the lexer so it only tries valid terminals.
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
                        current_token = Some(Token::end().with_position(
                            lex_state.line,
                            lex_state.col,
                            lex_state.pos,
                            lex_state.pos,
                        ));
                        break;
                    }
                    match lexer.next_token(lex_state.text, lex_state.pos, current_state, lex_state.line, lex_state.col)? {
                        Some(tok) => {
                            if lexer.is_ignored(tok.type_id) {
                                lex_state.advance_by_lines(tok.value.len(), &tok.value);
                                continue;
                            }
                            current_token = Some(tok);
                            break;
                        }
                        None => {
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

            match self.table.action_at(current_state, token.type_id) {
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

// ─── Stack values ─────────────────────────────────────────────────────────────

/// A value on the parser's semantic stack.
enum StackValue {
    Token(Token),
    Tree(Tree),
    /// Children of a transparent (`_rule` / `__anon_*`) reduction, to be spliced
    /// into the parent's child list rather than wrapped in a node.
    Inline(Vec<Child>),
}
