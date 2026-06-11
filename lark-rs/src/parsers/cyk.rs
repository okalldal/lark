//! The CYK (Cocke–Younger–Kasami) parser.
//!
//! CYK is an O(n³) dynamic-programming parser that handles *any* context-free
//! grammar once it is put in Chomsky Normal Form (CNF) — including highly
//! ambiguous grammars that LALR cannot express and that Earley handles less
//! efficiently. It cannot handle ε-rules (empty productions): a grammar that
//! produces one (directly, or after lark-rs's EBNF expansion of `*`/`?`/`[]`)
//! fails to build, exactly as Python Lark's CYK does.
//!
//! This is a faithful port of Python Lark's `lark/parsers/cyk.py` — our oracle —
//! adapted to lark-rs's interned [`CompiledGrammar`]:
//!
//!   * CNF conversion applies TERM (lift non-solitary terminals into `__T_`
//!     wrapper rules), BIN (binarize rules with >2 symbols via `__SP_` split
//!     rules), then UNIT (eliminate non-terminal unit rules, recording the
//!     skipped chain so it can be unrolled afterwards).
//!   * The DP fills a triangular table over token spans and, per cell, keeps the
//!     lightest (lowest total `weight = priority`) derivation per non-terminal —
//!     Python's min-weight selection (note: this means a *lower* `priority`
//!     value wins under CYK, the inverse of Earley; we mirror it for parity).
//!   * `revert` undoes the three CNF transforms to recover a tree of *original*
//!     rule applications, which is then handed to the shared
//!     [`TreeBuilder`](super::tree_builder::TreeBuilder) — the same rule→tree
//!     shaping (filtering, `expand1`, transparent splicing, `keep_all_tokens`,
//!     `maybe_placeholders`) the LALR and Earley backends use. So a CYK parse of
//!     an unambiguous grammar yields a byte-identical tree to the other backends.
//!
//! Like Earley, CYK uses the basic lexer (the contextual lexer is LALR-only) and
//! dispatches terminals on the interned [`SymbolId`], never on a name.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;

use crate::error::{GrammarError, ParseError};
use crate::grammar::intern::{CompiledGrammar, SymbolId};
use crate::tree::{Child, ParseTree, Token, Tree};

use super::tree_builder::{NodeValue, TreeBuilder};

/// Safety bound on the UNIT-elimination fixpoint. Real grammars converge in a
/// handful of iterations; a pathological unit cycle (which Python's CYK would
/// also mishandle) is rejected at build time rather than looped on forever.
const UNIT_ITERATION_LIMIT: usize = 2_000_000;

/// Cap on the number of nullable symbol occurrences in a single rule. ε-removal
/// duplicates a rule into 2ⁿ present/absent variants, so an absurd `n` is
/// rejected at build time rather than allowed to explode (such a grammar would
/// also be hopelessly slow in Python's CYK — its suite skips those).
const MAX_NULLABLE_POSITIONS: u32 = 20;

/// A CNF non-terminal: an original grammar symbol, a TERM wrapper around a
/// terminal (`__T_t`), or a BIN split helper (`__SP_<group>_<pos>`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Nt {
    /// An original grammar non-terminal (carries its interned id).
    Orig(SymbolId),
    /// TERM wrapper rule's left-hand side: `__T_t → t` for a terminal `t`.
    Term(SymbolId),
    /// BIN split helper: `(split group, position)`. Reverted away by splicing.
    Split(u32, u32),
}

/// A symbol on a CNF rule's right-hand side: a terminal (matches a token's type)
/// or a non-terminal.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Sym {
    T(SymbolId),
    N(Nt),
}

/// A CNF rule. After full conversion every rule has `rhs` of length 1 (a single
/// terminal) or 2 (two non-terminals).
#[derive(Debug, Clone)]
struct CnfRule {
    lhs: Nt,
    rhs: Vec<Sym>,
    /// Disambiguation weight (sum of priorities); the DP keeps the minimum.
    weight: i32,
    /// Index into the [`EffRule`] table of the original rule (and ε-variant) this
    /// came from, or `None` for a synthetic TERM/BIN helper (never used to build a
    /// tree node).
    alias: Option<usize>,
    /// For a UNIT-eliminated rule: the [`EffRule`] indices of the unit chain that
    /// was collapsed, in order, so `revert` can unroll it back into nested nodes.
    /// Empty for every other rule.
    skipped: Vec<usize>,
}

/// An "effective" rule: a (possibly ε-reduced) view of one original grammar rule.
///
/// ε-removal duplicates a rule into variants that *omit* some nullable
/// occurrences. Each variant points back at the original rule and records, per
/// original expansion position, whether that symbol is present in the variant.
/// At tree-assembly time the omitted positions are refilled with the symbol's
/// precomputed ε-value (see [`Cnf::epsilon_values`]), so the shared [`TreeBuilder`]
/// still sees a child per original expansion symbol and applies the original
/// rule's filtering / shaping unchanged.
#[derive(Debug, Clone)]
struct EffRule {
    /// Index into [`CompiledGrammar::rules`].
    rule_idx: usize,
    /// One flag per original expansion position: `true` = present in this variant
    /// (consumes a parsed child), `false` = omitted (a nullable symbol that derived
    /// ε here; refilled with its ε-value on assembly).
    present_mask: Vec<bool>,
}

