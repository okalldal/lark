//! Forest → tree conversion: the de-recursed SPPF walk that turns the forest into
//! a [`ParseTree`](crate::tree::ParseTree), routed through the shared
//! [`TreeOutputBuilder`] (ADR-0015).
//!
//! [`Transformer`] plus its frame-stack machinery ([`Frame`]/[`Ret`]/[`Walk`]).
//! The walk is **iterative**, not natively recursive (#33/#151) — `test_earley_stack`
//! is the tripwire — and the explicit-mode `_ambig` dedup (`node_value_key`, #159,
//! ADR-0017) lives here with its guard tests. Split out of the former monolithic
//! `earley.rs` (no logic change).

use std::collections::{HashMap, HashSet};

use crate::grammar::intern::{CompiledGrammar, SymbolId};
use crate::tree::{Child, Token, Tree};

use super::super::tree_builder::{ContainerSpan, Slot, TreeOutputBuilder};
use super::forest::{Forest, ForestRef, Packed};

// Backward-compat alias within earley — keeps diff minimal for this refactor.
type NodeValue = Slot;

// ─── Forest → tree conversion ─────────────────────────────────────────────────

/// Walks the SPPF bottom-up, building parse trees through the shared
/// [`TreeOutputBuilder`]. Symbol-node results are memoized (a forest node is reached by
/// many parents); intermediate nodes are expanded inline into their parent rule's
/// child list. Priorities are computed lazily, à la Lark's `ForestSumVisitor`.
pub(crate) struct Transformer<'a> {
    grammar: &'a CompiledGrammar,
    forest: &'a Forest,
    builder: TreeOutputBuilder<'a>,
    resolve: bool,
    /// Per-terminal-id priority, summed into the forest priority only when the
    /// dynamic lexer is used (the basic lexer consumes terminal priorities in its
    /// terminal ordering). Empty otherwise.
    term_priority: HashMap<SymbolId, i64>,
    /// Memoized symbol-node values (final assembled trees).
    memo: HashMap<usize, NodeValue>,
    /// Memoized per-symbol-node derivation lists (the deduped alternative values
    /// before they are collapsed into a single value / `_ambig`). Shared by
    /// [`Transformer::eval_symbol`] and the transparent-child ambiguity lifting in
    /// [`Transformer::expand_packed`].
    deriv_memo: HashMap<usize, Vec<NodeValue>>,
    /// Explicit mode (#59): memoized "is this node's whole subtree unambiguous?"
    /// (every reachable node has ≤ 1 family, no forest cycle). A `true` node has
    /// exactly one derivation, so the explicit walk would produce the *same* single
    /// value resolve mode does — letting a distributed *transparent* such node be
    /// **spliced** into the parent in one streaming pass (the `Stream*` frames)
    /// instead of re-materializing a growing `Inline` per spine level (the O(n²)
    /// the issue tracked). Genuine ambiguity (any node with > 1 family) stays
    /// `false` and keeps the cartesian `Derivs` distribution that the `_ambig`
    /// oracles pin. Computed once per node by [`Transformer::single_deriv`].
    single_deriv: HashMap<usize, bool>,
    /// Memoized node priorities + the in-progress set for cycle-safe summing.
    prio: HashMap<usize, i64>,
    prio_visiting: HashSet<usize>,
    /// Resolve mode: nodes whose value has been fully built at least once. A value
    /// is memoized (and thereafter cloned) only on its *second* visit — so a node
    /// reached just once (the common case, including every node of a left-recursive
    /// `expr: expr "+" term` chain) is moved into its single parent with no clone,
    /// keeping the walk linear instead of re-cloning each growing subtree. The
    /// Earley SPPF over-shares nodes even for unambiguous grammars, so a static
    /// reference count over-counts; this tracks *actual* reuse (issue #54).
    seen: HashSet<usize>,
    /// Python Lark's `propagate_positions` (#402). When set, the resolve walk
    /// accumulates each node's pre-filter container span — including the
    /// punctuation tokens it filters out of the buffer — so the shaped node's
    /// `meta` spans the filtered children, matching `TreeOutputBuilder::assemble`'s
    /// non-streaming path (and Python's `PropagatePositions`, which runs outside
    /// the child filter).
    pp: bool,
}

impl<'a> Transformer<'a> {
    pub(crate) fn new(
        grammar: &'a CompiledGrammar,
        forest: &'a Forest,
        resolve: bool,
        term_priority: bool,
    ) -> Self {
        // `term_priority` is set exactly when the dynamic lexer built the forest.
        // Map each terminal id to its declared priority (only consulted under the
        // dynamic lexer; built empty otherwise so the lookup is a no-op).
        let term_priority = if term_priority {
            grammar
                .terminals
                .iter()
                .filter_map(|t| grammar.symbols.id(&t.name).map(|id| (id, t.priority)))
                .filter(|(_, p)| *p != 0)
                .collect()
        } else {
            HashMap::new()
        };
        Transformer {
            grammar,
            forest,
            builder: TreeOutputBuilder::with_propagate_positions(
                &grammar.rules,
                grammar.propagate_positions,
            ),
            resolve,
            term_priority,
            memo: HashMap::new(),
            deriv_memo: HashMap::new(),
            single_deriv: HashMap::new(),
            prio: HashMap::new(),
            prio_visiting: HashSet::new(),
            seen: HashSet::new(),
            pp: grammar.propagate_positions,
        }
    }

    /// ForestSumVisitor: a node's priority is the max over its derivations.
    ///
    /// Iterative two-phase DFS (issue #33 — the priority sum recurses to forest
    /// depth just like the value walk did): `Enter` pushes a node's family
    /// children, `Exit` combines their now-memoized priorities. Semantics are
    /// identical to the natural recursion: results memoize in `prio`, and an edge
    /// back into an in-progress node (`prio_visiting`) contributes 0.
    fn node_priority(&mut self, id: usize) -> i64 {
        if let Some(&p) = self.prio.get(&id) {
            return p;
        }
        if self.prio_visiting.contains(&id) {
            return 0; // cycle: contribute nothing
        }
        enum Step {
            Enter(usize),
            Exit(usize),
        }
        let mut stack = vec![Step::Enter(id)];
        while let Some(step) = stack.pop() {
            match step {
                Step::Enter(n) => {
                    if self.prio.contains_key(&n) || !self.prio_visiting.insert(n) {
                        continue; // memoized, or in-progress (a cycle edge)
                    }
                    stack.push(Step::Exit(n));
                    // Children in reverse push order so they evaluate in family
                    // order, left before right — the recursive call order, which
                    // is load-bearing in cyclic forests (a child's value depends
                    // on which ancestors are in-progress when it is reached).
                    for p in self.forest.nodes[n].families.iter().rev() {
                        for r in [p.right, p.left] {
                            if let ForestRef::Node(c) = r {
                                stack.push(Step::Enter(c));
                            }
                        }
                    }
                }
                Step::Exit(n) => {
                    let node = &self.forest.nodes[n];
                    let parent_inter = node.is_intermediate;
                    let mut best = if node.families.is_empty() {
                        0
                    } else {
                        i64::MIN
                    };
                    for k in 0..self.forest.nodes[n].families.len() {
                        let p = self.forest.nodes[n].families[k];
                        let v = self.packed_priority_value(&p, parent_inter);
                        if v > best {
                            best = v;
                        }
                    }
                    self.prio_visiting.remove(&n);
                    self.prio.insert(n, best);
                }
            }
        }
        self.prio.get(&id).copied().unwrap_or(0)
    }

