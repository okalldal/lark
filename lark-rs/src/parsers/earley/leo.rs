//! The Joop-Leo right-recursion optimization and the ε-tail spine reconstruction.
//!
//! [`EarleyParser::is_quasi_complete`], [`EarleyParser::create_leo`],
//! [`EarleyParser::load_leo_paths`], [`EarleyParser::materialize_leo_paths`], and
//! the ε-node helper [`EarleyParser::eps_node`]. The laziness here is load-bearing
//! (`load_leo_paths` is reachability-bounded — eager expansion reintroduces the
//! O(n²) of #61). Split out of the former monolithic `earley.rs` (no logic change).

use std::collections::{HashMap, HashSet};

use crate::grammar::intern::SymbolId;

use super::chart::{Column, Item};
use super::forest::{Forest, ForestRef, NodeKey, Trans};
use super::EarleyParser;

impl EarleyParser {
    /// A reduction path through `item` is taken by Leo only when advancing past
    /// the symbol at the dot leaves the rule *deterministically completable* — i.e.
    /// consuming the recognized symbol either completes the rule (strict right
    /// recursion, `a: X a | X`) or leaves only a **nullable tail** that ε-derives
    /// to completion (`a: X a opt | X` with `opt:` ε-only, the minimal
    /// nullable-tail shape — #64). The topmost item is then non-complete, and the
    /// SPPF spine reconstruction threads the tail's ε-completions through it
    /// (`materialize_leo_paths`), the case upstream Lark never finished
    /// (lark-parser/lark#397).
    ///
    /// **The tail must be ε-ONLY, not merely nullable.** Linearizing collapses the
    /// tail to ε on the Leo shortcut and skips the regular completer's cascade; if
    /// a tail symbol could also match real tokens (an *optional* tail like
    /// `opt: Y |`), that non-empty derivation would become unreachable and valid
    /// input (`a: X a opt | X`, `opt: Y |` on `"xxy"`) would be wrongly rejected.
    /// So `eps_only` (strictly stronger than nullable) is the admission test.
    /// (Python Lark's reference uses `NULLABLE` here, but its Leo completer is dead
    /// code — lark-parser/lark#397 — so it never exercised this distinction; ours
    /// is a live reimplementation and must get it right.)
    ///
    /// The `start_id` guard refuses to special-case a directly self-recursive
    /// start — at the recognized position OR anywhere in the tail (matches the
    /// original reference guard), falling back to the regular completer there.
    pub(crate) fn is_quasi_complete(&self, item: &Item, start_id: SymbolId) -> bool {
        let expansion = &self.grammar.rules[item.rule].expansion;
        let origin = self.grammar.rules[item.rule].origin;
        // Refuse a directly self-recursive start at the recognized position.
        if origin == start_id && expansion[item.dot] == start_id {
            return false;
        }
        // Walk the tail after the recognized symbol (positions `item.dot + 1 ..`).
        // Each must be ε-only (so the rule completes with no further input AND the
        // Leo collapse cannot drop a real derivation), and none may re-enter the
        // recursive start.
        for &sym in &expansion[item.dot + 1..] {
            if !self.eps_only[sym.index()] {
                return false;
            }
            if origin == start_id && sym == start_id {
                return false;
            }
        }
        true
    }

    /// Memoize (or extend) the Joop-Leo transitive chain for completing
    /// `recognized` at column `start`. Walks the deterministic reduction path
    /// upward — each step requires a *unique* quasi-complete originator — recording
    /// one [`Trans`] per level, all sharing the topmost item the chain collapses
    /// to. A no-op if the chain already exists or the path is not deterministic.
    pub(crate) fn create_leo(
        &self,
        recognized: SymbolId,
        start: usize,
        columns: &[Column],
        transitives: &mut [HashMap<SymbolId, usize>],
        trans_arena: &mut Vec<Trans>,
        start_id: SymbolId,
    ) {
        // Benchmark/test affordance (perf-counters only): with Leo disabled, never
        // build transitives, so the completer falls back to the regular cascade —
        // the "without the fix" arm of the #58 before/after proof.
        if crate::perf::leo_disabled() {
            return;
        }
        if transitives[start].contains_key(&recognized) {
            return;
        }
        let mut visited: HashSet<(usize, SymbolId)> = HashSet::new();
        // Levels bottom→top: (recognized, key_start, originator item).
        let mut to_create: Vec<(SymbolId, usize, Item)> = Vec::new();
        let mut rec = recognized;
        let mut col = start;
        // The transitive already present at the top of the walk (if it stopped by
        // meeting an existing chain), and the topmost item the chain collapses to.
        let mut parent: Option<usize> = None;
        let mut top: Option<Item> = None;
        loop {
            if let Some(&t) = transitives[col].get(&rec) {
                parent = Some(t);
                top = Some(trans_arena[t].top);
                break;
            }
            if !visited.insert((col, rec)) {
                break; // cycle guard
            }
            let waiters = columns[col].waiting_on(rec);
            if waiters.len() != 1 {
                break;
            }
            let o = columns[col].items[waiters[0]];
            if !self.is_quasi_complete(&o, start_id) {
                break;
            }
            to_create.push((rec, col, o));
            // The topmost item the chain reaches is the *completed* form of the
            // highest unique originator seen so far — its dot at the end of the
            // rule (its `node` is unused downstream). For strict right recursion
            // `o.dot + 1` already equals the length; with a nullable tail (#64) the
            // tail's ε-completions are consumed here so `top` lands on the
            // completed `Sym(origin)` node, exactly as the regular completer would
            // after held-ε-advancing through the tail. `materialize_leo_paths`
            // rebuilds the skipped intermediate + ε-tail spine lazily.
            top = Some(Item {
                rule: o.rule,
                dot: self.grammar.rules[o.rule].expansion.len(),
                origin: o.origin,
                node: ForestRef::None,
            });
            rec = self.grammar.rules[o.rule].origin;
            col = o.origin;
        }
        let Some(top) = top else { return };
        // Build top→bottom so each new level's `parent` is already created.
        for &(rec, key_start, o) in to_create.iter().rev() {
            let tid = trans_arena.len();
            trans_arena.push(Trans {
                recognized: rec,
                key_start,
                red: o,
                parent,
                top,
            });
            transitives[key_start].insert(rec, tid);
            parent = Some(tid);
        }
    }

