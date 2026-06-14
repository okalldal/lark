# lark-rs — Glossary

A one-page decoder ring for the terms that show up across the code, the ADRs,
[`CLAUDE.md`](CLAUDE.md), and [`docs/STATUS.md`](docs/STATUS.md). Pitched at a
reader with cursory parser theory. When a term has a "load-bearing" nuance in
*this* codebase, that's called out.

## Grammar & parsing basics

- **Grammar** — the set of rules describing a language. In lark-rs the
  word also names the in-memory `Grammar` struct: the *string-named, human-
  readable* form right after loading, before it is lowered.
- **EBNF** — Extended Backus–Naur Form: BNF plus `* + ? | (...)` and friends.
  Lark's `.lark` files are EBNF.
- **Terminal** — a leaf token type, matched by a regex/string (e.g. `NUMBER`,
  `IF`). Conventionally UPPERCASE. Produces a **Token**.
- **Rule** (a.k.a. non-terminal) — a named combination of terminals/rules
  (e.g. `expr`, `start`). Conventionally lowercase. Produces a **Tree**.
- **Token** — a matched terminal instance, carrying its text + position.
- **Tree** — a parse-tree node: a rule name plus its children (Trees/Tokens).
- **Parse table** — the precomputed state machine an LALR parser runs.

## The three algorithms

- **LALR(1)** — a fast, table-driven bottom-up parser; one token of lookahead.
  Rejects ambiguous grammars at build time. lark-rs's default.
- **Earley** — a general parser that handles *any* context-free grammar,
  including ambiguous ones; slower but maximally flexible.
- **CYK** — another general algorithm; works on grammars in CNF, O(n³).
- **SLR vs LALR** — SLR is a cheaper, *less precise* table-construction method
  that over-reports conflicts. lark-rs computes **true LALR(1)** lookaheads
  instead (see [ADR-0002](docs/decisions/0002-true-lalr1-not-slr.md)).

## Lexing

- **Lexer / scanner** — turns raw text into a stream of Tokens.
- **Basic lexer** — one combined regex tries every terminal, leftmost-first.
- **Contextual lexer** — the parser tells the lexer which terminals are valid in
  the current parse state, so overlapping terminals stop conflicting. Lark's
  headline feature.
- **Dynamic lexer** — Earley-only: tokenization is folded *into* the parse loop,
  so the terminals tried at each position are exactly those the parser predicts.
- **DFA backend** — the default combined-scanner engine, built on
  `regex-automata`. The alternative `regex`-crate backend is kept as a cross-check
  (see [ADR-0006](docs/decisions/0006-dfa-default-lexer-backend.md)).
- **Lookaround** — regex assertions like `(?!...)`, `(?<=...)`. The regex engines
  can't run these, so lark-rs **lowers** bounded shapes into the DFA instead of
  using a backtracking engine (see [ADR-0005](docs/decisions/0005-lower-lookaround-into-the-dfa.md)).

## Earley internals

- **SPPF** — Shared Packed Parse Forest: a compact graph that represents *all*
  parses of an ambiguous input at once, with sharing so it doesn't blow up.
- **`_ambig` node** — the tree node Earley emits (in `ambiguity='explicit'`
  mode) to mark a point with multiple valid derivations. Its children are an
  *unordered set* — never assert an order on them.
- **Joop-Leo** — an optimization that collapses deterministic right-recursion in
  Earley from O(n²) to linear.

## Tree shaping (rule modifiers)

- **`?rule` (expand1)** — if the rule produced exactly one child, return that
  child directly instead of a wrapper Tree. In this codebase it returns a
  `Child` (Token *or* Tree), not always a Tree — a load-bearing subtlety.
- **`_rule` (transparent)** — the rule's children are spliced into its parent
  rather than kept as a node.
- **`!rule` (keep_all_tokens)** — keep punctuation/filtered tokens that would
  otherwise be dropped.
- **`__anon_*`** — anonymous helper rules the loader synthesizes when expanding
  EBNF operators (`a*`, `(a b)?`, …). Transparent by construction.

## Lowering & interning

- **Lower / lowering** — the Stage-2 transform `Grammar → CompiledGrammar`.
- **Interning / `SymbolId`** — replacing every symbol *name* with a small
  integer id so the engine indexes arrays instead of hashing strings. `$END`
  (end-of-input) is id 0; terminals get the low ids. See
  [ADR-0003](docs/decisions/0003-intern-symbols-to-ids-with-flags.md).
- **NULLABLE / FIRST** — standard grammar-analysis sets: which symbols can
  derive the empty string, and which terminals can begin a symbol. lark-rs
  deliberately computes **no FOLLOW set** (true LALR doesn't need it).

## Testing vocabulary

- **Oracle** — Python Lark used as ground truth. We generate its parse tree and
  assert lark-rs matches it. The whole testing philosophy
  ([ADR-0001](docs/decisions/0001-python-lark-is-the-oracle.md)).
- **Compliance bank** — Python Lark's own test suite, strip-mined into replayable
  `(grammar, input, expected)` records.
- **Wild bank** — real-world grammars + inputs vendored from open-source projects.
- **XFAIL** — an "expected failure": a known gap, allow-listed so CI stays green
  but the list can only shrink ([ADR-0009](docs/decisions/0009-xfail-burndown-discipline.md)).
- **Scaling gate** — a test that asserts work *per unit of input* stays flat,
  using deterministic counters instead of wall-clock time
  ([ADR-0007](docs/decisions/0007-deterministic-perf-counters.md)).
- **Standalone parser** — a grammar baked into a single self-contained `.rs`
  file that depends only on `regex` + std, not on lark-rs.
