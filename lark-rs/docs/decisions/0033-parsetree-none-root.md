# ADR-0033: `ParseTree::None` — a bare-`None` root parse result

- **Status:** Proposed (pending architect ratification)
- **Date:** 2026-06-24

## Context

A root `?start` rule whose sole alternative is an absent `maybe_placeholders`
`[...]` collapses its lone placeholder `None` through `?`-expand1 to a bare `None`
at the very root of the parse. Python Lark (the oracle) returns a literal `None`
for `?start: [A]` / `A: "a"` on input `""` with `maybe_placeholders=true`, on
LALR, Earley/basic, and Earley/dynamic (#289).

lark-rs's public parse result type was `enum ParseTree { Tree(Tree), Token(Token) }`
— it could represent the two non-empty `?start` collapses (a wrapping `Tree`, or a
bare `Token` via expand1) but **not** a bare `None`. So the three backends each
diverged from the oracle at the augmented-start root: LALR returned `UnexpectedEOF`
(rejected the empty input at `accept()`), and Earley/dynamic returned an empty
`start[]` tree. The collapse itself (`tree_builder::shape()`, RC9: lone-`None`
expand1 → `Slot::Inline([Child::None])`) was already correct; only the
root-unwrapping was wrong.

Options considered for representing Python's bare `None`:

1. **Add `ParseTree::None`** — a third, additive variant.
2. Change `parse()`'s return to `Result<Option<ParseTree>, ParseError>` — a
   breaking signature change rippling through every caller and binding.
3. Keep returning an empty `Tree`/`Token` or an error — the status-quo divergence
   #289 exists to fix; rejected by the issue's Done-when (oracle = bare `None`).

## Decision

Add a third variant `ParseTree::None` (plus an `is_none()` accessor) representing
Python Lark's bare `None` parse result, and map a start rule's
`Inline([Child::None])` root value to it in all three backends
(`lalr.rs::accept`, `earley.rs::forest_to_tree`, `cyk.rs` — the last unreachable,
since CYK rejects nullable starts at build, kept symmetric). This is the minimal
change at the type level: it leaves the `parse()` signature and the two existing
variants untouched.

**This is a source-breaking public API change, not "merely additive."** In Rust,
adding a variant to a public enum is *not* backward-compatible for downstream code
that `match`es `ParseTree` exhaustively (without a `_ =>` arm or `#[non_exhaustive]`):
every such match stops compiling until it grows a `ParseTree::None` arm. So while
this leaves existing *values* and the `parse()` *signature* intact, it breaks
downstream *callers* at the type level. It is the **right** break — `?start: [A]`
on `""` genuinely yields three outcomes (a wrapping `Tree`, a bare `Token`, or a
bare `None`), and the type must be able to name all three to match the oracle — but
the record and release notes must call it a **breaking change**, not an additive
one. Under ADR-0025 (no backward-compatibility constraint pre-users) the break is
*free* today; this entry exists so the break is recorded as a break, not mislabeled.

Recommendation (not enacted in this PR): once `ParseTree` has real downstream
dependents, mark it `#[non_exhaustive]` so future variants (a fourth output shape,
should one arise) are additive-by-construction — downstream exhaustive matches would
then be required to carry a `_ =>` arm and would not break on a new variant. We do
**not** add `#[non_exhaustive]` here: it would itself be an API-shape decision better
made deliberately with the stability policy that supersedes ADR-0025 at the first
real dependent, and adding it now would force a wildcard arm on every in-tree match
for no present benefit. Flagged here as the documented next step, left to the
architect.

## Consequences

- **Public-API surface grows by one variant — a source-breaking change.** A new
  variant on the public `ParseTree` enum breaks any downstream exhaustive `match`
  (see Decision above): this is `escalate`-tier (new public API/semantics, ADR-0016)
  *and* a breaking change, hence this ADR, architect ratification on merge rather
  than auto-merge, and a release-note entry that names it a **break**.
- Every match on the public `ParseTree` must now handle three variants. All
  in-tree consumers were updated to map the bare `None` to each surface's natural
  representation: PyO3 → Python `None`; C API → a valueless `"None"` leaf; WASM and
  `generate_oracles.py`-mirroring `differ` → `{"type":"unknown","repr":"None"}`;
  `diffcheck` (mirrors `diffcheck.py`'s explicit `node is None → null`) → JSON
  `null`. The two JSON shapes are each correct against the Python serializer that
  binary mirrors.
- Not more-permissive (ADR-0017): `Inline([Child::None])` is produced only by an
  expand1 (`?`) rule with exactly one placeholder-`None` child; a non-`?` start
  rule reaches `Slot::Tree`, never this path. Pinned by the negative control
  `non_optional_root_start_unchanged`.
- **Tripwire:** if a future change lets a non-lone-`None` `Inline` reach a start
  root, the guarded arms fall through to the existing reject/empty paths, not to a
  silent `None`. Enforced by `tests/test_root_optional_start.rs` (all three
  backends → bare `None`; present-branch → bare token; negative control).
