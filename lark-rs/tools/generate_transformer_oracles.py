#!/usr/bin/env python3
"""
Transformer oracle generator for lark-rs semantic-output tests.

Runs Python Lark with a set of small, deterministic "action specs" and writes
committed fixtures under tests/fixtures/oracles/transformer/.  Each fixture
case carries both the **final transformed value** and the **ordered callback
trace** (rule + token callbacks).

The action-spec format is deliberately small and language-neutral — no
arbitrary Python lambdas — so a Rust backend can implement the same spec
later.

Usage:
    python3 tools/generate_transformer_oracles.py

Output directory: tests/fixtures/oracles/transformer/
"""

from __future__ import annotations

import json
import os
import sys
from pathlib import Path
from typing import Any

# Allow running from lark-rs/ or from repo root.
SCRIPT_DIR = Path(__file__).parent
LARK_RS_DIR = SCRIPT_DIR.parent
FIXTURES_DIR = LARK_RS_DIR / "tests" / "fixtures"
ORACLES_DIR = FIXTURES_DIR / "oracles" / "transformer"

# Add the Python Lark source to path.
sys.path.insert(0, str(LARK_RS_DIR.parent))

from lark import Lark, Token, Tree, Discard, Transformer  # noqa: E402


# ---------------------------------------------------------------------------
# Action-spec language
# ---------------------------------------------------------------------------
# An "action spec" is a JSON-serialisable dict that tells a language-agnostic
# backend what to do for each callback.  Supported actions:
#
#   {"action": "identity"}                 — return the node unchanged
#   {"action": "return_value", "value": X} — return literal X
#   {"action": "sum_children"}             — return sum(children)
#   {"action": "product_children"}         — return product(children)
#   {"action": "join_children", "sep": S}  — return S.join(str(c) for c in children)
#   {"action": "concat_children"}          — return "".join(str(c) for c in children)
#   {"action": "first_child"}              — return children[0]
#   {"action": "int_value"}                — return int(token_value) [tokens only]
#   {"action": "float_value"}              — return float(token_value) [tokens only]
#   {"action": "upper"}                    — return token_value.upper() [tokens only]
#   {"action": "lower"}                    — return token_value.lower() [tokens only]
#   {"action": "prefix", "value": P}       — return P + token_value [tokens only]
#   {"action": "discard"}                  — return Discard sentinel
#   {"action": "wrap_list"}                — return list(children)
#   {"action": "wrap_dict", "key": K}      — return {K: children}
#   {"action": "stringify"}                — return str(value) [for tokens]
#   {"action": "default_rule"}             — use __default__ handler
#   {"action": "default_token"}            — use __default_token__ handler
#
# Each case also specifies the parser/lexer configuration and optionally
# grammar options like keep_all_tokens, maybe_placeholders.


def _apply_rule_action(action: dict, children: list, data: str, meta) -> Any:
    """Execute a rule action spec against already-transformed children."""
    kind = action["action"]
    if kind == "identity":
        return Tree(data, children)
    elif kind == "return_value":
        return action["value"]
    elif kind == "sum_children":
        return sum(children)
    elif kind == "product_children":
        result = 1
        for c in children:
            result *= c
        return result
    elif kind == "join_children":
        return action["sep"].join(str(c) for c in children)
    elif kind == "concat_children":
        return "".join(str(c) for c in children)
    elif kind == "first_child":
        return children[0] if children else None
    elif kind == "wrap_list":
        return list(children)
    elif kind == "wrap_dict":
        return {action["key"]: list(children)}
    elif kind == "discard":
        return Discard
    else:
        raise ValueError(f"Unknown rule action: {kind}")


