#!/usr/bin/env python3
"""
Oracle generator for lark-rs end-to-end tests.

Runs Python Lark against each (grammar, input) test case and serializes
the resulting parse tree to JSON. These JSON files are committed to the
repository and used by Rust tests to verify lark-rs produces identical output.

Usage:
    python3 tools/generate_oracles.py

Output directory: tests/fixtures/oracles/
"""

import json
import sys
import os
from pathlib import Path

# Allow running from lark-rs/ or from repo root
SCRIPT_DIR = Path(__file__).parent
LARK_RS_DIR = SCRIPT_DIR.parent
FIXTURES_DIR = LARK_RS_DIR / "tests" / "fixtures"
ORACLES_DIR = FIXTURES_DIR / "oracles"
GRAMMARS_DIR = LARK_RS_DIR / "tests" / "grammars"

# Add the Lark Python source to path
sys.path.insert(0, str(LARK_RS_DIR.parent))

from lark import Lark, Tree, Token


def tree_to_dict(node):
    """Recursively convert a Lark parse tree to a serialisable dict."""
    if isinstance(node, Tree):
        return {
            "type": "tree",
            "data": node.data,
            "children": [tree_to_dict(c) for c in node.children],
        }
    elif isinstance(node, Token):
        return {
            "type": "token",
            "token_type": str(node.type),
            "value": str(node),
        }
    else:
        return {"type": "unknown", "repr": repr(node)}


def make_parser(grammar_text, parser="lalr", lexer="contextual", start="start"):
    return Lark(grammar_text, parser=parser, lexer=lexer, start=start)


# ─── Grammar definitions ────────────────────────────────────────────────────

def load_grammar(name):
    path = GRAMMARS_DIR / f"{name}.lark"
    return path.read_text()

# ─── Test cases ────────────────────────────────────────────────────────────

ARITHMETIC_CASES = [
    # (input_text, expected to parse?)
    ("1 + 2",           True),
    ("1 + 2 * 3",       True),
    ("(1 + 2) * 3",     True),
    ("-1",              True),
    ("--1",             True),
    ("a + b",           True),
    ("a * b + c * d",   True),
    ("1 + 2 + 3 + 4",   True),
    ("",                False),
    ("1 +",             False),
    ("( 1 + 2",         False),
]

# From test_python_grammar.py — number literals
# Format: (input, parser_grammar, should_parse)
PYTHON_NUMBER_GRAMMAR = r"""
start: number+
number: INT | FLOAT | HEX | OCT | BIN | IMAG
INT: /[0-9][0-9_]*/
FLOAT: /[0-9][0-9_]*\.[0-9_]*/
     | /\.[0-9][0-9_]*/
     | /[0-9][0-9_]*[eE][+-]?[0-9][0-9_]*/
     | /[0-9][0-9_]*\.[0-9_]*[eE][+-]?[0-9][0-9_]*/
HEX: /0[xX][0-9a-fA-F][0-9a-fA-F_]*/
OCT: /0[oO][0-7][0-7_]*/
BIN: /0[bB][01][01_]*/
IMAG: /[0-9][0-9_]*[jJ]/
    | /[0-9][0-9_]*\.[0-9_]*[jJ]/
    | /\.[0-9][0-9_]*[jJ]/
%ignore /[ \t\n]+/
"""

PYTHON_NUMBER_VALID = [
    "0", "1", "42", "1000000",
    "0x0", "0xDEADBEEF", "0xdeadbeef", "0XABCDEF", "0X0",
    "0o0", "0o777", "0O123",
    "0b0", "0b101010", "0B1111",
    "3.14", "3.", ".14", "3.14e10", "3.14e+10", "3.14e-10",
    "3j", "3.14j", ".5j",
    "1_000_000", "1_0", "0x_1A", "0b_1010", "0o_17",
]

PYTHON_NUMBER_INVALID = [
    "0x",    # hex with no digits
    "0o9",   # invalid octal digit
    "0b2",   # invalid binary digit
    "._4",   # leading dot needs digit
    "3e",    # exponent with no digits
]