/// The CNF grammar plus the lookup indices the DP needs.
struct Cnf {
    rules: Vec<CnfRule>,
    /// Original-rule views (see [`EffRule`]); `CnfRule::alias` / `skipped` index it.
    eff_rules: Vec<EffRule>,
    /// The value an ε-deriving non-terminal contributes when it is *omitted* by
    /// ε-removal — precomputed by assembling its empty production through the
    /// shared [`TreeBuilder`], so an absent optional refills with exactly what the
    /// other backends emit (an empty splice for `*`/`+`, a `None` placeholder for a
    /// `maybe_placeholders` `[...]`, an aliased empty node, …). Keyed by symbol id.
    epsilon_values: HashMap<SymbolId, NodeValue>,
    /// Terminal id → rule indices of `X → [terminal]` productions.
    terminal_rules: HashMap<SymbolId, Vec<usize>>,
    /// `(A, B)` → rule indices of `X → [A B]` productions.
    nonterminal_rules: HashMap<(Nt, Nt), Vec<usize>>,
}

/// A node of the CYK parse tree (still in CNF). Children are shared via `Rc`
/// because a span/non-terminal's best tree is referenced by every larger span
/// that builds on it (Python relies on the same structural sharing).
enum PNode {
    Leaf(Token),
    Rule(usize, Vec<Rc<PNode>>),
}

/// A best-derivation table cell: the lightest tree for one non-terminal over a
/// span, plus its total weight (for the min-weight comparison).
#[derive(Clone)]
struct Cell {
    node: Rc<PNode>,
    weight: i32,
}

/// The reverted (original-grammar) parse tree, ready for [`TreeBuilder`].
enum Rev {
    Tok(Token),
    /// An application of original rule `usize` over these children, in expansion
    /// order.
    Node(usize, Vec<Rev>),
    /// Children of a BIN split helper, spliced into the parent in place.
    Splice(Vec<Rev>),
}

/// A CYK parser over the interned grammar. CNF conversion happens once at
/// construction (mirroring Python's `cyk.Parser.__init__`), so an unconvertible
/// grammar — e.g. one with ε-rules — is rejected as a build error.
pub struct CykParser {
    grammar: CompiledGrammar,
    cnf: Cnf,
}

impl CykParser {
    /// Convert `grammar` to CNF and build the lookup indices. Returns a
    /// [`GrammarError`] if the grammar cannot be represented in CNF (an empty
    /// production, or a malformed rule after conversion), matching Python Lark,
    /// which raises while constructing the CYK frontend.
    pub fn new(grammar: CompiledGrammar) -> Result<Self, GrammarError> {
        let (rules, eff_rules, epsilon_values) = to_cnf(&grammar)?;
        let cnf = build_indices(rules, eff_rules, epsilon_values)?;
        Ok(CykParser { grammar, cnf })
    }

    fn start_id(&self, start: Option<&str>) -> Option<SymbolId> {
        match start {
            Some(name) => self.grammar.symbols.id(name),
            None => self.grammar.start.first().copied(),
        }
    }

    /// Parse `tokens` (a basic-lexer stream; a trailing `$END` is ignored) from
    /// `start` into a [`ParseTree`].
    pub fn parse(&self, tokens: &[Token], start: Option<&str>) -> Result<ParseTree, ParseError> {
        let start_id = self.start_id(start).ok_or_else(parse_failed)?;
        let toks: Vec<&Token> = tokens
            .iter()
            .filter(|t| t.type_id != SymbolId::END)
            .collect();
        let n = toks.len();
        // CYK can't derive the empty string (it has no ε-rules), so an empty
        // token stream is always a parse failure — like Python Lark's CYK.
        if n == 0 {
            return Err(parse_failed());
        }

        // trees[i][j] (i ≤ j): the lightest derivation per non-terminal spanning
        // tokens i..=j. Lower triangle is unused.
        let mut trees: Vec<Vec<BTreeMap<Nt, Cell>>> = vec![vec![BTreeMap::new(); n]; n];

        // Base case: each token, every terminal production that matches it.
        for (i, w) in toks.iter().enumerate() {
            if let Some(rule_ids) = self.cnf.terminal_rules.get(&w.type_id) {
                for &rid in rule_ids {
                    let r = &self.cnf.rules[rid];
                    let better = trees[i][i].get(&r.lhs).is_none_or(|c| r.weight < c.weight);
                    if better {
                        let node =
                            Rc::new(PNode::Rule(rid, vec![Rc::new(PNode::Leaf((*w).clone()))]));
                        trees[i][i].insert(
                            r.lhs.clone(),
                            Cell {
                                node,
                                weight: r.weight,
                            },
                        );
                    }
                }
            }
        }

        // Fill spans of increasing length, combining two adjacent sub-spans.
        for l in 2..=n {
            for i in 0..=(n - l) {
                let j = i + l - 1;
                let mut best: BTreeMap<Nt, Cell> = BTreeMap::new();
                for p in (i + 1)..=j {
                    let left = &trees[i][p - 1];
                    let right = &trees[p][j];
                    for (lhs_a, cell_a) in left {
                        for (lhs_b, cell_b) in right {
                            // One table-fill combination step. Summed over all
                            // spans/splits this is the `O(n³ · |grammar|)` DP cost
                            // the scaling gate (`tests/test_cyk_scaling.rs`) pins.
                            crate::perf::add_cyk_table_steps(1);
                            let key = (lhs_a.clone(), lhs_b.clone());
                            let Some(rule_ids) = self.cnf.nonterminal_rules.get(&key) else {
                                continue;
                            };
                            for &rid in rule_ids {
                                let r = &self.cnf.rules[rid];
                                let total = r
                                    .weight
                                    .saturating_add(cell_a.weight)
                                    .saturating_add(cell_b.weight);
                                let better = best.get(&r.lhs).is_none_or(|c| total < c.weight);
                                if better {
                                    let node = Rc::new(PNode::Rule(
                                        rid,
                                        vec![cell_a.node.clone(), cell_b.node.clone()],
                                    ));
                                    best.insert(
                                        r.lhs.clone(),
                                        Cell {
                                            node,
                                            weight: total,
                                        },
                                    );
                                }
                            }
                        }
                    }
                }
                trees[i][j] = best;
            }
        }

        let root = trees[0][n - 1]
            .get(&Nt::Orig(start_id))
            .ok_or_else(parse_failed)?
            .node
            .clone();

        // Undo the CNF transforms to recover original rule applications, then
        // shape them through the shared TreeBuilder. The recursion depth is the
        // parse-tree depth, O(n) in the worst case — but CYK's O(n³) table fill
        // already keeps any feasible input (and so the depth) small, unlike the
        // Earley perf path that needs a dedicated large stack.
        let builder = TreeBuilder::new(&self.grammar.rules);
        let rev = revert(&root, &self.cnf.rules);
        let value = assemble_rev(rev, &builder, &self.cnf, &self.grammar);
        let start_name = self.grammar.symbols.name(start_id).to_string();

        Ok(match value {
            NodeValue::Tree(t) => ParseTree::Tree(t),
            NodeValue::Token(t) => ParseTree::Token(t),
            // A start rule is never transparent, so its value is never Inline; be
            // defensive rather than panic (mirrors Earley's forest_to_tree).
            NodeValue::Inline(mut cs) if cs.len() == 1 => match cs.pop().unwrap() {
                Child::Tree(t) => ParseTree::Tree(t),
                Child::Token(t) => ParseTree::Token(t),
                Child::None => ParseTree::Tree(Tree::new(start_name, vec![])),
            },
            NodeValue::Inline(cs) => ParseTree::Tree(Tree::new(start_name, cs)),
        })
    }
}

