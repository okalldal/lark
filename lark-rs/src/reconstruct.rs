//! Tree → text reconstruction: serialize a shaped parse tree back to source.
//!
//! The parse trees [`crate::Lark::parse`] returns are *shaped*: punctuation
//! tokens are filtered, transparent (`_rule` / `__anon_*`) rules are spliced
//! into their parents, `?rule` (expand1) wrappers collapse, and `%ignore`d
//! text is gone entirely. Reconstruction inverts the invertible part of that
//! shaping: it matches each tree node back onto the grammar rules that could
//! have produced it, re-inserts the discarded fixed-string terminals in their
//! grammar-mandated positions, and emits the kept tokens verbatim.
//!
//! The design mirrors Python Lark's experimental `lark.reconstruct.Reconstructor`
//! (a `TreeMatcher` + `WriteTokensTransformer`): grammar rules are rewritten
//! into *recons rules* whose alphabet is tree children — a child `Tree` or
//! `Token` matches atomically as a "terminal", while symbols that vanish from
//! trees (transparent rules, expand1 origins, aliased origins) stay
//! non-terminals — and each node's child list is matched with a small
//! nullable-safe Earley recognizer. Python Lark is **not** an oracle here:
//! reconstruction output is grounded by the *metamorphic* round-trip property
//! instead (`parse(reconstruct(parse(x)))` must equal `parse(x)` structurally),
//! enforced by `tests/test_reconstruct.rs` over curated grammars and the whole
//! LALR compliance bank. See ADR-0040.
//!
//! Guarantees and limits:
//!
//! - The output is *a* source text that re-parses to the same tree — not the
//!   original text. `%ignore`d trivia is not recoverable; where two adjacent
//!   emitted pieces would fuse into one identifier-like token, a separator the
//!   grammar can ignore is inserted (Python's `insert_spaces` heuristic, made
//!   grammar-aware: no `%ignore`able whitespace → no insertion).
//! - A **discarded** terminal (filtered from the tree) can only be re-emitted
//!   when its pattern is a fixed string. A discarded regex or `%declare`d
//!   terminal needs a substitution via [`Reconstructor::with_term_subs`],
//!   exactly like Python's `term_subs`; otherwise reconstruction returns
//!   [`ReconstructError::NonLiteralTerminal`] when (and only when) a
//!   derivation actually needs it.
//! - Grammars built with `maybe_placeholders` are refused up front
//!   ([`ReconstructError::MaybePlaceholders`]): a `None` placeholder child
//!   corresponds to no grammar symbol, so the match is ill-defined (Python's
//!   `TreeMatcher` asserts the same).
//!
//! ```
//! use lark_rs::{Lark, LarkOptions};
//! use lark_rs::reconstruct::Reconstructor;
//!
//! let grammar = r#"
//!     start: pair ("," pair)*
//!     pair: NAME "=" NAME
//!     NAME: /[a-z]+/
//!     %ignore " "
//! "#;
//! let lark = Lark::new(grammar, LarkOptions::default()).unwrap();
//! let tree = lark.parse("a = b ,  c=d").unwrap();
//!
//! let recons = Reconstructor::new(&lark).unwrap();
//! let text = recons.reconstruct(&tree).unwrap();
//! assert_eq!(text, "a=b,c=d");
//! // The metamorphic guarantee: the round-trip preserves the tree.
//! assert_eq!(format!("{}", lark.parse(&text).unwrap()), format!("{tree}"));
//! ```

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::rc::Rc;

use crate::grammar::symbol::Symbol;
use crate::grammar::terminal::Pattern;
use crate::grammar::Grammar;
use crate::tree::{Child, ParseTree, Tree};
use crate::Lark;

// ─── Errors ──────────────────────────────────────────────────────────────────

/// Why a tree could not be reconstructed into text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReconstructError {
    /// The parser was built with `maybe_placeholders`: `None` placeholder
    /// children correspond to no grammar symbol, so tree matching is
    /// ill-defined. (Python's `TreeMatcher` refuses this too.)
    MaybePlaceholders,
    /// No rule of the grammar can produce a node named `data` with the child
    /// sequence found in the tree. Either the tree was not produced by this
    /// grammar, or it was edited into an unproducible shape.
    NoMatch { data: String },
    /// A derivation needs to re-emit the discarded terminal `name`, but its
    /// pattern is not a fixed string (a regex, or a `%declare`d terminal with
    /// no pattern at all). Provide its text via
    /// [`Reconstructor::with_term_subs`].
    NonLiteralTerminal { name: String },
    /// A `None` placeholder child was encountered inside the tree (it can only
    /// come from a `maybe_placeholders` parse, which reconstruction refuses).
    Placeholder,
}

