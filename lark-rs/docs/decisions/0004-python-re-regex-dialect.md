# ADR-0004: Terminal regexes follow the Python `re` dialect

- **Status:** Accepted
- **Date:** backfilled 2026-06-13 (decision made during terminal/lexer work)

## Context

Lark grammars are authored against Python's `re` module. lark-rs matches with
Rust's `regex` (and `regex-automata`) crates. The two dialects mostly agree, but
in places the *same syntax means different things*. The load-bearing case: `\<`
and `\>` are literal `<` and `>` in Python, but **word-boundary assertions** in
the regex crate. Outside a character class, `\<\>` silently matches *nothing*
where Python matches `"<>"`; inside a class they are a compile error.

Since the oracle is Python Lark ([ADR-0001](0001-python-lark-is-the-oracle.md)),
a grammar that parses under Python must parse identically here, or fidelity
breaks silently.

## Decision

Where the dialects assign different meanings to the same syntax, normalize toward
**Python's** interpretation. `PatternRe::new` (`normalize_python_escapes`)
rewrites exactly `\<` / `\>` to bare characters before compiling.

## Consequences

- Grammars authored for Python Lark lex identically here — the common, intended
  case.
- The flip side is explicit and accepted: an author who *expects* the regex
  crate's word-boundary `\<` / `\>` is silently overridden. That trade is
  deliberate, because oracle fidelity is the project goal.
- This is a targeted normalization, not a general regex translator. New dialect
  divergences are handled case-by-case as the oracle surfaces them, each with a
  pinning test — not by a blanket rewrite layer.