# JSON test cases (supplement to JSONTestSuite)
JSON_CASES = [
    ('{}',                          True),
    ('{"key": "value"}',            True),
    ('{"a": 1, "b": 2}',            True),
    ('[]',                          True),
    ('[1, 2, 3]',                   True),
    ('[true, false, null]',         True),
    ('{"nested": {"a": [1,2,3]}}',  True),
    ('"hello"',                     True),
    ('42',                          True),
    ('-3.14e10',                    True),
    ('{key: value}',                False),  # unquoted keys
    ('[1, 2,]',                     False),  # trailing comma
    ('',                            False),  # empty
    ('{"a": }',                     False),  # missing value
]


# ─── LALR core: LALR-but-not-SLR grammar + conflict outcome parity ───────────

# Dangling-else is the canonical grammar that is LALR(1) but NOT SLR(1):
# an SLR table reports a spurious shift/reduce conflict on it. Python Lark
# (parser='lalr') builds it cleanly, which proves our lookaheads are true LALR.
DANGLING_ELSE_GRAMMAR = r"""
start: stmt
?stmt: "if" cond "then" stmt           -> if_then
     | "if" cond "then" stmt "else" stmt -> if_then_else
     | "s"                              -> simple
cond: "c"
%import common.WS
%ignore WS
"""

DANGLING_ELSE_CASES = [
    ("s",                              True),
    ("if c then s",                    True),
    ("if c then s else s",             True),
    ("if c then if c then s else s",   True),
    ("if c then if c then s",          True),
    ("if c then",                      False),
]

# Grammars that exercise the conflict detector. `construct_error` records
# whether Python Lark raises at *construction* time (our outcome-parity oracle):
#   * genuine reduce/reduce        → Lark raises GrammarError
#   * reduce/reduce + rule priority → Lark resolves it, no error
#   * shift/reduce (dangling-else)  → Lark resolves as shift, no error
#   * unambiguous                   → no conflict
CONFLICT_GRAMMARS = {
    "reduce_reduce": r"""
start: a | b
a: X
b: X
X: "x"
""",
    "reduce_reduce_priority": r"""
start: a | b
a.2: X
b.1: X
X: "x"
""",
    "shift_reduce_dangling_else": DANGLING_ELSE_GRAMMAR,
    "clean": r"""
start: "a" "b"
""",
}


def generate_lalr_core():
    print("Generating LALR core oracles (dangling-else + conflict parity)...")
    cases = []
    for inp, should_pass in DANGLING_ELSE_CASES:
        ok, result = run_case(DANGLING_ELSE_GRAMMAR, inp, parser_type="lalr")
        if should_pass and not ok:
            print(f"  WARNING: dangling-else expected to parse {inp!r}: {result}")
        cases.append({
            "input": inp, "should_pass": should_pass, "ok": ok,
            "tree": result if ok else None, "error": result if not ok else None,
        })
    save_oracle("lalr_core", "dangling_else",
                {"grammar": DANGLING_ELSE_GRAMMAR, "cases": cases})

    conflicts = []
    for name, g in CONFLICT_GRAMMARS.items():
        try:
            Lark(g, parser="lalr", maybe_placeholders=False)
            construct_error, msg = False, None
        except Exception as e:
            construct_error = True
            msg = (str(e).splitlines() or [type(e).__name__])[0]
        conflicts.append({
            "name": name, "grammar": g,
            "construct_error": construct_error, "error": msg,
        })
    save_oracle("lalr_core", "conflicts", conflicts)


def run_case(grammar_text, input_text, parser_type="lalr", start="start"):
    """Return (ok, tree_dict_or_error_msg)."""
    try:
        lark = Lark(grammar_text, parser=parser_type, start=start, maybe_placeholders=False)
        tree = lark.parse(input_text)
        return True, tree_to_dict(tree)
    except Exception as e:
        return False, str(e)


