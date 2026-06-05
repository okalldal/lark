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

- `parser` — `"earley"` (default), `"lalr"`, `"cyk"` *(CYK not yet implemented)*
- `lexer` — `"auto"`, `"basic"` (alias `"standard"`), `"contextual"`,
  `"dynamic"`, `"dynamic_complete"`
- `start` — a rule name or a list of rule names
- `ambiguity` — `"resolve"` (default), `"explicit"`, `"forest"`
- `propagate_positions`, `keep_all_tokens`, `maybe_placeholders`, `strict`,
  `g_regex_flags`

`parse(text, start=None)` returns a `Tree` (or a bare `Token` for a collapsing
`?start` rule). `Tree` exposes `.data` / `.children` / `.pretty()`; `Token` is a
`str`-like object exposing `.type` / `.value` and position info, so it compares
equal to its own text value just like Python Lark's `Token`.

## Building

```bash
# from this directory, inside a virtualenv
pip install maturin
maturin develop            # build + install into the active venv
maturin build --release    # build a wheel under target/wheels/
```
