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
additive change: it leaves the `parse()` signature and the two existing variants
untouched.

## Consequences

- **Public-API surface grows by one variant.** This is an `escalate`-tier change
  (new public API/semantics, ADR-0016) — hence this ADR and architect ratification
  on merge rather than auto-merge.
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