impl fmt::Display for ReconstructError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReconstructError::MaybePlaceholders => write!(
                f,
                "cannot reconstruct from a maybe_placeholders parser: `None` \
                 placeholder children correspond to no grammar symbol"
            ),
            ReconstructError::NoMatch { data } => write!(
                f,
                "no grammar rule matches tree node `{data}` with its child sequence"
            ),
            ReconstructError::NonLiteralTerminal { name } => write!(
                f,
                "cannot emit discarded terminal `{name}`: its pattern is not a fixed \
                 string — provide its text via Reconstructor::with_term_subs"
            ),
            ReconstructError::Placeholder => write!(
                f,
                "tree contains a `None` placeholder child, which has no textual form"
            ),
        }
    }
}

impl std::error::Error for ReconstructError {}

// ─── Recons-rule representation ──────────────────────────────────────────────

/// Interned symbol id, local to one `Reconstructor`.
type SymId = u32;

/// A symbol of a recons-rule expansion. The alphabet is tree children:
/// a `Term` matches one child atomically (a `Token` by its type name, a `Tree`
/// node by its data), while a `NonTerm` expands through further recons rules —
/// exactly the symbols that leave no node behind in a shaped tree (transparent
/// rules, expand1 origins, aliased origins).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum RSym {
    Term(SymId),
    NonTerm(SymId),
}

/// One step of a rule's write-out plan, aligned with the *original* expansion:
/// discarded terminals are emitted from the grammar, every other symbol
/// consumes the next matched element.
#[derive(Debug, Clone, Copy)]
enum WriteStep {
    /// Emit the fixed text of the discarded terminal with this (interned) name.
    Discarded(SymId),
    /// Consume the next matched element (a scanned child, or a sub-derivation).
    Consume,
}

/// A tree-matching rule derived from one grammar rule (or a synthesized
/// bridging rule). `expansion` is the original expansion minus discarded
/// terminals, over the tree-children alphabet; `steps` is the original
/// expansion with the discarded terminals kept as literal write-outs.
#[derive(Debug)]
struct ReconsRule {
    origin: SymId,
    expansion: Vec<RSym>,
    steps: Vec<WriteStep>,
}

/// How a discarded terminal is written back out.
#[derive(Debug, Clone)]
enum TermText {
    /// A fixed string (a `PatternStr` terminal, or a user substitution).
    Literal(String),
    /// A regex or `%declare`d terminal — unwritable without a substitution.
    NonLiteral,
}

// ─── Reconstructor ───────────────────────────────────────────────────────────

/// Serializes shaped parse trees back to text, given the grammar (via the
/// [`Lark`] instance) that produced them. See the [module docs](self) for the
/// contract; construction copies what it needs, so the `Lark` borrow ends at
/// `new`.
pub struct Reconstructor {
    /// Interned symbol names (terminal names, rule names, aliases).
    names: Vec<String>,
    /// `names[i]` with any template-instance suffix (`{N}`) stripped — what a
    /// tree node's `data` carries for a template rule (Lark's `template_source`
    /// labels the tree with the base name).
    bases: Vec<String>,
    ids: HashMap<String, SymId>,
    /// Rules available in every match (transparent / expand1 / bridging rules).
    global_rules: Vec<Rc<ReconsRule>>,
    /// Rules only used when matching a node *of that name* as the match root,
    /// keyed by the base (tree-label) name. Mirrors Python's `rules_for_root`.
    rules_for_root: HashMap<SymId, Vec<Rc<ReconsRule>>>,
    /// Write-out text per discarded-terminal name (term_subs already folded in).
    term_text: HashMap<SymId, TermText>,
    /// Per-root-name matcher cache (built lazily, like Python's parser cache).
    matchers: RefCell<HashMap<SymId, Rc<Matcher>>>,
    /// The separator inserted between pieces that would otherwise fuse into one
    /// token — a piece of text the grammar's `%ignore` terminals can absorb
    /// (`" "`, `"\n"`, or `"\t"`, first that matches). `None` when the grammar
    /// ignores none of them: then *no* separator is parseable, and exact
    /// concatenation is the faithful output (nothing ignorable was dropped).
    separator: Option<String>,
}

impl Reconstructor {
    /// Build a reconstructor for `parser`'s grammar.
    pub fn new(parser: &Lark) -> Result<Self, ReconstructError> {
        Self::with_term_subs(parser, std::iter::empty::<(String, String)>())
    }