def _apply_token_action(action: dict, token: Token) -> Any:
    """Execute a token action spec against a token."""
    kind = action["action"]
    if kind == "identity":
        return token
    elif kind == "int_value":
        return int(token)
    elif kind == "float_value":
        return float(token)
    elif kind == "upper":
        return str(token).upper()
    elif kind == "lower":
        return str(token).lower()
    elif kind == "prefix":
        return action["value"] + str(token)
    elif kind == "stringify":
        return str(token)
    elif kind == "discard":
        return Discard
    elif kind == "return_value":
        return action["value"]
    else:
        raise ValueError(f"Unknown token action: {kind}")


def _serialize_value(val: Any) -> Any:
    """Convert a transformed value to a JSON-serialisable form."""
    if val is None:
        return None
    if isinstance(val, bool):
        return val
    if isinstance(val, int):
        return val
    if isinstance(val, float):
        return val
    # Token must be checked before str — Token subclasses str in Python,
    # so isinstance(token, str) is True.  Checking Token first preserves
    # the token_type metadata that a Rust backend needs.
    if isinstance(val, Token):
        return {
            "type": "token",
            "token_type": str(val.type),
            "value": str(val),
        }
    if isinstance(val, str):
        return val
    if isinstance(val, list):
        return [_serialize_value(v) for v in val]
    if isinstance(val, dict):
        return {k: _serialize_value(v) for k, v in val.items()}
    if isinstance(val, Tree):
        return {
            "type": "tree",
            "data": val.data,
            "children": [_serialize_value(c) for c in val.children],
        }
    # Fallback for unknown types.
    return {"type": "unknown", "repr": repr(val)}


def build_transformer(
    rule_actions: dict[str, dict],
    token_actions: dict[str, dict],
    default_rule: dict | None,
    default_token: dict | None,
    trace: list,
    visit_tokens: bool = True,
):
    """Build a Transformer class dynamically from action specs.

    Returns a Transformer instance.  Every callback appends to *trace*
    (a shared list) before executing its action, giving the caller the
    ordered callback sequence.
    """

    class SpecTransformer(Transformer):
        __visit_tokens__ = visit_tokens

    # Wire up rule callbacks.
    for rule_name, action in rule_actions.items():
        _make_rule_method(SpecTransformer, rule_name, action, trace)

    # Wire up token callbacks.
    for token_name, action in token_actions.items():
        _make_token_method(SpecTransformer, token_name, action, trace)

    # Wire up __default__ if specified.
    if default_rule is not None:
        def __default__(self, data, children, meta, _action=default_rule, _trace=trace):
            _trace.append({"kind": "default_rule", "name": data})
            return _apply_rule_action(_action, children, data, meta)
        SpecTransformer.__default__ = __default__

    # Wire up __default_token__ if specified.
    if default_token is not None:
        def __default_token__(self, token, _action=default_token, _trace=trace):
            _trace.append({"kind": "default_token", "name": str(token.type)})
            return _apply_token_action(_action, token)
        SpecTransformer.__default_token__ = __default_token__

    return SpecTransformer()


def _make_rule_method(cls, rule_name, action, trace):
    """Attach a rule-handler method to the transformer class."""
    def method(self, children, _name=rule_name, _action=action, _trace=trace):
        _trace.append({"kind": "rule", "name": _name})
        # meta is None here — named rule callbacks in Python Lark receive only
        # `children`, not meta.  Only __default__ receives (data, children, meta).
        return _apply_rule_action(_action, children, _name, None)
    method.__name__ = rule_name
    setattr(cls, rule_name, method)


def _make_token_method(cls, token_name, action, trace):
    """Attach a token-handler method to the transformer class."""
    def method(self, token, _name=token_name, _action=action, _trace=trace):
        _trace.append({"kind": "token", "name": _name})
        return _apply_token_action(_action, token)
    method.__name__ = token_name
    setattr(cls, token_name, method)


