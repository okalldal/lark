#!/usr/bin/env python3
"""Time Python Lark on the cross-engine workloads (JSON / Python / SQL / NL-CYK).

This is the Python half of the Phase-4 cross-engine comparison (issue #50). The
Rust bench `cargo bench --bench vs_python_lark` drives it: it generates the three
workloads, writes them to a temp directory, times lark-rs, then invokes this
script with `--inputs <dir>` so Python Lark times the *byte-identical* inputs. It
also runs standalone (generating its own equivalent inputs):

    python3 benches/vs_python_lark.py             # generate inputs, time Python Lark
    python3 benches/vs_python_lark.py --inputs D  # time the files in dir D

The grammars here are byte-identical to the ones in `benches/vs_python_lark.rs`,
so the two engines parse the same language. Output: one machine-readable line per
workload —

    PYBENCH<TAB>name<TAB>bytes<TAB>median_ns<TAB>min_ns<TAB>mb_per_s

plus a human table. Like the oracle generators, it imports the *in-tree* Python
Lark (repo root on sys.path), so it is version-locked to this repo.
"""

import argparse
import os
import sys
import time
from pathlib import Path

# Import the in-tree Python Lark (the same oracle the test suite uses).
REPO_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO_ROOT))

import lark  # noqa: E402
from lark import Lark  # noqa: E402
from lark.indenter import PythonIndenter  # noqa: E402

# Python Lark's own grammars directory — added to `import_paths` so `python.lark`'s
# library imports resolve when loaded by absolute text (the in-tree copy).
LARK_GRAMMARS_DIR = os.path.join(os.path.dirname(lark.__file__), "grammars")

# ---------------------------------------------------------------------------
# Grammars — byte-identical to benches/vs_python_lark.rs
# ---------------------------------------------------------------------------

JSON_GRAMMAR = r"""
    ?start: value
    ?value: object
          | array
          | string
          | SIGNED_NUMBER  -> number
          | "true"         -> true
          | "false"        -> false
          | "null"         -> null
    array  : "[" [value ("," value)*] "]"
    object : "{" [pair ("," pair)*] "}"
    pair   : string ":" value
    string : ESCAPED_STRING
    %import common.ESCAPED_STRING
    %import common.SIGNED_NUMBER
    %import common.WS
    %ignore WS
"""

# The **real upstream** Python 3 grammar (issue #79): the in-tree, byte-for-byte
# copy that lark-rs bundles and `include_str!`s, so both engines parse the exact
# same grammar. Start symbol is `file_input`; the `PythonIndenter` postlex drives
# INDENT/DEDENT off `_NEWLINE`.
PY_GRAMMAR = (REPO_ROOT / "lark-rs" / "src" / "grammars" / "python.lark").read_text()

SQL_GRAMMAR = r"""
start: (stmt ";")+
?stmt: select_stmt | insert_stmt | update_stmt | delete_stmt
select_stmt: "SELECT" select_list "FROM" table_ref join* where_clause? group_by? order_by? limit_clause?
insert_stmt: "INSERT" "INTO" NAME "(" name_list ")" "VALUES" "(" value_list ")"
update_stmt: "UPDATE" NAME "SET" assignment ("," assignment)* where_clause?
delete_stmt: "DELETE" "FROM" NAME where_clause?
assignment: NAME "=" value
select_list: "*" | expr ("," expr)*
name_list: NAME ("," NAME)*
value_list: value ("," value)*
table_ref: NAME [NAME]
join: ("INNER" | "LEFT" | "RIGHT")? "JOIN" table_ref "ON" condition
where_clause: "WHERE" condition
group_by: "GROUP" "BY" expr ("," expr)*
order_by: "ORDER" "BY" order_term ("," order_term)*
order_term: expr ("ASC" | "DESC")?
limit_clause: "LIMIT" NUMBER
?condition: or_cond
?or_cond: and_cond ("OR" and_cond)*
?and_cond: comparison ("AND" comparison)*
?comparison: expr COMP_OP expr
           | expr "BETWEEN" expr "AND" expr -> between
           | expr "IN" "(" value_list ")"   -> in_list
           | "(" condition ")"
?expr: term (("+"|"-") term)*
?term: factor (("*"|"/") factor)*
?factor: NUMBER | STRING | column_ref | func_call | "(" expr ")"
column_ref: NAME ("." NAME)?
func_call: NAME "(" (select_list)? ")"
?value: NUMBER | STRING | "NULL" | "TRUE" | "FALSE"
COMP_OP: "=" | "!=" | "<>" | "<=" | ">=" | "<" | ">"
NAME: /[a-zA-Z_]\w*/
NUMBER: /\d+(\.\d+)?/
STRING: /'[^']*'/
COMMENT: /--[^\n]*/
%import common.WS
%ignore WS
%ignore COMMENT
"""