/// The uniform "couldn't parse" error. CYK does not localize failures the way the
/// shift/reduce backends do (the DP only reports whether the start symbol spans
/// the input), so it reports a single end-of-input failure, like Python Lark's
/// `ParseError('Parsing failed.')`.
fn parse_failed() -> ParseError {
    ParseError::unexpected_eof(0, 0, vec![])
}

// ─── CNF conversion ─────────────────────────────────────────────────────────

/// Lower `grammar`'s (non-augmented) rules to CNF. The pipeline is: prune
/// unreachable rules, eliminate ε-productions (so CYK can handle lark-rs's
/// nullable `*`/`?`/`+` helpers), then TERM, BIN, UNIT — mirroring Python Lark,
/// whose own EBNF expansion and reachability pruning leave a comparable ε-free
/// rule set before its CYK runs.
type CnfResult = (Vec<CnfRule>, Vec<EffRule>, HashMap<SymbolId, NodeValue>);

fn to_cnf(grammar: &CompiledGrammar) -> Result<CnfResult, GrammarError> {
    let mut eff_rules: Vec<EffRule> = Vec::new();
    let mut rules: Vec<CnfRule> = Vec::new();
    for (idx, r) in grammar.rules.iter().enumerate() {
        // Skip the synthetic augmented-start rules ($root_X → X): Python's CYK is
        // given the user rules and checks the user start symbol directly.
        if r.is_start {
            continue;
        }
        let rhs: Vec<Sym> = r
            .expansion
            .iter()
            .map(|&s| {
                if grammar.symbols.is_terminal(s) {
                    Sym::T(s)
                } else {
                    Sym::N(Nt::Orig(s))
                }
            })
            .collect();
        let eff = eff_rules.len();
        eff_rules.push(EffRule {
            rule_idx: idx,
            present_mask: vec![true; r.expansion.len()],
        });
        rules.push(CnfRule {
            lhs: Nt::Orig(r.origin),
            rhs,
            weight: r.options.priority,
            alias: Some(eff),
            skipped: Vec::new(),
        });
    }

    // Keep only rules reachable from a user start symbol, so an unreachable
    // nullable rule (e.g. `unused: x*`) can't force a spurious ε-rule rejection —
    // Python prunes these before CYK too.
    let rules = prune_unreachable(rules, &grammar.start);

    // CYK cannot represent a node that derives ε. lark-rs's nullable helpers are
    // transparent (spliced away when empty), so they ε-reduce without changing the
    // tree; but a nullable *non-transparent* rule would produce an observable empty
    // node CYK can't model — reject it, exactly as Python Lark's CYK rejects the
    // empty rule its own expansion would emit.
    let nullable = compute_nullable(&rules);
    for nt in &nullable {
        if let Nt::Orig(id) = nt {
            if !grammar.symbols.info(*id).inline {
                return Err(GrammarError::Other {
                    msg: "CYK doesn't support empty rules".to_string(),
                });
            }
        }
    }
    // Precompute, before the rules are mutated, the value each nullable symbol
    // contributes when ε-removal omits it — so an absent `[...]`'s `None`
    // placeholder (and any other ε-derivation shaping) survives.
    let epsilon_values = compute_epsilon_values(grammar, &nullable);

    let rules = eliminate_epsilon(rules, &nullable, &mut eff_rules)?;

    let rules = dedup(term(rules));
    let mut split_counter: u32 = 0;
    let rules = dedup(bin(rules, &mut split_counter));
    let rules = unit(rules)?;
    Ok((rules, eff_rules, epsilon_values))
}

/// For every nullable non-terminal, precompute the [`NodeValue`] it yields when it
/// derives ε — by assembling its lightest ε-production through the shared
/// [`TreeBuilder`], recursively over its (also nullable) children. This is exactly
/// what LALR/Earley produce for an empty derivation, so refilling an ε-removed
/// (omitted) position with it keeps CYK's tree identical: an empty splice for a
/// plain `*`/`?` helper, a `None` placeholder for a `maybe_placeholders` `[...]`,
/// or an aliased empty node where the empty production is non-transparent.
fn compute_epsilon_values(
    grammar: &CompiledGrammar,
    nullable: &HashSet<Nt>,
) -> HashMap<SymbolId, NodeValue> {
    let builder = TreeBuilder::new(&grammar.rules);
    let mut memo: HashMap<SymbolId, NodeValue> = HashMap::new();
    let mut visiting: HashSet<SymbolId> = HashSet::new();
    for nt in nullable {
        if let Nt::Orig(id) = nt {
            eps_value(*id, grammar, nullable, &builder, &mut memo, &mut visiting);
        }
    }
    memo
}