    /// As [`new`](Self::new), with substitution text for terminals that cannot
    /// be re-emitted from their pattern (discarded regex or `%declare`d
    /// terminals) — Python's `term_subs`. Keys are terminal names, values the
    /// exact text to write. A substitution also overrides a fixed-string
    /// pattern when both exist.
    pub fn with_term_subs<K, V>(
        parser: &Lark,
        term_subs: impl IntoIterator<Item = (K, V)>,
    ) -> Result<Self, ReconstructError>
    where
        K: Into<String>,
        V: Into<String>,
    {
        let grammar = &parser.grammar;
        if uses_placeholders(grammar) {
            return Err(ReconstructError::MaybePlaceholders);
        }

        let mut this = Reconstructor {
            names: Vec::new(),
            bases: Vec::new(),
            ids: HashMap::new(),
            global_rules: Vec::new(),
            rules_for_root: HashMap::new(),
            term_text: HashMap::new(),
            matchers: RefCell::new(HashMap::new()),
            separator: pick_separator(grammar),
        };

        // Resolve every terminal's write-out text up front; errors stay lazy
        // (only a derivation that actually *needs* a NonLiteral terminal fails).
        let subs: HashMap<String, String> = term_subs
            .into_iter()
            .map(|(k, v)| (k.into(), v.into()))
            .collect();
        for t in &grammar.terminals {
            let text = if let Some(sub) = subs.get(&t.name) {
                TermText::Literal(sub.clone())
            } else if t.declared {
                TermText::NonLiteral
            } else {
                match &t.pattern {
                    Pattern::Str(s) => TermText::Literal(s.value.clone()),
                    Pattern::Re(_) => TermText::NonLiteral,
                }
            };
            let id = this.intern(&t.name);
            this.term_text.insert(id, text);
        }
        // A substitution may name a terminal the grammar pruned (e.g. one only
        // reachable through inlining); honor it anyway so callers can be liberal.
        for (name, text) in &subs {
            let id = this.intern(name);
            this.term_text
                .entry(id)
                .or_insert_with(|| TermText::Literal(text.clone()));
        }

        this.build_recons_rules(grammar);
        Ok(this)
    }

    /// Reconstruct `tree` into source text. Where two adjacent pieces would
    /// fuse into one identifier-like token, an *ignorable* separator is
    /// inserted — the first of `" "`, `"\n"`, `"\t"` that the grammar's
    /// `%ignore` terminals can absorb. A grammar that ignores none of them
    /// gets pure concatenation (there, inserting anything would break the
    /// re-parse, and nothing ignorable was dropped in the first place).
    pub fn reconstruct(&self, tree: &ParseTree) -> Result<String, ReconstructError> {
        self.render(tree, true)
    }

    /// As [`reconstruct`](Self::reconstruct) without separator insertion — the
    /// exact concatenation of the emitted pieces (Python's
    /// `insert_spaces=False`). Useful when a caller post-processes the pieces
    /// with grammar-specific spacing instead.
    pub fn reconstruct_exact(&self, tree: &ParseTree) -> Result<String, ReconstructError> {
        self.render(tree, false)
    }

    // ── Construction: grammar rules → recons rules ──────────────────────────

    fn intern(&mut self, name: &str) -> SymId {
        if let Some(&id) = self.ids.get(name) {
            return id;
        }
        let id = self.names.len() as SymId;
        self.names.push(name.to_string());
        // A template instance `foo{3}` is labeled `foo` in the tree.
        let base = match name.find('{') {
            Some(i) => &name[..i],
            None => name,
        };
        self.bases.push(base.to_string());
        self.ids.insert(name.to_string(), id);
        id
    }