# ---------------------------------------------------------------------------
# Test-case definitions
# ---------------------------------------------------------------------------
# Each case is a dict with:
#   name:             unique identifier
#   grammar:          Lark grammar text
#   input:            input string to parse
#   rule_actions:     {rule_name: action_spec}
#   token_actions:    {TOKEN_NAME: action_spec}
#   default_rule:     optional action_spec for __default__
#   default_token:    optional action_spec for __default_token__
#   parser_options:   extra Lark() kwargs (keep_all_tokens, maybe_placeholders, etc.)
#   visit_tokens:     whether __visit_tokens__ is True (default True)
#   description:      human-readable description of what this case tests
#
# The generator runs each case with parser='lalr' x lexer='basic'|'contextual'.


CASES = [
    # ── arithmetic evaluator ──────────────────────────────────────────
    {
        "name": "arithmetic_eval",
        "description": "Bottom-up arithmetic evaluator: tokens -> int, rules -> sum/product",
        "grammar": r"""
            start: expr
            expr: term "+" term
                | term
            term: factor "*" factor
                | factor
            factor: NUMBER
            NUMBER: /[0-9]+/
            %ignore /\s+/
        """,
        "input": "2 + 3 * 4",
        "rule_actions": {
            "start": {"action": "first_child"},
            "expr": {"action": "sum_children"},
            "term": {"action": "product_children"},
            "factor": {"action": "first_child"},
        },
        "token_actions": {
            "NUMBER": {"action": "int_value"},
        },
    },
    # ── JSON value builder ────────────────────────────────────────────
    {
        "name": "json_value_builder",
        "description": "JSON-like value builder: strings, numbers, arrays, objects",
        "grammar": r"""
            start: value
            ?value: string | number | array | object | "true" | "false" | "null"
            string: ESCAPED_STRING
            number: NUMBER
            array: "[" (value ("," value)*)? "]"
            object: "{" (pair ("," pair)*)? "}"
            pair: ESCAPED_STRING ":" value

            NUMBER: /\-?[0-9]+(\.[0-9]+)?/
            ESCAPED_STRING: "\"" /[^"]*/ "\""
            %ignore /\s+/
        """,
        "input": '{"key": 42}',
        "rule_actions": {
            "start": {"action": "first_child"},
            "string": {"action": "first_child"},
            "number": {"action": "first_child"},
            "array": {"action": "wrap_list"},
            "object": {"action": "wrap_dict", "key": "object"},
            "pair": {"action": "wrap_list"},
        },
        "token_actions": {
            "NUMBER": {"action": "int_value"},
            "ESCAPED_STRING": {"action": "stringify"},
        },
    },
    # ── token normalizer (upper) ──────────────────────────────────────
    {
        "name": "token_normalizer_upper",
        "description": "Token normalizer: uppercase all NAME tokens, rules identity",
        "grammar": r"""
            start: greeting+
            greeting: NAME PUNCT
            NAME: /[a-zA-Z]+/
            PUNCT: /[!.?]/
            %ignore /\s+/
        """,
        "input": "hello! world.",
        "rule_actions": {},
        "token_actions": {
            "NAME": {"action": "upper"},
        },
    },
    # ── alias (-> alias) ──────────────────────────────────────────────
    {
        "name": "alias",
        "description": "Rule alias: 'a -> alias_a' renames the tree node",
        "grammar": r"""
            start: item+
            item: "a" -> item_a
                | "b" -> item_b
            %ignore /\s+/
        """,
        "input": "a b a",
        "rule_actions": {
            "start": {"action": "wrap_list"},
            "item_a": {"action": "return_value", "value": "got_a"},
            "item_b": {"action": "return_value", "value": "got_b"},
        },
        "token_actions": {},
    },
    # ── transparent _rule ─────────────────────────────────────────────
    {
        "name": "transparent_rule",
        "description": "Transparent rule (_inner) inlines into parent",
        "grammar": r"""
            start: _inner+
            _inner: WORD
            WORD: /[a-z]+/
            %ignore /\s+/
        """,
        "input": "foo bar",
        "rule_actions": {
            "start": {"action": "join_children", "sep": ","},
        },
        "token_actions": {
            "WORD": {"action": "upper"},
        },
    },
    # ── ?expand1 ──────────────────────────────────────────────────────
    {
        "name": "expand1",
        "description": "?rule (expand1) collapses single-child rules",
        "grammar": r"""
            start: wrapper
            ?wrapper: inner
            inner: WORD
            WORD: /[a-z]+/
        """,
        "input": "hello",
        "rule_actions": {
            "start": {"action": "first_child"},
            "inner": {"action": "first_child"},
        },
        "token_actions": {
            "WORD": {"action": "upper"},
        },
    },
    # ── filtered punctuation ──────────────────────────────────────────
    {
        "name": "filtered_punctuation",
        "description": "Punctuation tokens (string literals) are filtered by default",
        "grammar": r"""
            start: item ("," item)*
            item: WORD
            WORD: /[a-z]+/
            %ignore /\s+/
        """,
        "input": "foo, bar, baz",
        "rule_actions": {
            "start": {"action": "join_children", "sep": "+"},
            "item": {"action": "first_child"},
        },
        "token_actions": {
            "WORD": {"action": "upper"},
        },
    },
    # ── keep_all_tokens ───────────────────────────────────────────────
    {
        "name": "keep_all_tokens",
        "description": "keep_all_tokens=True preserves punctuation tokens",
        "grammar": r"""
            start: item ("," item)*
            item: WORD
            WORD: /[a-z]+/
            %ignore /\s+/
        """,
        "input": "foo, bar",
        "rule_actions": {
            "start": {"action": "concat_children"},
            "item": {"action": "first_child"},
        },
        "token_actions": {
            "WORD": {"action": "identity"},
        },
        "parser_options": {"keep_all_tokens": True},
    },
    # ── maybe_placeholders true ───────────────────────────────────────
    {
        "name": "maybe_placeholders_true",
        "description": "maybe_placeholders=True: bracket-optional [X] missing -> None placeholder",
        "grammar": r"""
            start: WORD [NUMBER]
            WORD: /[a-z]+/
            NUMBER: /[0-9]+/
            %ignore /\s+/
        """,
        "input": "hello",
        "rule_actions": {
            "start": {"action": "wrap_list"},
        },
        "token_actions": {
            "WORD": {"action": "stringify"},
            "NUMBER": {"action": "int_value"},
        },
        "parser_options": {"maybe_placeholders": True},
    },
    # ── maybe_placeholders false ──────────────────────────────────────
    {
        "name": "maybe_placeholders_false",
        "description": "maybe_placeholders=False: bracket-optional [X] missing -> absent",
        "grammar": r"""
            start: WORD [NUMBER]
            WORD: /[a-z]+/
            NUMBER: /[0-9]+/
            %ignore /\s+/
        """,
        "input": "hello",
        "rule_actions": {
            "start": {"action": "wrap_list"},
        },
        "token_actions": {
            "WORD": {"action": "stringify"},
            "NUMBER": {"action": "int_value"},
        },
        "parser_options": {"maybe_placeholders": False},
    },
    # ── Discard token ─────────────────────────────────────────────────
    {
        "name": "discard_token",
        "description": "Token callback returns Discard, removing it from parent's children",
        "grammar": r"""
            start: item+
            item: WORD NOISE?
            WORD: /[a-z]+/
            NOISE: /[!@#]+/
            %ignore /\s+/
        """,
        "input": "hello! world",
        "rule_actions": {
            "start": {"action": "wrap_list"},
            "item": {"action": "first_child"},
        },
        "token_actions": {
            "WORD": {"action": "upper"},
            "NOISE": {"action": "discard"},
        },
    },
    # ── Discard rule ──────────────────────────────────────────────────
    {
        "name": "discard_rule",
        "description": "Rule callback returns Discard, removing it from parent's children",
        "grammar": r"""
            start: (keep | drop)+
            keep: WORD
            drop: NUMBER
            WORD: /[a-z]+/
            NUMBER: /[0-9]+/
            %ignore /\s+/
        """,
        "input": "hello 42 world",
        "rule_actions": {
            "start": {"action": "wrap_list"},
            "keep": {"action": "first_child"},
            "drop": {"action": "discard"},
        },
        "token_actions": {
            "WORD": {"action": "upper"},
            "NUMBER": {"action": "int_value"},
        },
    },
    # ── __default__ rule handler ──────────────────────────────────────
    {
        "name": "default_rule_handler",
        "description": "__default__ handles rules without explicit callbacks",
        "grammar": r"""
            start: alpha beta
            alpha: WORD
            beta: NUMBER
            WORD: /[a-z]+/
            NUMBER: /[0-9]+/
            %ignore /\s+/
        """,
        "input": "hello 42",
        "rule_actions": {
            "alpha": {"action": "first_child"},
        },
        "token_actions": {
            "WORD": {"action": "upper"},
            "NUMBER": {"action": "int_value"},
        },
        "default_rule": {"action": "wrap_list"},
    },
    # ── __default_token__ handler ─────────────────────────────────────
    {
        "name": "default_token_handler",
        "description": "__default_token__ handles tokens without explicit callbacks",
        "grammar": r"""
            start: item+
            item: WORD | NUMBER
            WORD: /[a-z]+/
            NUMBER: /[0-9]+/
            %ignore /\s+/
        """,
        "input": "hello 42 world",
        "rule_actions": {
            "start": {"action": "wrap_list"},
            "item": {"action": "first_child"},
        },
        "token_actions": {
            "WORD": {"action": "upper"},
        },
        "default_token": {"action": "prefix", "value": "tok:"},
    },
    # ── callback-order trace ──────────────────────────────────────────
    {
        "name": "callback_order_trace",
        "description": "Traces the exact bottom-up callback order across rules and tokens",
        "grammar": r"""
            start: first second
            first: A B
            second: C
            A: "a"
            B: "b"
            C: "c"
            %ignore /\s+/
        """,
        "input": "a b c",
        "rule_actions": {
            "start": {"action": "identity"},
            "first": {"action": "identity"},
            "second": {"action": "identity"},
        },
        "token_actions": {
            "A": {"action": "identity"},
            "B": {"action": "identity"},
            "C": {"action": "identity"},
        },
    },
    # ── C4 (a): keyword/identifier `unless` retyping ──────────────────
    {
        "name": "unless_keyword_retype",
        "description": (
            "A string-literal keyword terminal (IF) retypes a NAME match via "
            "Lark's `unless` mechanism; the token's retyped type drives callback "
            "dispatch — IF tokens hit the IF method, plain identifiers hit NAME"
        ),
        "grammar": r"""
            start: word+
            word: NAME | IF
            IF: "if"
            NAME: /[a-z]+/
            %ignore /\s+/
        """,
        "input": "if foo if bar",
        "rule_actions": {
            "start": {"action": "wrap_list"},
            "word": {"action": "first_child"},
        },
        "token_actions": {
            # Retyped keyword and plain identifier dispatch to distinct methods.
            "IF": {"action": "prefix", "value": "kw:"},
            "NAME": {"action": "upper"},
        },
    },
    # ── C4 (a): ignored token never surfaces (no callback) ────────────
    {
        "name": "ignored_token_no_callback",
        "description": (
            "A %ignore'd NAMED terminal (WS) never reaches the tree, so its "
            "callback never fires even though a WS method is defined — ignored "
            "tokens do not surface to the transformer"
        ),
        "grammar": r"""
            start: WORD+
            WORD: /[a-z]+/
            WS: /\s+/
            %ignore WS
        """,
        "input": "foo bar baz",
        "rule_actions": {
            "start": {"action": "wrap_list"},
        },
        "token_actions": {
            "WORD": {"action": "upper"},
            # A method exists for the ignored terminal; it must NOT be invoked.
            "WS": {"action": "discard"},
        },
    },
    # ── C4 (b): Discard from a token callback under maybe_placeholders ─
    {
        "name": "discard_token_maybe_placeholders",
        "description": (
            "A token callback returns Discard for a PRESENT optional [B]: the "
            "child is removed entirely (no None hole left behind), distinct from "
            "the maybe_placeholders None inserted only for an ABSENT optional"
        ),
        "grammar": r"""
            start: A [B] C
            A: "a"
            B: "b"
            C: "c"
            %ignore /\s+/
        """,
        "input": "a b c",
        "rule_actions": {
            "start": {"action": "wrap_list"},
        },
        "token_actions": {
            "A": {"action": "stringify"},
            # B is present in the input but its callback discards it.
            "B": {"action": "discard"},
            "C": {"action": "stringify"},
        },
        "parser_options": {"maybe_placeholders": True},
    },
    # ── C4 (b): the ABSENT-optional sibling under maybe_placeholders ──
    {
        "name": "discard_token_maybe_placeholders_absent",
        "description": (
            "Same grammar as discard_token_maybe_placeholders but with [B] "
            "ABSENT: maybe_placeholders inserts a None at B's position and the "
            "B token callback never fires — pins the sibling-position semantics"
        ),
        "grammar": r"""
            start: A [B] C
            A: "a"
            B: "b"
            C: "c"
            %ignore /\s+/
        """,
        "input": "a c",
        "rule_actions": {
            "start": {"action": "wrap_list"},
        },
        "token_actions": {
            "A": {"action": "stringify"},
            "B": {"action": "discard"},
            "C": {"action": "stringify"},
        },
        "parser_options": {"maybe_placeholders": True},
    },
    # ── C4 (c): template-expanded rule ────────────────────────────────
    {
        "name": "template_expanded",
        "description": (
            "A parameterized template `sep{item, COMMA}` expands but dispatches "
            "on the template SOURCE name (`sep`); the named COMMA punctuation is "
            "kept (a named terminal surfaces) while default-handled rules nest"
        ),
        "grammar": r"""
            start: sep{item, COMMA}
            sep{x, s}: x (s x)*
            item: WORD
            WORD: /[a-z]+/
            COMMA: ","
            %ignore /\s+/
        """,
        "input": "a, b, c",
        "rule_actions": {
            "start": {"action": "first_child"},
            "item": {"action": "first_child"},
            # `sep` is the template source name the engine dispatches on.
            "sep": {"action": "wrap_list"},
        },
        "token_actions": {
            "WORD": {"action": "upper"},
            "COMMA": {"action": "stringify"},
        },
    },
]