/// Memoized helper for [`compute_epsilon_values`]: the ε-value of one symbol.
fn eps_value(
    id: SymbolId,
    grammar: &CompiledGrammar,
    nullable: &HashSet<Nt>,
    builder: &TreeBuilder,
    memo: &mut HashMap<SymbolId, NodeValue>,
    visiting: &mut HashSet<SymbolId>,
) -> NodeValue {
    if let Some(v) = memo.get(&id) {
        return v.clone();
    }
    // A nullable symbol that recurs through its own ε-derivation is pathological;
    // contribute nothing rather than loop.
    if !visiting.insert(id) {
        return NodeValue::Inline(Vec::new());
    }
    // The lightest ε-deriving production (all-nullable rhs), matching the DP's
    // min-weight, then first, selection.
    let chosen = grammar
        .rules
        .iter()
        .enumerate()
        .filter(|(_, r)| {
            !r.is_start
                && r.origin == id
                && r.expansion
                    .iter()
                    .all(|&s| !grammar.symbols.is_terminal(s) && nullable.contains(&Nt::Orig(s)))
        })
        .min_by_key(|(i, r)| (r.options.priority, *i))
        .map(|(i, _)| i);
    let value = match chosen {
        Some(ri) => {
            let child_values: Vec<NodeValue> = grammar.rules[ri]
                .expansion
                .iter()
                .map(|&s| eps_value(s, grammar, nullable, builder, memo, visiting))
                .collect();
            builder.assemble(ri, child_values)
        }
        // `id` is nullable but has no all-nullable production reachable here; treat
        // as an empty contribution.
        None => NodeValue::Inline(Vec::new()),
    };
    visiting.remove(&id);
    memo.insert(id, value.clone());
    value
}

/// Drop every rule whose left-hand side is unreachable from any user start
/// symbol. (At this stage every left-hand side is an original non-terminal.)
fn prune_unreachable(rules: Vec<CnfRule>, starts: &[SymbolId]) -> Vec<CnfRule> {
    let mut reachable: HashSet<SymbolId> = starts.iter().copied().collect();
    loop {
        let mut changed = false;
        for r in &rules {
            let Nt::Orig(o) = r.lhs else { continue };
            if !reachable.contains(&o) {
                continue;
            }
            for s in &r.rhs {
                if let Sym::N(Nt::Orig(id)) = s {
                    if reachable.insert(*id) {
                        changed = true;
                    }
                }
            }
        }
        if !changed {
            break;
        }
    }
    rules
        .into_iter()
        .filter(|r| matches!(&r.lhs, Nt::Orig(o) if reachable.contains(o)))
        .collect()
}