    /// The inversion of tree-shaping, rule by rule — the port of Python's
    /// `TreeMatcher._build_recons_rules`:
    ///
    /// - Symbols whose nodes *survive* shaping (plain rules, terminals) become
    ///   match-terminals: one tree child, matched atomically by name.
    /// - Symbols whose nodes *vanish* (transparent `_rule`s, expand1 `?rule`
    ///   origins, aliased origins) stay non-terminals, expanded structurally.
    /// - Discarded terminals leave the expansion and enter the write plan.
    /// - An expand1 rule with a non-unary matched expansion only applies when
    ///   its (uncollapsed) node is the match root; a unary one applies
    ///   everywhere (that is the collapse). Bridging rules let a reference to
    ///   an expand1/aliased origin also match an actual surviving node.
    fn build_recons_rules(&mut self, grammar: &Grammar) {
        let expand1s: HashSet<&str> = grammar
            .rules
            .iter()
            .filter(|r| r.options.expand1)
            .map(|r| r.origin.name.as_str())
            .collect();
        let mut aliases: HashMap<&str, Vec<&str>> = HashMap::new();
        for r in &grammar.rules {
            if let Some(alias) = &r.alias {
                let list = aliases.entry(r.origin.name.as_str()).or_default();
                if !list.contains(&alias.as_str()) {
                    list.push(alias.as_str());
                }
            }
        }
        let nonterminals: HashSet<&str> = grammar
            .rules
            .iter()
            .map(|r| r.origin.name.as_str())
            .filter(|name| {
                name.starts_with('_') || expand1s.contains(name) || aliases.contains_key(name)
            })
            .collect();

        let mut global: Vec<ReconsRule> = Vec::new();
        let mut for_root: HashMap<SymId, Vec<ReconsRule>> = HashMap::new();
        let mut bridged: HashSet<SymId> = HashSet::new();

        for r in &grammar.rules {
            // A terminal occurrence is discarded iff it is filtered at this
            // position and the rule does not keep all tokens. (lark-rs filtering
            // is per-occurrence: `Terminal::filter_out` on each expansion slot.)
            let discarded = |sym: &Symbol| -> bool {
                matches!(sym, Symbol::Terminal(t) if t.filter_out) && !r.options.keep_all_tokens
            };

            let mut expansion: Vec<RSym> = Vec::new();
            let mut steps: Vec<WriteStep> = Vec::new();
            for sym in &r.expansion {
                if discarded(sym) {
                    let id = self.intern(sym.name());
                    steps.push(WriteStep::Discarded(id));
                } else {
                    let id = self.intern(sym.name());
                    let rsym = match sym {
                        Symbol::NonTerminal(nt) if nonterminals.contains(nt.name.as_str()) => {
                            RSym::NonTerm(id)
                        }
                        _ => RSym::Term(id),
                    };
                    expansion.push(rsym);
                    steps.push(WriteStep::Consume);
                }
            }

            let origin_id = self.intern(&r.origin.name);
            // Skip the degenerate self-recursive shape `x → x` (it matches
            // nothing new and would only add derivational noise).
            if r.alias.is_none() && expansion == [RSym::NonTerm(origin_id)] {
                continue;
            }

            // The symbol this rule reconstructs: the alias when present (an
            // aliased alternative labels its node with the alias), else the origin.
            let sym_name = r.alias.clone().unwrap_or_else(|| r.origin.name.clone());
            let sym_id = self.intern(&sym_name);
            let root_key = self.base_id(sym_id);
            let rule = ReconsRule {
                origin: sym_id,
                expansion,
                steps,
            };

            if expand1s.contains(sym_name.as_str()) && rule.expansion.len() != 1 {
                // An expand1 node that did NOT collapse (≠1 matched children):
                // only valid when that node is the match root.
                for_root.entry(root_key).or_default().push(rule);
                // A reference to the expand1 origin may still meet a surviving
                // node — bridge it once.
                if bridged.insert(sym_id) {
                    global.push(bridge_rule(sym_id));
                }
            } else if sym_name.starts_with('_') || expand1s.contains(sym_name.as_str()) {
                // Transparent rules and collapsing (unary) expand1 rules match
                // structurally wherever their origin is referenced.
                global.push(rule);
            } else {
                for_root.entry(root_key).or_default().push(rule);
            }
        }

        // Every expand1 origin gets a bridge, even when all its rules have a
        // unary matched expansion: an *uncollapsed* node can still exist — a
        // `?list: item+` node keeps 2+ children through the spliced `+` helper
        // while its only recons rule is the unary helper reference — and a
        // reference to `list` must be able to consume that surviving node
        // whole. (Found by the metamorphic bank sweep; Python's reconstructor
        // has this gap.)
        {
            let mut expand1_names: Vec<&str> = expand1s.iter().copied().collect();
            expand1_names.sort_unstable(); // deterministic rule order
            for name in expand1_names {
                let sym_id = self.intern(name);
                if bridged.insert(sym_id) {
                    global.push(bridge_rule(sym_id));
                }
            }
        }

        // Aliased origins: a reference to the origin may meet a node labeled
        // with any of its aliases, or (for unaliased alternatives) the origin
        // name itself.
        let mut alias_pairs: Vec<(SymId, SymId)> = Vec::new();
        {
            let mut origin_names: Vec<&str> = aliases.keys().copied().collect();
            origin_names.sort_unstable(); // deterministic rule order
            for origin in origin_names {
                let origin_id = self.ids[origin];
                for alias in &aliases[origin] {
                    let alias_id = self.ids[*alias];
                    alias_pairs.push((origin_id, alias_id));
                }
                alias_pairs.push((origin_id, origin_id));
            }
        }
        for (origin_id, target_id) in alias_pairs {
            let mut rule = bridge_rule(origin_id);
            rule.expansion = vec![RSym::Term(target_id)];
            global.push(rule);
        }

        self.global_rules = dedup_and_sort(global, &self.term_text);
        self.rules_for_root = for_root
            .into_iter()
            .map(|(k, v)| (k, dedup_and_sort(v, &self.term_text)))
            .collect();
    }

    fn base_id(&mut self, id: SymId) -> SymId {
        let base = self.bases[id as usize].clone();
        self.intern(&base)
    }

