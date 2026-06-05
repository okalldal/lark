"""Type stubs for the `lark_rs` extension module."""

from typing import List, Optional, Union

__version__: str

class LarkError(Exception): ...
class GrammarError(LarkError): ...
class ParseError(LarkError): ...

class Token:
    type: str
    value: str
    line: int
    column: int
    end_line: int
    end_column: int
    start_pos: int
    end_pos: int
    def __init__(self, type_: str, value: str) -> None: ...
    def __str__(self) -> str: ...
    def __len__(self) -> int: ...
    def __eq__(self, other: object) -> bool: ...
    def __hash__(self) -> int: ...

class Tree:
    data: str
    children: List[Union["Tree", Token, None]]
    def __init__(
        self, data: str, children: Optional[List[Union["Tree", Token, None]]] = ...
    ) -> None: ...
    def pretty(self, indent_str: str = ...) -> str: ...
    def __eq__(self, other: object) -> bool: ...

ParseTree = Union[Tree, Token]

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