    /// Expand every deferred Joop-Leo path into real packed families. For each
    /// recorded `(transitive chain, bottom node, end)` on a topmost node, walk the
    /// chain top→bottom rebuilding one symbol node per skipped reduction level —
    /// the same spine the non-Leo completer would have built, but materialized once
    /// here (O(n)) instead of O(n²) times during the parse.
    pub(crate) fn load_leo_paths(&self, forest: &mut Forest, trans_arena: &[Trans], root: usize) {
        // Reachability DFS from the root: expand only the paths the forest->tree
        // walk will actually read. A Leo completion records a top node at EVERY
        // column, so `start` over every prefix carries a path; expanding them all
        // would rebuild a length-c spine for every column c -- back to O(n^2).
        // Touching only nodes reachable from the real root keeps reconstruction
        // O(n), mirroring Python's `load_paths`-on-`children` laziness.
        let mut stack = vec![root];
        let mut visited: HashSet<usize> = HashSet::new();
        while let Some(id) = stack.pop() {
            if !visited.insert(id) {
                continue;
            }
            self.materialize_leo_paths(forest, trans_arena, id);
            // Descend into the now-complete family children so their own paths, if
            // any, expand too — but only along edges reachable from the root.
            for p in &forest.nodes[id].families {
                for r in [p.left, p.right] {
                    if let ForestRef::Node(c) = r {
                        stack.push(c);
                    }
                }
            }
        }
    }

    /// Expand the deferred Joop-Leo paths recorded on a *single* node into real
    /// packed families (the per-node body of [`load_leo_paths`]). Each recorded
    /// `(transitive chain, bottom node, end)` is walked top→bottom, rebuilding one
    /// symbol node per skipped reduction level. The chain's topmost level lands on
    /// this node itself (the path owner), so afterwards the node carries its
    /// derivations as families. The node's `paths` are taken, so a later call (or
    /// the reachability DFS) never re-expands them.
    ///
    /// Used both by `load_leo_paths`'s DFS and by the dynamic scanner's `%ignore`
    /// carry-over, which must materialize a carried completed item's Leo-deferred
    /// derivation before copying its families to the carried-span node.
    pub(crate) fn materialize_leo_paths(
        &self,
        forest: &mut Forest,
        trans_arena: &[Trans],
        id: usize,
    ) {
        let paths = std::mem::take(&mut forest.nodes[id].paths);
        for (bottom_trans, _bottom_node, end) in paths {
            // Collect the chain bottom→top via `parent`, then walk top→bottom.
            // Every node is addressed by its global `(key, start, end)` identity,
            // so the top level reuses this node, the bottom reuses the chart's
            // already-built completed node, and any intermediate symbol nodes
            // merge with whatever the chart already has at that span — exactly
            // the merge `ambiguity='resolve'` needs.
            let mut chain = Vec::new();
            let mut cur = Some(bottom_trans);
            while let Some(t) = cur {
                chain.push(t);
                cur = trans_arena[t].parent;
            }
            for &t in chain.iter().rev() {
                let tr = trans_arena[t];
                // Build the node for the originator advanced past the recognized
                // symbol: `[red.rule → … recognized • γ]`. With strict right
                // recursion γ is empty, so this is already the completed
                // `Sym(origin)`; with a nullable tail it is the intermediate
                // `Inter(red.rule, red.dot + 1)`.
                let mut prev_node = forest.get_or_create(
                    self.node_key(tr.red.rule, tr.red.dot + 1),
                    tr.red.origin,
                    end,
                );
                let right = forest.get_or_create(NodeKey::Sym(tr.recognized), tr.key_start, end);
                forest.add_family(
                    prev_node,
                    tr.red.rule,
                    self.grammar.rules[tr.red.rule].order,
                    tr.red.node,
                    ForestRef::Node(right),
                    tr.red.dot,
                );
                // Thread the nullable tail (#64): each remaining symbol after the
                // recognized one is nullable (guaranteed by `is_quasi_complete`),
                // so it ε-completes at `end`. Advance one binarized level per tail
                // position, each carrying the tail symbol's ε-node as the right
                // child — exactly the spine the regular completer's held-ε
                // advancing would have built, but materialized once here. The empty
                // span `(end, end)` reuses any ε-node the chart already built.
                let expansion = &self.grammar.rules[tr.red.rule].expansion;
                for pos in (tr.red.dot + 1)..expansion.len() {
                    let tail_sym = expansion[pos];
                    let mut building = HashSet::new();
                    let eps = self.eps_node(forest, tail_sym, end, &mut building);
                    let next_node = forest.get_or_create(
                        self.node_key(tr.red.rule, pos + 1),
                        tr.red.origin,
                        end,
                    );
                    forest.add_family(
                        next_node,
                        tr.red.rule,
                        self.grammar.rules[tr.red.rule].order,
                        ForestRef::Node(prev_node),
                        ForestRef::Node(eps),
                        pos,
                    );
                    prev_node = next_node;
                }
            }
        }
    }