def run_case(case: dict, parser: str, lexer: str) -> dict:
    """Run a single test case and return the fixture entry."""
    parser_options = case.get("parser_options", {})
    visit_tokens = case.get("visit_tokens", True)

    # Build the Lark parser.
    lark_opts = {
        "parser": parser,
        "lexer": lexer,
        "start": "start",
        **parser_options,
    }
    try:
        lark_parser = Lark(case["grammar"], **lark_opts)
    except Exception as e:
        return {
            "status": "build_error",
            "error": str(e),
        }

    # Parse the input.
    try:
        tree = lark_parser.parse(case["input"])
    except Exception as e:
        return {
            "status": "parse_error",
            "error": str(e),
        }

    # Build and run the transformer.
    trace: list[dict] = []
    transformer = build_transformer(
        rule_actions=case.get("rule_actions", {}),
        token_actions=case.get("token_actions", {}),
        default_rule=case.get("default_rule"),
        default_token=case.get("default_token"),
        trace=trace,
        visit_tokens=visit_tokens,
    )

    try:
        result = transformer.transform(tree)
    except Exception as e:
        return {
            "status": "transform_error",
            "error": str(e),
        }

    entry = {
        "status": "ok",
        "value": _serialize_value(result),
        "trace": trace,
    }

    # Also run the *embedded* transformer path (transformer=… on the parser),
    # which Python wires differently from the post-parse `.transform(tree)` path:
    # `_get_lexer_callbacks` only attaches a terminal callback for terminals the
    # transformer defines an explicit method for, so the embedded path NEVER
    # invokes `__default_token__` (settling the RFC §5 open question — see C4 /
    # issue #229).  Recording both lets the fixture *pin* the divergence rather
    # than guess it.  Embedded transform requires the LALR parser (Python rejects
    # it on Earley/CYK), which every case here already uses.
    entry["embedded"] = run_case_embedded(case, parser, lexer)

    return entry


