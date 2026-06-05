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

# CSV cases. These exercise transparent `_rule` inlining: `_anything` is a
# single-underscore rule whose children must be spliced into the parent `row`
# rather than appearing as a `Tree("_anything", …)` wrapper. `_SEPARATOR` / `_NL`
# are `_`-prefixed terminals that are filtered out.
#
# `_anything`'s alternatives overlap on bare letter runs (WORD vs
# NON_SEPARATOR_STRING); csv.lark gives NON_SEPARATOR_STRING an explicit priority so
# the choice is principled — both Python Lark and lark-rs honor priority first — and
# letter cells lex deterministically as NON_SEPARATOR_STRING in both.
CSV_CASES = [
    ("#a,b,c\n1,2,3\n",        True),
    ("#x\n1\n",                True),
    ("#h1,h2\n10,20\n30,40\n", True),
    ("#name,age\nfoo,42\n",    True),  # letter cell → NON_SEPARATOR_STRING (priority)
    ("",                       False),
    ("1,2,3\n",                False),  # missing header
]


# Keyword-vs-identifier cases. These exercise true maximal-munch lexing: a
# reserved word ("if", "else", "while") must NOT shadow a longer identifier that
# merely starts with it ("iffy", "elsewhere", "whiled"). A preference-order lexer
# that tries the keyword terminal first mis-tokenizes "iffy" as ["if", "fy"].
KEYWORDS_CASES = [
    ("iffy = 1;",            True),
    ("elsewhere = 2;",       True),
    ("whiled = 3;",          True),
    ("thenable = 4;",        True),
    ("if x then iffy = 5;",  True),
    ("while x do y = 6;",    True),
    ("if x then y = 7;",     True),
    ("",                     False),
    ("if x then",            False),
]


