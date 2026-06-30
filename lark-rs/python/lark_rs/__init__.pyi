"""Type stubs for the `lark_rs` package — the user-facing API surface."""

from typing import Union

from ._lark_rs import (
    GrammarError as GrammarError,
    Lark as Lark,
    LarkError as LarkError,
    ParseError as ParseError,
    Tree as Tree,
    __version__ as __version__,
)
from ._types import (
    Meta as Meta,
    Token as Token,
)

ParseTree = Union[Tree, Token, None]