    // ── Matching ────────────────────────────────────────────────────────────

    /// The matcher for nodes labeled `root` (an interned base name): all global
    /// rules plus the root-only rules of that name, with prediction indexes.
    fn matcher_for(&self, root: SymId) -> Rc<Matcher> {
        if let Some(m) = self.matchers.borrow().get(&root) {
            return Rc::clone(m);
        }
        let mut rules: Vec<Rc<ReconsRule>> = self.global_rules.clone();
        if let Some(extra) = self.rules_for_root.get(&root) {
            rules.extend(extra.iter().cloned());
        }
        let mut by_origin: HashMap<SymId, Vec<usize>> = HashMap::new();
        let mut root_candidates: Vec<usize> = Vec::new();
        let root_base = &self.bases[root as usize];
        for (i, r) in rules.iter().enumerate() {
            by_origin.entry(r.origin).or_default().push(i);
            if &self.bases[r.origin as usize] == root_base {
                root_candidates.push(i);
            }
        }
        let m = Rc::new(Matcher {
            rules,
            by_origin,
            root_candidates,
        });
        self.matchers.borrow_mut().insert(root, Rc::clone(&m));
        m
    }

    /// Does child `c` match the match-terminal named `names[t]`? A token by its
    /// type; a subtree by its label vs. the terminal's *base* name (template
    /// instances are referenced as `foo{N}` but labeled `foo` in the tree).
    fn child_matches(&self, c: &Child, t: SymId) -> bool {
        match c {
            Child::Token(tok) => tok.type_ == self.names[t as usize],
            Child::Tree(tree) => tree.data == self.bases[t as usize],
            Child::None => false,
        }
    }

    /// Match one tree node's children against the grammar: find a derivation
    /// of `tree.data` over `tree.children`.
    fn match_node(&self, tree: &Tree) -> Result<Deriv, ReconstructError> {
        let no_match = || ReconstructError::NoMatch {
            data: tree.data.clone(),
        };
        let root = *self.ids.get(&tree.data).ok_or_else(no_match)?;
        let matcher = self.matcher_for(root);
        self.earley_match(&matcher, &tree.children)
            .ok_or_else(no_match)
    }

    /// A minimal Earley recognizer over a child list, with backpointers for
    /// derivation extraction. Nullable completions use per-set fixpoint
    /// processing plus an "already completed empty" check at prediction time
    /// (the Aycock–Horspool ε-subtlety), so empty recons rules (`sep: ","` has
    /// an *empty* matched expansion) work. First derivation found wins —
    /// any valid derivation reconstructs a tree-preserving text, and rule
    /// order (dedup + shortest-expansion-first) makes the choice deterministic.
    fn earley_match(&self, m: &Matcher, children: &[Child]) -> Option<Deriv> {
        let n = children.len();
        // sets[i] = items whose progress reaches child position i.
        let mut sets: Vec<Vec<Item>> = vec![Vec::new(); n + 1];
        let mut seen: Vec<HashMap<(usize, usize, usize), usize>> = vec![HashMap::new(); n + 1];
        // Per set: origin → first item completed with an empty span there.
        let mut empty_done: Vec<HashMap<SymId, (usize, usize)>> = vec![HashMap::new(); n + 1];
        // Per set: NT symbol → items in that set whose dot is before it.
        let mut waiting: Vec<HashMap<SymId, Vec<usize>>> = vec![HashMap::new(); n + 1];

        let add = |sets: &mut Vec<Vec<Item>>,
                   seen: &mut Vec<HashMap<(usize, usize, usize), usize>>,
                   waiting: &mut Vec<HashMap<SymId, Vec<usize>>>,
                   set: usize,
                   item: Item|
         -> Option<usize> {
            let key = (item.rule, item.dot, item.start);
            if seen[set].contains_key(&key) {
                return None; // keep the first backpointer — one derivation is enough
            }
            let idx = sets[set].len();
            seen[set].insert(key, idx);
            if let Some(RSym::NonTerm(x)) = m.rules[item.rule].expansion.get(item.dot) {
                waiting[set].entry(*x).or_default().push(idx);
            }
            sets[set].push(item);
            Some(idx)
        };

        for &r in &m.root_candidates {
            add(
                &mut sets,
                &mut seen,
                &mut waiting,
                0,
                Item {
                    rule: r,
                    dot: 0,
                    start: 0,
                    bp: None,
                },
            );
        }

        for i in 0..=n {
            let mut cursor = 0;
            while cursor < sets[i].len() {
                let item = sets[i][cursor].clone();
                let idx = cursor;
                cursor += 1;
                let rule = &m.rules[item.rule];
                match rule.expansion.get(item.dot) {
                    Some(&RSym::Term(t)) => {
                        if i < n && self.child_matches(&children[i], t) {
                            add(
                                &mut sets,
                                &mut seen,
                                &mut waiting,
                                i + 1,
                                Item {
                                    rule: item.rule,
                                    dot: item.dot + 1,
                                    start: item.start,
                                    bp: Some((i, idx, Cause::Scan)),
                                },
                            );
                        }
                    }
                    Some(&RSym::NonTerm(x)) => {
                        // Predict.
                        if let Some(rs) = m.by_origin.get(&x) {
                            for &r in rs {
                                add(
                                    &mut sets,
                                    &mut seen,
                                    &mut waiting,
                                    i,
                                    Item {
                                        rule: r,
                                        dot: 0,
                                        start: i,
                                        bp: None,
                                    },
                                );
                            }
                        }
                        // ε-completion: X already completed empty in this set.
                        if let Some(&(cset, cidx)) = empty_done[i].get(&x) {
                            add(
                                &mut sets,
                                &mut seen,
                                &mut waiting,
                                i,
                                Item {
                                    rule: item.rule,
                                    dot: item.dot + 1,
                                    start: item.start,
                                    bp: Some((i, idx, Cause::Complete(cset, cidx))),
                                },
                            );
                        }
                    }
                    None => {
                        // Complete: advance items in set `start` waiting on origin.
                        let origin = rule.origin;
                        if item.start == i {
                            empty_done[i].entry(origin).or_insert((i, idx));
                        }
                        let waiters: Vec<usize> = waiting[item.start]
                            .get(&origin)
                            .map(|v| v.clone())
                            .unwrap_or_default();
                        for widx in waiters {
                            let w = sets[item.start][widx].clone();
                            add(
                                &mut sets,
                                &mut seen,
                                &mut waiting,
                                i,
                                Item {
                                    rule: w.rule,
                                    dot: w.dot + 1,
                                    start: w.start,
                                    bp: Some((item.start, widx, Cause::Complete(i, idx))),
                                },
                            );
                        }
                    }
                }
            }
        }

        // Accept: a root-candidate rule spanning the whole child list.
        let accepted = sets[n].iter().position(|it| {
            it.start == 0
                && it.dot == m.rules[it.rule].expansion.len()
                && m.root_candidates.contains(&it.rule)
        })?;
        Some(extract_derivation(m, &sets, (n, accepted)))
    }

