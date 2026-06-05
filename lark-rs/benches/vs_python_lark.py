#!/usr/bin/env python3
"""Time Python Lark on the cross-engine workloads (JSON / Python / SQL).

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
import sys
import time
from pathlib import Path

# Import the in-tree Python Lark (the same oracle the test suite uses).
REPO_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO_ROOT))

import lark  # noqa: E402
from lark import Lark  # noqa: E402
from lark.indenter import Indenter  # noqa: E402

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

PY_GRAMMAR = r"""
start: _NL? stmt*
?stmt: simple_stmt | compound_stmt
simple_stmt: expr_stmt _NL
?expr_stmt: expr ("=" expr)* -> assign
          | "return" [expr]  -> return_stmt
          | "pass"           -> pass_stmt
?compound_stmt: func_def | class_def | if_stmt | for_stmt | while_stmt
func_def: "def" NAME "(" [params] ")" ":" suite
class_def: "class" NAME ["(" [arglist] ")"] ":" suite
if_stmt: "if" expr ":" suite ("elif" expr ":" suite)* ["else" ":" suite]
for_stmt: "for" NAME "in" expr ":" suite
while_stmt: "while" expr ":" suite
suite: _NL _INDENT stmt+ _DEDENT
params: NAME ("," NAME)*
arglist: expr ("," expr)*
?expr: or_test
?or_test: and_test ("or" and_test)*
?and_test: comparison ("and" comparison)*
?comparison: arith (comp_op arith)*
comp_op: "==" | "!=" | "<" | ">" | "<=" | ">="
?arith: term (("+"|"-") term)*
?term: factor (("*"|"/"|"%") factor)*
?factor: "-" factor | power
?power: trailer ("**" factor)?
?trailer: trailer "(" [arglist] ")" -> call
        | trailer "." NAME           -> getattr
        | trailer "[" expr "]"        -> getitem
        | atom
?atom: NAME | NUMBER | STRING | "True" | "False" | "None"
     | "(" expr ")"
     | "[" [arglist] "]" -> list
     | "{" [pair ("," pair)*] "}" -> dict
pair: expr ":" expr
LPAR: "("
RPAR: ")"
LSQB: "["
RSQB: "]"
LBRACE: "{"
RBRACE: "}"
NAME: /[a-zA-Z_]\w*/
NUMBER: /\d+(\.\d+)?/
STRING: /"[^"\n]*"/ | /'[^'\n]*'/
COMMENT: /#[^\n]*/
_NL: /(\r?\n[\t ]*)+/
%ignore /[\t ]+/
%ignore COMMENT
%declare _INDENT _DEDENT
"""

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


class PyIndenter(Indenter):
    NL_type = "_NL"
    OPEN_PAREN_types = ["LPAR", "LSQB", "LBRACE"]
    CLOSE_PAREN_types = ["RPAR", "RSQB", "RBRACE"]
    INDENT_type = "_INDENT"
    DEDENT_type = "_DEDENT"
    tab_len = 8


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
    out = []
    for c in range(classes):
        out.append(f"class Account{c}:")
        out.append("    def __init__(self, owner, balance):")
        out.append("        self.owner = owner")
        out.append("        self.balance = balance")
        out.append("")
        out.append("    def deposit(self, amount):")
        out.append("        if amount > 0:")
        out.append("            self.balance = self.balance + amount")
        out.append("            return self.balance")
        out.append("        else:")
        out.append("            return None")
        out.append("")
        out.append("    def summarize(self, items):")
        out.append("        total = 0")
        out.append("        for it in items:")
        out.append("            total = total + it * 2")
        out.append("            if total > 100:")
        out.append("                total = total - 1")
        out.append("        return total")
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


GRAMMARS = {"json": JSON_GRAMMAR, "python": PY_GRAMMAR, "sql": SQL_GRAMMAR}

# (algo, workload, lexer, postlex) — mirrored in benches/vs_python_lark.rs.
# LALR + contextual on all three; Earley on the two workloads it can run
# cross-engine (JSON/basic, SQL/dynamic). Python has no Earley row: postlex is
# incompatible with the dynamic lexer, and the basic lexer can't drive the
# Indenter the way the workload needs — see the .rs module header.
CONFIGS = [
    ("lalr", "json", "contextual", False),
    ("lalr", "python", "contextual", True),
    ("lalr", "sql", "contextual", False),
    ("earley", "json", "basic", False),
    ("earley", "sql", "dynamic", False),
]


def build(algo, name, lexer, postlex):
    kwargs = dict(parser=algo, lexer=lexer, start="start")
    if postlex:
        kwargs["postlex"] = PyIndenter()
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
        }
    else:
        inputs = {
            "json": gen_json(512, 5),
            "python": gen_python(220),
            "sql": gen_sql(700),
        }

    print(f"# Python Lark {lark.__version__} cross-engine workloads (JSON / Python / SQL)")
    print("# columns: PYBENCH<TAB>algo<TAB>name<TAB>bytes<TAB>median_ns<TAB>min_ns<TAB>mb_per_s")
    print()

    for algo, name, lexer, postlex in CONFIGS:
        parser = build(algo, name, lexer, postlex)
        text = inputs[name]
        parser.parse(text)  # fail loudly if the workload does not parse
        mn, md = measure(lambda p=parser, t=text: p.parse(t))
        emit(algo, name, len(text), mn, md)


if __name__ == "__main__":
    main()