    /// A derivation's priority: the rule's own priority (only counted at a real
    /// symbol node, not at intermediates) plus its children's priorities. Token
    /// leaves count 0 — the basic lexer already "used up" terminal priorities.
    fn packed_priority(&mut self, packed: &Packed, parent_inter: bool) -> i64 {
        // Make sure both node children are computed (left before right, the
        // recursive evaluation order), then combine by lookup.
        for r in [packed.left, packed.right] {
            if let ForestRef::Node(c) = r {
                self.node_priority(c);
            }
        }
        self.packed_priority_value(packed, parent_inter)
    }

    /// Lookup-only half of [`packed_priority`](Self::packed_priority): combines
    /// child priorities already computed (or in-progress → 0) by the DFS.
    fn packed_priority_value(&self, packed: &Packed, parent_inter: bool) -> i64 {
        let rule_prio = self.grammar.rules[packed.rule].options.priority;
        let base = if !parent_inter && rule_prio != 0 {
            rule_prio
        } else {
            0
        };
        let child = |r: ForestRef| match r {
            ForestRef::Node(id) => {
                if self.prio_visiting.contains(&id) {
                    0 // in-progress: a cycle edge contributes nothing
                } else {
                    self.prio.get(&id).copied().unwrap_or(0)
                }
            }
            // A scanned token contributes its terminal's priority — but only under
            // the dynamic lexer (`term_priority` is empty for the basic lexer).
            ForestRef::Tok(t) => self
                .term_priority
                .get(&self.forest.tokens[t].type_id)
                .copied()
                .unwrap_or(0),
            ForestRef::None => 0,
        };
        // Saturating accumulation: priorities are a bounded i64 domain (ADR-0034),
        // so a derivation summing priorities near the i64 boundary saturates rather
        // than wrapping/panicking — mirrors CYK's `weight.saturating_add` chains.
        base.saturating_add(child(packed.left))
            .saturating_add(child(packed.right))
    }

    /// Family indices of `node_id` in Lark's `sort_key` order: non-empty
    /// derivations first, then higher priority, then lower rule order. A stable
    /// sort keeps insertion order among ties (which is how Lark breaks otherwise
    /// equal derivations — its `StableSymbolNode` stores packed children in an
    /// `OrderedSet`, so insertion order is the final tie-break there too).
    ///
    /// This is pure `(is_empty, -priority, rule.order)` + insertion order for *both*
    /// lexers. The dynamic-lexer split-point tie-break that #32/#90 added here is
    /// gone: it compensated for lark-rs building a grouped repetition through a
    /// nested `(A|B)` group node whose LIFO completion reversed Python's
    /// earliest-split-first segmentation order. With the EBNF expansion now inlining
    /// the group arms straight into the recurse rule (#91 — matching Python's
    /// `EBNF_to_BNF`), the last symbol of the recursion is a *terminal* built during
    /// the scan, so the segmentations already arrive in Python's order and the
    /// `rule.order` key alone disambiguates `dynamic_complete` ties (e.g. `WORD+`
    /// over `"bc"` resolves to one `WORD "bc"`, the `parse:49/72` cases).
    fn sorted_families(&mut self, node_id: usize) -> Vec<usize> {
        let forest = self.forest;
        let node = &forest.nodes[node_id];
        let parent_inter = node.is_intermediate;
        let prios: Vec<i64> = (0..node.families.len())
            .map(|k| {
                let p = node.families[k];
                self.packed_priority(&p, parent_inter)
            })
            .collect();
        let mut idx: Vec<usize> = (0..node.families.len()).collect();
        let fams = &forest.nodes[node_id].families;
        let grammar = self.grammar;
        idx.sort_by(|&a, &b| {
            let empty = |p: &Packed| {
                matches!(p.left, ForestRef::None) && matches!(p.right, ForestRef::None)
            };
            empty(&fams[a])
                .cmp(&empty(&fams[b]))
                .then(prios[b].cmp(&prios[a]))
                .then(
                    grammar.rules[fams[a].rule]
                        .order
                        .cmp(&grammar.rules[fams[b].rule].order),
                )
        });
        idx
    }

    /// Is the symbol node `id` produced by a transparent (`_rule` / `__anon_*`)
    /// rule? All families of a symbol node share its origin non-terminal, and
    /// transparency is a property of the origin, so the first family decides.
    /// Transparent symbols are exactly Lark's `_should_expand` positions — the ones
    /// whose ambiguity must be lifted into the parent (`AmbiguousExpander`).
    fn is_transparent_node(&self, id: usize) -> bool {
        self.forest.nodes[id]
            .families
            .first()
            .map(|p| self.grammar.rules[p.rule].transparent)
            .unwrap_or(false)
    }

    /// Explicit mode (#59): does `id`'s whole subtree have exactly one derivation
    /// — every reachable node ≤ 1 family, and no forest cycle through it? Such a
    /// node's explicit value is identical to the value resolve mode would build (no
    /// ambiguity to fan out), so a distributed *transparent* one can be spliced in a
    /// single streaming pass instead of re-materializing a growing `Inline` per
    /// spine level. Iterative two-phase DFS (`Enter`/`Exit`), memoized in
    /// `single_deriv` and bounded to O(1) native stack per #33: a node reached while
    /// still in-progress is a cycle → not single-derivation (conservatively `false`,
    /// so the cartesian `Derivs` path — which already handles cycles by discarding —
    /// keeps owning it). A node is single-derivation iff it has exactly one family
    /// and every `Node` child of that family is single-derivation.
    fn single_deriv(&mut self, id: usize) -> bool {
        if let Some(&b) = self.single_deriv.get(&id) {
            return b;
        }
        enum Step {
            Enter(usize),
            Exit(usize),
        }
        // In-progress set: a re-entry is a cycle (→ false). Local to this query
        // chain; every node we settle lands in `single_deriv`, so a later query
        // short-circuits at the memo.
        let mut on_stack: HashSet<usize> = HashSet::new();
        let mut stack = vec![Step::Enter(id)];
        while let Some(step) = stack.pop() {
            match step {
                Step::Enter(n) => {
                    if self.single_deriv.contains_key(&n) {
                        continue;
                    }
                    if !on_stack.insert(n) {
                        // Cycle edge: this node participates in a forest cycle.
                        self.single_deriv.insert(n, false);
                        continue;
                    }
                    let node = &self.forest.nodes[n];
                    if node.families.len() != 1 {
                        // 0 families (a discarded/empty node) or > 1 (ambiguous):
                        // not a single clean derivation.
                        on_stack.remove(&n);
                        self.single_deriv.insert(n, node.families.len() == 1);
                        continue;
                    }
                    stack.push(Step::Exit(n));
                    let p = node.families[0];
                    for r in [p.left, p.right] {
                        if let ForestRef::Node(c) = r {
                            stack.push(Step::Enter(c));
                        }
                    }
                }
                Step::Exit(n) => {
                    on_stack.remove(&n);
                    // Already decided as a cycle while on the stack? Keep it.
                    if self.single_deriv.contains_key(&n) {
                        continue;
                    }
                    let p = self.forest.nodes[n].families[0];
                    let child_ok = |r: ForestRef, this: &Self| match r {
                        ForestRef::Node(c) => this.single_deriv.get(&c).copied() == Some(true),
                        _ => true,
                    };
                    let ok = child_ok(p.left, self) && child_ok(p.right, self);
                    self.single_deriv.insert(n, ok);
                }
            }
        }
        self.single_deriv.get(&id).copied().unwrap_or(false)
    }