def run_case_embedded(case: dict, parser: str, lexer: str) -> dict:
    """Run the case through the *embedded* transformer path.

    The transformer is attached at parse time (`Lark(..., transformer=T())`), so
    Python applies callbacks during the parse rather than in a post-parse
    `.transform(tree)` walk.  For these specs the two paths differ in exactly one
    observable way: the embedded path does not wire `__default_token__`.
    """
    parser_options = case.get("parser_options", {})
    visit_tokens = case.get("visit_tokens", True)

    trace: list[dict] = []
    transformer = build_transformer(
        rule_actions=case.get("rule_actions", {}),
        token_actions=case.get("token_actions", {}),
        default_rule=case.get("default_rule"),
        default_token=case.get("default_token"),
        trace=trace,
        visit_tokens=visit_tokens,
    )

    lark_opts = {
        "parser": parser,
        "lexer": lexer,
        "start": "start",
        "transformer": transformer,
        **parser_options,
    }
    try:
        lark_parser = Lark(case["grammar"], **lark_opts)
    except Exception as e:
        return {"status": "build_error", "error": str(e)}

    try:
        result = lark_parser.parse(case["input"])
    except Exception as e:
        return {"status": "parse_error", "error": str(e)}

    return {
        "status": "ok",
        "value": _serialize_value(result),
        "trace": trace,
    }