# A small ambiguous natural-language grammar (PP-attachment + coordination) — the
# realistic CYK/CKY use case. Byte-identical to NL_GRAMMAR in vs_python_lark.rs.
NL_GRAMMAR = r"""
start: s
?s: np vp
?np: nominal
   | np pp
   | np "and" np
nominal: DET NOUN | NOUN | ADJ nominal
?vp: VERB np
   | vp pp
   | VERB
pp: PREP np
DET:  "the" | "a" | "an"
ADJ:  "big" | "small" | "red" | "old"
NOUN: "man" | "dog" | "park" | "telescope" | "boy" | "hill" | "girl" | "cat"
VERB: "saw" | "watched" | "found" | "liked"
PREP: "in" | "with" | "on" | "near" | "by"
%import common.WS
%ignore WS
"""


# ---------------------------------------------------------------------------
# Input generators — mirror benches/vs_python_lark.rs (used only in standalone
# mode; with --inputs the Rust harness supplies byte-identical files).
# ---------------------------------------------------------------------------

def gen_json(records, fields):
    parts = []
    for r in range(records):
        obj = ", ".join(
            f'"key{f}": {r * 10 + f}, "name{f}": "value{r}_{f}"' for f in range(fields)
        )
        parts.append("{" + obj + "}")
    return "[" + ",".join(parts) + "]"


def gen_python(classes):
    # Exercises the **full** upstream python.lark (issue #79). Mirrors
    # gen_python in vs_python_lark.rs line-for-line — keep them byte-identical.
    out = []
    # Module header — imports + a top-level function (def-site *args/**kwargs;
    # method defs can't carry star-params after the leading `self`).
    out.append("import os")
    out.append("from typing import List, Dict")
    out.append("from collections import defaultdict as dd")
    out.append("")
    out.append("")
    out.append("def make(*args, **kwargs):")
    out.append("    return wrap(*args, **kwargs)")
    out.append("")
    for c in range(classes):
        out.append("")
        out.append("@register")
        out.append(f"class Account{c}(Base):")
        out.append(f'    tag: str = "acct{c}"')
        out.append("")
        out.append("    def __init__(self, owner, balance):")
        out.append("        self.owner = owner")
        out.append("        self.balance = balance")
        out.append("        self.history = [x for x in [1, 2, 3] if x is not None]")
        out.append("        self.meta = {k: v for k, v in zip(keys, vals)}")
        out.append("        self.tags = {t for t in [1, 2, 3]}")
        out.append("")
        out.append("    @property")
        out.append("    def label(self):")
        out.append('        return f"{self.owner}: {self.balance}"')
        out.append("")
        out.append("    async def sync(self, source):")
        out.append("        async with source.lock() as handle:")
        out.append("            data = await handle.read()")
        out.append("        async for chunk in source.stream():")
        out.append("            self.balance += chunk")
        out.append("        return data")
        out.append("")
        out.append("    def deposit(self, amount):")
        out.append("        if (total := self.balance + amount) > 0:")
        out.append("            self.balance = total")
        out.append("        else:")
        out.append('            raise ValueError("negative")')
        out.append("        return self.balance if self.balance else 0")
        out.append("")
        out.append("    def summarize(self, items):")
        out.append("        total = 0")
        out.append("        for it in items[1:]:")
        out.append("            total += it * 2")
        out.append("        squares = {n: n ** 2 for n in range(10)}")
        out.append("        evens = [n for n in range(20) if n % 2 == 0]")
        out.append("        first = items[::2]")
        out.append("        seq = [*evens, 0]")
        out.append('        pair = {**self.meta, "extra": 1}')
        out.append("        key = lambda p: p[1]")
        out.append('        assert total >= 0, "bad"')
        out.append("        while total > 1000:")
        out.append("            total -= 1")
        out.append("        try:")
        out.append("            result = total / len(items)")
        out.append("        except ZeroDivisionError as e:")
        out.append("            result = 0")
        out.append("        finally:")
        out.append("            handler = lambda x: x + total")
        out.append("        del first")
        out.append('        with open("log.txt") as fh:')
        out.append("            fh.write(str(result))")
        out.append("        return make(*evens, **squares)")
        out.append("")
    return "\n".join(out) + "\n"


def gen_sql(statements):
    templates = [
        "SELECT id, name, email FROM users WHERE age >= {n} AND status = 'active' ORDER BY name ASC LIMIT 100",
        "SELECT u.name, o.total FROM users u INNER JOIN orders o ON u.id = o.user_id WHERE o.total > {n} ORDER BY o.total DESC",
        "INSERT INTO products (id, name, price) VALUES ({n}, 'Widget', 9)",
        "UPDATE accounts SET balance = {n}, status = 'ok' WHERE id = {n}",
        "DELETE FROM sessions WHERE id = {n}",
        "SELECT COUNT(id), category FROM products WHERE price BETWEEN 10 AND {n} GROUP BY category ORDER BY category",
        "SELECT * FROM logs WHERE level IN ('warn', 'error') AND service = 'api' AND id > {n}",
    ]
    out = []
    for i in range(statements):
        out.append(templates[i % len(templates)].replace("{n}", str(i % 900 + 1)) + ";")
    return "\n".join(out) + "\n"