    // ─── The de-recursed walk (issue #33) ──────────────────────────────────────
    //
    // The walk's natural shape is a set of mutually recursive functions, but its
    // recursion depth is the SPPF chain length — O(input length) for any
    // list-like rule (`x*`, `x+`, `expr: expr "+" term`) — which used to require
    // running the whole walk on a dedicated thread with a 256 MB stack. The
    // recursion is reified instead: each former function is a *work* [`Frame`]
    // variant, each point after a recursive call a *continuation* variant, the
    // locals live in the frame, and the value a call would have returned travels
    // through [`Walk::ret`]. Frames are heap-allocated, so native-stack use is
    // O(1) regardless of forest depth (`std::thread` does not exist on WASM, #47,
    // so this is also what makes the engine portable there).
    //
    // The de-recursion is mechanical and preserves the original semantics
    // exactly, including the parts that are easy to get wrong:
    //  * the `visiting` cycle set (formerly a `&mut HashSet` parameter): inserted
    //    on entry, removed on *every* exit path of the former function;
    //  * resolve-mode rollback: a failed family truncates the shared child buffer
    //    back to the mark taken before its attempt;
    //  * the memoization points (`memo` / `deriv_memo` / resolve's
    //    second-visit-only `seen` rule) fire at the same places.

    /// Walk the forest from `root` to its final value — the de-recursed
    /// `eval_symbol(root)` (see [`Frame`] for the correspondence).
    pub(crate) fn transform(&mut self, root: usize) -> Option<NodeValue> {
        let mut walk = Walk {
            frames: vec![Frame::Eval { node: root }],
            bufs: Vec::new(),
            containers: Vec::new(),
            visiting: HashSet::new(),
            ret: None,
        };
        while let Some(frame) = walk.frames.pop() {
            self.step(frame, &mut walk);
        }
        match walk.ret {
            Some(Ret::Value(v)) => v,
            _ => unreachable!("the root Eval frame returns Ret::Value"),
        }
    }

