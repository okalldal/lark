//! Deterministic work counters for the profile-first perf discipline (#54/#55/#56).
//!
//! `BENCH.md` is emphatic that the wall-clock bench is a *recorded trend, not a
//! gate* — shared-runner timing is too noisy to enforce, and a flaky red perf
//! gate gets muted. A super-linearity **regression test** therefore cannot key on
//! time. The noise-free analog these counters provide is an instrumented
//! copy/scan counter: assert *flat per-byte scaling* of a deterministic count and
//! you have something that can actually gate (`tests/test_earley_scaling.rs`).
//!
//! The counters are compiled to genuine no-ops unless the `perf-counters` feature
//! is on, so the normal build and the hot parse path are untouched (the increments
//! sit inside the Earley completer scan and the explicit forest walk). Enable them
//! with `--features perf-counters`; the scaling test self-gates on the same `cfg`.
//!
//! Three counters, mapping onto the two candidate culprits tracked in #56:
//!
//! * [`completer_scan_steps`] (Arm 1) — every item the Earley completer examines
//!   when it looks up an origin column's waiters (`predict_and_complete`). With the
//!   per-column `waiting` index this is the *named* "completer rescans the origin
//!   column" cost; the index makes it flat per byte on realistic shapes (the fix).
//! * [`explicit_prefix_copies`] (Arm 2, the *named* suspect) — every
//!   [`NodeValue`](crate::parsers) copied by `expand_packed`'s cartesian-product
//!   loop (`l = list.clone()`). This counter **disproves** the issue's guess: it
//!   stays *linear* even on a transparent left-recursive helper.
//! * [`explicit_node_children`] (Arm 2, the *real* cost) — every materialized
//!   derivation-value child built per symbol node in `symbol_derivations`. This is
//!   where the explicit walk is genuinely O(n²) on a transparent left-recursive
//!   helper (Inlines of size 1,2,…,n); the streaming fix that would flatten it is a
//!   tracked follow-up (the explicit analog of #55's resolve-mode fix).

#[cfg(feature = "perf-counters")]
mod imp {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    static COMPLETER_SCAN_STEPS: AtomicU64 = AtomicU64::new(0);
    static EXPLICIT_PREFIX_COPIES: AtomicU64 = AtomicU64::new(0);
    static EXPLICIT_NODE_CHILDREN: AtomicU64 = AtomicU64::new(0);
    static FOREST_NODES: AtomicU64 = AtomicU64::new(0);
    static LEO_DISABLED: AtomicBool = AtomicBool::new(false);

    #[inline]
    pub fn add_completer_scan_steps(n: u64) {
        COMPLETER_SCAN_STEPS.fetch_add(n, Ordering::Relaxed);
    }

    #[inline]
    pub fn add_explicit_prefix_copies(n: u64) {
        EXPLICIT_PREFIX_COPIES.fetch_add(n, Ordering::Relaxed);
    }

    #[inline]
    pub fn add_explicit_node_children(n: u64) {
        EXPLICIT_NODE_CHILDREN.fetch_add(n, Ordering::Relaxed);
    }

    /// Count one SPPF node creation. This is the mode-neutral size metric used to
    /// prove the Joop-Leo win (#58): the forest is O(n²) nodes on right recursion
    /// without Leo and O(n) with it — a comparison the scan counter alone cannot
    /// make (Leo zeroes the scan by skipping the cascade, but the question is
    /// whether *total* forest work is now linear).
    #[inline]
    pub fn add_forest_node() {
        FOREST_NODES.fetch_add(1, Ordering::Relaxed);
    }

    /// Zero every counter. Call before the workload you want to measure.
    pub fn reset() {
        COMPLETER_SCAN_STEPS.store(0, Ordering::Relaxed);
        EXPLICIT_PREFIX_COPIES.store(0, Ordering::Relaxed);
        EXPLICIT_NODE_CHILDREN.store(0, Ordering::Relaxed);
        FOREST_NODES.store(0, Ordering::Relaxed);
    }

    pub fn completer_scan_steps() -> u64 {
        COMPLETER_SCAN_STEPS.load(Ordering::Relaxed)
    }

    pub fn explicit_prefix_copies() -> u64 {
        EXPLICIT_PREFIX_COPIES.load(Ordering::Relaxed)
    }

    pub fn explicit_node_children() -> u64 {
        EXPLICIT_NODE_CHILDREN.load(Ordering::Relaxed)
    }

    pub fn forest_nodes() -> u64 {
        FOREST_NODES.load(Ordering::Relaxed)
    }

    /// Turn the Joop-Leo optimization off (`true`) or on (`false`). Lets a
    /// benchmark/test measure the *same* engine with and without Leo, so the
    /// before/after comparison is apples-to-apples (the "prove it was super-linear
    /// without the fix" half of #58). Production never touches this — the toggle
    /// only exists under `perf-counters`.
    pub fn set_leo_disabled(disabled: bool) {
        LEO_DISABLED.store(disabled, Ordering::Relaxed);
    }

    #[inline]
    pub fn leo_disabled() -> bool {
        LEO_DISABLED.load(Ordering::Relaxed)
    }

    /// Whether the counters are live (the `perf-counters` feature is enabled).
    pub const ENABLED: bool = true;
}

#[cfg(not(feature = "perf-counters"))]
mod imp {
    #[inline]
    pub fn add_completer_scan_steps(_n: u64) {}

    #[inline]
    pub fn add_explicit_prefix_copies(_n: u64) {}

    #[inline]
    pub fn add_explicit_node_children(_n: u64) {}

    #[inline]
    pub fn add_forest_node() {}

    pub fn reset() {}

    pub fn completer_scan_steps() -> u64 {
        0
    }

    pub fn explicit_prefix_copies() -> u64 {
        0
    }

    pub fn explicit_node_children() -> u64 {
        0
    }

    pub fn forest_nodes() -> u64 {
        0
    }

    pub fn set_leo_disabled(_disabled: bool) {}

    #[inline]
    pub fn leo_disabled() -> bool {
        false
    }

    pub const ENABLED: bool = false;
}

pub use imp::*;
