//! Earley chart items and per-column work-sets.
//!
//! [`Item`] (a dotted rule + origin + its SPPF node), [`Column`] (the set `R` of
//! complete / non-terminal-expecting items, indexed by the symbol each waits on),
//! [`ScanSet`] (the scan buffer `Q`), and [`Delayed`] (the dynamic scanner's
//! `delayed_matches`). Pure data structures shared by the recognizer and the
//! dynamic lexer — split out of the former monolithic `earley.rs` (no logic change).

use std::collections::{HashMap, HashSet};

use crate::grammar::intern::SymbolId;
use crate::tree::Token;

use super::forest::ForestRef;

// ─── Chart items ──────────────────────────────────────────────────────────────

/// An Earley item: a dotted rule, the column where the rule began, and the SPPF
/// node for the symbol/intermediate it has built so far (`None` before the first
/// symbol is consumed — Scott's `w`).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct Item {
    pub(crate) rule: usize,
    pub(crate) dot: usize,
    pub(crate) origin: usize,
    pub(crate) node: ForestRef,
}

/// One Earley chart column: items that are complete or expect a non-terminal (the
/// set `R`). Terminal-expecting items live in the separate scan buffer. Ordered +
/// de-duplicated; insertion order is load-bearing for resolve tie-breaks.
#[derive(Default)]
pub(crate) struct Column {
    pub(crate) items: Vec<Item>,
    seen: HashSet<(usize, usize, usize)>,
    /// Index from the non-terminal an item expects next → positions in `items` of
    /// the items waiting on it. The completer reads only the relevant bucket
    /// instead of rescanning the whole column with a linear `.filter`. That rescan
    /// is the "completer rescans the origin column" cost #54 suspected and #56
    /// confirmed: later columns accumulate O(n) completed items, so an unindexed
    /// scan is O(column) per completion → super-linear on a right-recursive grammar
    /// (`a: X a | X`), even though only O(1) items per column actually wait on any
    /// given symbol. This index is the Joop-Leo-free cure (the reference's Leo
    /// transitives are dead code; this fixes the same asymptotics without them).
    waiting: HashMap<SymbolId, Vec<usize>>,
}

impl Column {
    pub(crate) fn new() -> Self {
        Column::default()
    }

    /// Add `item` unless an equal one (same rule, dot, origin) is already present;
    /// returns whether it was newly inserted. `expected` is the non-terminal `item`
    /// expects next (`None` if it is complete), used to index it for the completer.
    pub(crate) fn add(&mut self, item: Item, expected: Option<SymbolId>) -> bool {
        if self.seen.insert((item.rule, item.dot, item.origin)) {
            let idx = self.items.len();
            self.items.push(item);
            if let Some(sym) = expected {
                self.waiting.entry(sym).or_default().push(idx);
            }
            true
        } else {
            false
        }
    }

    /// Positions in `items` of the items waiting on `sym`, in insertion order
    /// (load-bearing for resolve tie-breaks). Empty if none — an O(1) lookup that
    /// replaces the O(column) rescan.
    pub(crate) fn waiting_on(&self, sym: SymbolId) -> &[usize] {
        self.waiting.get(&sym).map(Vec::as_slice).unwrap_or(&[])
    }
}

/// The scan buffer (`Q` in Scott's paper): terminal-expecting items for the
/// current column. Ordered + de-duplicated.
#[derive(Default)]
pub(crate) struct ScanSet {
    pub(crate) items: Vec<Item>,
    seen: HashSet<(usize, usize, usize)>,
}

impl ScanSet {
    pub(crate) fn new() -> Self {
        ScanSet::default()
    }
    pub(crate) fn add(&mut self, item: Item) {
        if self.seen.insert((item.rule, item.dot, item.origin)) {
            self.items.push(item);
        }
    }
}

/// A match queued by the **dynamic** scanner, to be acted on at the input step
/// where it ends (Scott's `delayed_matches`). A token advances the item that
/// predicted it; an ignored span instead carries the item over unchanged.
pub(crate) enum Delayed {
    Tok { item: Item, token: Token },
    Carry { item: Item },
}
