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
//!
//! Two more counters back the **cyclic explicit-Earley re-assembly** gate (#518).
//! `ambiguity='explicit'` over a *cyclic* (nullable+recursive) grammar disables the
//! per-symbol `deriv_memo`/`memo` for cycle nodes and governs them via the
//! per-packed-node `packed_cache` (Python's `_cache` model, #348). `packed_cache`
//! bounds re-*descent*, but a cyclic symbol node's derivation list is still
//! re-`assemble`d on each reach (its `deriv_memo` is never written). Cyclic
//! ambiguous grammars have an inherently exponential *distinct-derivation* count
//! (the true answer, not an artifact — `1,1,2,8,48,352` for `z: | "b" z | z z`), so
//! the gate keys on *per-materialized-derivation* work, never raw total work
//! (BENCH.md / §2.5):
//!
//! * [`explicit_assemble_children`] — every child *slot* the explicit walk feeds to
//!   `TreeOutputBuilder::assemble` while re-building a packed node's derivation
//!   values (`DerivsNext`). This is the re-assembly work the cyclic path repeats on
//!   each reach of a cycle node. On its own it grows with the (exponential)
//!   derivation count, so it is only meaningful *per derivation*.
//! * [`explicit_derivations`] — every materialized derivation value the walk admits
//!   into a node's deduped list (`DerivsNext`'s `push_deduped`). The denominator: the
//!   count of materialized output derivations. A flat `assemble_children /
//!   derivations` envelope says each materialized derivation costs a bounded amount
//!   of re-assembly regardless of how many times its cyclic node is reached; a future
//!   super-linearity in the re-assembly path (e.g. dropping `packed_cache`, or
//!   re-assembling a memoizable subtree per reach) makes the ratio climb and trips the
//!   gate (`tests/test_earley_scaling.rs`, the cyclic arm).
//!
//! A fourth counter backs the CYK scaling gate (#87):
//!
//! * [`cyk_table_steps`] — every `(split, left-nt, right-nt)` combination the CYK
//!   DP examines while filling its triangular table. CYK is inherently
//!   `O(n³ · |grammar|)`; asserting this count scales cubically (flat per `n³`)
//!   over a size sweep catches an accidental complexity regression in the CNF
//!   conversion or the DP, mirroring the Earley methodology
//!   (`tests/test_cyk_scaling.rs`).
//!
//! A sixth counter backs the **dense-DFA build-cost** gate (`docs/LEXER_DFA_PLAN.md`,
//! the determinization-blowup risk):
//!
//! * [`dense_build_bytes`] — the determinized heap size (`dense::DFA::memory_usage`,
//!   a deterministic proxy for the state count × stride) of each `dense::DFA` the
//!   lookaround-lowering `DfaScanner` builds (the combined base engines plus each
//!   guard body). Determinization is the cost the **L5 bake** pays (`to_bytes` needs a
//!   fully-determinized dense DFA), and a lowering that blows it up — parity
//!   duplication, spliced branches, an interacting union of per-state contextual
//!   scanners — would show as a super-linear climb in this count. Asserting it stays
//!   flat per terminal (and per guard width) over a size sweep
//!   (`tests/test_lexer_dfa_build_scaling.rs`) is the deterministic gate, the codegen-
//!   time analog of the Earley/CYK scaling gates (paid at standalone generation, not
//!   every runtime load).
//!
//! A seventh counter backs the **grammar-build cross-product** gate (#404, H6-7):
//!
//! * [`expansion_alts`] — every intermediate alternative the EBNF rule-body
//!   compiler materializes in `compile_expansion`'s per-position cartesian fold
//!   (the running `acc` product, summed across fold steps). A chain of `k`
//!   duplicate-arm inline groups (`(X|X) (X|X) … (X|X)`) that folds *without*
//!   per-step dedup materializes the full `m^k` product before collapsing — a
//!   `2^k` build blowup. Folding with `concat_alts_dedup` bounds the running set
//!   to the *distinct* alternatives at each prefix length, so this count stays
//!   flat in `k`. Asserting that (`tests/test_grammar_build_scaling.rs`) is the
//!   deterministic gate — the grammar-build analog of the Earley/CYK/lexer
//!   scaling gates, paid at load, never wall-clock.

