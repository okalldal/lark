# lark-rs

**Lark grammars, native Rust.** A ground-up Rust implementation of the
[Lark](https://github.com/lark-parser/lark) parsing toolkit's core: the same EBNF
grammar language behind **LALR**, **Earley** and **CYK**, with contextual lexing,
automatic tree construction and explicit ambiguity — checked against Python Lark as
the behavioural oracle.

> **One grammar. Three engines. Evidence at every layer.**

- 🌐 **Showcase & live playground:** <https://okalldal.github.io/lark/>
- 📖 **Architecture:** [`ARCHITECTURE.md`](ARCHITECTURE.md) · **Glossary:** [`GLOSSARY.md`](GLOSSARY.md)
- 🧪 **Status ledger:** [`docs/STATUS.md`](docs/STATUS.md) · **Benchmarks:** [`BENCH.md`](BENCH.md)

## Why lark-rs

If you already have a `.lark` grammar, keep it — change the runtime, not the grammar:

- **Multi-algorithm** — pick LALR, Earley or CYK by changing one option; the grammar
  is unchanged.
- **Contextual lexer** — parser state narrows which terminals the lexer tries,
  resolving most LALR terminal conflicts with no user intervention.
- **Rich EBNF** — `+ * ?`, alternation, char ranges, priorities, aliases, parameterized
  templates and `%import` grammar composition.
- **Automatic trees** — `Tree` / `Token` with no action code, plus the `?rule`,
  `_rule` and `!rule` shaping modifiers.
- **Many targets** — one core reachable from Rust, Python (PyO3), WebAssembly and a C
  API, plus standalone parser generation.

## Quick start

```bash
git clone https://github.com/okalldal/lark.git
cd lark/lark-rs
cargo run --release --example json_parser   # the canonical JSON example
cargo test                                   # the full suite
```

```rust
use lark_rs::{Lark, LarkOptions, ParserAlgorithm, LexerType};

let grammar = r#"
    ?start: value
    ?value: object | array | ESCAPED_STRING | SIGNED_NUMBER -> number
          | "true" | "false" | "null"
    array  : "[" [value ("," value)*] "]"
    object : "{" [pair ("," pair)*] "}"
    pair   : ESCAPED_STRING ":" value
    %import common.ESCAPED_STRING
    %import common.SIGNED_NUMBER
    %import common.WS
    %ignore WS
"#;

let opts = LarkOptions {
    parser: ParserAlgorithm::Lalr,
    lexer: LexerType::Contextual,
    ..LarkOptions::default()
};
let parser = Lark::new(grammar, opts)?;
let tree = parser.parse(r#"{"hello": [1, 2, 3]}"#)?;
println!("{tree}");
```

## Conformance (verified)

Each figure is enforced by a committed oracle bank — never hand-edited — and CI fails
on any regression. See [`docs/STATUS.md`](docs/STATUS.md).

| Bank | Result |
| --- | --- |
| LALR compliance | **512/512** |
| Earley basic | **211/211** |
| Earley dynamic lexer | **454/454** |
| CYK | **124/124** |
| JSONTestSuite corpus | **293/293** |

Performance is a documented snapshot, not a slogan: LALR is currently **~4–5× faster
than in-tree Python Lark** on the reference JSON workloads (see [`BENCH.md`](BENCH.md));
the broader 10–100× figure is a stated **goal**, not the present general result.

## How it is built

lark-rs is developed with coding agents under a written constitution
([`docs/PRINCIPLES.md`](docs/PRINCIPLES.md)): **autonomy ends where verification ends.**
A change proceeds autonomously only when its result can be checked against Python Lark,
a compliance bank, a regression test or a deterministic complexity gate; product
direction and untestable trade-offs stay human decisions.

## Status

Pre-user: the public API is not yet stabilized and packaging/release cadence is still
open. Backward compatibility is currently free because there are no real dependants
yet — this is a good time to try it and report friction.

## License

MIT — see [`LICENSE`](../LICENSE). Python Lark remains in-tree as the behavioural
oracle and reference implementation.
