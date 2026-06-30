//! The recurse-overshare **audit shadow** mechanism (RC7/#272, amends ADR-0013),
//! concentrated behind one [`AuditShadow`] type the compiler owns.
//!
//! ## The problem it solves
//!
//! The load-bearing EBNF helper *sharing* ([`GrammarCompiler::recurse_cache`])
//! keys its recurse helpers on the *compiled arms*, which collapse a single-symbol
//! group wrapper — so `r0*` and `(r0)*` share **one** helper. Python Lark keys on
//! the inner `expr` **Tree** (`EBNF_to_BNF._add_recurse_rule`), mints **two**
//! distinct helpers, and rejects `start: r0* | (r0)*` with a reduce/reduce
//! collision. Un-sharing to match regresses the LALR bank 512→482 (the sharing is
//! load-bearing — ADR-0013).
//!
//! ## The mechanism, as one concern
//!
//! Rather than change the cache key or the conflict detector, the loader keeps the
//! real (compiled-arms) sharing **and**, when it detects a real over-share, builds
//! a Python-faithful **audit shadow**: the same grammar re-lowered with recurse
//! helpers keyed on the inner source-AST ([`super::ast::Expr::python_recurse_key`]).
//! The LALR build runs the *real* conflict detector over that shadow's lowering to
//! surface the collision the sharing masks. The shadow only gates the build; it
//! never parses.
//!
//! This type owns the "real vs Python-keyed shadow" duality as one piece of state
//! instead of the four scattered fields it replaces:
//!
//! - the **mode flag** ([`python_keyed`](AuditShadow::python_keyed)) — `false` in
//!   the real (sharing) pass, `true` in the shadow re-lowering;
//! - the **Python-AST-keyed recurse cache** ([`recurse_cache_ast`]), populated only
//!   in the shadow pass, so it never perturbs the real `recurse_cache`;
//! - the **over-share evidence** ([`recurse_cache_origin_key`]) — the inner-AST key
//!   that first minted each *real* helper, so a later real cache hit with a
//!   different inner-AST key is recognized as the over-share;
//! - the resulting **over-share signal** ([`overshare_seen`]) the loader reads to
//!   decide whether to build the shadow at all (and which `imports` propagates
//!   across an `%import` boundary).
//!
//! The two passes are distinguished only by [`python_keyed`](AuditShadow::python_keyed);
//! every branch that used to read the bare `python_keyed_recurse` boolean now goes
//! through a named method on this type.

use super::ebnf::RecurseShareKey;
use std::collections::HashMap;

/// What [`AuditShadow::lookup`] decided for one recurse-helper request.
pub(super) enum RecurseDecision {
    /// A cache hit: reuse this already-minted helper rule name. The over-share
    /// evidence (if any) was already recorded by [`AuditShadow::lookup`].
    Cached(String),
    /// A cache miss: the caller must mint a fresh helper for this request's `arms`,
    /// then record it via [`AuditShadow::record_minted`].
    Mint,
}

/// The recurse-overshare audit concern (RC7/#272, ADR-0013), owned by the
/// [`GrammarCompiler`]. Encapsulates the "real vs Python-keyed shadow" duality:
/// in the real pass it observes the compiled-arms [`recurse_cache`] to detect an
/// over-share; in the shadow pass it *is* the Python-AST-keyed cache that reproduces
/// Python Lark's un-shared helper split.
///
/// **Construction is byte-identical to the prior scattered-field form.** The cache
/// lookups, the over-share predicate, and the import propagation all reproduce the
/// exact same decisions; this type only concentrates the state and its branches.
///
/// [`recurse_cache`]: super::compiler::GrammarCompiler::recurse_cache
/// [`GrammarCompiler`]: super::compiler::GrammarCompiler
#[derive(Default)]
pub(super) struct AuditShadow {
    /// `false` in the real (sharing) pass; `true` while re-lowering the Python-keyed
    /// shadow. When set, [`recurse_helper`](super::ebnf) keys the recurse cache on
    /// the inner expression's **source-AST** structural key
    /// ([`super::ast::Expr::python_recurse_key`]) instead of the compiled arms,
    /// reproducing Python's *un-shared* helper split — so the post-lowering audit can
    /// run the real LALR conflict detector over a Python-faithful shadow grammar and
    /// surface the collision the real (shared) grammar masks, **without** un-sharing
    /// the real `recurse_cache` (the sharing is load-bearing — ADR-0013). The shadow
    /// grammar is build-gating only; it never parses.
    python_keyed: bool,
    /// Audit-only recurse-helper cache keyed on `(inner-AST key, keep_all)`, matching
    /// Python Lark's `EBNF_to_BNF.rules_cache` (keyed on the inner `expr` Tree).
    /// Populated only while [`python_keyed`](Self::python_keyed) is set, so it never
    /// affects the real (compiled-arms-keyed) `recurse_cache`.
    recurse_cache_ast: HashMap<(String, bool), String>,
    /// The inner-AST key that first created each *real* `recurse_cache` entry, keyed
    /// by that entry's [`RecurseShareKey`] (the same filter-out-agnostic share key the
    /// real cache uses, #377). On a later real cache *hit* with a **different**
    /// inner-AST key, the real sharing has collapsed two helpers Python Lark would have
    /// minted distinctly — exactly the RC7/#272 over-share. Keying on the share key
    /// (not the raw arms) keeps this over-share detection aligned with the real cache:
    /// the two sites that now share a helper carry one origin entry, and a later hit
    /// from a distinct inner-AST (`r0*` vs `(r0)*`) still flips the over-share signal.
    /// Tracked only in the real pass.
    recurse_cache_origin_key: HashMap<RecurseShareKey, String>,
    /// Set in the real pass when a `recurse_cache` hit fuses two distinct inner-AST
    /// shapes into one helper (see [`recurse_cache_origin_key`](Self::recurse_cache_origin_key)),
    /// or in [`note_imported_audit`](Self::note_imported_audit) when an imported
    /// grammar carried its own over-share. When `false`, the Python-keyed shadow is
    /// byte-identical to the real grammar's recurse helpers, so the loader skips
    /// building it (no audit needed).
    overshare_seen: bool,
}