#[cfg(feature = "perf-counters")]
mod imp {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    static COMPLETER_SCAN_STEPS: AtomicU64 = AtomicU64::new(0);
    static EXPLICIT_PREFIX_COPIES: AtomicU64 = AtomicU64::new(0);
    static EXPLICIT_NODE_CHILDREN: AtomicU64 = AtomicU64::new(0);
    static EXPLICIT_ASSEMBLE_CHILDREN: AtomicU64 = AtomicU64::new(0);
    static EXPLICIT_DERIVATIONS: AtomicU64 = AtomicU64::new(0);
    static FOREST_NODES: AtomicU64 = AtomicU64::new(0);
    static CYK_TABLE_STEPS: AtomicU64 = AtomicU64::new(0);
    static LEXER_SCAN_STEPS: AtomicU64 = AtomicU64::new(0);
    static DENSE_BUILD_BYTES: AtomicU64 = AtomicU64::new(0);
    static EXPANSION_ALTS: AtomicU64 = AtomicU64::new(0);
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

    /// Count the child slots fed to one `TreeOutputBuilder::assemble` call in the
    /// explicit walk's per-packed-node re-assembly (`DerivsNext`) — the re-assembly
    /// work a cyclic node repeats on each reach (#518). Only meaningful divided by
    /// [`explicit_derivations`]; on its own it tracks the exponential derivation count.
    #[inline]
    pub fn add_explicit_assemble_children(n: u64) {
        EXPLICIT_ASSEMBLE_CHILDREN.fetch_add(n, Ordering::Relaxed);
    }