# Terminal-reference cases. These exercise terminals that reference *other*
# terminals (`GREETING: HELLO | HOWDY`, `HOWDY: HOW DY`, `WORD: LETTER+`): the
# referenced terminal's pattern is inlined into the referencing one (including
# scoped flags for a case-insensitive `"hey"i`), and a terminal referenced only by
# another terminal is pruned and never produces a token of its own.
TERMINAL_REFS_CASES = [
    ("hello world", True),
    ("howdy yall",  True),
    ("HEY there",   True),   # scoped case-insensitive inline match
    ("hey now",     True),
    ("",            False),
    ("hello",       False),  # missing trailing WORD
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


def generate_fuzz_corpus():
    """Derive the differential-fuzz oracle from the committed regression corpus.

    `tests/fixtures/oracles/fuzz/inputs.json` is the source of truth: a small,
    curated set of *minimized finds* (grammar + input + note) — inputs that
    actually exposed a lark-rs ↔ Python-Lark divergence — not random samples.
    Here we run each input through Python Lark and freeze the (ok, tree|error)
    result, exactly like the other oracles, so `cargo test --test
    test_fuzz_corpus` replays it without Python. This step is deterministic — it
    never generates new inputs — so the CI freshness gate stays stable;
    fuzz_differential.py is the only thing that adds inputs (via `--record`).

    Out-of-band discovery (the nightly job) points `LARK_FUZZ_INPUTS` at a
    throwaway scratch batch so the same freeze→replay pipeline can diff lark-rs
    against a large generated batch without committing the haystack.
    """
    inputs_path = Path(os.environ.get(
        "LARK_FUZZ_INPUTS", ORACLES_DIR / "fuzz" / "inputs.json"))
    if not inputs_path.exists():
        print("Fuzz inputs not found — skipping fuzz corpus")
        return

    print(f"Generating differential-fuzz corpus oracle from {inputs_path}...")
    entries = json.loads(inputs_path.read_text())
    parsers = {}  # grammar name -> Lark, built once per grammar
    results = []
    for entry in entries:
        grammar = entry["grammar"]
        if grammar not in parsers:
            parsers[grammar] = Lark(load_grammar(grammar), parser="lalr",
                                    lexer="contextual", start="start",
                                    maybe_placeholders=False)
        inp = entry["input"]
        try:
            tree = parsers[grammar].parse(inp)
            ok, payload = True, tree_to_dict(tree)
        except Exception as e:
            ok, payload = False, str(e)
        results.append({
            "grammar": grammar,
            "input": inp,
            "note": entry.get("note"),  # why this find is guarded (scratch: None)
            "ok": ok,
            "tree": payload if ok else None,
            "error": payload if not ok else None,
        })
    save_oracle("fuzz", "corpus", results)
    n_ok = sum(1 for r in results if r["ok"])
    print(f"  {len(results)} cases ({n_ok} parse, {len(results) - n_ok} reject)")


def run_case(grammar_text, input_text, parser_type="lalr", start="start"):
    """Return (ok, tree_dict_or_error_msg)."""
    try:
        lark = Lark(grammar_text, parser=parser_type, start=start, maybe_placeholders=False)
        tree = lark.parse(input_text)
        return True, tree_to_dict(tree)
    except Exception as e:
        return False, str(e)


# ─── Earley + SPPF (Phase 2, Sprint 0) ───────────────────────────────────────
#
# The Earley engine is the second USP: it parses any CFG, including ambiguous
# ones, and (with ambiguity='explicit') returns *every* derivation as an `_ambig`
# node. These curated oracles are the regression net Sprints 1–4 land against:
#
#   * an UNAMBIGUOUS grammar — Earley must produce the *same* single tree LALR
#     does (exercises the forest→tree walk through the shared TreeBuilder:
#     expand1, aliases, anonymous-token filtering).
#   * AMBIGUOUS grammars at ambiguity='resolve' (one tree, Lark's choice) and
#     ambiguity='explicit' (an `_ambig` node whose children are the alternative
#     derivations, in NO guaranteed order — the Rust matcher compares them as a
#     set). One grammar is ambiguous at the *root*, one *nested* below it.
#
# Each group records its `ambiguity` and `lexer` so the Rust replay
# (test_earley_oracle.rs) builds the parser the same way Python Lark did.

# Unambiguous expression grammar. `?sum`/`?product` exercise expand1 under the
# forest walk; `"+"`/`"*"` are anonymous tokens that must be filtered.
EARLEY_UNAMBIGUOUS_GRAMMAR = r"""
start: sum
?sum: product
    | sum "+" product   -> add
?product: atom
        | product "*" atom -> mul
atom: NUMBER
NUMBER: /[0-9]+/
%ignore " "
"""

# Textbook ambiguous grammar (S → S S | "a"): "aaa" has two parses. `!` keeps the
# "a" tokens so the two shapes are visible. Ambiguous at the *root*.
EARLEY_AMBIG_ROOT_GRAMMAR = r'!start: start start | "a"'

# Ambiguity nested below the start rule: `inner` is the ambiguous S→S S|"a", wrapped
# by anonymous "(" … ")" that get filtered, so the `_ambig` node appears as a
# *child* of `start`, not at the root.
EARLEY_AMBIG_NESTED_GRAMMAR = r"""
start: "(" inner ")"
!inner: inner inner | "a"
%ignore " "
"""

# ── Joop-Leo right-recursion danger zone (issue #58) ──────────────────────────
# These four grammars pin the *correctness* invariant the Leo optimization must
# preserve. Leo linearizes hand-written right recursion by short-circuiting the
# completer's item cascade — but the SPPF it builds must stay byte-identical to
# the non-Leo forest (Python Lark's Leo is dead code, so the oracle below is the
# non-Leo ground truth). Each grammar targets a way upstream's forest
# reconstruction historically broke (lark-parser/lark#397: "duplicate start
# symbols"):
#
#   * right_rec            — the plain win case `a: X a | X`; Leo SHOULD fire and
#                            the right-nested tree must be unchanged.
#   * right_rec_nullable   — the recursion terminates through a nullable rule
#                            (`a: X a | empty`, `empty:`), so Leo interacts with
#                            held (ε) completions. (The trailing-bar empty alt
#                            `a: X a |` is valid Lark but lark-rs's loader does not
#                            accept it yet — a separate gap — so we use a named
#                            empty rule, which both engines accept.)
#   * right_rec_transparent— recursion through a transparent `_tail` helper, whose
#                            node is inlined away: the tree is identical to
#                            right_rec, so the Leo path-reconstruction must rebuild
#                            an inlined intermediate node correctly.
#   * right_rec_ambig      — two parallel right-recursive chains
#                            (`start: a | b`, `a: X a | X`, `b: X b | X`): every
#                            input has a constant 2-way ambiguity at the root, so
#                            the `_ambig` forest stays shallow at all lengths (no
#                            dependence on lark-rs's ambiguity-flattening shape).
#                            Each chain is *individually* a deterministic reduction
#                            path, so Leo should fire on `a` and `b` independently
#                            while the root ambiguity is preserved exactly.
EARLEY_RIGHT_REC_GRAMMAR = r"""
start: a
a: X a | X
X: "x"
"""

EARLEY_RIGHT_REC_NULLABLE_GRAMMAR = r"""
start: a
a: X a | empty
empty:
X: "x"
"""

EARLEY_RIGHT_REC_TRANSPARENT_GRAMMAR = r"""
start: a
a: X _tail | X
_tail: a
X: "x"
"""

EARLEY_RIGHT_REC_AMBIG_GRAMMAR = r"""
start: a | b
a: X a | X
b: X b | X
X: "x"
"""

# (name, grammar, [(input, should_parse)])
EARLEY_GRAMMARS = [
    ("unambiguous", EARLEY_UNAMBIGUOUS_GRAMMAR, [
        ("1",        True),
        ("1 + 2",    True),
        ("1 + 2 * 3", True),
        ("",         False),
        ("1 +",      False),
    ]),
    ("ambig_root", EARLEY_AMBIG_ROOT_GRAMMAR, [
        ("a",   True),
        ("aa",  True),
        ("aaa", True),
        ("",    False),
    ]),
    ("ambig_nested", EARLEY_AMBIG_NESTED_GRAMMAR, [
        ("(a)",   True),
        ("(aaa)", True),
        ("()",    False),
    ]),
    ("right_rec", EARLEY_RIGHT_REC_GRAMMAR, [
        ("x",   True),
        ("xx",  True),
        ("xxx", True),
        ("xxxx", True),
        ("",    False),
    ]),
    ("right_rec_nullable", EARLEY_RIGHT_REC_NULLABLE_GRAMMAR, [
        ("",    True),
        ("x",   True),
        ("xx",  True),
        ("xxx", True),
    ]),
    ("right_rec_transparent", EARLEY_RIGHT_REC_TRANSPARENT_GRAMMAR, [
        ("x",   True),
        ("xx",  True),
        ("xxx", True),
    ]),
    ("right_rec_ambig", EARLEY_RIGHT_REC_AMBIG_GRAMMAR, [
        ("x",   True),
        ("xx",  True),
        ("xxx", True),
    ]),
]


def generate_earley():
    print("Generating Earley + SPPF oracles (resolve + explicit ambiguity)...")
    groups = []
    for name, grammar, cases in EARLEY_GRAMMARS:
        for ambiguity in ("resolve", "explicit"):
            built = []
            for inp, should_parse in cases:
                try:
                    lark = Lark(grammar, parser="earley", lexer="basic",
                                ambiguity=ambiguity, start="start",
                                maybe_placeholders=False)
                    tree = lark.parse(inp)
                    ok, payload = True, tree_to_dict(tree)
                except Exception as e:
                    ok, payload = False, str(e)
                if should_parse and not ok:
                    print(f"  WARNING: {name}/{ambiguity} expected to parse {inp!r}: {payload}")
                built.append({
                    "input": inp,
                    "should_parse": should_parse,
                    "ok": ok,
                    "tree": payload if ok else None,
                    "error": payload if not ok else None,
                })
            groups.append({
                "name": name,
                "grammar": grammar,
                "ambiguity": ambiguity,
                "lexer": "basic",
                "cases": built,
            })
    save_oracle("earley", "cases", groups)
    n_ambig = sum(
        1 for g in groups for c in g["cases"]
        if c["ok"] and c["tree"] and c["tree"].get("data") == "_ambig"
    )
    print(f"  {len(groups)} groups; {n_ambig} cases have an `_ambig` root forest")


# ─── Earley dynamic lexer (Phase 2, Sprint 5) ────────────────────────────────
#
# The dynamic lexer integrates scanning into the Earley loop: the terminals
# tried at each position are exactly those the parser predicts there, instead of
# a token stream fixed up front. These curated grammars exercise what only the
# dynamic lexer can do:
#
#   * overlapping terminals (`A: /a+/  B: /a+/`) that the basic lexer would
#     tokenize one fixed way — under `dynamic` the regex is greedy (one parse or
#     none), under `dynamic_complete` *every* segmentation is explored;
#   * `%ignore` interacting with the dynamic scanner (leading / inner / trailing
#     whitespace carried across the ignore);
#   * context-sensitive tokenization where a keyword and an identifier overlap
#     but the grammar position decides which terminal applies.
#
# Each group records the `lexer` ("dynamic" | "dynamic_complete") so the Rust
# replay (test_earley_dynamic.rs) builds the parser exactly as Python Lark did.

# Overlapping `/a+/` terminals — the canonical dynamic-lexer grammar.
DYN_OVERLAP_GRAMMAR = r"""
start: A B
A: /a+/
B: /a+/
"""

# Whitespace handling through the dynamic scanner.
DYN_WS_GRAMMAR = r"""
start: A B
A: "a"
B: "b"
%ignore " "
"""

# Arithmetic with ignored spaces, multi-digit numbers (variable-length tokens).
DYN_ARITH_GRAMMAR = r"""
start: sum
?sum: NUMBER | sum "+" NUMBER -> add
NUMBER: /[0-9]+/
%ignore " "
"""

# A keyword that is a prefix of an identifier; the rule position decides which
# terminal applies at each spot (dynamic lexing, no contextual-lexer state).
DYN_KEYWORD_GRAMMAR = r"""
start: "if" NAME
NAME: /[a-z]+/
%ignore " "
"""

# (name, grammar, lexer, [(input, should_parse)])
EARLEY_DYNAMIC_GRAMMARS = [
    ("overlap", DYN_OVERLAP_GRAMMAR, "dynamic", [
        ("aa", False),    # greedy A eats everything → B starves
        ("aaa", False),
    ]),
    ("overlap_complete", DYN_OVERLAP_GRAMMAR, "dynamic_complete", [
        ("aa", True),     # unique split a|a
        ("aaa", True),    # ambiguous: a|aa and aa|a
        ("a", False),
    ]),
    ("ws", DYN_WS_GRAMMAR, "dynamic", [
        ("ab", True),
        (" a b ", True),
        ("a  b", True),
        ("a", False),
    ]),
    ("arith", DYN_ARITH_GRAMMAR, "dynamic", [
        ("1", True),
        ("1 + 2", True),
        ("12 + 34", True),
        ("1 +", False),
    ]),
    ("keyword", DYN_KEYWORD_GRAMMAR, "dynamic", [
        ("if x", True),
        ("if foo", True),
        ("ifx", True),     # "if" then "x" — dynamic lexer splits at the rule boundary
    ]),
]


def generate_earley_dynamic():
    print("Generating Earley dynamic-lexer oracles...")
    groups = []
    for name, grammar, lexer, cases in EARLEY_DYNAMIC_GRAMMARS:
        for ambiguity in ("resolve", "explicit"):
            built = []
            for inp, should_parse in cases:
                try:
                    lark = Lark(grammar, parser="earley", lexer=lexer,
                                ambiguity=ambiguity, start="start",
                                maybe_placeholders=False)
                    tree = lark.parse(inp)
                    ok, payload = True, tree_to_dict(tree)
                except Exception as e:
                    ok, payload = False, str(e)
                if should_parse and not ok:
                    print(f"  WARNING: {name}/{lexer}/{ambiguity} expected to parse {inp!r}: {payload}")
                built.append({
                    "input": inp,
                    "should_parse": should_parse,
                    "ok": ok,
                    "tree": payload if ok else None,
                    "error": payload if not ok else None,
                })
            groups.append({
                "name": name,
                "grammar": grammar,
                "ambiguity": ambiguity,
                "lexer": lexer,
                "cases": built,
            })
    save_oracle("earley", "dynamic_cases", groups)
    print(f"  {len(groups)} dynamic-lexer groups")


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


def generate_csv():
    print("Generating CSV (transparent `_rule` inlining) oracles...")
    grammar = load_grammar("csv")
    results = []
    for inp, should_pass in CSV_CASES:
        ok, result = run_case(grammar, inp, parser_type="lalr")
        if should_pass and not ok:
            print(f"  WARNING: expected to parse {inp!r}: {result}")
        results.append({
            "input": inp,
            "should_pass": should_pass,
            "ok": ok,
            "tree": result if ok else None,
            "error": result if not ok else None,
        })
    save_oracle("csv", "cases", results)


def generate_keywords():
    print("Generating keyword/identifier (maximal-munch) oracles...")
    grammar = load_grammar("keywords")
    results = []
    for inp, should_pass in KEYWORDS_CASES:
        ok, result = run_case(grammar, inp, parser_type="lalr")
        if should_pass and not ok:
            print(f"  WARNING: expected to parse {inp!r}: {result}")
        results.append({
            "input": inp,
            "should_pass": should_pass,
            "ok": ok,
            "tree": result if ok else None,
            "error": result if not ok else None,
        })
    save_oracle("keywords", "cases", results)


def generate_terminal_refs():
    print("Generating terminal-reference oracles...")
    grammar = load_grammar("terminal_refs")
    results = []
    for inp, should_pass in TERMINAL_REFS_CASES:
        ok, result = run_case(grammar, inp, parser_type="lalr")
        if should_pass and not ok:
            print(f"  WARNING: expected to parse {inp!r}: {result}")
        results.append({
            "input": inp,
            "should_pass": should_pass,
            "ok": ok,
            "tree": result if ok else None,
            "error": result if not ok else None,
        })
    save_oracle("terminal_refs", "cases", results)


# ─── common.lark terminal library (Phase 3) ──────────────────────────────────
#
# Each user-facing terminal from `lark/grammars/common.lark` is exercised through
# a grammar that imports it (`start: TERM` / `%import common.TERM`) against
# representative valid + invalid inputs. lark-rs resolves `%import common.X` by
# parsing its *own* bundled copy of common.lark through the same terminal-algebra
# path it uses for user grammars, so these oracles pin that the bundled copy stays
# faithful to Python Lark rather than drifting (the old hand-transcribed regex
# table could).
#
# (name, [(input, should_parse)])
COMMON_TERMINAL_CASES = [
    ("DIGIT",         [("5", True), ("0", True), ("12", False), ("a", False), ("", False)]),
    ("HEXDIGIT",      [("a", True), ("F", True), ("9", True), ("g", False), ("G", False)]),
    ("INT",           [("123", True), ("0", True), ("12a", False), ("", False)]),
    ("SIGNED_INT",    [("5", True), ("+5", True), ("-5", True), ("++5", False)]),
    ("DECIMAL",       [("1.5", True), ("1.", True), (".5", True), ("1", False)]),
    ("FLOAT",         [("1e5", True), ("1.5", True), ("1.5e-3", True), (".5e3", True), ("1", False)]),
    ("SIGNED_FLOAT",  [("-1.5", True), ("+1e3", True), ("1.0", True), ("1", False)]),
    ("NUMBER",        [("1", True), ("1.5", True), ("1e5", True), ("a", False)]),
    ("SIGNED_NUMBER", [("-1", True), ("+1.5", True), ("2e2", True)]),
    ("LETTER",        [("a", True), ("Z", True), ("1", False), ("ab", False)]),
    ("WORD",          [("abc", True), ("Hello", True), ("ab1", False), ("", False)]),
    ("CNAME",         [("_foo", True), ("a1", True), ("Bar_2", True), ("1abc", False)]),
    ("LCASE_LETTER",  [("q", True), ("Q", False)]),
    ("UCASE_LETTER",  [("Q", True), ("q", False)]),
    ("WS_INLINE",     [(" ", True), ("\t", True), ("  \t", True), ("\n", False)]),
    ("WS",            [(" ", True), (" \n\t", True), ("\r\n", True), ("a", False)]),
    ("CR",            [("\r", True), ("\n", False)]),
    ("LF",            [("\n", True), ("\r", False)]),
    ("NEWLINE",       [("\n", True), ("\r\n", True), ("\n\n", True), ("a", False)]),
    ("SH_COMMENT",    [("# hi", True), ("#", True), ("// hi", False)]),
    ("CPP_COMMENT",   [("// hi", True), ("//", True), ("# hi", False)]),
    ("C_COMMENT",     [("/* hi */", True), ("/* a\nb */", True), ("/* x", False)]),
    ("SQL_COMMENT",   [("-- hi", True), ("--", True), ("# hi", False)]),
    # The bundled common.lark replaces Lark's lookbehind escaped-string helpers
    # (`(?<!\\)(\\\\)*?`) with a lookbehind-free equivalent (P3-1). These
    # adversarial cases lock the backslash-counting / newline edges against the
    # oracle so the adaptation can't silently diverge.
    ("ESCAPED_STRING", [
        ('"hi"', True), ('""', True), (r'"a\"b"', True), ('"x', False),
        (r'"a\\"', True),       # ends in an escaped backslash, then the real quote
        (r'"a\"', False),       # trailing \" escapes the quote → no closing quote
        (r'"\\"', True),        # body is a single escaped backslash
        (r'"\"', False),        # \" escapes the only quote → unterminated
        (r'"a\\\"b"', True),    # escaped backslash then escaped quote, then more
        (r'"a\nb"', True),      # \n is a two-char escape (backslash + 'n'), well-formed
        ('"a\nb"', False),      # a *raw* newline in the body is not allowed
        ('"a\\\n"', False),     # backslash directly before a raw newline
    ]),
]


def generate_common():
    print("Generating common.lark terminal oracles...")
    results = {}
    for name, cases in COMMON_TERMINAL_CASES:
        grammar = f"start: {name}\n%import common.{name}\n"
        recorded = []
        for inp, should_pass in cases:
            ok, result = run_case(grammar, inp, parser_type="lalr")
            if should_pass and not ok:
                print(f"  WARNING: {name} expected to parse {inp!r}: {result}")
            if not should_pass and ok:
                print(f"  WARNING: {name} expected to reject {inp!r}")
            recorded.append({
                "input": inp,
                "should_pass": should_pass,
                "ok": ok,
                "tree": result if ok else None,
                "error": result if not ok else None,
            })
        results[name] = recorded
    save_oracle("common", "cases", results)


# Relative file imports (`%import .module (...)`). Each case loads a grammar
# *from its file* (via Lark.open) so Python resolves imports relative to the
# grammar's directory — the behaviour lark-rs mirrors with LarkOptions.base_path.
IMPORTS_CASES = [
    # (grammar_file_relative_to_GRAMMARS_DIR, input, should_pass)
    ("imports/main.lark", "x = 42", True),        # import terminals NUMBER, NAME
    ("imports/main.lark", "1 = 2", False),        # NAME required on the left
    ("imports/rule_main.lark", "hello world !", True),  # import a rule + its deps
    ("imports/rule_main.lark", "world !", False),       # `greeting` needs "hello"
]


def run_file_case(grammar_file, input_text):
    """Return (ok, tree_dict_or_error) loading the grammar from its file path so
    relative imports resolve against the grammar's directory."""
    try:
        lark = Lark.open(str(GRAMMARS_DIR / grammar_file), parser="lalr",
                         maybe_placeholders=False)
        tree = lark.parse(input_text)
        return True, tree_to_dict(tree)
    except Exception as e:
        return False, str(e)


def generate_imports():
    print("Generating relative file-import oracles...")
    results = []
    for grammar_file, inp, should_pass in IMPORTS_CASES:
        ok, result = run_file_case(grammar_file, inp)
        if should_pass and not ok:
            print(f"  WARNING: expected {grammar_file} to parse {inp!r}: {result}")
        if not should_pass and ok:
            print(f"  WARNING: expected {grammar_file} to reject {inp!r}")
        results.append({
            "grammar": grammar_file,
            "input": inp,
            "should_pass": should_pass,
            "ok": ok,
            "tree": result if ok else None,
            "error": result if not ok else None,
        })
    save_oracle("imports", "cases", results)


# ─── Indenter / postlex (Phase 3) ────────────────────────────────────────────
#
# Python-style significant whitespace via `%declare`d INDENT/DEDENT terminals and
# the `Indenter` postlex hook. The oracle is generated with `lexer='basic'` (which
# lark-rs's postlex path uses): the lexer produces the whole token stream, the
# Indenter rewrites it, then the parser replays it. Both grammars `%declare
# _INDENT _DEDENT` and measure indentation off the `_NL` terminal.
#
# (grammar_file, open_paren_types, close_paren_types, [(input, should_parse)])
INDENTER_GROUPS = [
    ("indent", [], [], [
        ("a\n",                                   True),
        ("a\nb\n",                                True),
        ("if x:\n    a\n",                        True),   # single block
        ("if x:\n    a\n    b\nc\n",              True),   # multi-stmt block + dedent
        ("if x:\n    if y:\n        a\n    b\nc\n", True), # nested, multi-level dedent
        ("",                                      False),  # stmt+ needs one stmt
        ("a",                                     False),  # simple needs a newline
        ("if x:\nb\n",                            False),  # block body needs INDENT
        ("if x:\n    a\n  b\n",                   False),  # dedent to an unknown column
    ]),
    ("indent_paren", ["LPAR"], ["RPAR"], [
        ("f (x)\n",                               True),
        ("f (\n   x\n)\n",                        True),   # newlines inside parens ignored
        ("a\nf (y)\n",                            True),
    ]),
]

# Indenter grammars where the *contextual* lexer's state-narrowing is load-bearing
# (issue #67). `NAME` and `VALUE` are distinct regex terminals matching the same
# span, disambiguated only by parser state — so these are generated and replayed
# under `lexer='contextual'` only (the basic lexer cannot parse them; it always
# picks `NAME`). This pins that postlex and contextual narrowing interact, which
# the `indent`/`indent_paren` grammars cannot (there basic == contextual).
#
# (grammar_file, open_paren_types, close_paren_types, [(input, should_parse)])
INDENTER_CONTEXTUAL_GROUPS = [
    ("indent_context", [], [], [
        ("x = y\n",                               True),   # top-level assign (VALUE needs state)
        ("a = b\nc = d\n",                        True),   # two assigns
        ("if a:\n    x = y\n",                    True),   # block with one assign
        ("if a:\n    x = y\n    p = q\nz = w\n",  True),   # block + assigns + dedent
        ("if a:\nx = y\n",                        False),  # block body needs INDENT
    ]),
]



def generate_indenter():
    from lark.indenter import Indenter

    print("Generating Indenter / postlex oracles...")
    # One oracle suite per grammar file (suite name == grammar stem) so the
    # oracle-coverage meta-test maps each tests/grammars/<name>.lark to its dir.
    # The `indent`/`indent_paren` grammars are generated with `lexer='basic'` (the
    # lexer lark-rs's materialized postlex path uses, and which produces trees
    # identical to the contextual lexer for them). The `indent_context` grammar is
    # generated with `lexer='contextual'` — the basic lexer cannot parse it (#67).
    for groups, lexer in [(INDENTER_GROUPS, "basic"),
                          (INDENTER_CONTEXTUAL_GROUPS, "contextual")]:
        for name, open_types, close_types, cases in groups:
            grammar = load_grammar(name)

            class _TI(Indenter):
                NL_type = "_NL"
                OPEN_PAREN_types = open_types
                CLOSE_PAREN_types = close_types
                INDENT_type = "_INDENT"
                DEDENT_type = "_DEDENT"
                tab_len = 8

            built = []
            for inp, should_parse in cases:
                try:
                    lark = Lark(grammar, parser="lalr", lexer=lexer,
                                postlex=_TI(), start="start", maybe_placeholders=False)
                    tree = lark.parse(inp)
                    ok, payload = True, tree_to_dict(tree)
                except Exception as e:
                    ok, payload = False, str(e)
                if should_parse and not ok:
                    print(f"  WARNING: {name} expected to parse {inp!r}: {payload}")
                if not should_parse and ok:
                    print(f"  WARNING: {name} expected to reject {inp!r}")
                built.append({
                    "input": inp,
                    "should_parse": should_parse,
                    "ok": ok,
                    "tree": payload if ok else None,
                    "error": payload if not ok else None,
                })
            save_oracle(name, "cases", {
                "name": name,
                "open_paren_types": open_types,
                "close_paren_types": close_types,
                "cases": built,
            })


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
    generate_csv()
    generate_keywords()
    generate_terminal_refs()
    generate_common()
    generate_imports()
    generate_indenter()
    generate_python_numbers()
    generate_lalr_core()
    generate_earley()
    generate_earley_dynamic()
    generate_fuzz_corpus()
    generate_json_corpus_manifest()
    print("\nDone. Commit tests/fixtures/oracles/ to track expected outputs.")
