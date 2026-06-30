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


# ─── #416: Token IS-A str (eq/hash invariant) + folded surface gaps ──────────
#
# Oracle: Python Lark 1.3.1. `Token` is a genuine `str` subclass, so
# `isinstance`/`__hash__`/`__eq__` all derive from `str`, set/dict membership
# keyed by the token's own string value works, and the constructor accepts
# Python's optional position kwargs. These tests are written failed-first
# against the pre-#416 binding (standalone class + Rust DefaultHasher).


def _word_token():
    """A real parsed token from the engine, not a hand-built one."""
    parser = lark_rs.Lark(
        "start: WORD\nWORD: /[a-z]+/\n", parser="lalr"
    )
    return parser.parse("hello").children[0]


def test_token_is_str_subclass():
    """H7-4: isinstance(tok, str) — the headline contract Python Lark gives."""
    tok = _word_token()
    assert isinstance(tok, str)


def test_token_set_membership():
    """H7-4: `tok in {tok.value}` must hold (was False: Rust hash != str hash)."""
    tok = _word_token()
    assert tok == "hello"  # already worked
    assert tok in {"hello"}  # FAILED pre-fix — silent wrong result
    assert tok not in {"goodbye"}


def test_token_dict_key_membership():
    """H7-4: a token finds its own value's dict entry (was KeyError)."""
    tok = _word_token()
    assert {"hello": 1}[tok] == 1  # FAILED pre-fix — KeyError


def test_token_hash_matches_str_hash():
    """H7-4: hash(tok) == hash(tok.value) — the root cause of the set/dict break."""
    tok = _word_token()
    assert hash(tok) == hash("hello")


def test_constructed_token_set_membership():
    """H7-4: a hand-built Token honors the same str contract."""
    tok = lark_rs.Token("WORD", "hello")
    assert isinstance(tok, str)
    assert tok in {"hello"}
    assert {"hello": 1}[tok] == 1


def test_token_repr_single_quote_style():
    """H7-4c: repr() uses Python single-quote style, not Rust {:?} double quotes."""
    tok = lark_rs.Token("WORD", "ab")
    assert repr(tok) == "Token('WORD', 'ab')"


def test_token_ctor_position_kwargs():
    """H7-4d: constructor accepts Python's optional position kwargs."""
    tok = lark_rs.Token(
        "WORD",
        "ab",
        start_pos=3,
        line=2,
        column=4,
        end_line=2,
        end_column=6,
        end_pos=5,
    )
    assert tok.start_pos == 3
    assert tok.line == 2
    assert tok.column == 4
    assert tok.end_line == 2
    assert tok.end_column == 6
    assert tok.end_pos == 5


def test_token_ctor_defaults_are_none():
    """H7-4d: omitted position kwargs default to None, matching Python Lark."""
    tok = lark_rs.Token("WORD", "ab")
    assert tok.start_pos is None
    assert tok.line is None
    assert tok.column is None
    assert tok.end_line is None
    assert tok.end_column is None
    assert tok.end_pos is None


def test_tree_has_meta():
    """H7-4b: Tree always carries a .meta (Python's Tree always has a Meta)."""
    parser = lark_rs.Lark("start: WORD\nWORD: /[a-z]+/\n", parser="lalr")
    tree = parser.parse("hello")
    assert hasattr(tree, "meta")
    # Without propagate_positions the Meta is empty, exactly like Python Lark.
    assert tree.meta.empty is True


def test_constructed_tree_has_meta():
    """H7-4b: a hand-built Tree also carries a .meta."""
    tree = lark_rs.Tree("x", [lark_rs.Token("N", "1")])
    assert hasattr(tree, "meta")
    assert tree.meta.empty is True


@pytest.mark.parametrize("text", ["hello", "abc", "x"])
def test_token_str_contract_matches_oracle(text):
    """H7-4: the full str contract is byte-identical to Python Lark's Token."""
    rs = lark_rs.Token("WORD", text)
    py = lark.Token("WORD", text)
    assert isinstance(rs, str) == isinstance(py, str)
    assert hash(rs) == hash(py)
    assert (rs in {text}) == (py in {text})
    assert repr(rs) == repr(py)


def test_token_eq_is_type_aware():
    """#416: type-aware equality, like Python Lark — same text + different type
    are NOT equal, but a token still equals a plain str of its value."""
    a = lark_rs.Token("A", "x")
    b = lark_rs.Token("B", "x")
    same = lark_rs.Token("A", "x")
    # Oracle parity:
    pa, pb = lark.Token("A", "x"), lark.Token("B", "x")
    assert (a == b) == (pa == pb) == False  # noqa: E712
    assert (a == same) is True
    assert a == "x"  # still equals its plain-str value
    # Overriding __eq__ must not have made the token unhashable (would re-break
    # the #416 set/dict membership fix).
    assert a in {"x"}
    assert {"x": 1}[a] == 1


def test_token_deepcopy_roundtrips():
    """#416: copy.deepcopy preserves type + positions, matching Python Lark."""
    import copy

    tok = lark_rs.Token("WORD", "ab", start_pos=3, line=2, column=4)
    d = copy.deepcopy(tok)
    assert d == tok and d.type == "WORD" and d.value == "ab"
    assert d.start_pos == 3 and d.line == 2 and d.column == 4


def test_token_pickle_roundtrips():
    """#416: a token round-trips through pickle, matching Python Lark."""
    import pickle

    tok = lark_rs.Token("WORD", "ab", start_pos=3, line=2, column=4)
    p = pickle.loads(pickle.dumps(tok))
    assert isinstance(p, lark_rs.Token)
    assert p == tok and p.type == "WORD"
    assert p.start_pos == 3 and p.line == 2 and p.column == 4


def test_tree_eq_distinguishes_token_type():
    """#416 follow-on: type-aware token eq must propagate into Tree.__eq__ so two
    trees differing only in a child token's TYPE are not equal."""
    a = lark_rs.Tree("x", [lark_rs.Token("A", "v")])
    b = lark_rs.Tree("x", [lark_rs.Token("B", "v")])
    assert a != b
