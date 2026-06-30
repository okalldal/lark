"""``lark_rs`` — a Rust-powered, drop-in speedup for the Lark parsing toolkit.

The parsing engine lives in the compiled extension ``lark_rs._lark_rs``; the
``str``-subclass ``Token`` and the ``Meta`` value type live in pure Python
(``lark_rs._types``) because PyO3 cannot subclass ``str`` under the ``abi3`` build
(ADR-0036). The Rust core constructs the Python ``Token``/``Meta`` objects as it
shapes each parse tree, so callers get genuine ``str``-subclass tokens with no
Python-side re-walk of the tree.
"""

from typing import Optional, Union

from ._lark_rs import (
    GrammarError,
    Lark,
    LarkError,
    ParseError,
    Tree,
    __version__,
)
from ._types import Meta, Token

# A parse result is a `Tree`, a bare `Token` (a collapsing `?start`), or `None`
# (a `?start: [A]` placeholder root, #289/ADR-0033) — mirror that honestly at
# runtime, matching the `lark_rs/__init__.pyi` stub, rather than aliasing `Tree`.
ParseTree = Union[Tree, Token, None]

__all__ = [
    "Lark",
    "Tree",
    "Token",
    "Meta",
    "ParseTree",
    "LarkError",
    "GrammarError",
    "ParseError",
    "__version__",
]
