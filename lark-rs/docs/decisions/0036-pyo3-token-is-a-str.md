# ADR-0036: The PyO3 `Token` IS-A `str` — realised in Python under `abi3`

- **Status:** Accepted (2026-06-30; was Proposed — architect ratified)
- **Date:** 2026-06-30

## Context

Python Lark's `Token` is a genuine `str` *subclass* (`Token.__mro__` is
`[Token, str, object]`). That single fact gives a load-bearing contract: a token
is an honest string, so `isinstance(tok, str)`, `tok == tok.value`,
`hash(tok) == hash(tok.value)`, `tok in {value}`, and `{value: ...}[tok]` all hold
and agree across the two engines. Drop-in replacement code routinely relies on it
(`token in keywords_set`, dict lookups keyed by token text).

The PyO3 binding's `Token` was a **standalone** `#[pyclass]`, not a `str`
subclass. It hand-rolled `__eq__` (equal to a plain `str` of the same value) but
hashed with Rust's `DefaultHasher`, so `hash(tok) != hash(tok.value)`. The result
was a silent wrong answer: `tok == "hello"` was `True` yet `tok in {"hello"}` was
`False` and `{"hello": 1}[tok]` raised `KeyError` (issue #416). Three smaller
surface gaps shared the same fix site: `Tree` had no `.meta` (Python's `Tree`
always carries a `Meta`); `repr()` used Rust `{:?}` double quotes; and the `Token`
constructor rejected Python's optional position kwargs.

The architect-resolved direction (issue #416, 2026-06-30) is unambiguous: make the
binding `Token` a genuine `str` subclass so `isinstance`/`__hash__`/`__eq__` all
derive from `str`. The open question this ADR records is **how**, given a hard
constraint discovered during implementation:

> **PyO3 cannot subclass a native type such as `str` while the extension is built
> with the `abi3` feature.** The binding ships one `abi3-py38` wheel for all
> CPython 3.8+. With `abi3`, `#[pyclass(extends=PyString)]` is a *compile error*
> ("with the `abi3` feature enabled, PyO3 does not support subclassing native
> types"); and even without `abi3`, PyO3 invokes the base `str` `tp_new` with no
> arguments, so the immutable string payload could not be set. Dropping `abi3` to
> reach a native subclass would mean a per-interpreter wheel matrix (a
> distribution regression) *and* still hit the unset-payload limitation.

So the `str`-subclass contract cannot live in the compiled extension. It must be
realised in Python.

## Decision

Realise `Token IS-A str` in **pure Python**, and have the Rust core construct it:

1. `Token` (a `class Token(str)`) and `Meta` move to a pure-Python module
   `lark_rs._types`. `Token.__new__` calls `str.__new__(cls, value)`, so the
   instance *is* its own text — `__eq__`/`__hash__`/`__str__` come from `str` for
   free and match Python Lark exactly. It accepts Python's optional position
   kwargs (`start_pos`, `line`, `column`, `end_line`, `end_column`, `end_pos`,
   defaulting to `None`) and reprs in Python single-quote style.
2. The compiled extension is renamed `lark_rs._lark_rs` (a mixed Rust/Python
   maturin layout). It keeps the parser engine and the Rust-built `Tree`, and now
   constructs token leaves by **calling the cached Python `Token` class** while it
   shapes a parse tree — so callers get genuine `str`-subclass tokens with no
   Python-side re-walk of the tree. `lark_rs._types` imports nothing from the
   extension, so the extension can import it at parse time without a cycle.
3. `Tree` (still built in Rust) always carries a `.meta` — an empty `Meta` until
   `propagate_positions=True`, matching Python Lark's always-present `Meta`.
4. The user-facing `lark_rs` package (`__init__.py`) re-exports `Lark`, `Tree`,
   `Token`, `Meta`, the exception hierarchy, and `__version__`.

This is the minimum that honours the architect's decision under the shipped
`abi3` build: the contract lives where it *can* be a real `str` subclass (Python),
and Rust stays in charge of parsing and shaping.

## Consequences

- **The eq/hash invariant holds.** `Token` is a real `str` subclass:
  `isinstance(tok, str)`, set/dict membership keyed by the token's value, and
  `hash` all agree with Python Lark. The folded gaps are closed in the same move
  (`Tree.meta`, single-quote `repr`, position kwargs).
- **Public binding-surface change → `escalate`-tier.** This alters a load-bearing
  binding contract (`Token` IS-A `str`) and the package layout (the importable
  engine is now the submodule `lark_rs._lark_rs`; `Token`/`Meta` are Python types
  in `lark_rs._types`). Per ADR-0016 the architect merges it; this ADR is its
  doc-maintenance record. Under ADR-0025 (no users yet) the layout change is free.
- **A Python construction cost per token.** Each token leaf is now one cached
  class `call`. Tokens are leaves (one call each), the cost is bounded by token
  count, and a genuine `str` subclass is unreachable any other way under `abi3` —
  an accepted trade for correctness in a drop-in replacement.
- **Scope held.** WASM/C `Tree.meta` exposure is explicitly *not* touched (it has
  its own surface taxonomy, cf. #244). This ADR is about the Python binding only.
- **Tripwire — un-park the native route.** If the binding ever drops `abi3` for a
  per-interpreter wheel matrix *and* PyO3 gains the ability to set a native base's
  immutable payload (the `tp_new` FIXME in `pyo3::impl_::pyclass_init`), a native
  `#[pyclass(extends=PyString)]` `Token` could replace the Python class. Until both
  hold, the Python realisation is the only faithful option, not a stopgap to rush
  past.
- **Enforced by:** the failed-first repro in
  `lark-rs/python/tests/test_roundtrip.py` (isinstance/set/dict/hash, repr style,
  ctor kwargs, `Tree.meta`, and a direct differential against `lark.Token`),
  exercised by the `python-binding` CI job (`maturin develop` + `pytest`).