    /// Execute one frame. A *work* frame ignores `w.ret`; a *continuation* frame
    /// consumes the return value of the child item it was pushed above.
    fn step(&mut self, frame: Frame, w: &mut Walk) {
        match frame {
            // ── eval_symbol: evaluate a real (non-intermediate) symbol node to a
            //    single value — the best derivation under `resolve`, or an
            //    `_ambig` over all of them under explicit. `Ret::Value(None)` if
            //    every derivation is discarded (e.g. an ambiguity cycle).
            Frame::Eval { node } => {
                if let Some(v) = self.memo.get(&node) {
                    w.ret = Some(Ret::Value(Some(v.clone())));
                } else if self.resolve {
                    // Resolve mode keeps a single derivation, so its value is
                    // assembled by streaming children straight into one buffer —
                    // a left-recursive transparent helper (`x*`/`x+`/`_rule`)
                    // then costs O(total children) instead of the O(children²)
                    // the materialize-then-splice path pays re-copying each
                    // growing prefix (issue #54). The streamed frames mirror
                    // `TreeOutputBuilder::assemble`'s filtering + shaping (via
                    // `keep_token` / `shape`) so resolve trees stay
                    // byte-for-byte identical to the explicit path and to LALR.
                    w.bufs.push(Vec::new());
                    if self.pp {
                        w.containers.push(ContainerSpan::new());
                    }
                    w.frames.push(Frame::EvalShape { node });
                    w.frames.push(Frame::AppendRule { node });
                } else {
                    w.frames.push(Frame::EvalAmbig { node });
                    w.frames.push(Frame::Derivs { node });
                }
            }
            // Resolve: shape the children streamed into this node's buffer.
            Frame::EvalShape { node } => {
                let children = w.bufs.pop().expect("Eval pushed a buffer");
                match w.take_ret() {
                    Ret::Rule(None) => w.ret = Some(Ret::Value(None)),
                    Ret::Rule(Some(rule)) => {
                        // With `propagate_positions`, widen the node's span to its
                        // pre-filter container (the punctuation the buffer dropped);
                        // otherwise the plain `shape` path (post-filter span) — #402.
                        let v = if self.pp {
                            let container = w.containers.pop().expect("Eval pushed a container");
                            self.builder.shape_with_container(rule, children, container)
                        } else {
                            self.builder.shape(rule, children)
                        };
                        // Memoize only on the second visit: a single-use node is
                        // returned by move (no clone). `insert` returns false
                        // when the node was already present, i.e. this is its
                        // second full build — cache it so any further reuse is a
                        // cheap clone, bounding rebuilds to at most two per node.
                        if !self.seen.insert(node) {
                            self.memo.insert(node, v.clone());
                        }
                        w.ret = Some(Ret::Value(Some(v)));
                    }
                    _ => unreachable!("AppendRule returns Ret::Rule"),
                }
            }

            // ── append_rule_children (resolve): pick `node`'s best non-discarded
            //    family and append its rule-position children — post-filter, with
            //    transparent children spliced in place — to the current buffer.
            //    Returns the chosen rule (so the parent can `shape` it), or `None`
            //    if every family is discarded (a forest cycle). Works for both
            //    symbol nodes (a complete rule) and intermediate nodes (a rule
            //    prefix); both just contribute children in left-to-right order.
            Frame::AppendRule { node } => {
                if !w.visiting.insert(node) {
                    w.ret = Some(Ret::Rule(None)); // cycle — discard this derivation
                } else {
                    let fams = self.sorted_families(node);
                    self.rule_try_family(w, node, fams, 0);
                }
            }
            Frame::RuleNext {
                node,
                fams,
                idx,
                mark,
                container_mark,
                rule,
            } => {
                let Ret::Packed(ok) = w.take_ret() else {
                    unreachable!("AppendPacked returns Ret::Packed")
                };
                if ok {
                    w.visiting.remove(&node);
                    w.ret = Some(Ret::Rule(Some(rule)));
                } else {
                    // Discarded part-way: roll back the buffer (and, under
                    // propagate_positions, the container span observed during this
                    // family), then try the next family.
                    w.buf().truncate(mark);
                    if let Some(snapshot) = container_mark {
                        *w.container() = snapshot;
                    }
                    self.rule_try_family(w, node, fams, idx + 1);
                }
            }

            // ── append_packed (resolve): append one derivation's children — its
            //    left prefix (an intermediate of the same rule) then its right
            //    symbol at `packed.right_pos`. `Ret::Packed(false)` if any
            //    sub-node is discarded, so the parent can try another family.
            Frame::AppendPacked { packed } => match packed.left {
                ForestRef::None => self.packed_right(w, packed),
                ForestRef::Node(lid) => {
                    w.frames.push(Frame::PackedRight { packed });
                    w.frames.push(Frame::AppendRule { node: lid });
                }
                // `left` is always an intermediate node or nothing in the
                // binarized forest; handle a token defensively for symmetry with
                // the explicit path.
                ForestRef::Tok(t) => {
                    let tok = self.forest.tokens[t].clone();
                    if self.pp {
                        w.container().observe_token(&tok);
                    }
                    w.buf().push(Child::Token(tok));
                    self.packed_right(w, packed);
                }
            },
            Frame::PackedRight { packed } => match w.take_ret() {
                Ret::Rule(None) => w.ret = Some(Ret::Packed(false)),
                Ret::Rule(Some(_)) => self.packed_right(w, packed),
                _ => unreachable!("AppendRule returns Ret::Rule"),
            },
            Frame::PackedAfterSplice => {
                let Ret::Rule(rule) = w.take_ret() else {
                    unreachable!("Splice returns Ret::Rule")
                };
                w.ret = Some(Ret::Packed(rule.is_some()));
            }
            // A real (non-transparent) right symbol contributes one shaped value;
            // mirror `assemble`'s per-value handling (a `Token` is subject to the
            // position's filter, a `Tree` is always kept).
            Frame::PackedAfterEval { rule, right_pos } => {
                let Ret::Value(v) = w.take_ret() else {
                    unreachable!("Eval returns Ret::Value")
                };
                match v {
                    None => w.ret = Some(Ret::Packed(false)),
                    Some(NodeValue::Token(tk)) => {
                        // Observe before the per-position filter so a dropped
                        // punctuation token still bounds the container span (#402).
                        if self.pp {
                            w.container().observe_token(&tk);
                        }
                        if self.builder.keep_token(rule, right_pos) {
                            w.buf().push(Child::Token(tk));
                        }
                        w.ret = Some(Ret::Packed(true));
                    }
                    Some(NodeValue::Tree(tr)) => {
                        if self.pp {
                            w.container().observe_meta(&tr.meta);
                        }
                        w.buf().push(Child::Tree(tr));
                        w.ret = Some(Ret::Packed(true));
                    }
                    Some(NodeValue::Inline(cs)) => {
                        if self.pp {
                            for c in &cs {
                                w.container().observe_child(c);
                            }
                        }
                        w.buf().extend(cs);
                        w.ret = Some(Ret::Packed(true));
                    }
                }
            }

            // ── splice_node (resolve): append a transparent symbol node's spliced
            //    children (plus its rule's `maybe_placeholders`) to the current
            //    buffer, so a chain of transparent helpers flattens in one linear
            //    pass.
            Frame::Splice { node } => {
                w.frames.push(Frame::SpliceTail);
                w.frames.push(Frame::AppendRule { node });
            }
            Frame::SpliceTail => {
                let Ret::Rule(rule) = w.take_ret() else {
                    unreachable!("AppendRule returns Ret::Rule")
                };
                match rule {
                    None => w.ret = Some(Ret::Rule(None)),
                    Some(rule) => {
                        for _ in 0..self.grammar.rules[rule].options.placeholder_count {
                            w.buf().push(Child::None);
                        }
                        // Trailing placeholders of a distributed absent `[...]`
                        // (the streaming mirror of `TreeOutputBuilder::shape`'s
                        // trailing append).
                        let len = self.grammar.rules[rule].expansion.len();
                        self.push_nones_before(rule, len, w.buf());
                        w.ret = Some(Ret::Rule(Some(rule)));
                    }
                }
            }

            // ── eval_symbol, explicit tail: collapse the derivation list to one
            //    value, or an `_ambig` over all of them.
            Frame::EvalAmbig { node } => {
                let Ret::Derivs(mut derivs) = w.take_ret() else {
                    unreachable!("Derivs returns Ret::Derivs")
                };
                let result = match derivs.len() {
                    0 => None,
                    1 => Some(derivs.pop().unwrap()),
                    _ => {
                        let children: Vec<Child> =
                            derivs.into_iter().map(node_value_to_child).collect();
                        Some(NodeValue::Tree(Tree::new("_ambig", children)))
                    }
                };
                if let Some(v) = &result {
                    self.memo.insert(node, v.clone());
                }
                w.ret = Some(Ret::Value(result));
            }

            // ── stream a single-derivation transparent child (explicit, #59):
            //    splice `node` into a fresh buffer with the *resolve* transparent
            //    splice (`Frame::Splice` → `SpliceTail`) — one linear pass down the
            //    spine, no growing per-level `Inline` — then hand the buffer back
            //    wrapped as the lone `Inline` derivation alternative `ExpandCombine`
            //    expects. Reusing `Splice` (not bare `AppendRule`) is what makes the
            //    streamed value byte-identical to the `Derivs` + `assemble` value it
            //    replaces: `SpliceTail` appends the transparent rule's rule-level
            //    `placeholder_count` and trailing `nones_before` `None` slots that
            //    `AppendRule` alone omits. Sound because `single_deriv(node)`
            //    guaranteed exactly one derivation (no ambiguity to fan out).
            Frame::StreamDistribute { node } => {
                w.bufs.push(Vec::new());
                // Keep `containers` aligned with `bufs` so the filter sites' top-of-
                // stack `container()` never desyncs. A transparent node builds no
                // wrapper, so this span is discarded — its spliced children carry
                // their own positions up to the parent's container (#402).
                if self.pp {
                    w.containers.push(ContainerSpan::new());
                }
                w.frames.push(Frame::StreamDistributeDone);
                w.frames.push(Frame::Splice { node });
            }
            Frame::StreamDistributeDone => {
                let children = w.bufs.pop().expect("StreamDistribute pushed a buffer");
                if self.pp {
                    w.containers
                        .pop()
                        .expect("StreamDistribute pushed a container");
                }
                let derivs = match w.take_ret() {
                    // A discarded family would mean a cycle, which `single_deriv`
                    // already excludes — but stay defensive: no derivation, no
                    // alternative.
                    Ret::Rule(None) => Vec::new(),
                    Ret::Rule(Some(_)) => vec![NodeValue::Inline(children)],
                    _ => unreachable!("Splice returns Ret::Rule"),
                };
                #[cfg(feature = "perf-counters")]
                crate::perf::add_explicit_node_children(
                    derivs.iter().map(node_value_size).sum::<u64>(),
                );
                w.ret = Some(Ret::Derivs(derivs));
            }

            // ── symbol_derivations (explicit): the deduped list of derivation
            //    values for a symbol node — every distinct derivation. Memoized,
            //    since a shared SPPF node is reachable from many parents.
            Frame::Derivs { node } => {
                debug_assert!(
                    !self.resolve,
                    "resolve mode streams; it never materializes derivation lists"
                );
                if let Some(d) = self.deriv_memo.get(&node) {
                    w.ret = Some(Ret::Derivs(d.clone()));
                } else if !w.visiting.insert(node) {
                    // Cycle in the forest — discard this family.
                    w.ret = Some(Ret::Derivs(Vec::new()));
                } else {
                    let fams = self.sorted_families(node);
                    self.derivs_try_family(w, node, fams, 0, Vec::new(), HashSet::new());
                }
            }
            Frame::DerivsNext {
                node,
                fams,
                idx,
                rule,
                mut derivs,
                mut keys,
            } => {
                let Ret::Lists(lists) = w.take_ret() else {
                    unreachable!("ExpandPacked returns Ret::Lists")
                };
                let mut push_deduped = |v: NodeValue| {
                    if keys.insert(node_value_key(&v)) {
                        derivs.push(v);
                    }
                };
                for list in lists {
                    // Python's `_collapse_ambig`: a derivation that assembles
                    // to an `_ambig` (an expand1 rule whose single kept child
                    // is ambiguous) contributes its alternatives flat, not as
                    // a nested `_ambig` (#63). (`Tree` has a manual `Drop`, so
                    // the children are taken, not moved out.)
                    match self.builder.assemble(rule, list) {
                        NodeValue::Tree(mut t) if t.data == "_ambig" => {
                            for c in std::mem::take(&mut t.children) {
                                push_deduped(child_to_node_value(c));
                            }
                        }
                        v => push_deduped(v),
                    }
                }
                self.derivs_try_family(w, node, fams, idx + 1, derivs, keys);
            }

            // ── expand_packed (explicit): expand one derivation into its rule's
            //    child-lists. `left` is always an intermediate (the accumulated
            //    prefix) or nothing; `right` is the symbol just consumed (a
            //    symbol node or token leaf) or nothing (ε).
            Frame::ExpandPacked { packed } => match packed.left {
                ForestRef::None => self.expand_right(w, packed, vec![Vec::new()]),
                ForestRef::Node(lid) => {
                    w.frames.push(Frame::ExpandRight { packed });
                    w.frames.push(Frame::ExpandInter { node: lid });
                }
                ForestRef::Tok(t) => {
                    let tok = self.forest.tokens[t].clone();
                    self.expand_right(w, packed, vec![vec![NodeValue::Token(tok)]]);
                }
            },
            Frame::ExpandRight { packed } => {
                let Ret::Lists(lefts) = w.take_ret() else {
                    unreachable!("ExpandInter returns Ret::Lists")
                };
                if lefts.is_empty() {
                    w.ret = Some(Ret::Lists(Vec::new()));
                } else {
                    self.expand_right(w, packed, lefts);
                }
            }
            Frame::ExpandCombine {
                lefts,
                distribute_right,
            } => {
                let right_alts: Vec<NodeValue> = if distribute_right {
                    let Ret::Derivs(alts) = w.take_ret() else {
                        unreachable!("Derivs returns Ret::Derivs")
                    };
                    alts
                } else {
                    match w.take_ret() {
                        Ret::Value(Some(v)) => vec![v],
                        Ret::Value(None) => Vec::new(),
                        _ => unreachable!("Eval returns Ret::Value"),
                    }
                };
                if right_alts.is_empty() {
                    // Right discarded → the whole derivation is gone.
                    w.ret = Some(Ret::Lists(Vec::new()));
                } else {
                    self.expand_combine(w, &lefts, &right_alts);
                }
            }

            // ── expand_intermediate (explicit): expand an intermediate node into
            //    the alternative child-lists it contributes to its parent rule.
            Frame::ExpandInter { node } => {
                if !w.visiting.insert(node) {
                    w.ret = Some(Ret::Lists(Vec::new())); // cycle — discard
                } else {
                    let fams = self.sorted_families(node);
                    self.inter_try_family(w, node, fams, 0, Vec::new());
                }
            }
            Frame::InterNext {
                node,
                fams,
                idx,
                mut out,
            } => {
                let Ret::Lists(lists) = w.take_ret() else {
                    unreachable!("ExpandPacked returns Ret::Lists")
                };
                out.extend(lists);
                self.inter_try_family(w, node, fams, idx + 1, out);
            }
        }
    }