    // ── Writing ─────────────────────────────────────────────────────────────

    fn render(&self, tree: &ParseTree, insert_spaces: bool) -> Result<String, ReconstructError> {
        let mut out = String::new();
        // Last character of the previously emitted piece (None after an empty
        // piece), for the identifier-fusion space heuristic.
        let mut prev_last: Option<char> = None;
        let sep = if insert_spaces {
            self.separator.as_deref()
        } else {
            None
        };
        let emit = |piece: &str, out: &mut String, prev_last: &mut Option<char>| {
            if let Some(sep) = sep {
                if let (Some(p), Some(c)) = (*prev_last, piece.chars().next()) {
                    if is_id_continue(p) && is_id_continue(c) {
                        out.push_str(sep);
                    }
                }
            }
            out.push_str(piece);
            *prev_last = piece.chars().last();
        };

        let root = match tree {
            ParseTree::Tree(t) => t,
            // A `?start` that collapsed to a bare token: the text is the token.
            ParseTree::Token(tok) => return Ok(tok.value.clone()),
            ParseTree::None => return Err(ReconstructError::Placeholder),
        };

        // Iterative walk: no native recursion to tree depth (#151 discipline).
        enum Frame<'t> {
            Node(&'t Tree),
            Walk {
                rule: Rc<ReconsRule>,
                step: usize,
                elems: std::vec::IntoIter<Elem>,
                children: &'t [Child],
            },
        }
        let mut stack: Vec<Frame> = vec![Frame::Node(root)];
        while let Some(top) = stack.pop() {
            match top {
                Frame::Node(t) => {
                    let deriv = self.match_node(t)?;
                    stack.push(Frame::Walk {
                        rule: deriv.rule,
                        step: 0,
                        elems: deriv.elems.into_iter(),
                        children: &t.children,
                    });
                }
                Frame::Walk {
                    rule,
                    step,
                    mut elems,
                    children,
                } => {
                    let Some(&s) = rule.steps.get(step) else {
                        continue; // this rule application is fully written
                    };
                    match s {
                        WriteStep::Discarded(tid) => {
                            match self.term_text.get(&tid) {
                                Some(TermText::Literal(text)) => {
                                    let text = text.clone();
                                    emit(&text, &mut out, &mut prev_last);
                                }
                                _ => {
                                    return Err(ReconstructError::NonLiteralTerminal {
                                        name: self.names[tid as usize].clone(),
                                    })
                                }
                            }
                            stack.push(Frame::Walk {
                                rule,
                                step: step + 1,
                                elems,
                                children,
                            });
                        }
                        WriteStep::Consume => {
                            let elem = elems
                                .next()
                                .expect("derivation elems align with Consume steps");
                            stack.push(Frame::Walk {
                                rule,
                                step: step + 1,
                                elems,
                                children,
                            });
                            match elem {
                                Elem::Child(ci) => match &children[ci] {
                                    Child::Token(tok) => emit(&tok.value, &mut out, &mut prev_last),
                                    Child::Tree(sub) => stack.push(Frame::Node(sub)),
                                    Child::None => return Err(ReconstructError::Placeholder),
                                },
                                Elem::Sub(d) => stack.push(Frame::Walk {
                                    rule: d.rule,
                                    step: 0,
                                    elems: d.elems.into_iter(),
                                    children,
                                }),
                            }
                        }
                    }
                }
            }
        }
        Ok(out)
    }
}