    /// Build (or reuse) the SPPF node for a *nullable* symbol's ε-derivation at the
    /// empty span `(col, col)`, returning its node id. Mirrors the ε-nodes the
    /// regular completer materializes for a held ε-completion: a node keyed by its
    /// global `(Sym(sym), col, col)` identity, carrying one family per fully
    /// nullable rule of `sym`, each binarized over that rule's own (nullable)
    /// children. Used by the Joop-Leo nullable-tail spine reconstruction (#64) to
    /// thread the skipped ε-tail. Idempotent: a node whose families are already
    /// present is returned untouched (it merges with the chart's own ε-node when
    /// the chart built one), and recursion terminates because every ε-child spans
    /// the same empty column and is dedup-guarded by the global index.
    ///
    /// `building` is a per-reconstruction "already descended into" set guarding a
    /// nullable ε-cycle (e.g. `a: b |`, `b: a`): a symbol already entered returns
    /// its node without re-descending, so the reconstruction terminates. It is
    /// monotonic (never un-set) — safe because every symbol this builds ends with
    /// non-empty `families`, so any later visit short-circuits at the
    /// `families.is_empty()` check above rather than relying on the guard. Such
    /// mutually-ε-recursive nullable symbols are outside the linearization target;
    /// the regular completer still builds their canonical ε-node, which this node
    /// merges into by global identity. A node keeps a single ε family (the lowest
    /// `rule.order`, see [`SymbolNode::eps_family`]), so re-adding the same rule's ε
    /// derivation is a no-op (equal order keeps the incumbent).
    pub(crate) fn eps_node(
        &self,
        forest: &mut Forest,
        sym: SymbolId,
        col: usize,
        building: &mut HashSet<SymbolId>,
    ) -> usize {
        let id = forest.get_or_create(NodeKey::Sym(sym), col, col);
        // Already populated (by the chart or an earlier call): reuse as-is.
        if !forest.nodes[id].families.is_empty() {
            return id;
        }
        // ε-cycle guard: this symbol is already being built further up the stack.
        if !building.insert(sym) {
            return id;
        }
        // Build a family for each fully nullable production of `sym`.
        let Some(prods) = self.rules_by_origin.get(&sym) else {
            return id;
        };
        let prods = prods.clone();
        for ri in prods {
            let expansion = &self.grammar.rules[ri].expansion;
            // Only productions that derive ε wholesale (every symbol nullable)
            // contribute an ε-derivation.
            if !expansion.iter().all(|s| self.nullable[s.index()]) {
                continue;
            }
            if expansion.is_empty() {
                // The empty production: Scott's (None, None) ε family.
                forest.add_family(
                    id,
                    ri,
                    self.grammar.rules[ri].order,
                    ForestRef::None,
                    ForestRef::None,
                    0,
                );
            } else {
                // A non-empty all-nullable production: binarize its ε-children
                // left to right, mirroring the regular completer's spine.
                let mut left = ForestRef::None;
                let len = expansion.len();
                for pos in 0..len {
                    let child = self.eps_node(forest, expansion[pos], col, building);
                    let key = self.node_key(ri, pos + 1);
                    let node = forest.get_or_create(key, col, col);
                    forest.add_family(
                        node,
                        ri,
                        self.grammar.rules[ri].order,
                        left,
                        ForestRef::Node(child),
                        pos,
                    );
                    left = ForestRef::Node(node);
                }
            }
        }
        id
    }
}