def save_oracle(suite, name, data):
    out_dir = ORACLES_DIR / suite
    out_dir.mkdir(parents=True, exist_ok=True)
    path = out_dir / f"{name}.json"
    path.write_text(json.dumps(data, indent=2, ensure_ascii=False) + "\n")
    print(f"  wrote {path.relative_to(LARK_RS_DIR)}")


def generate_arithmetic():
    print("Generating arithmetic oracles...")
    grammar = load_grammar("arithmetic")
    results = []
    for i, (inp, should_pass) in enumerate(ARITHMETIC_CASES):
        ok, result = run_case(grammar, inp, parser_type="lalr")
        results.append({
            "input": inp,
            "should_pass": should_pass,
            "ok": ok,
            "tree": result if ok else None,
            "error": result if not ok else None,
        })
    save_oracle("arithmetic", "cases", results)


def generate_python_numbers():
    print("Generating Python number literal oracles...")
    valid = []
    for num in PYTHON_NUMBER_VALID:
        ok, result = run_case(PYTHON_NUMBER_GRAMMAR, num)
        if not ok:
            print(f"  WARNING: expected valid but got error for {num!r}: {result}")
        valid.append({"input": num, "should_pass": True, "ok": ok,
                      "tree": result if ok else None, "error": result if not ok else None})
    invalid = []
    for num in PYTHON_NUMBER_INVALID:
        ok, result = run_case(PYTHON_NUMBER_GRAMMAR, num)
        invalid.append({"input": num, "should_pass": False, "ok": ok,
                        "tree": result if ok else None, "error": result if not ok else None})

    save_oracle("python_numbers", "valid", valid)
    save_oracle("python_numbers", "invalid", invalid)


def generate_json():
    print("Generating JSON oracles...")
    grammar = load_grammar("json")
    results = []
    for i, (inp, should_pass) in enumerate(JSON_CASES):
        ok, result = run_case(grammar, inp, parser_type="lalr")
        results.append({
            "input": inp,
            "should_pass": should_pass,
            "ok": ok,
            "tree": result if ok else None,
            "error": result if not ok else None,
        })
    save_oracle("json", "cases", results)


def generate_json_corpus_manifest():
    """Write a manifest of JSONTestSuite files with their expected pass/fail."""
    corpus_dir = LARK_RS_DIR / "tests" / "corpora" / "JSONTestSuite" / "test_parsing"
    if not corpus_dir.exists():
        print("JSONTestSuite submodule not found — skipping corpus manifest")
        return

    print("Generating JSONTestSuite manifest...")
    grammar = load_grammar("json")
    lark = Lark(grammar, parser="lalr", start="start", maybe_placeholders=False)

    manifest = []
    for f in sorted(corpus_dir.iterdir()):
        if not f.suffix == ".json":
            continue
        prefix = f.name[0]  # y = must pass, n = must fail, i = implementation-defined
        try:
            text = f.read_text(errors="replace")
        except Exception:
            continue
        try:
            lark.parse(text)
            parse_ok = True
        except Exception:
            parse_ok = False

        must_pass = (prefix == "y")
        must_fail = (prefix == "n")
        correct = (must_pass and parse_ok) or (must_fail and not parse_ok) or (prefix == "i")

        manifest.append({
            "file": f.name,
            "prefix": prefix,
            "python_lark_ok": parse_ok,
            "correct_for_prefix": correct,
        })

    save_oracle("json_corpus", "manifest", manifest)
    passed = sum(1 for m in manifest if m["correct_for_prefix"])
    print(f"  {passed}/{len(manifest)} files correctly handled by Python Lark")


if __name__ == "__main__":
    ORACLES_DIR.mkdir(parents=True, exist_ok=True)
    generate_arithmetic()
    generate_json()
    generate_python_numbers()
    generate_lalr_core()
    generate_json_corpus_manifest()
    print("\nDone. Commit tests/fixtures/oracles/ to track expected outputs.")