def generate_all() -> dict:
    """Generate all fixture cases across parser x lexer configurations."""
    output = {
        "generator": "tools/generate_transformer_oracles.py",
        "schema_version": 1,
        "cases": [],
    }

    configs = [
        ("lalr", "basic"),
        ("lalr", "contextual"),
    ]

    for case in CASES:
        case_entry = {
            "name": case["name"],
            "description": case["description"],
            "grammar": case["grammar"],
            "input": case["input"],
            "rule_actions": case.get("rule_actions", {}),
            "token_actions": case.get("token_actions", {}),
            "default_rule": case.get("default_rule"),
            "default_token": case.get("default_token"),
            "visit_tokens": case.get("visit_tokens", True),
            "parser_options": case.get("parser_options", {}),
            "configs": {},
        }

        for parser, lexer in configs:
            config_key = f"{parser}_{lexer}"
            result = run_case(case, parser, lexer)
            case_entry["configs"][config_key] = result

        output["cases"].append(case_entry)

    return output


def main():
    ORACLES_DIR.mkdir(parents=True, exist_ok=True)

    data = generate_all()
    out_path = ORACLES_DIR / "cases.json"
    with open(out_path, "w") as f:
        json.dump(data, f, indent=2, sort_keys=False)
        f.write("\n")

    # Summary.
    n_cases = len(data["cases"])
    n_ok = sum(
        1
        for case in data["cases"]
        for cfg in case["configs"].values()
        if cfg["status"] == "ok"
    )
    n_total = sum(len(case["configs"]) for case in data["cases"])
    print(f"Wrote {out_path}: {n_cases} cases, {n_ok}/{n_total} configs ok")

    # Fail loudly if any case errored unexpectedly.
    for case in data["cases"]:
        for cfg_key, cfg_val in case["configs"].items():
            if cfg_val["status"] not in ("ok",):
                print(
                    f"  WARNING: {case['name']}:{cfg_key} -> {cfg_val['status']}: "
                    f"{cfg_val.get('error', '?')}"
                )


if __name__ == "__main__":
    main()
