//! Earley parser stub — Phase 2.
//!
//! The Earley algorithm can parse any context-free grammar (including
//! ambiguous ones) and is the key differentiator of Lark vs. other
//! Rust parsing libraries. Full implementation is Phase 2 of the rewrite.
//!
//! Key design notes for the full implementation:
//! - Use Elizabeth Scott's SPPF (Shared Packed Parse Forest) algorithm
//!   to represent all derivations without duplication.
//! - Use arena allocation (bumpalo or typed-arena) for SPPF nodes, since
//!   they form a DAG with shared references that Rust ownership can't express
//!   naively. Prefer a `Vec<Node>` + `NodeId` index form over `Rc<RefCell>`.
//! - Key forest nodes by `SymbolId` (the interned IR), never names.
//! - Reuse the shared [`TreeBuilder`](super::tree_builder::TreeBuilder) for the
//!   forest→tree walk (filter / transparent splice / expand1 / `_ambig`), and the
//!   [`TokenSource`](super::token_source::TokenSource) trait for input — both were
//!   built for this.
//! - The dynamic lexer integrates regex matching into the Earley loop, calling
//!   back into the lexer for each predicted terminal at each position (Sprint 5).
//!
//! The test harness is already in place (Phase 2, Sprint 0): `test_earley_oracle`
//! (curated resolve + explicit-`_ambig` oracles) and `test_earley_compliance`
//! (the Earley compliance bank) self-activate the moment `build_frontend` returns
//! a working Earley frontend instead of the not-yet-implemented error. See
//! `PHASE_2_PLAN.md`.