    /// Count one materialized derivation value admitted into a node's deduped list
    /// (`DerivsNext`'s `push_deduped`) — the denominator for the #518 per-derivation
    /// re-assembly envelope. Tracks the true (exponential) distinct-derivation count
    /// of a cyclic explicit grammar (`1,1,2,8,48,352` for `z: | "b" z | z z`).
    #[inline]
    pub fn add_explicit_derivation() {
        EXPLICIT_DERIVATIONS.fetch_add(1, Ordering::Relaxed);
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

    /// Count one CYK table-fill combination step: every `(split, left-nt,
    /// right-nt)` triple the DP examines when filling a span cell. This is the
    /// dominant cost of the `O(n³ · |grammar|)` table fill, so asserting it scales
    /// cubically (flat per `n³`) catches an accidental complexity regression in the
    /// CNF conversion or the DP — the CYK analog of the Earley completer-scan gate.
    #[inline]
    pub fn add_cyk_table_steps(n: u64) {
        CYK_TABLE_STEPS.fetch_add(n, Ordering::Relaxed);
    }

    /// Count lexer scan work: per per-position match attempt, the number of input
    /// bytes the (unanchored) regql search had to skip past `pos` before it found a
    /// candidate or gave up, plus one for the attempt itself. On a correctly
    /// *anchored* scanner this is ~1 per attempt (the search only looks at `pos`), so
    /// the total is linear in the token count. On an *unanchored* scanner a terminal
    /// that fails at `pos` makes the engine forward-scan toward the next possible
    /// match — O(remaining input) per position — so a low-rank lookaround terminal
    /// tried at every token boundary makes the total O(n²). Asserting this count
    /// stays flat per byte (`tests/test_lexer_scaling.rs`) is the deterministic gate
    /// for that pathology.
    #[inline]
    pub fn add_lexer_scan_steps(n: u64) {
        LEXER_SCAN_STEPS.fetch_add(n, Ordering::Relaxed);
    }

    /// Count the determinized heap size (`dense::DFA::memory_usage`) of one
    /// `dense::DFA` built while constructing a lookaround-lowering `DfaScanner` (a
    /// combined base engine or a guard body). Summing this across a scanner build is its
    /// determinization cost — the work the L5 bake pays — so asserting it stays flat per
    /// terminal/width catches a lowering that blows up dense-DFA construction
    /// (`tests/test_lexer_dfa_build_scaling.rs`).
    #[inline]
    pub fn add_dense_build_bytes(n: u64) {
        DENSE_BUILD_BYTES.fetch_add(n, Ordering::Relaxed);
    }

    /// Count intermediate alternatives materialized by `compile_expansion`'s
    /// per-position cartesian fold — the size of the running `acc` product after
    /// each fold step, summed over the rule body. A duplicate-arm inline-group
    /// chain (`(X|X) (X|X) …`) folded without per-step dedup makes this `2^k`; the
    /// deduping fold keeps it flat in `k` (#404, H6-7). The grammar-build analog of
    /// the Earley/CYK/lexer scaling counters; gated by
    /// `tests/test_grammar_build_scaling.rs`.
    #[inline]
    pub fn add_expansion_alts(n: u64) {
        EXPANSION_ALTS.fetch_add(n, Ordering::Relaxed);
    }

    /// Zero every counter. Call before the workload you want to measure.
    pub fn reset() {
        COMPLETER_SCAN_STEPS.store(0, Ordering::Relaxed);
        EXPLICIT_PREFIX_COPIES.store(0, Ordering::Relaxed);
        EXPLICIT_NODE_CHILDREN.store(0, Ordering::Relaxed);
        EXPLICIT_ASSEMBLE_CHILDREN.store(0, Ordering::Relaxed);
        EXPLICIT_DERIVATIONS.store(0, Ordering::Relaxed);
        FOREST_NODES.store(0, Ordering::Relaxed);
        CYK_TABLE_STEPS.store(0, Ordering::Relaxed);
        LEXER_SCAN_STEPS.store(0, Ordering::Relaxed);
        DENSE_BUILD_BYTES.store(0, Ordering::Relaxed);
        EXPANSION_ALTS.store(0, Ordering::Relaxed);
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

    pub fn explicit_assemble_children() -> u64 {
        EXPLICIT_ASSEMBLE_CHILDREN.load(Ordering::Relaxed)
    }

    pub fn explicit_derivations() -> u64 {
        EXPLICIT_DERIVATIONS.load(Ordering::Relaxed)
    }

    pub fn forest_nodes() -> u64 {
        FOREST_NODES.load(Ordering::Relaxed)
    }

    pub fn cyk_table_steps() -> u64 {
        CYK_TABLE_STEPS.load(Ordering::Relaxed)
    }

    pub fn lexer_scan_steps() -> u64 {
        LEXER_SCAN_STEPS.load(Ordering::Relaxed)
    }

    pub fn dense_build_bytes() -> u64 {
        DENSE_BUILD_BYTES.load(Ordering::Relaxed)
    }

    pub fn expansion_alts() -> u64 {
        EXPANSION_ALTS.load(Ordering::Relaxed)
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
    pub fn add_explicit_assemble_children(_n: u64) {}

    #[inline]
    pub fn add_explicit_derivation() {}

    #[inline]
    pub fn add_forest_node() {}

    #[inline]
    pub fn add_cyk_table_steps(_n: u64) {}

    #[inline]
    pub fn add_lexer_scan_steps(_n: u64) {}

    #[inline]
    pub fn add_dense_build_bytes(_n: u64) {}

    #[inline]
    pub fn add_expansion_alts(_n: u64) {}

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

    pub fn explicit_assemble_children() -> u64 {
        0
    }

    pub fn explicit_derivations() -> u64 {
        0
    }

    pub fn forest_nodes() -> u64 {
        0
    }

    pub fn cyk_table_steps() -> u64 {
        0
    }

    pub fn lexer_scan_steps() -> u64 {
        0
    }

    pub fn dense_build_bytes() -> u64 {
        0
    }

    pub fn expansion_alts() -> u64 {
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