/// Compute the set of non-terminals that can derive the empty string (a rule with
/// an all-nullable — in particular empty — right-hand side), by fixpoint.
fn compute_nullable(rules: &[CnfRule]) -> HashSet<Nt> {
    let mut nullable: HashSet<Nt> = HashSet::new();
    loop {
        let mut changed = false;
        for r in rules {
            if nullable.contains(&r.lhs) {
                continue;
            }
            let all_nullable = r.rhs.iter().all(|s| match s {
                Sym::N(nt) => nullable.contains(nt),
                Sym::T(_) => false,
            });
            if all_nullable && nullable.insert(r.lhs.clone()) {
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    nullable
}

/// Eliminate ε-productions: drop empty rules, and for every other rule emit the
/// present/absent variants over its nullable occurrences (skipping the all-absent
/// one). Each variant records its present mask in a fresh [`EffRule`] so the
/// omitted positions can be refilled on assembly.
fn eliminate_epsilon(
    rules: Vec<CnfRule>,
    nullable: &HashSet<Nt>,
    eff_rules: &mut Vec<EffRule>,
) -> Result<Vec<CnfRule>, GrammarError> {
    let mut out: Vec<CnfRule> = Vec::with_capacity(rules.len());
    for r in rules {
        if r.rhs.is_empty() {
            // An ε-production; its effect is captured by the omit-variants of every
            // rule that references this (transparent) symbol.
            continue;
        }
        let null_positions: Vec<usize> = r
            .rhs
            .iter()
            .enumerate()
            .filter(|(_, s)| matches!(s, Sym::N(nt) if nullable.contains(nt)))
            .map(|(i, _)| i)
            .collect();

        if null_positions.is_empty() {
            out.push(r);
            continue;
        }
        let m = null_positions.len() as u32;
        if m > MAX_NULLABLE_POSITIONS {
            return Err(GrammarError::Other {
                msg: "CYK: too many nullable symbols in one rule".to_string(),
            });
        }

        // The rule is an original (pre-TERM/BIN) rule, so its single EffRule has an
        // all-present mask and `rhs` equals the original expansion.
        let base_rule_idx = eff_rules[r.alias.expect("real rule")].rule_idx;
        for omit_bits in 0u32..(1u32 << m) {
            // Build this variant's present mask over the full expansion.
            let mut present_mask = vec![true; r.rhs.len()];
            for (b, &pos) in null_positions.iter().enumerate() {
                if omit_bits & (1 << b) != 0 {
                    present_mask[pos] = false;
                }
            }
            let new_rhs: Vec<Sym> = r
                .rhs
                .iter()
                .zip(&present_mask)
                .filter(|(_, &keep)| keep)
                .map(|(s, _)| s.clone())
                .collect();
            if new_rhs.is_empty() {
                // All-absent variant: would re-introduce an ε-production.
                continue;
            }
            let eff = eff_rules.len();
            eff_rules.push(EffRule {
                rule_idx: base_rule_idx,
                present_mask,
            });
            out.push(CnfRule {
                lhs: r.lhs.clone(),
                rhs: new_rhs,
                weight: r.weight,
                alias: Some(eff),
                skipped: Vec::new(),
            });
        }
    }
    Ok(out)
}

/// TERM: replace every non-solitary terminal (a terminal in a rule of length > 1)
/// with a reference to a wrapper non-terminal `__T_t`, and emit `__T_t → [t]`.
fn term(rules: Vec<CnfRule>) -> Vec<CnfRule> {
    let mut out: Vec<CnfRule> = Vec::with_capacity(rules.len());
    let mut wrapped: std::collections::BTreeSet<SymbolId> = std::collections::BTreeSet::new();
    for r in rules {
        let has_terminal = r.rhs.iter().any(|s| matches!(s, Sym::T(_)));
        if r.rhs.len() > 1 && has_terminal {
            let new_rhs = r
                .rhs
                .iter()
                .map(|s| match s {
                    Sym::T(id) => {
                        wrapped.insert(*id);
                        Sym::N(Nt::Term(*id))
                    }
                    Sym::N(nt) => Sym::N(nt.clone()),
                })
                .collect();
            out.push(CnfRule {
                lhs: r.lhs,
                rhs: new_rhs,
                weight: r.weight,
                alias: r.alias,
                skipped: r.skipped,
            });
        } else {
            out.push(r);
        }
    }
    // One wrapper production per distinct lifted terminal.
    for id in wrapped {
        out.push(CnfRule {
            lhs: Nt::Term(id),
            rhs: vec![Sym::T(id)],
            weight: 0,
            alias: None,
            skipped: Vec::new(),
        });
    }
    out
}

/// BIN: binarize every rule with more than two right-hand-side symbols into a
/// right-leaning chain of `__SP_` split rules.
fn bin(rules: Vec<CnfRule>, split_counter: &mut u32) -> Vec<CnfRule> {
    let mut out: Vec<CnfRule> = Vec::with_capacity(rules.len());
    for r in rules {
        if r.rhs.len() > 2 {
            split(r, split_counter, &mut out);
        } else {
            out.push(r);
        }
    }
    out
}

/// Split one long rule `A → x0 x1 … x_{m-1}` (m > 2) into binary rules. The first
/// keeps `A`'s left-hand side, weight, and alias so `revert` rebuilds the original
/// node; the rest are `__SP_` helpers (weight 0, no alias) that splice away.
fn split(r: CnfRule, split_counter: &mut u32, out: &mut Vec<CnfRule>) {
    let g = *split_counter;
    *split_counter += 1;
    let m = r.rhs.len();

    // A → x0 __SP_g_1
    out.push(CnfRule {
        lhs: r.lhs,
        rhs: vec![r.rhs[0].clone(), Sym::N(Nt::Split(g, 1))],
        weight: r.weight,
        alias: r.alias,
        skipped: Vec::new(),
    });
    // __SP_g_i → x_i __SP_g_{i+1}   for i in 1..=m-3
    for i in 1..(m - 2) {
        out.push(CnfRule {
            lhs: Nt::Split(g, i as u32),
            rhs: vec![r.rhs[i].clone(), Sym::N(Nt::Split(g, (i + 1) as u32))],
            weight: 0,
            alias: None,
            skipped: Vec::new(),
        });
    }
    // __SP_g_{m-2} → x_{m-2} x_{m-1}
    out.push(CnfRule {
        lhs: Nt::Split(g, (m - 2) as u32),
        rhs: vec![r.rhs[m - 2].clone(), r.rhs[m - 1].clone()],
        weight: 0,
        alias: None,
        skipped: Vec::new(),
    });
}

/// UNIT: repeatedly eliminate non-terminal unit rules `A → [B]`, folding each
/// referent of `B` into `A` and recording the skipped chain for later unrolling.
fn unit(mut rules: Vec<CnfRule>) -> Result<Vec<CnfRule>, GrammarError> {
    let mut iterations = 0usize;
    while let Some(pos) = rules.iter().position(is_nt_unit) {
        rules = remove_unit_rule(rules, pos);
        rules = dedup(rules);
        iterations += 1;
        if iterations > UNIT_ITERATION_LIMIT {
            return Err(GrammarError::Other {
                msg: "CYK: grammar has a non-terminating unit-rule cycle".to_string(),
            });
        }
    }
    Ok(rules)
}

/// A non-terminal unit rule: a single right-hand-side symbol that is a
/// non-terminal (`A → [B]`).
fn is_nt_unit(r: &CnfRule) -> bool {
    r.rhs.len() == 1 && matches!(r.rhs[0], Sym::N(_))
}

/// Remove the unit rule at `pos` (and any structurally identical duplicate) and
/// fold every production of its referent into a unit-skip rule.
fn remove_unit_rule(rules: Vec<CnfRule>, pos: usize) -> Vec<CnfRule> {
    let unit_rule = rules[pos].clone();
    let target_nt = match &unit_rule.rhs[0] {
        Sym::N(nt) => nt.clone(),
        Sym::T(_) => unreachable!("is_nt_unit guarantees a non-terminal rhs"),
    };

    let mut new_rules: Vec<CnfRule> = Vec::with_capacity(rules.len());
    for r in &rules {
        // Drop the unit rule itself (and exact duplicates of it), mirroring
        // Python's `[x for x in g.rules if x != rule]` over a deduped rule set.
        if same_rule(r, &unit_rule) {
            continue;
        }
        new_rules.push(r.clone());
    }
    for r in &rules {
        if r.lhs == target_nt {
            new_rules.push(build_unit_skiprule(&unit_rule, r));
        }
    }
    new_rules
}

/// Combine a unit rule `A → [B]` with one production `B → rhs` of its referent
/// into `A → rhs`, accumulating the weight and the skipped-rule chain (the unit
/// rule's prior chain, then this target, then the target's own chain).
fn build_unit_skiprule(unit_rule: &CnfRule, target: &CnfRule) -> CnfRule {
    let mut skipped: Vec<usize> =
        Vec::with_capacity(unit_rule.skipped.len() + target.skipped.len() + 1);
    skipped.extend(unit_rule.skipped.iter().copied());
    skipped.push(target.alias.expect("unit-rule target is an original rule"));
    skipped.extend(target.skipped.iter().copied());
    CnfRule {
        lhs: unit_rule.lhs.clone(),
        rhs: target.rhs.clone(),
        weight: unit_rule.weight.saturating_add(target.weight),
        alias: unit_rule.alias,
        skipped,
    }
}

/// Structural rule identity for dedup / unit removal: left-hand side, right-hand
/// side, and the skipped chain. (Weight and alias are intentionally ignored, as
/// Python's `Rule.__eq__` / `UnitSkipRule.__eq__` are.)
fn same_rule(a: &CnfRule, b: &CnfRule) -> bool {
    a.lhs == b.lhs && a.rhs == b.rhs && a.skipped == b.skipped
}

/// Remove structurally identical duplicate rules, keeping the first occurrence —
/// mirroring Python Lark's `frozenset(rules)` at each conversion step (keeps the
/// rule set, and so the DP, bounded).
fn dedup(rules: Vec<CnfRule>) -> Vec<CnfRule> {
    let mut seen: HashSet<(Nt, Vec<Sym>, Vec<usize>)> = HashSet::new();
    let mut out = Vec::with_capacity(rules.len());
    for r in rules {
        let key = (r.lhs.clone(), r.rhs.clone(), r.skipped.clone());
        if seen.insert(key) {
            out.push(r);
        }
    }
    out
}

/// Build the terminal/non-terminal lookup indices, validating that every rule is
/// in CNF (rhs of length 1 = a terminal, or length 2 = two non-terminals).
/// Anything else — most importantly an empty production — is a build error.
fn build_indices(
    rules: Vec<CnfRule>,
    eff_rules: Vec<EffRule>,
    epsilon_values: HashMap<SymbolId, NodeValue>,
) -> Result<Cnf, GrammarError> {
    let mut terminal_rules: HashMap<SymbolId, Vec<usize>> = HashMap::new();
    let mut nonterminal_rules: HashMap<(Nt, Nt), Vec<usize>> = HashMap::new();
    for (i, r) in rules.iter().enumerate() {
        match r.rhs.as_slice() {
            [Sym::T(id)] => terminal_rules.entry(*id).or_default().push(i),
            [Sym::N(a), Sym::N(b)] => nonterminal_rules
                .entry((a.clone(), b.clone()))
                .or_default()
                .push(i),
            // A lone non-terminal would be an un-eliminated unit rule; a terminal
            // inside a binary rule would be a TERM failure; any other arity
            // (notably 0 — an ε-rule) is unsupported. Python's CnfWrapper raises
            // here too.
            _ => {
                return Err(GrammarError::Other {
                    msg: "CYK doesn't support empty rules".to_string(),
                })
            }
        }
    }
    Ok(Cnf {
        rules,
        eff_rules,
        epsilon_values,
        terminal_rules,
        nonterminal_rules,
    })
}

// ─── CNF revert + tree assembly ─────────────────────────────────────────────

/// Undo the CNF transforms over a CYK parse node, recovering a tree of original
/// rule applications. TERM wrappers collapse to their token, BIN split helpers
/// splice their children into the parent, and UNIT-eliminated rules unroll back
/// into the nested chain of original rules they replaced.
fn revert(node: &Rc<PNode>, cnf: &[CnfRule]) -> Rev {
    match &**node {
        PNode::Leaf(t) => Rev::Tok(t.clone()),
        PNode::Rule(rid, children) => {
            let r = &cnf[*rid];
            // TERM: `__T_t → [t]` reverts to just the token.
            if let Nt::Term(_) = r.lhs {
                return revert(&children[0], cnf);
            }
            // Revert children, splicing any BIN split helper's children in place.
            let mut kids: Vec<Rev> = Vec::with_capacity(children.len());
            for c in children {
                match revert(c, cnf) {
                    Rev::Splice(inner) => kids.extend(inner),
                    other => kids.push(other),
                }
            }
            // BIN: a split helper hands its (now flattened) children up to splice.
            if let Nt::Split(_, _) = r.lhs {
                return Rev::Splice(kids);
            }
            // An original rule. If it absorbed a unit chain, unroll it back into
            // one node per skipped rule (outermost = this rule's alias, innermost
            // = the deepest skipped rule, which actually has these children).
            let alias = r.alias.expect("original rule carries an alias");
            if r.skipped.is_empty() {
                Rev::Node(alias, kids)
            } else {
                let mut chain: Vec<usize> = Vec::with_capacity(1 + r.skipped.len());
                chain.push(alias);
                chain.extend(r.skipped.iter().copied());
                let mut acc = Rev::Node(*chain.last().unwrap(), kids);
                for &a in chain[..chain.len() - 1].iter().rev() {
                    acc = Rev::Node(a, vec![acc]);
                }
                acc
            }
        }
    }
}

/// Fold a reverted tree into a [`NodeValue`] via the shared [`TreeBuilder`], so
/// CYK output is shaped (filtered, expanded, spliced) identically to LALR/Earley.
///
/// The `present_mask` of each node's ε-variant drives reconstruction: the present
/// children (in order) come from `kids`, and every omitted position is refilled
/// with that symbol's precomputed ε-value (`cnf.epsilon_values`) — an empty splice
/// for a plain `*`/`?` helper, but a `None` placeholder for a `maybe_placeholders`
/// `[...]` — so the original rule still sees one value per expansion symbol and the
/// resulting tree matches LALR/Earley/Python exactly.
fn assemble_rev(
    rev: Rev,
    builder: &TreeBuilder,
    cnf: &Cnf,
    grammar: &CompiledGrammar,
) -> NodeValue {
    match rev {
        Rev::Tok(t) => NodeValue::Token(t),
        Rev::Node(eff_idx, kids) => {
            let e = &cnf.eff_rules[eff_idx];
            let kid_values: Vec<NodeValue> = kids
                .into_iter()
                .map(|k| assemble_rev(k, builder, cnf, grammar))
                .collect();
            let expansion = &grammar.rules[e.rule_idx].expansion;
            let mut present = kid_values.into_iter();
            let values: Vec<NodeValue> = e
                .present_mask
                .iter()
                .enumerate()
                .map(|(pos, &keep)| {
                    if keep {
                        present
                            .next()
                            .expect("a present child per present position")
                    } else {
                        // Omitted: refill with the symbol's ε-value (an empty splice
                        // unless it carries placeholders / an aliased empty node).
                        let sym = expansion[pos];
                        cnf.epsilon_values
                            .get(&sym)
                            .cloned()
                            .unwrap_or_else(|| NodeValue::Inline(Vec::new()))
                    }
                })
                .collect();
            builder.assemble(e.rule_idx, values)
        }
        // Only reachable if a split helper were somehow the root; flatten
        // defensively rather than panic.
        Rev::Splice(kids) => {
            let mut out: Vec<Child> = Vec::new();
            for k in kids {
                match assemble_rev(k, builder, cnf, grammar) {
                    NodeValue::Token(t) => out.push(Child::Token(t)),
                    NodeValue::Tree(t) => out.push(Child::Tree(t)),
                    NodeValue::Inline(cs) => out.extend(cs),
                }
            }
            NodeValue::Inline(out)
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{Ambiguity, Lark, LarkOptions, LexerType, ParserAlgorithm};

    fn cyk(grammar: &str) -> Lark {
        Lark::new(
            grammar,
            LarkOptions {
                parser: ParserAlgorithm::Cyk,
                lexer: LexerType::Basic,
                ..Default::default()
            },
        )
        .unwrap_or_else(|e| panic!("CYK build failed: {e}"))
    }

    fn lalr(grammar: &str) -> Lark {
        Lark::new(
            grammar,
            LarkOptions {
                parser: ParserAlgorithm::Lalr,
                lexer: LexerType::Basic,
                ..Default::default()
            },
        )
        .unwrap_or_else(|e| panic!("LALR build failed: {e}"))
    }

    /// On an unambiguous grammar, CYK must produce a byte-identical tree to LALR —
    /// it shares the same lexer and TreeBuilder, so the only thing under test is the
    /// CNF round-trip (TERM/BIN/UNIT then revert). Covers a >2-symbol rule (BIN),
    /// punctuation filtering, a unit chain (UNIT), and left recursion.
    #[test]
    fn cyk_matches_lalr_on_unambiguous() {
        let grammar = "\
start: \"(\" item \",\" list \")\"
list: list item | item
item: value
value: WORD
WORD: /[a-z]+/
%ignore /[ ]+/
";
        let input = "(a, b c d)";
        let cyk_tree = cyk(grammar).parse(input).unwrap().to_string();
        let lalr_tree = lalr(grammar).parse(input).unwrap().to_string();
        assert_eq!(cyk_tree, lalr_tree, "CYK and LALR disagree on {input:?}");
    }

    /// CYK's headline capability: it parses a highly ambiguous grammar (`a: a a |
    /// "x"`, exponentially ambiguous) where the DP must combine sub-spans. It must
    /// resolve to a single valid `start` tree, and still reject invalid input.
    #[test]
    fn cyk_parses_ambiguous_grammar() {
        let parser = cyk("start: a\na: a a | \"x\"\n");
        for input in ["x", "xx", "xxxx", "xxxxx"] {
            let tree = parser
                .parse(input)
                .unwrap_or_else(|e| panic!("CYK should parse {input:?}: {e}"));
            assert_eq!(tree.as_tree().map(|t| t.data.as_str()), Some("start"));
        }
        // And it still rejects input outside the grammar.
        assert!(parser.parse("xyx").is_err());
        assert!(parser.parse("").is_err());
    }

    /// EBNF operators expand to nullable helper rules in lark-rs; CYK's ε-removal
    /// must handle `*` / `+` / `?` and match LALR, including the zero-repetition
    /// case (which exercises the omit-variant path).
    #[test]
    fn cyk_handles_ebnf_repetition() {
        let grammar = "\
start: pre item* post
pre: \"<\"
post: \">\"
item: WORD
WORD: /[a-z]+/
%ignore /[ ]+/
";
        for input in ["< >", "< a >", "< a b c >"] {
            let cyk_tree = cyk(grammar).parse(input).unwrap().to_string();
            let lalr_tree = lalr(grammar).parse(input).unwrap().to_string();
            assert_eq!(cyk_tree, lalr_tree, "EBNF mismatch on {input:?}");
        }
    }

    /// A grammar that produces an observable empty node (a nullable *non*-transparent
    /// rule) cannot be represented in CNF; CYK must reject it at build time, the same
    /// outcome Python Lark's CYK gives ("CYK doesn't support empty rules").
    #[test]
    fn cyk_rejects_genuine_epsilon_rule() {
        let built = Lark::new(
            "start: a\na: \"x\" | \n",
            LarkOptions {
                parser: ParserAlgorithm::Cyk,
                lexer: LexerType::Basic,
                ..Default::default()
            },
        );
        assert!(built.is_err(), "a nullable named rule must fail to build");
    }

    /// CYK's empty-rule rejection is a function of whether the *user rule* can
    /// derive ε — **not** of how a nullable EBNF operator is lowered. lark-rs
    /// distributes a *leading* nullable into the parent (`a: B? C` → `a: B C | C`)
    /// but keeps a shared helper for a *trailing* one (`a: C B?` → `a: C __opt`);
    /// it is tempting to assume that switching the trailing form to distribution
    /// too (full Python `SimplifyRule` parity) would push an ε into a
    /// non-transparent parent and start failing CYK builds. It would not: the
    /// parent's *nullability* is a property of the language, invariant under the
    /// lowering choice, and that is the only thing this rejection keys on. This
    /// test pins exactly that boundary so the invariance is not re-litigated:
    ///
    ///   * a parent that stays non-nullable builds under **either** lowering
    ///     (leading-distributed `B? C` and trailing-helper `C B?` both succeed);
    ///   * a *wholly*-nullable parent is rejected under **either** lowering
    ///     (`B?` and `B? C?` both fail), because the parent can derive ε either way.
    ///
    /// Verified equal to Python Lark 1.3.1's CYK on all four (PR #100 review).
    #[test]
    fn cyk_epsilon_rejection_is_lowering_invariant() {
        let build = |body: &str| {
            Lark::new(
                &format!("start: a\na: {body}\nB: \"b\"\nC: \"c\"\n"),
                LarkOptions {
                    parser: ParserAlgorithm::Cyk,
                    lexer: LexerType::Basic,
                    ..Default::default()
                },
            )
        };
        // Non-nullable parent: builds regardless of leading vs trailing lowering.
        assert!(
            build("B? C").is_ok(),
            "leading-distributed `B? C` (parent non-nullable) must build"
        );
        assert!(
            build("C B?").is_ok(),
            "trailing-helper `C B?` (parent non-nullable) must build"
        );
        // Wholly-nullable parent: rejected regardless of lowering — the parent can
        // derive ε, which is what CYK can't model. (So distributing trailing
        // nullables too would *not* change which grammars CYK accepts.)
        assert!(
            build("B?").is_err(),
            "wholly-nullable `B?` must be rejected (parent derives ε)"
        );
        assert!(
            build("B? C?").is_err(),
            "wholly-nullable `B? C?` must be rejected (parent derives ε)"
        );
    }

    /// Known divergence from the oracle — tracked by #101. A wholly-nullable
    /// *transparent* rule (`_a: B?`) is rejected by Python Lark's CYK
    /// (`CYK doesn't support empty rules`) but lark-rs currently **accepts** it: its
    /// ε-removal can splice away a transparent rule's empty derivation, so the build
    /// proceeds (only a *non*-transparent nullable is rejected — see
    /// `cyk_rejects_genuine_epsilon_rule`). This test encodes the oracle-target
    /// (rejection) and is `#[ignore]`d until #101 is decided: run it with
    /// `cargo test --ignored cyk_transparent_nullable` to reproduce the gap. If #101
    /// resolves toward *accepting* the divergence instead, flip this to assert
    /// `is_ok()` and drop the `#[ignore]`.
    #[test]
    #[ignore = "#101: lark-rs CYK accepts a transparent wholly-nullable rule the oracle rejects"]
    fn cyk_transparent_nullable_rule_diverges_from_oracle() {
        let built = Lark::new(
            "start: _a \"x\"\n_a: B?\nB: \"b\"\n",
            LarkOptions {
                parser: ParserAlgorithm::Cyk,
                lexer: LexerType::Basic,
                ..Default::default()
            },
        );
        assert!(
            built.is_err(),
            "oracle parity: a transparent wholly-nullable rule should be rejected (#101)"
        );
    }

    /// Under `maybe_placeholders`, an absent `[...]` optional must still emit a
    /// `None` placeholder — even though CYK ε-removes the (nullable, transparent)
    /// helper that carries it. CYK must match LALR on present *and* absent cases,
    /// including two optionals where only one is filled.
    #[test]
    fn cyk_maybe_placeholders_none() {
        let grammar = "\
start: A [B] C [D]
A: \"a\"
B: \"b\"
C: \"c\"
D: \"d\"
";
        let build = |p: ParserAlgorithm| {
            Lark::new(
                grammar,
                LarkOptions {
                    parser: p,
                    lexer: LexerType::Basic,
                    maybe_placeholders: true,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let cyk = build(ParserAlgorithm::Cyk);
        let lalr = build(ParserAlgorithm::Lalr);
        for input in ["ac", "abc", "acd", "abcd"] {
            let c = cyk.parse(input).unwrap().to_string();
            let l = lalr.parse(input).unwrap().to_string();
            assert_eq!(c, l, "maybe_placeholders mismatch on {input:?}");
            // The absent-optional cases must actually carry a None, not drop it.
            if input == "ac" {
                assert!(c.contains("None"), "absent optional lost its None: {c}");
            }
        }
    }

    /// The `ambiguity` option does not apply to CYK (it has no `_ambig` forest), but
    /// requesting it must not break construction — CYK simply resolves to one tree.
    #[test]
    fn cyk_ignores_ambiguity_option() {
        let lark = Lark::new(
            "start: WORD\nWORD: /[a-z]+/\n",
            LarkOptions {
                parser: ParserAlgorithm::Cyk,
                lexer: LexerType::Basic,
                ambiguity: Ambiguity::Explicit,
                ..Default::default()
            },
        )
        .expect("CYK builds regardless of ambiguity option");
        assert!(lark.parse("hello").is_ok());
    }
}
