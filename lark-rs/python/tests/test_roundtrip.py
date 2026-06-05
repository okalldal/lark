"""Round-trip parity tests: lark_rs vs Python Lark (the oracle).

Build the *same* grammar with both engines, parse the same inputs, and assert the
trees are structurally identical (data, child shape, token types and values).
This is the Phase-4 "Done-when" smoke test for the PyO3 binding.

Run with:  python -m pytest lark-rs/python/tests/
"""

import pytest

import lark_rs

# Python Lark is the oracle. Skip the comparison tests (not the binding's own
# behaviour tests) if it isn't installed.
lark = pytest.importorskip("lark")


JSON_GRAMMAR = r"""
start: value
?value: object | array | STRING | NUMBER | "true" | "false" | "null"
array: "[" [value ("," value)*] "]"
object: "{" [pair ("," pair)*] "}"
pair: STRING ":" value
STRING: /"[^"]*"/
NUMBER: /-?[0-9]+(\.[0-9]+)?/
%ignore /[ \t\n\r]+/
"""

JSON_INPUTS = [
    "1",
    "-42",
    "3.14",
    '"hello"',
    "true",
    "[1, 2, 3]",
    "[]",
    "{}",
    '{"a": 1}',
    '{"a": [1, 2], "b": {"c": "d"}}',
    '[true, false, null, 0, "x"]',
]

ARITH_GRAMMAR = r"""
?start: sum
?sum: product | sum "+" product -> add | sum "-" product -> sub
?product: atom | product "*" atom -> mul | product "/" atom -> div
?atom: NUMBER | "(" sum ")"
NUMBER: /[0-9]+/
%ignore /[ \t]+/
"""

ARITH_INPUTS = ["1", "1+2", "1+2*3", "(1+2)*3", "10-2-3", "2*3+4*5"]


def normalize(node):
    """Engine-agnostic structural view of a parse tree."""
    if isinstance(node, (lark.Tree, lark_rs.Tree)):
        return ("tree", node.data, [normalize(c) for c in node.children])
    if node is None:
        return None
    # Token (lark.Token is a str subclass; lark_rs.Token exposes .type/.value).
    return ("token", node.type, str(node))


def assert_parity(grammar, text, **options):
    rs = lark_rs.Lark(grammar, **options).parse(text)
    py = lark.Lark(grammar, **options).parse(text)
    assert normalize(rs) == normalize(py), (
        f"\ninput: {text!r}\nlark_rs: {normalize(rs)}\nlark:    {normalize(py)}"
    )


@pytest.mark.parametrize("text", JSON_INPUTS)
def test_json_lalr(text):
    assert_parity(JSON_GRAMMAR, text, parser="lalr")


@pytest.mark.parametrize("text", JSON_INPUTS)
def test_json_earley(text):
    assert_parity(JSON_GRAMMAR, text, parser="earley")


@pytest.mark.parametrize("text", ARITH_INPUTS)
def test_arithmetic_lalr(text):
    assert_parity(ARITH_GRAMMAR, text, parser="lalr")


# ─── Binding-specific behaviour (no oracle needed) ──────────────────────────


def test_token_is_str_like():
    tok = lark_rs.Token("WORD", "hello")
    assert tok == "hello"
    assert str(tok) == "hello"
    assert tok.type == "WORD"
    assert tok.value == "hello"
    assert len(tok) == 5


def test_tree_construction_and_eq():
    a = lark_rs.Tree("x", [lark_rs.Token("N", "1")])
    b = lark_rs.Tree("x", [lark_rs.Token("N", "1")])
    c = lark_rs.Tree("x", [lark_rs.Token("N", "2")])
    assert a == b
    assert a != c
    assert a.data == "x"
    assert len(a.children) == 1


def test_token_positions():
    parser = lark_rs.Lark(
        "start: WORD\nWORD: /[a-z]+/\n", parser="lalr", propagate_positions=True
    )
    tree = parser.parse("abc")
    tok = tree.children[0]
    assert tok.value == "abc"
    assert tok.line == 1
    assert tok.column == 1


def test_parse_error_raised():
    parser = lark_rs.Lark("start: \"a\"\n", parser="lalr")
    with pytest.raises(lark_rs.ParseError):
        parser.parse("b")


def test_grammar_error_raised():
    # An unterminated character class is an invalid regex — the core rejects it
    # at construction time, surfaced here as a GrammarError.
    with pytest.raises(lark_rs.GrammarError):
        lark_rs.Lark("start: A\nA: /[/\n", parser="lalr")


def test_start_as_list():
    parser = lark_rs.Lark(
        "a: \"x\"\nb: \"y\"\n", parser="lalr", start=["a", "b"]
    )
    assert parser.parse("x", start="a").data == "a"
    assert parser.parse("y", start="b").data == "b"
