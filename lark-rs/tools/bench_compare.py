#!/usr/bin/env python3
"""Time Python Lark on the same grammars/inputs as benches/parse.rs.

This is the other half of the perf baseline: `cargo bench --bench parse` gives the
Rust numbers, this gives Python Lark's, and the ratio is the defensible "10-100x"
story. It deliberately mirrors the grammars and input generators in
benches/parse.rs so the two tables line up name-for-name.

Like the oracle generators, it imports the *in-tree* Python Lark (repo root on
sys.path), so it is version-locked to this repo rather than to a pip install.

    python3 tools/bench_compare.py            # human table + BENCH<TAB>… lines

Compare row-by-row with the cargo bench output; a Rust median of R ns and a
Python median of P ns means lark-rs is P/R times faster on that workload.
"""

import sys
import time
from pathlib import Path

# Import the in-tree Python Lark (the same oracle the test suite uses).
REPO_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO_ROOT))

from lark import Lark  # noqa: E402

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

ARITH_GRAMMAR = r"""
    ?start : expr
    ?expr  : expr "+" term  -> add
           | expr "-" term  -> sub
           | term
    ?term  : term "*" factor -> mul
           | term "/" factor -> div
           | factor
    ?factor : "+" factor    -> pos
            | "-" factor    -> neg
            | atom
    ?atom  : NUMBER
           | NAME
           | "(" expr ")"
    %import common.NUMBER
    %import common.CNAME -> NAME
    %import common.WS_INLINE
    %ignore WS_INLINE
"""


def gen_json(records, fields):
    parts = []
    for r in range(records):
        obj = ", ".join(
            f'"key{f}": {r * 10 + f}, "name{f}": "value{r}_{f}"' for f in range(fields)
        )
        parts.append("{" + obj + "}")
    return "[" + ",".join(parts) + "]"


def gen_arith(terms):
    ops = ["+", "*", "-", "/"]
    s = "1"
    for i in range(terms):
        s += f" {ops[i % len(ops)]} {i % 9 + 2}"
    return s


def build(grammar):
    return Lark(grammar, parser="lalr", lexer="contextual")


def measure(fn):
    """min/median ns per call; mirrors the Rust harness (calibrate, then sample)."""
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


def emit(kind, name, nbytes, min_ns, median_ns):
    mb_per_s = (nbytes / median_ns * 1e3) if kind == "parse" else 0.0
    print(f"BENCH\t{kind}\t{name}\t{nbytes}\t{median_ns:.0f}\t{min_ns:.0f}\t{mb_per_s:.1f}")
    if kind == "parse":
        print(
            f"  parse  {name:<16} {nbytes:>8} B   {median_ns:>10.0f} ns/iter "
            f"(min {min_ns:>10.0f})   {mb_per_s:>7.1f} MB/s"
        )
    else:
        print(
            f"  build  {name:<16} {nbytes:>8} B   {median_ns:>10.0f} ns/iter "
            f"(min {min_ns:>10.0f})"
        )


def main():
    print("# Python Lark parse benchmarks (LALR + contextual lexer)")
    print("# columns: BENCH<TAB>kind<TAB>name<TAB>bytes<TAB>median_ns<TAB>min_ns<TAB>mb_per_s")
    print("# compare row-by-row with `cargo bench --bench parse`: speedup = python_median / rust_median")
    print()

    print("Construction (Lark(...)):")
    for name, grammar in (("json", JSON_GRAMMAR), ("arithmetic", ARITH_GRAMMAR)):
        mn, md = measure(lambda g=grammar: build(g))
        emit("build", name, len(grammar), mn, md)
    print()

    print("Parsing (build once, parse many):")
    json = build(JSON_GRAMMAR)
    for name, records, fields in (
        ("json_small", 4, 3),
        ("json_medium", 64, 4),
        ("json_large", 512, 5),
    ):
        inp = gen_json(records, fields)
        mn, md = measure(lambda i=inp: json.parse(i))
        emit("parse", name, len(inp), mn, md)

    arith = build(ARITH_GRAMMAR)
    for name, terms in (("arith_small", 8), ("arith_large", 512)):
        inp = gen_arith(terms)
        mn, md = measure(lambda i=inp: arith.parse(i))
        emit("parse", name, len(inp), mn, md)


if __name__ == "__main__":
    main()
