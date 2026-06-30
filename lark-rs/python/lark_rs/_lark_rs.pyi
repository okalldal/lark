"""Type stubs for the compiled extension `lark_rs._lark_rs`.

`Token` and `Meta` are pure-Python types (see `lark_rs/__init__.pyi`); this
module exposes only the parser and the `Tree`/exception types built in Rust.
"""

from typing import Any, List, Optional, Union

from ._types import Meta, Token

__version__: str

class LarkError(Exception): ...
class GrammarError(LarkError): ...
class ParseError(LarkError): ...

class Tree:
    data: str
    children: List[Union["Tree", Token, None]]
    meta: Meta
    def __init__(
        self, data: str, children: Optional[List[Union["Tree", Token, None]]] = ...
    ) -> None: ...
    def pretty(self, indent_str: str = ...) -> str: ...
    def __eq__(self, other: object) -> bool: ...

ParseTree = Union[Tree, Token, None]

class Lark:
    def __init__(
        self,
        grammar: str,
        *,
        parser: str = ...,
        lexer: str = ...,
        start: Optional[Union[str, List[str]]] = ...,
        ambiguity: str = ...,
        propagate_positions: bool = ...,
        keep_all_tokens: bool = ...,
        maybe_placeholders: bool = ...,
        strict: bool = ...,
        g_regex_flags: int = ...,
    ) -> None: ...
    def parse(self, text: str, start: Optional[str] = ...) -> ParseTree: ...