    /// Resolve: try family `fams[idx]` of `node`, or finish with `Ret::Rule(None)`
    /// once every family has been discarded (the loop of the former
    /// `append_rule_children`).
    fn rule_try_family(&mut self, w: &mut Walk, node: usize, fams: Vec<usize>, idx: usize) {
        match fams.get(idx) {
            None => {
                w.visiting.remove(&node);
                w.ret = Some(Ret::Rule(None));
            }
            Some(&fi) => {
                let packed = self.forest.nodes[node].families[fi];
                let mark = w.buf().len();
                let container_mark = self.pp.then(|| w.container().clone());
                w.frames.push(Frame::RuleNext {
                    node,
                    fams,
                    idx,
                    mark,
                    container_mark,
                    rule: packed.rule,
                });
                w.frames.push(Frame::AppendPacked { packed });
            }
        }
    }

    /// Resolve: handle `packed.right` once the left prefix has streamed into the
    /// buffer — the tail half of the former `append_packed`.
    fn packed_right(&mut self, w: &mut Walk, packed: Packed) {
        match packed.right {
            // ε production: no right child.
            ForestRef::None => w.ret = Some(Ret::Packed(true)),
            ForestRef::Tok(t) => {
                self.push_nones_before(packed.rule, packed.right_pos, w.buf());
                let tok = self.forest.tokens[t].clone();
                // Observe before filtering so a dropped punctuation token still
                // bounds the container span (#402).
                if self.pp {
                    w.container().observe_token(&tok);
                }
                if self.builder.keep_token(packed.rule, packed.right_pos) {
                    w.buf().push(Child::Token(tok));
                }
                w.ret = Some(Ret::Packed(true));
            }
            ForestRef::Node(rid) => {
                self.push_nones_before(packed.rule, packed.right_pos, w.buf());
                if self.is_transparent_node(rid) {
                    // Splice the transparent child's children straight into the
                    // buffer.
                    w.frames.push(Frame::PackedAfterSplice);
                    w.frames.push(Frame::Splice { node: rid });
                } else {
                    w.frames.push(Frame::PackedAfterEval {
                        rule: packed.rule,
                        right_pos: packed.right_pos,
                    });
                    w.frames.push(Frame::Eval { node: rid });
                }
            }
        }
    }

    /// Explicit: expand family `fams[idx]` of `node`, or finish (memoize + hand
    /// back) the derivation list once every family has been processed (the loop
    /// of the former `symbol_derivations`).
    fn derivs_try_family(
        &mut self,
        w: &mut Walk,
        node: usize,
        fams: Vec<usize>,
        idx: usize,
        derivs: Vec<NodeValue>,
        keys: HashSet<String>,
    ) {
        match fams.get(idx) {
            None => {
                w.visiting.remove(&node);
                // The *real* #56 Arm-2 cost: explicit mode materializes one owned
                // value per symbol node, and a transparent left-recursive helper's
                // value is the whole accumulated child list — so the SPPF chain of
                // n helper nodes builds Inlines of size 1, 2, …, n = O(n²) elements
                // total (and `deriv_memo` then clones them). Counting the
                // materialized derivation sizes here (behind `perf-counters`)
                // exhibits that quadratic deterministically — the signal the
                // streaming fix (the explicit analog of #55) must flatten. It is
                // *not* the cartesian clone loop the issue guessed (that is
                // linear; see `expand_combine`). Gated at the call site (not just
                // inside the no-op) so the `sum` — itself O(materialized
                // children), i.e. the quadratic we are measuring — is never
                // computed in a normal build.
                #[cfg(feature = "perf-counters")]
                crate::perf::add_explicit_node_children(
                    derivs.iter().map(node_value_size).sum::<u64>(),
                );
                self.deriv_memo.insert(node, derivs.clone());
                w.ret = Some(Ret::Derivs(derivs));
            }
            Some(&fi) => {
                let packed = self.forest.nodes[node].families[fi];
                w.frames.push(Frame::DerivsNext {
                    node,
                    fams,
                    idx,
                    rule: packed.rule,
                    derivs,
                    keys,
                });
                w.frames.push(Frame::ExpandPacked { packed });
            }
        }
    }

