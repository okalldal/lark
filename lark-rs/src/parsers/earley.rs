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
//!   naively.
//! - The dynamic lexer integrates regex matching into the Earley loop, calling
//!   back into the lexer for each predicted terminal at each position.
