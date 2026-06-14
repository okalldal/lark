# lark-rs — Architecture (the tourist map)

This is the **human-facing** orientation doc: a mental model of how lark-rs is
put together, pitched at a reader who steers the project and has cursory parser
theory. It is deliberately short and changes slowly. For agent-facing operational
detail see [`CLAUDE.md`](CLAUDE.md); for *what is done / open* see
[`docs/STATUS.md`](docs/STATUS.md); for *why we decided things* see
[`docs/decisions/`](docs/decisions/); for unfamiliar terms see
[`GLOSSARY.md`](GLOSSARY.md).

> If you take one thing from this file: a grammar goes through **four
> transformations** — *load → lower → build → parse* — and almost every module
> belongs to exactly one of those four stages. Find the stage and you've found
> the code.

---

## What lark-rs is

A Rust rewrite of the [Lark](https://github.com/lark-parser/lark) parsing
toolkit. You hand it a grammar written in Lark's EBNF dialect (a `.lark` file)
plus some input text; it hands you back a parse **tree**. The same grammar can
be parsed by three different algorithms (LALR, Earley, CYK) by flipping one
option — that interchangeability is the project's headline feature, and most of
the architecture exists to preserve it.

The reference for *correct behavior* is always Python Lark: we generate its
parse trees and assert ours match (see
[ADR-0001](docs/decisions/0001-python-lark-is-the-oracle.md)).

---

## The pipeline (the one diagram worth knowing)

```
  grammar.lark (text)                         input text
        │                                         │
        ▼                                         │
  ┌───────────────┐  STAGE 1: LOAD               │
  │ grammar/loader│  .lark text → Grammar         │
  │  tokenizer →  │  (a string-named, human-      │
  │  parser →     │   readable description of      │
  │  compiler     │   rules + terminals)           │
  └───────┬───────┘                                │
          ▼                                         │
  ┌───────────────┐  STAGE 2: LOWER                │
  │ grammar/intern│  Grammar → CompiledGrammar     │
  │               │  every symbol becomes a small  │
  │               │  integer id; semantics become  │
  │               │  flags, not name-prefixes      │
  └───────┬───────┘                                │
          ▼                                         │
  ┌───────────────┐  STAGE 3: BUILD                │
  │ parsers/ +    │  CompiledGrammar → a ready      │
  │ lexer/        │  parser: LALR table / Earley    │
  │               │  chart / CYK, plus a lexer      │
  └───────┬───────┘                                │
          ▼                                         ▼
  ┌─────────────────────────────────────────────────┐
  │ STAGE 4: PARSE   Lark::parse(input)              │
  │   lexer  →  parser driver  →  TreeBuilder        │
  │   (text → tokens → reductions → tree shaping)    │
  └────────────────────────┬─────────────────────────┘
                           ▼
                    Tree / Token  (the result)
```

Stages 1–3 happen once, when you construct a `Lark`. Stage 4 happens on every
`parse()` call. The public entry point that wires them together is
[`src/lib.rs`](src/lib.rs) (`Lark::new` → load → build; `Lark::parse` → stage 4).

---

## The six things lark-rs must preserve

These are the Lark differentiators every design choice has to keep working.
When evaluating a change, ask "does this break one of these?":

1. **Multi-algorithm** — same grammar runs on LALR, Earley, or CYK (one flag).
2. **Contextual lexer** — the parser tells the lexer which tokens are even
   possible in the current state, which resolves almost all LALR conflicts
   automatically. This is Lark's primary selling point.
3. **SPPF-based Earley** — parses *any* context-free grammar and can output
   ambiguity explicitly.
4. **Rich EBNF** — `+ * ? |`, char ranges, templates, priorities, aliases,
   `%import` grammar composition.
5. **Automatic tree building** — you get a `Tree`/`Token` with no action code.
6. **Rule modifiers** — `?rule` (collapse single child), `_rule` (transparent /
   inlined), `!rule` (keep all tokens).

---

## Module map — where things live

Paths are under [`src/`](src/). Each module carries a `//!` header that says
more; this table is the index.

### Stage 1 — Load (`grammar/loader/`)
Turn `.lark` text into a `Grammar`. One module per pipeline phase:

| Module | Responsibility |
|---|---|
| `tokenizer.rs` | hand-written lexer for the `.lark` syntax itself |
| `parser.rs` | recursive-descent parser → raw AST (`ast.rs`) |
| `compiler.rs` | lowers the AST into a `Grammar`; orchestrates the helpers below |
| `terminals.rs` | terminal algebra → regex; terminal ordering rules |
| `ebnf.rs` | expands `* + ? \| (...)` into anonymous helper rules |
| `templates.rs` | parameterized rules (`_sep{x, sep}`) |
| `imports.rs` | `%import` resolution (bundled libs + sibling files) |

### Stage 2 — Lower (`grammar/intern.rs`)
`Grammar` → `CompiledGrammar`. Interns every symbol to a `Copy` integer
`SymbolId` (terminals get the low ids, `$END` = 0), and replaces every
name-prefix convention with an explicit flag (`is_start`, `transparent`, …).
After this stage the engine never looks at a symbol *name* again — see
[ADR-0003](docs/decisions/0003-intern-symbols-to-ids-with-flags.md).

### Stage 3+4 — Build & parse the input

**Lexer** (`lexer/`) — text → tokens. `BasicLexer` (one combined regex) and
`ContextualLexer` (a per-parser-state scanner) plus the scanner backends. The
default backend is a `regex-automata` **DFA** (`dfa.rs`); a `regex`-crate
backend (`scanner.rs`) is kept as a cross-check. Lookaround that the regex
engines can't express is *lowered* into the DFA rather than run on a
backtracking engine — that whole story lives in `lookaround/` and
[ADR-0005](docs/decisions/0005-lower-lookaround-into-the-dfa.md).

**Parsers** (`parsers/`) — tokens → tree. A `ParsingFrontend` sits over a
`ParserDriver` trait; each parser × lexer wiring is one driver impl:

| Module | Responsibility |
|---|---|
| `lalr.rs` | dense LALR(1) parse table + the parse loop |
| `earley.rs` | Earley recognizer + SPPF forest + forest→tree + dynamic lexer |
| `cyk.rs` | CYK parser (CNF conversion + O(n³) DP) |
| `tree_builder.rs` | shared rule→tree shaping used by all three |
| `token_source.rs` | the lexer⇄parser pull API |

**Result types** (`tree.rs`, `error.rs`) — `Tree`, `Token`, and the error
hierarchy (`GrammarError` at build time, `ParseError` at parse time).

### Cross-cutting
| Area | Where | Note |
|---|---|---|
| Significant whitespace | `postlex.rs` | the `Indenter` (INDENT/DEDENT injection) |
| Standalone codegen | `standalone/` | bake a grammar into a self-contained `.rs` |
| Perf instrumentation | `perf.rs` | deterministic work counters ([ADR-0007](docs/decisions/0007-deterministic-perf-counters.md)) |
| Distribution bindings | `python/`, `wasm/`, `lark_h/`, `lark_proc/` | PyO3, WASM, C API, proc-macro (separate crates) |

---

## How correctness is enforced (so you can trust a green build)

The test suite is the real specification; the prose docs only orient you toward
it. Four layers, all in CI:

- **Oracle tests** — curated grammars parsed by both engines, trees compared
  (`tests/test_oracle.rs`, etc.). Oracle JSON is generated by Python Lark and
  committed (`tests/fixtures/oracles/`).
- **Compliance banks** — Python Lark's *own* test suite, strip-mined into
  `(grammar, input, expected)` records and replayed (`test_compliance.rs` and
  the Earley/CYK siblings). Known gaps are allow-listed in `xfail.json` files
  and only ever shrink ([ADR-0009](docs/decisions/0009-xfail-burndown-discipline.md)).
- **Wild bank** — real-world grammars (Terraform, GraphQL, …) vendored verbatim
  (`tests/wild/`).
- **Scaling gates** — deterministic work-counter envelopes that catch a
  complexity regression without flaky wall-clock timing ([ADR-0007](docs/decisions/0007-deterministic-perf-counters.md)).

When you ask "is feature X actually correct," the honest answer is "there is an
oracle/compliance case that pins it" — find that case.

---

## Where to go next

- **"Why is it built this way?"** → [`docs/decisions/`](docs/decisions/) (the ADR log).
- **"What's done / what's open?"** → [`docs/STATUS.md`](docs/STATUS.md).
- **"What does this term mean?"** → [`GLOSSARY.md`](GLOSSARY.md).
- **"How do I run / extend it?"** → [`CLAUDE.md`](CLAUDE.md).
- **"How does *this specific module* work?"** → read its `//!` header, or just
  ask Claude to walk you through it against the live code — that's cheaper and
  more current than any standing prose.