def gen_nl(pps):
    # One ambiguous sentence with `pps` trailing PPs. Mirrors gen_nl in
    # vs_python_lark.rs byte-for-byte. Short on purpose — CYK is O(n³).
    phrases = ["in the park", "with the telescope", "on the hill", "near the boy", "by the girl"]
    return "the man saw the dog" + "".join(" " + phrases[i % len(phrases)] for i in range(pps))


# ---------------------------------------------------------------------------
# Timing — mirrors the Rust harness (calibrate, then min/median over samples).
# ---------------------------------------------------------------------------

def measure(fn):
    iters = 1
    while True:
        t = time.perf_counter_ns()
        for _ in range(iters):
            fn()
        if time.perf_counter_ns() - t >= 1_000_000 or iters >= 1 << 22:
            break
        iters *= 2

    samples = []
    overall = time.perf_counter_ns()
    while len(samples) < 50 and time.perf_counter_ns() - overall < 1_500_000_000:
        t = time.perf_counter_ns()
        for _ in range(iters):
            fn()
        samples.append((time.perf_counter_ns() - t) / iters)
    samples.sort()
    return samples[0], samples[len(samples) // 2]


GRAMMARS = {"json": JSON_GRAMMAR, "python": PY_GRAMMAR, "sql": SQL_GRAMMAR, "nl": NL_GRAMMAR}

# (algo, workload, lexer, postlex, start) — mirrored in benches/vs_python_lark.rs.
# LALR + contextual on JSON/Python/SQL; Earley on the two workloads it can run
# cross-engine (JSON/basic, SQL/dynamic). Python uses the real upstream
# python.lark (start="file_input" + PythonIndenter); it has no Earley row: postlex
# is incompatible with the dynamic lexer, and the basic lexer can't drive the
# Indenter the way the workload needs — see the .rs module header. CYK runs the
# NL workload (the one genuinely ambiguous grammar that needs a general-CFG
# engine), bounded to a short sentence since CYK is O(n³).
CONFIGS = [
    ("lalr", "json", "contextual", False, "start"),
    ("lalr", "python", "contextual", True, "file_input"),
    ("lalr", "sql", "contextual", False, "start"),
    ("earley", "json", "basic", False, "start"),
    ("earley", "sql", "dynamic", False, "start"),
    ("cyk", "nl", "basic", False, "start"),
]


def build(algo, name, lexer, postlex, start):
    kwargs = dict(parser=algo, lexer=lexer, start=start, import_paths=[LARK_GRAMMARS_DIR])
    if postlex:
        kwargs["postlex"] = PythonIndenter()
    return Lark(GRAMMARS[name], **kwargs)


def emit(algo, name, nbytes, min_ns, median_ns):
    mb_per_s = nbytes / median_ns * 1e3
    print(f"PYBENCH\t{algo}\t{name}\t{nbytes}\t{median_ns:.0f}\t{min_ns:.0f}\t{mb_per_s:.1f}")
    print(
        f"  {algo:<7} {name:<7} {nbytes:>8} B   {median_ns:>12.0f} ns/iter "
        f"(min {min_ns:>12.0f})   {mb_per_s:>7.1f} MB/s"
    )


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--inputs", help="directory holding json.txt / python.txt / sql.txt")
    args = ap.parse_args()

    if args.inputs:
        d = Path(args.inputs)
        inputs = {
            "json": (d / "json.txt").read_text(),
            "python": (d / "python.txt").read_text(),
            "sql": (d / "sql.txt").read_text(),
            "nl": (d / "nl.txt").read_text(),
        }
    else:
        inputs = {
            "json": gen_json(512, 5),
            "python": gen_python(80),
            "sql": gen_sql(700),
            "nl": gen_nl(12),
        }

    print(f"# Python Lark {lark.__version__} cross-engine workloads (JSON / Python / SQL / NL-CYK)")
    print("# columns: PYBENCH<TAB>algo<TAB>name<TAB>bytes<TAB>median_ns<TAB>min_ns<TAB>mb_per_s")
    print()

    for algo, name, lexer, postlex, start in CONFIGS:
        parser = build(algo, name, lexer, postlex, start)
        text = inputs[name]
        parser.parse(text)  # fail loudly if the workload does not parse
        mn, md = measure(lambda p=parser, t=text: p.parse(t))
        emit(algo, name, len(text), mn, md)


if __name__ == "__main__":
    main()