impl AuditShadow {
    /// The real (sharing) pass — observes the compiled-arms cache to detect an
    /// over-share, but never re-keys the recurse helpers. The compiler's
    /// [`new`](super::compiler::GrammarCompiler::new) uses this (via `Default`).
    pub(super) fn new() -> Self {
        Self::default()
    }

    /// Put this audit into **shadow** mode: the recurse cache is keyed on the inner
    /// source-AST so helpers split exactly as Python mints them. Set by the loader on
    /// the shadow compiler before re-lowering (`mod.rs`).
    pub(super) fn set_python_keyed(&mut self) {
        self.python_keyed = true;
    }

    /// Whether this is the Python-keyed shadow pass (vs the real sharing pass).
    pub(super) fn python_keyed(&self) -> bool {
        self.python_keyed
    }

    /// Whether a recurse over-share was detected (directly, or propagated from an
    /// imported grammar's own audit). The loader reads this to decide whether to
    /// build the shadow at all.
    pub(super) fn overshare_seen(&self) -> bool {
        self.overshare_seen
    }

    /// Decide whether a recurse-helper request for `share_key` (the filter-out-agnostic
    /// arm-shape + keep-all key, #377; inner source-AST key `ast_key`) hits an existing
    /// helper or must mint a fresh one, against whichever cache this pass keys on.
    ///
    /// - **Shadow pass** ([`python_keyed`](Self::python_keyed)): keys on
    ///   `(ast_key, keep_all)`, reproducing Python's `rules_cache[expr]` split.
    /// - **Real pass**: keys on the caller-owned `recurse_cache` (passed in, since the
    ///   load-bearing ADR-0013 sharing lives on the compiler). On a real hit, this also
    ///   records the over-share: if the inner-AST key that first minted the helper
    ///   differs from `ast_key`, the sharing fused two helpers Python would mint
    ///   distinctly, so [`overshare_seen`](Self::overshare_seen) flips.
    ///
    /// On a miss the caller mints the helper and calls
    /// [`record_minted`](Self::record_minted) to populate the matching cache.
    pub(super) fn lookup(
        &mut self,
        recurse_cache: &HashMap<RecurseShareKey, String>,
        share_key: &RecurseShareKey,
        ast_key: &str,
    ) -> RecurseDecision {
        if self.python_keyed {
            let ast_cache_key = (ast_key.to_string(), share_key.1);
            if let Some(name) = self.recurse_cache_ast.get(&ast_cache_key) {
                return RecurseDecision::Cached(name.clone());
            }
            return RecurseDecision::Mint;
        }
        if let Some(name) = recurse_cache.get(share_key) {
            // A real (filter-out-agnostic) cache hit. If the inner-AST shape differs
            // from the one that created this helper, the sharing has fused two helpers
            // Python Lark would mint distinctly — flag the over-share so the loader
            // knows an audit shadow (RC7/#272) is worth building.
            if self
                .recurse_cache_origin_key
                .get(share_key)
                .is_some_and(|origin| origin != ast_key)
            {
                self.overshare_seen = true;
            }
            return RecurseDecision::Cached(name.clone());
        }
        RecurseDecision::Mint
    }

    /// Record a freshly minted recurse helper `name` into the cache this pass keys
    /// on, after a [`lookup`](Self::lookup) returned [`RecurseDecision::Mint`]. The
    /// real pass also records the inner-AST origin key so a future hit can recognize
    /// an over-share; the caller inserts into its own `recurse_cache`.
    ///
    /// Returns whether this pass owns the cache entry: in the shadow pass the entry
    /// lives here (`true`, the caller need not touch `recurse_cache`); in the real
    /// pass the caller still inserts into its `recurse_cache` (`false`).
    pub(super) fn record_minted(
        &mut self,
        share_key: &RecurseShareKey,
        ast_key: &str,
        name: &str,
    ) -> bool {
        if self.python_keyed {
            self.recurse_cache_ast
                .insert((ast_key.to_string(), share_key.1), name.to_string());
            return true;
        }
        self.recurse_cache_origin_key
            .insert(share_key.clone(), ast_key.to_string());
        false
    }

    /// Import propagation, **real pass** (`imports::copy_imported`): an imported
    /// grammar that built its own `lalr_audit` over-shares internally, so the parent
    /// must build a shadow too. Flip [`overshare_seen`](Self::overshare_seen); the
    /// real parse table still copies the imported grammar's *shared* rules (ADR-0013
    /// sharing untouched).
    pub(super) fn note_imported_audit(&mut self) {
        self.overshare_seen = true;
    }
}