    /// Explicit: handle `packed.right` once the left prefixes are known — the
    /// tail half of the former `expand_packed`.
    ///
    /// The alternative values the right symbol contributes are normally one — but
    /// an ambiguous child at one of Lark's `AmbiguousExpander` *to_expand*
    /// positions contributes one alternative per derivation, distributed over
    /// the parent's child-lists: rather than nest an `_ambig` under the child's
    /// position, the ambiguity is shifted up so the parent itself becomes the
    /// `_ambig` (`parent(_ambig(a, b))` → `_ambig(parent(a), parent(b))`).
    /// to_expand covers a *transparent* (`_rule` / `__anon_*`) child always (its
    /// alternatives are `Inline` splice values with no node to nest under) and —
    /// since `keep_all_tokens` puts every position in to_expand — ANY child of a
    /// `!` rule (#63). Both consume the node's derivation list (`Derivs`)
    /// directly: it is exactly the list `Eval` would wrap in an `_ambig`, so
    /// distributing it skips the wrap/unwrap and reuses `deriv_memo`.
    fn expand_right(&mut self, w: &mut Walk, packed: Packed, lefts: Vec<Vec<NodeValue>>) {
        match packed.right {
            // ε right: the child-lists are exactly the left prefixes.
            ForestRef::None => w.ret = Some(Ret::Lists(lefts)),
            ForestRef::Node(rid) => {
                let distribute_right = !self.resolve
                    && (self.is_transparent_node(rid)
                        || self.grammar.rules[packed.rule].options.keep_all_tokens);
                w.frames.push(Frame::ExpandCombine {
                    lefts,
                    distribute_right,
                });
                if distribute_right {
                    // #59: a *transparent* distributed child whose whole subtree is
                    // unambiguous has exactly one derivation, so its explicit value
                    // equals the value resolve mode would build — splice it into a
                    // fresh buffer in one streaming pass (yielding the single
                    // `Inline` alternative) instead of materializing a growing
                    // per-spine-level `Inline` through `Derivs` (the O(n²) on a
                    // transparent left-recursive helper that #59 fixes). Ambiguous
                    // children (> 1 derivation anywhere in the subtree) and the
                    // `keep_all_tokens` distribution of a non-transparent child keep
                    // the cartesian `Derivs` path, which the `_ambig` oracles pin.
                    if self.is_transparent_node(rid) && self.single_deriv(rid) {
                        w.frames.push(Frame::StreamDistribute { node: rid });
                    } else {
                        w.frames.push(Frame::Derivs { node: rid });
                    }
                } else {
                    w.frames.push(Frame::Eval { node: rid });
                }
            }
            ForestRef::Tok(t) => {
                let tok = self.forest.tokens[t].clone();
                self.expand_combine(w, &lefts, &[NodeValue::Token(tok)]);
            }
        }
    }

    /// Explicit: the cartesian product of left prefixes × right alternatives.
    ///
    /// The named #56 Arm-2 suspect: clone each growing prefix to form the
    /// cartesian product of left prefixes × right values. Counting the
    /// `NodeValue`s copied here (behind `perf-counters`) is what *disproves* that
    /// guess — it stays **linear** even on a transparent left-recursive helper
    /// (`x*` / `x+` / `_rule`), because every rule's binarized RHS prefix is
    /// bounded (≤ its arity), so this clone is O(1) per node. The real explicit
    /// super-linearity is the per-node derivation-value rebuild counted in
    /// `derivs_try_family` — the still-missing explicit analog of #55's
    /// resolve-mode streaming. Kept verbatim (no fast path) so the disproof
    /// measures the actual loop; the true fix is tracked as a follow-up (#59).
    fn expand_combine(&self, w: &mut Walk, lefts: &[Vec<NodeValue>], right_alts: &[NodeValue]) {
        let mut out: Vec<Vec<NodeValue>> = Vec::with_capacity(lefts.len() * right_alts.len());
        for list in lefts {
            for rv in right_alts {
                crate::perf::add_explicit_prefix_copies(list.len() as u64);
                let mut l = list.clone();
                l.push(rv.clone());
                out.push(l);
            }
        }
        w.ret = Some(Ret::Lists(out));
    }

    /// Explicit: expand family `fams[idx]` of intermediate `node`, or finish (the
    /// loop of the former `expand_intermediate`).
    fn inter_try_family(
        &mut self,
        w: &mut Walk,
        node: usize,
        fams: Vec<usize>,
        idx: usize,
        out: Vec<Vec<NodeValue>>,
    ) {
        match fams.get(idx) {
            None => {
                w.visiting.remove(&node);
                w.ret = Some(Ret::Lists(out));
            }
            Some(&fi) => {
                let packed = self.forest.nodes[node].families[fi];
                w.frames.push(Frame::InterNext {
                    node,
                    fams,
                    idx,
                    out,
                });
                w.frames.push(Frame::ExpandPacked { packed });
            }
        }
    }

    /// Push the `None` placeholders a distributed absent `[...]` left before
    /// expansion position `gap` of `rule` (the streaming mirror of
    /// `TreeOutputBuilder::assemble`'s per-position insert).
    fn push_nones_before(&self, rule: usize, gap: usize, out: &mut Vec<Child>) {
        for _ in 0..self.builder.nones_at(rule, gap) {
            out.push(Child::None);
        }
    }
}

