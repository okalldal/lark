# ADR-0003: Lower to integer `SymbolId`s; semantics as flags, not name-prefixes

- **Status:** Accepted
- **Date:** backfilled 2026-06-13 (decision made when the intern pipeline was introduced)

## Context

Lark encodes a lot of meaning in symbol *names*: a leading `_` means transparent,
`__anon_*` is an EBNF helper, `$root_*` is an augmented start rule, and terminal
vs. non-terminal is a name-set membership test. A naive port carries those string
conventions onto the hot parse path, which means string hashing and prefix
sniffing on every token and every reduce — and subtle bugs when a heuristic like
`!name.starts_with('_')` misclassifies an anonymous non-terminal.

## Decision

Before any engine touches it, **lower** the string-named `Grammar` into a
`CompiledGrammar` (`src/grammar/intern.rs`):

- Intern every symbol to a `Copy` integer `SymbolId`. Assign all terminal ids
  first so terminals occupy `[0, n_terminals)` and `$END` is id 0 — then
  terminal-vs-non-terminal is just `id < n_terminals`.
- Precompute every semantic as an explicit flag on `CompiledRule`: `is_start`
  (was `name.starts_with("$root_")`), `transparent` (was the `_`/`__anon_`
  check), per-position token filtering (`filter_pos`), tree name, etc.

After lowering, the engine never inspects a symbol *name* again.

## Consequences

- The parse loop is an array index per token — never a string hash.
- The misclassification class of bugs disappears: semantics are decided once, at
  lowering, from the real terminal set rather than from a name spelling.
- Token filtering is **per rule position**, not per terminal, so a terminal that
  is unified for lexing can still be kept at one position and dropped at another
  (Lark's model).
- Cost: an extra transform stage and the discipline that *anything the engine
  needs must be lowered into a flag*. New engine-visible semantics belong in
  `CompiledRule`/`CompiledGrammar`, not in a name convention.
