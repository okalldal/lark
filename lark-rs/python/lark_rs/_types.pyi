"""Type stubs for the pure-Python value types in `lark_rs._types`."""

from typing import Optional

class Meta:
    empty: bool
    # Only populated on a non-empty Meta (position propagation); absent otherwise.
    line: Optional[int]
    column: Optional[int]
    start_pos: Optional[int]
    end_line: Optional[int]
    end_column: Optional[int]
    end_pos: Optional[int]
    def __init__(self) -> None: ...

class Token(str):
    type: str
    value: str
    start_pos: Optional[int]
    line: Optional[int]
    column: Optional[int]
    end_line: Optional[int]
    end_column: Optional[int]
    end_pos: Optional[int]
    def __new__(
        cls,
        type: str,
        value: str,
        start_pos: Optional[int] = ...,
        line: Optional[int] = ...,
        column: Optional[int] = ...,
        end_line: Optional[int] = ...,
        end_column: Optional[int] = ...,
        end_pos: Optional[int] = ...,
    ) -> "Token": ...
    def __repr__(self) -> str: ...