/// One step of the de-recursed forest walk (issue #33). *Work* variants are the
/// entries of the former recursive functions; *continuation* variants resume them
/// after the child item above finishes (its result in [`Walk::ret`]).
///
/// Correspondence to the former recursion — resolve mode (the streaming assembly
/// of #54/#55):
///
/// | function               | work          | continuation(s)                  |
/// |------------------------|---------------|----------------------------------|
/// | `eval_symbol`          | `Eval`        | `EvalShape`                      |
/// | `append_rule_children` | `AppendRule`  | `RuleNext`                       |
/// | `append_packed`        | `AppendPacked`| `PackedRight`, `PackedAfterSplice`, `PackedAfterEval` |
/// | `splice_node`          | `Splice`      | `SpliceTail`                     |
///
/// explicit mode:
///
/// | function               | work           | continuation(s)               |
/// |------------------------|----------------|-------------------------------|
/// | `eval_symbol`          | `Eval`         | `EvalAmbig`                   |
/// | `symbol_derivations`   | `Derivs`       | `DerivsNext`                  |
/// | `expand_packed`        | `ExpandPacked` | `ExpandRight`, `ExpandCombine`|
/// | `expand_intermediate`  | `ExpandInter`  | `InterNext`                   |
/// | stream single-deriv child (#59) | `StreamDistribute` | `StreamDistributeDone` (reuses the resolve `Splice`/`SpliceTail` frames) |
enum Frame {
    Eval {
        node: usize,
    },
    EvalShape {
        node: usize,
    },
    AppendRule {
        node: usize,
    },
    /// Resume after the attempt of family `fams[idx]` (whose rule is `rule`);
    /// `mark` is the buffer length to roll back to if it was discarded.
    RuleNext {
        node: usize,
        fams: Vec<usize>,
        idx: usize,
        mark: usize,
        /// Container snapshot at `mark`, to restore on rollback when this family is
        /// discarded part-way (`propagate_positions` only; `None` otherwise — #402).
        container_mark: Option<ContainerSpan>,
        rule: usize,
    },
    AppendPacked {
        packed: Packed,
    },
    /// Resume after the left prefix of `packed` streamed in.
    PackedRight {
        packed: Packed,
    },
    PackedAfterSplice,
    /// Resume after the right symbol's value; `rule`/`right_pos` locate its
    /// position for per-position token filtering.
    PackedAfterEval {
        rule: usize,
        right_pos: usize,
    },
    Splice {
        node: usize,
    },
    SpliceTail,
    EvalAmbig {
        node: usize,
    },
    /// #59: stream a single-derivation transparent distributed child into a fresh
    /// buffer (via the resolve `Splice`/`SpliceTail` frames), then wrap it as the
    /// lone `Inline` derivation alternative — the explicit reuse of resolve's splice.
    StreamDistribute {
        node: usize,
    },
    StreamDistributeDone,
    Derivs {
        node: usize,
    },
    /// Resume after family `fams[idx]` (rule `rule`) expanded; `derivs`/`keys`
    /// are the accumulated deduped values.
    DerivsNext {
        node: usize,
        fams: Vec<usize>,
        idx: usize,
        rule: usize,
        derivs: Vec<NodeValue>,
        keys: HashSet<String>,
    },
    ExpandPacked {
        packed: Packed,
    },
    /// Resume after the left intermediate's child-lists.
    ExpandRight {
        packed: Packed,
    },
    /// Resume after the right symbol's value(s); `distribute_right` records
    /// which child item was pushed (`Derivs` vs `Eval`), i.e. which `Ret`
    /// variant to consume: a child at one of Python's `AmbiguousExpander`
    /// to_expand positions — transparent, or any position of a
    /// `keep_all_tokens` rule (#63) — distributes its derivation list over
    /// the parent's child-lists instead of nesting an `_ambig`.
    ExpandCombine {
        lefts: Vec<Vec<NodeValue>>,
        distribute_right: bool,
    },
    ExpandInter {
        node: usize,
    },
    /// Resume after family `fams[idx]` expanded; `out` is the accumulated lists.
    InterNext {
        node: usize,
        fams: Vec<usize>,
        idx: usize,
        out: Vec<Vec<NodeValue>>,
    },
}

/// The value a finished walk item hands back — the return value of the
/// corresponding former recursive function.
enum Ret {
    /// `eval_symbol`: the node's single value, or `None` if every derivation was
    /// discarded.
    Value(Option<NodeValue>),
    /// `append_rule_children` / `splice_node`: the chosen rule, or `None` if
    /// every family was discarded.
    Rule(Option<usize>),
    /// `append_packed`: did this derivation contribute its children?
    Packed(bool),
    /// `symbol_derivations`: the node's deduped derivation values.
    Derivs(Vec<NodeValue>),
    /// `expand_packed` / `expand_intermediate`: alternative child-lists.
    Lists(Vec<Vec<NodeValue>>),
}

/// Mutable state of one [`Transformer::transform`] run: the frame stack, the
/// resolve-mode child-buffer stack (each `Eval` pushes a fresh buffer; splices
/// stream into the top one, so `RuleNext`'s rollback marks stay valid), the
/// in-progress cycle set (the former `visiting` parameter), and the return-value
/// slot connecting a finished item to its continuation.
struct Walk {
    frames: Vec<Frame>,
    bufs: Vec<Vec<Child>>,
    /// Pre-filter container span per open buffer (parallel to `bufs`), tracked only
    /// under `propagate_positions`. Each filtered/kept child is `observe`d into the
    /// top span as it streams by, so a node's `meta` can span the punctuation the
    /// buffer drops (#402). Empty (never pushed) when the flag is off.
    containers: Vec<ContainerSpan>,
    visiting: HashSet<usize>,
    ret: Option<Ret>,
}

impl Walk {
    /// Take the child item's return value (each continuation consumes exactly one).
    fn take_ret(&mut self) -> Ret {
        self.ret
            .take()
            .expect("a finished walk item set a return value")
    }

    /// The resolve-mode child buffer currently being streamed into.
    fn buf(&mut self) -> &mut Vec<Child> {
        self.bufs
            .last_mut()
            .expect("a resolve Eval frame pushed a buffer")
    }

    /// The pre-filter container span for the buffer currently being streamed into.
    fn container(&mut self) -> &mut ContainerSpan {
        self.containers
            .last_mut()
            .expect("a resolve Eval frame pushed a container (propagate_positions)")
    }
}

/// The number of child slots a materialized derivation value occupies — the unit
/// of the [`explicit_node_children`](crate::perf::explicit_node_children) cost
/// signal (#56 Arm 2). A transparent left-recursive helper's value grows by one
/// per SPPF level, so summing this over the chain is the O(n²) the explicit walk
/// pays where resolve mode streams (#55).
#[cfg(feature = "perf-counters")]
fn node_value_size(v: &NodeValue) -> u64 {
    match v {
        NodeValue::Token(_) => 1,
        NodeValue::Tree(t) => t.children.len() as u64,
        NodeValue::Inline(cs) => cs.len() as u64,
    }
}

