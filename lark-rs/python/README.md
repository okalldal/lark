# lark-rs (Python bindings)

A Rust-powered, drop-in speedup for the [Lark](https://github.com/lark-parser/lark)
parsing toolkit, exposed to Python via [PyO3](https://pyo3.rs).

```python
from lark_rs import Lark

parser = Lark(r"""
    start: value
    value: NUMBER
    NUMBER: /[0-9]+/
    %ignore /\s+/
""", parser="lalr")

tree = parser.parse("42")
print(tree.pretty())
```

## Status

This wraps the `lark-rs` core engine. The constructor accepts the same keyword
arguments as Python Lark for the options the Rust engine currently supports:

- `parser` — `"earley"` (default), `"lalr"`, `"cyk"`
- `lexer` — `"auto"`, `"basic"` (alias `"standard"`), `"contextual"`,
  `"dynamic"`, `"dynamic_complete"`
- `start` — a rule name or a list of rule names
- `ambiguity` — `"resolve"` (default), `"explicit"`, `"forest"`
- `propagate_positions`, `keep_all_tokens`, `maybe_placeholders`, `strict`,
  `g_regex_flags`

`parse(text, start=None)` returns a `Tree` (or a bare `Token` for a collapsing
`?start` rule). `Tree` exposes `.data` / `.children` / `.pretty()` and always
carries a `.meta` (a `Meta`, empty until `propagate_positions=True`). `Token` is
a genuine `str` *subclass* (like Python Lark's) exposing `.type` / `.value` and
position info, so `isinstance(tok, str)`, `tok == tok.value`, `tok in {value}`
and `{value: ...}[tok]` all behave exactly as in Python Lark.

`Token` and `Meta` are defined in pure Python (`lark_rs._types`) and the parser
engine in the compiled extension `lark_rs._lark_rs`; the user-facing `lark_rs`
package re-exports both. PyO3 cannot subclass `str` under the `abi3` build, so
the `str`-subclass contract is realised in Python and constructed by the Rust
core (see `lark-rs/docs/decisions/0036-pyo3-token-is-a-str.md`).

## Building

```bash
# from this directory, inside a virtualenv
pip install maturin
maturin develop            # build + install into the active venv
maturin build --release    # build a wheel under target/wheels/
```