/// A synthesized unit rule `origin → tree-node(origin)`: a reference to an
/// expand1/aliased origin meeting an actual surviving node consumes it whole.
fn bridge_rule(origin: SymId) -> ReconsRule {
    ReconsRule {
        origin,
        expansion: vec![RSym::Term(origin)],
        steps: vec![WriteStep::Consume],
    }
}

/// The separator text inserted between pieces that would otherwise fuse: the
/// first of `" "`, `"\n"`, `"\t"` that some `%ignore`d terminal matches in
/// full, so the inserted text vanishes on re-parse. `None` when the grammar
/// cannot ignore any of them — inserting anything would *break* the re-parse,
/// so exact concatenation is the only correct output there.
fn pick_separator(grammar: &Grammar) -> Option<String> {
    let ignored: Vec<_> = grammar
        .terminals
        .iter()
        .filter(|t| grammar.ignore.contains(&t.name) && !t.declared)
        .collect();
    for cand in [" ", "\n", "\t"] {
        if ignored.iter().any(|t| terminal_full_matches(t, cand)) {
            return Some(cand.to_string());
        }
    }
    None
}

/// Best-effort probe: does terminal `t` match `text` in full? Used only to
/// choose a separator, so a pattern the probe cannot compile is just "no".
fn terminal_full_matches(t: &crate::grammar::terminal::TerminalDef, text: &str) -> bool {
    use crate::grammar::terminal::flags;
    match &t.pattern {
        Pattern::Str(s) => {
            if s.ci {
                s.value.eq_ignore_ascii_case(text)
            } else {
                s.value == text
            }
        }
        Pattern::Re(r) => {
            let mut letters = String::new();
            for (bit, ch) in [
                (flags::IGNORECASE, 'i'),
                (flags::MULTILINE, 'm'),
                (flags::DOTALL, 's'),
                (flags::VERBOSE, 'x'),
            ] {
                if r.flags & bit != 0 {
                    letters.push(ch);
                }
            }
            let wrapped = if letters.is_empty() {
                format!("(?:{})", r.pattern)
            } else {
                format!("(?{}:{})", letters, r.pattern)
            };
            match regex::Regex::new(&wrapped) {
                Ok(re) => re
                    .find(text)
                    .is_some_and(|m| m.start() == 0 && m.end() == text.len()),
                Err(_) => false,
            }
        }
    }
}

/// `maybe_placeholders` leaves fingerprints on the surface rules; any of them
/// means `None` children can occur and matching is ill-defined.
fn uses_placeholders(grammar: &Grammar) -> bool {
    grammar
        .rules
        .iter()
        .any(|r| r.options.placeholder_count > 0 || r.options.nones_before.iter().any(|&n| n > 0))
}