/// A stable structural key for de-duplicating equal `_ambig` derivations.
/// Iterative (explicit work stack) — a derivation value is as deep as the tree it
/// describes, and the walk that calls this must not recurse to input depth (#33).
fn node_value_key(v: &NodeValue) -> String {
    enum K<'a> {
        Child(&'a Child),
        Tree(&'a Tree),
        Lit(&'static str),
    }
    fn token_key(t: &Token, out: &mut String) {
        out.push_str("T:");
        out.push_str(&t.type_);
        out.push('=');
        out.push_str(&t.value);
    }
    let mut out = String::new();
    let mut stack: Vec<K> = Vec::new();
    match v {
        NodeValue::Token(t) => token_key(t, &mut out),
        NodeValue::Tree(t) => stack.push(K::Tree(t)),
        NodeValue::Inline(cs) => {
            out.push_str("I[");
            stack.push(K::Lit("]"));
            for c in cs.iter().rev() {
                stack.push(K::Lit(","));
                stack.push(K::Child(c));
            }
        }
    }
    while let Some(k) = stack.pop() {
        match k {
            K::Lit(s) => out.push_str(s),
            K::Child(c) => match c {
                Child::Tree(t) => stack.push(K::Tree(t)),
                Child::Token(t) => token_key(t, &mut out),
                Child::None => out.push_str("None"),
            },
            K::Tree(t) => {
                out.push('(');
                out.push_str(&t.data);
                stack.push(K::Lit(")"));
                for c in t.children.iter().rev() {
                    stack.push(K::Child(c));
                    stack.push(K::Lit(" "));
                }
            }
        }
    }
    out
}

/// The inverse of [`node_value_to_child`], for an `_ambig` alternative being
/// lifted back out and re-distributed into a parent derivation (#63).
fn child_to_node_value(c: Child) -> NodeValue {
    match c {
        Child::Tree(t) => NodeValue::Tree(t),
        Child::Token(t) => NodeValue::Token(t),
        // An `_ambig`'s children are full alternative derivations, never a
        // `maybe_placeholders` slot — the same invariant the LALR expand1
        // collapse relies on (the guarded `Child::None` arm in
        // `tree_builder::TreeOutputBuilder::shape`).
        Child::None => unreachable!("an `_ambig` alternative is never a placeholder"),
    }
}

/// One `_ambig` alternative as a tree child.
fn node_value_to_child(v: NodeValue) -> Child {
    match v {
        NodeValue::Tree(t) => Child::Tree(t),
        NodeValue::Token(t) => Child::Token(t),
        NodeValue::Inline(mut cs) if cs.len() == 1 => cs.pop().unwrap(),
        NodeValue::Inline(cs) => Child::Tree(Tree::new("_ambig", cs)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grammar::{load_grammar, lower};
    use crate::parsers::earley::EarleyParser;

    fn compile(src: &str) -> CompiledGrammar {
        lower(&load_grammar(src, &["start".to_string()], false, false).unwrap())
    }

    /// A token carrying the interned id of terminal `name` in `cg`.
    fn tok(cg: &CompiledGrammar, name: &str, value: &str) -> Token {
        let mut t = Token::new(name, value);
        t.type_id = cg.symbols.id(name).expect("terminal interned");
        t
    }

    // ── #159 guard: the explicit-mode `_ambig` dedup (`node_value_key` keyed in
    //    `DerivsNext`) must ONLY ever collapse BYTE-IDENTICAL derivations, never
    //    structurally-distinct ones. lark-rs intentionally diverges from Python
    //    Lark here: Python's `ForestToParseTree` does not dedup, so its `_ambig`
    //    may repeat byte-identical children; we drop those (they carry zero
    //    information — see ADR-0017 "diverge & document" and `docs/STATUS.md`).
    //    Collapsing a *distinct* derivation would be a real bug; these tests are
    //    the tripwire. DO NOT relax them to make the dedup do more.

    /// The keying function is the dedup's decision procedure: equal keys collapse,
    /// distinct keys survive. Pin both directions directly on `node_value_key`.
    #[test]
    fn node_value_key_separates_distinct_collapses_identical() {
        let leaf = |data: &str| Child::Tree(Tree::new(data, vec![]));
        // Two byte-identical trees → identical keys (these are the ONLY thing the
        // dedup is allowed to collapse).
        let a1 = NodeValue::Tree(Tree::new("start", vec![leaf("x"), leaf("x")]));
        let a2 = NodeValue::Tree(Tree::new("start", vec![leaf("x"), leaf("x")]));
        assert_eq!(
            node_value_key(&a1),
            node_value_key(&a2),
            "byte-identical derivations must key equal (so they collapse)"
        );

        // Same node name, DIFFERENT child structure → distinct keys (must survive).
        let b = NodeValue::Tree(Tree::new("start", vec![leaf("y")]));
        assert_ne!(
            node_value_key(&a1),
            node_value_key(&b),
            "structurally-distinct derivations must key apart (never collapse)"
        );

        // Same shape, DIFFERENT kept token value → distinct keys (must survive).
        // `node_value_key` keys tokens by `type_` + `value` (not the interned id).
        let c = NodeValue::Tree(Tree::new("n", vec![Child::Token(Token::new("A", "a"))]));
        let d = NodeValue::Tree(Tree::new("n", vec![Child::Token(Token::new("A", "b"))]));
        assert_ne!(
            node_value_key(&c),
            node_value_key(&d),
            "derivations differing only in a kept token value must key apart"
        );
    }

    /// End-to-end tripwire: a grammar whose ambiguity yields two
    /// STRUCTURALLY-DISTINCT derivations (`start(x x)` vs `start(y(A A))`) must
    /// keep BOTH `_ambig` alternatives — the dedup must not over-merge them.
    #[test]
    fn explicit_keeps_structurally_distinct_ambig_alternatives() {
        let cg = compile("start: x x | y\nx: A\ny: A A\nA: \"a\"\n");
        let p = EarleyParser::new(cg.clone());
        let parsed = p
            .parse(
                &[tok(&cg, "A", "a"), tok(&cg, "A", "a")],
                Some("start"),
                false,
            )
            .expect("parses");
        let tree = parsed.as_tree().expect("root is a tree");
        assert_eq!(
            tree.data, "_ambig",
            "the two readings are genuinely ambiguous"
        );
        // Collect each alternative's shape (its sole child's `data`).
        let mut shapes: Vec<&str> = tree
            .children
            .iter()
            .filter_map(Child::as_tree)
            .flat_map(|alt: &Tree| alt.children.iter().filter_map(Child::as_tree))
            .map(|t| t.data.as_str())
            .collect();
        shapes.sort_unstable();
        shapes.dedup();
        assert_eq!(
            shapes,
            vec!["x", "y"],
            "both distinct derivations (x x and y(A A)) must survive the dedup, got {:?}",
            tree.pretty(0)
        );
    }

    /// End-to-end pin of the #159 *current* (intentional) behavior: when every
    /// derivation is BYTE-IDENTICAL (the distinguishing tokens are filtered out),
    /// the dedup collapses them to a single tree — no `_ambig`. Python Lark keeps
    /// the duplicates; we diverge by design (ADR-0017). This is the behavior the
    /// architect verdict says to KEEP; if it ever changes, that is a real decision,
    /// not an accident.
    #[test]
    fn explicit_collapses_byte_identical_ambig_alternatives() {
        // The issue's repro: `start: "x" start | start "x" | "x"` on "xxx". The
        // `"x"` tokens are filtered, so all derivations assemble byte-identically.
        let cg = compile("start: \"x\" start | start \"x\" | \"x\"\n");
        let p = EarleyParser::new(cg.clone());
        // The `"x"` literal lowers to an anonymous string terminal; resolve its
        // interned name by its pattern value rather than guessing the spelling.
        let term = cg
            .terminals
            .iter()
            .find(|t| matches!(&t.pattern, crate::grammar::terminal::Pattern::Str(s) if s.value == "x"))
            .expect("the \"x\" literal interned as a terminal")
            .name
            .clone();
        let input: Vec<Token> = (0..3).map(|_| tok(&cg, &term, "x")).collect();
        let parsed = p.parse(&input, Some("start"), false).expect("parses");
        let tree = parsed.as_tree().expect("root is a tree");
        assert_ne!(
            tree.data,
            "_ambig",
            "byte-identical derivations collapse to a single tree (no _ambig); got {}",
            tree.pretty(0)
        );
        assert_eq!(tree.data, "start");
        // No `_ambig` anywhere in the collapsed result.
        assert!(
            tree.iter_subtrees().all(|t| t.data != "_ambig"),
            "no nested _ambig should survive the collapse; got {}",
            tree.pretty(0)
        );
    }
}
