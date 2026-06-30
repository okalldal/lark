"""Pure-Python value types for the ``lark_rs`` binding.

These live in Python (not the compiled extension) for one load-bearing reason:
``Token`` must be a genuine ``str`` *subclass*, exactly like ``lark.Token``. PyO3
cannot subclass a native type such as ``str`` while the extension is built with
the ``abi3`` feature (one wheel for all CPython 3.8+), so the str-subclass
contract is realised here in Python and the Rust core (``lark_rs._lark_rs``)
constructs these objects when it shapes a parse tree.

This module imports nothing from the extension, so the extension can import it at
parse time without an import cycle (see ADR-0036).
"""

from typing import List, Optional, Union


class Meta:
    """Per-node metadata, mirroring ``lark.tree.Meta``.

    A ``Tree`` always carries a ``Meta``; ``empty`` is ``True`` until position
    information is propagated (``propagate_positions=True``), matching Python
    Lark, which only populates the line/column fields on a non-empty ``Meta``.
    """

    __slots__ = (
        "empty",
        "line",
        "column",
        "start_pos",
        "end_line",
        "end_column",
        "end_pos",
    )

    def __init__(self) -> None:
        self.empty = True

    def __repr__(self) -> str:
        if self.empty:
            return "Meta()"
        line = getattr(self, "line", None)
        return f"Meta(line={line!r})"


class Token(str):
    """A positioned lexer token — a genuine ``str`` subclass, like ``lark.Token``.

    Because it derives from ``str``, ``isinstance(tok, str)``,
    ``hash(tok) == hash(tok.value)``, ``tok == tok.value``, ``tok in {value}`` and
    ``{value: ...}[tok]`` all hold and agree with Python Lark exactly (issue #416,
    ADR-0036).
    """

    __slots__ = (
        "type",
        "value",
        "start_pos",
        "line",
        "column",
        "end_line",
        "end_column",
        "end_pos",
    )

    def __new__(
        cls,
        type: str,
        value: str,
        start_pos: Optional[int] = None,
        line: Optional[int] = None,
        column: Optional[int] = None,
        end_line: Optional[int] = None,
        end_column: Optional[int] = None,
        end_pos: Optional[int] = None,
    ) -> "Token":
        inst = super().__new__(cls, value)
        inst.type = type
        inst.value = value
        inst.start_pos = start_pos
        inst.line = line
        inst.column = column
        inst.end_line = end_line
        inst.end_column = end_column
        inst.end_pos = end_pos
        return inst

    def __repr__(self) -> str:
        return f"Token({self.type!r}, {self.value!r})"

    def __eq__(self, other: object) -> bool:
        # Type-aware equality, exactly like ``lark.Token``: two tokens with the
        # same text but different ``type`` are *not* equal, while a token still
        # compares equal to a plain ``str`` of its value (so ``tok == "foo"`` and
        # set/dict membership keyed by the text both keep working).
        if isinstance(other, Token) and self.type != other.type:
            return False
        return str.__eq__(self, other)

    # ``str.__eq__`` (used above) only ever cares about the text, so the inherited
    # ``str`` hash stays correct and consistent with equality. Overriding
    # ``__eq__`` otherwise clears ``__hash__`` to ``None`` (Python's rule), which
    # would make the token unhashable and re-break set/dict membership (#416).
    __hash__ = str.__hash__

    def __reduce__(self):
        # Mirror ``lark.Token`` so ``pickle`` and ``copy`` round-trip a parsed
        # token (a common need for caching / multiprocessing).
        return (
            self.__class__,
            (self.type, self.value, self.start_pos, self.line, self.column),
        )

    def __deepcopy__(self, memo) -> "Token":
        return Token(self.type, self.value, self.start_pos, self.line, self.column)


__all__ = ["Token", "Meta"]