/// Deduplicate rules with an identical `(origin, expansion)` match shape:
/// such alternatives produce indistinguishable nodes (same matched children),
/// so the matcher only needs one. Among duplicates, keep the alternative that
/// is (a) *writable* — the fewest non-literal discarded terminals, so
/// `_WS? → ε` beats `_WS? → _WS` instead of erroring — and then (b) *most
/// explicit* — the MOST discarded literal write-outs. The explicit variant
/// reproduces the tokens the parser actually consumed to reach this
/// alternative; dropping them can flip the re-parse to a different
/// same-shaped rule (e.g. `b.1: "A"+ "B"?` losing its `"B"` re-parses as a
/// higher-priority sibling `a.2: "A"+` — caught by the bank sweep). Ties keep
/// grammar order. Finally sort shortest-expansion-first so the matcher
/// prefers the least redundant derivation. The counterpart of Python's
/// `_best_rules_from_group`.
fn dedup_and_sort(
    rules: Vec<ReconsRule>,
    term_text: &HashMap<SymId, TermText>,
) -> Vec<Rc<ReconsRule>> {
    let cost = |r: &ReconsRule| -> (usize, isize) {
        let mut nonliteral = 0usize;
        let mut discarded = 0isize;
        for s in &r.steps {
            if let WriteStep::Discarded(tid) = s {
                discarded += 1;
                if !matches!(term_text.get(tid), Some(TermText::Literal(_))) {
                    nonliteral += 1;
                }
            }
        }
        (nonliteral, -discarded)
    };
    let mut best: HashMap<(SymId, Vec<RSym>), usize> = HashMap::new();
    let mut out: Vec<ReconsRule> = Vec::new();
    for r in rules {
        let key = (r.origin, r.expansion.clone());
        match best.get(&key) {
            None => {
                best.insert(key, out.len());
                out.push(r);
            }
            Some(&i) => {
                if cost(&r) < cost(&out[i]) {
                    out[i] = r;
                }
            }
        }
    }
    out.sort_by_key(|r| r.expansion.len()); // stable: ties keep grammar order
    out.into_iter().map(Rc::new).collect()
}

/// Approximation of Python's `is_id_continue` (Unicode ID_CONTINUE): would two
/// adjacent characters fuse into one identifier-like token? Alphanumerics plus
/// `_` covers every case the metamorphic bank exercises; this is a spacing
/// heuristic, not an oracle-bound behavior.
fn is_id_continue(c: char) -> bool {
    c == '_' || c.is_alphanumeric()
}

// ─── Earley items and derivation extraction ─────────────────────────────────

struct Matcher {
    rules: Vec<Rc<ReconsRule>>,
    /// Prediction index: exact origin id → rule indices.
    by_origin: HashMap<SymId, Vec<usize>>,
    /// Rules whose origin's base name equals the match root's.
    root_candidates: Vec<usize>,
}

#[derive(Clone)]
struct Item {
    rule: usize,
    dot: usize,
    start: usize,
    /// Predecessor (set, idx) + what advanced it. `None` at dot 0.
    bp: Option<(usize, usize, Cause)>,
}

#[derive(Clone, Copy)]
enum Cause {
    /// The predecessor scanned the child at its own set position.
    Scan,
    /// The predecessor completed the item at (set, idx).
    Complete(usize, usize),
}

/// One matched element of a derivation, aligned (in order) with the rule's
/// `Consume` steps: a scanned child (by index into the node's child list), or
/// a nested rule application.
enum Elem {
    Child(usize),
    Sub(Box<Deriv>),
}

/// One rule application over a node's children.
struct Deriv {
    rule: Rc<ReconsRule>,
    elems: Vec<Elem>,
}

/// Walk backpointers into a `Deriv`, iteratively (derivation nesting grows
/// with child-list length through the EBNF recurse helpers, so native
/// recursion here would be input-depth recursion).
fn extract_derivation(m: &Matcher, sets: &[Vec<Item>], target: (usize, usize)) -> Deriv {
    // The ordered causes of one completed item: walk its bp chain to dot 0.
    let causes_of = |set: usize, idx: usize| -> (usize, Vec<(usize, usize, Cause)>) {
        let rule = sets[set][idx].rule;
        let mut causes = Vec::new();
        let (mut s, mut i) = (set, idx);
        while let Some((ps, pi, cause)) = sets[s][i].bp {
            causes.push((ps, pi, cause));
            s = ps;
            i = pi;
        }
        causes.reverse();
        (rule, causes)
    };

    struct Frame {
        rule: usize,
        causes: Vec<(usize, usize, Cause)>,
        next: usize,
        elems: Vec<Elem>,
    }
    let make_frame = |set: usize, idx: usize| -> Frame {
        let (rule, causes) = causes_of(set, idx);
        Frame {
            rule,
            causes,
            next: 0,
            elems: Vec::new(),
        }
    };

    let mut stack = vec![make_frame(target.0, target.1)];
    loop {
        let top = stack.last_mut().expect("derivation stack never empties");
        if top.next < top.causes.len() {
            let (pset, _pidx, cause) = top.causes[top.next];
            top.next += 1;
            match cause {
                // The predecessor lived in set `pset`, so it scanned child `pset`.
                Cause::Scan => top.elems.push(Elem::Child(pset)),
                Cause::Complete(cs, ci) => stack.push(make_frame(cs, ci)),
            }
        } else {
            let done = stack.pop().expect("just peeked");
            let deriv = Deriv {
                rule: Rc::clone(&m.rules[done.rule]),
                elems: done.elems,
            };
            match stack.last_mut() {
                Some(parent) => parent.elems.push(Elem::Sub(Box::new(deriv))),
                None => return deriv,
            }
        }
    }
}
