#!/usr/bin/env python3
"""
Differential fuzzer for lark-rs — input generator + corpus grower.

This is the *discovery* tier of the differential fuzzer (run manually or on a
nightly schedule — never on the PR critical path). It generates inputs for the
trusted grammars, validates them against Python Lark (the oracle), and appends
interesting, de-duplicated inputs to the committed corpus:

    tests/fixtures/oracles/fuzz/inputs.json   (source of truth — grammar+input)

The actual differential comparison happens deterministically in Rust:

    python3 tools/fuzz_differential.py --grammar arithmetic -n 500 --write
    python3 tools/generate_oracles.py          # inputs.json -> fuzz/corpus.json
    cargo test --test test_fuzz_corpus         # replay + diff vs Python Lark
                                               #   RED == lark-rs diverges

So the loop is: grow the corpus here, regenerate the oracle, replay in Rust.
A red replay is a real divergence — minimize it (`--minimize`) and keep it; the
committed corpus then guards against that bug forever, exactly like the
strip-mined compliance bank.

Generation is deterministic given --seed, so a nightly job can reproduce a find.

Usage:
    python3 tools/fuzz_differential.py --grammar arithmetic -n 200
    python3 tools/fuzz_differential.py --grammar json -n 200 --mutate --write
    python3 tools/fuzz_differential.py --grammar arithmetic --minimize "1 +++ 2"
"""

import argparse
import json
import random
import sys
from pathlib import Path

SCRIPT_DIR = Path(__file__).parent
LARK_RS_DIR = SCRIPT_DIR.parent
INPUTS_PATH = LARK_RS_DIR / "tests" / "fixtures" / "oracles" / "fuzz" / "inputs.json"
GRAMMARS_DIR = LARK_RS_DIR / "tests" / "grammars"

# Import the in-repo Python Lark (our oracle), same as generate_oracles.py.
sys.path.insert(0, str(LARK_RS_DIR.parent))
from lark import Lark
from lark.exceptions import LarkError


# ─── Grammar-directed generators ─────────────────────────────────────────────
#
# Bespoke per-grammar generators emit mostly-valid strings (exercising the
# accept path and, crucially, tree *shape* — where lark-rs is most likely to
# drift: expand1, transparent splicing, operator precedence). A generic
# grammar-walker is a worthwhile future upgrade; these reliable hand generators
# get the differential loop paying off today.

_WS = ["", " ", "  ", " \t"]


def _sp(rng):
    return rng.choice(_WS)


def gen_arithmetic(rng, depth=4):
    def number():
        if rng.random() < 0.3:
            return f"{rng.randint(0, 999)}.{rng.randint(0, 99)}"
        return str(rng.randint(0, 9999))

    def name():
        first = rng.choice("abcxyz_")
        rest = "".join(rng.choice("abc_0123") for _ in range(rng.randint(0, 3)))
        return first + rest

    def atom(d):
        if d <= 0:
            return number() if rng.random() < 0.6 else name()
        r = rng.random()
        if r < 0.4:
            return number()
        if r < 0.6:
            return name()
        return f"({_sp(rng)}{expr(d - 1)}{_sp(rng)})"

    def factor(d):
        prefix = ""
        while rng.random() < 0.25:
            prefix += rng.choice(["+", "-"]) + _sp(rng)
        return prefix + atom(d)

    def term(d):
        out = factor(d)
        while rng.random() < 0.4:
            out += f"{_sp(rng)}{rng.choice(['*', '/'])}{_sp(rng)}{factor(d)}"
        return out

    def expr(d):
        out = term(d)
        while rng.random() < 0.5:
            out += f"{_sp(rng)}{rng.choice(['+', '-'])}{_sp(rng)}{term(d)}"
        return out

    return expr(depth)


def gen_json(rng, depth=4):
    def string():
        # ESCAPED_STRING — keep the body to safe, unambiguous characters.
        body = "".join(
            rng.choice("abc ABZ019_:-") for _ in range(rng.randint(0, 6))
        )
        return f'"{body}"'

    def number():
        s = "-" if rng.random() < 0.3 else ""
        s += str(rng.randint(0, 9999))
        if rng.random() < 0.3:
            s += f".{rng.randint(0, 999)}"
        return s

    def scalar():
        return rng.choice([string(), number(), "true", "false", "null"])

    def value(d):
        if d <= 0 or rng.random() < 0.5:
            return scalar()
        if rng.random() < 0.5:
            n = rng.randint(0, 3)
            items = [f"{_sp(rng)}{value(d - 1)}" for _ in range(n)]
            return "[" + ",".join(items) + _sp(rng) + "]"
        n = rng.randint(0, 3)
        pairs = [f"{_sp(rng)}{string()}:{_sp(rng)}{value(d - 1)}" for _ in range(n)]
        return "{" + ",".join(pairs) + _sp(rng) + "}"

    return value(depth)


GENERATORS = {
    "arithmetic": gen_arithmetic,
    "json": gen_json,
}


# ─── Mutation (near-valid inputs probe the reject path + boundaries) ──────────

_MUTATION_CHARS = list("+-*/(){}[],.:\"0a \t")


def mutate(rng, s):
    if not s:
        return rng.choice(_MUTATION_CHARS)
    chars = list(s)
    for _ in range(rng.randint(1, 3)):
        op = rng.randint(0, 3)
        i = rng.randrange(len(chars))
        if op == 0 and len(chars) > 1:  # delete
            del chars[i]
        elif op == 1:  # insert
            chars.insert(i, rng.choice(_MUTATION_CHARS))
        elif op == 2:  # duplicate
            chars.insert(i, chars[i])
        else:  # replace
            chars[i] = rng.choice(_MUTATION_CHARS)
        if not chars:
            chars = [rng.choice(_MUTATION_CHARS)]
    return "".join(chars)


# ─── Oracle plumbing ─────────────────────────────────────────────────────────

def load_parser(grammar):
    text = (GRAMMARS_DIR / f"{grammar}.lark").read_text()
    return Lark(text, parser="lalr", lexer="contextual", start="start",
                maybe_placeholders=False)


def parses(parser, text):
    try:
        parser.parse(text)
        return True
    except LarkError:
        return False


def minimize(parser, text, predicate):
    """ddmin-style shrink: smallest substring still satisfying `predicate`.

    The default predicate preserves parse-success, which trims a find down to a
    minimal *valid* core before committing it. A divergence-preserving predicate
    (one that also runs lark-rs) is the natural upgrade once an online Rust differ
    lands; the shrink loop itself is identical.
    """
    best = text
    changed = True
    while changed:
        changed = False
        n = max(1, len(best) // 2)
        while n >= 1:
            i = 0
            while i < len(best):
                cand = best[:i] + best[i + n:]
                if cand != best and predicate(cand):
                    best = cand
                    changed = True
                else:
                    i += n
            n //= 2
    return best


# ─── Corpus I/O ──────────────────────────────────────────────────────────────

def load_inputs():
    if INPUTS_PATH.exists():
        return json.loads(INPUTS_PATH.read_text())
    return []


def save_inputs(entries):
    INPUTS_PATH.parent.mkdir(parents=True, exist_ok=True)
    INPUTS_PATH.write_text(json.dumps(entries, indent=2, ensure_ascii=False) + "\n")


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--grammar", choices=sorted(GENERATORS), default="arithmetic")
    ap.add_argument("-n", "--count", type=int, default=100,
                    help="number of inputs to generate")
    ap.add_argument("--seed", type=int, default=0, help="RNG seed (deterministic)")
    ap.add_argument("--depth", type=int, default=4, help="max generation depth")
    ap.add_argument("--mutate", action="store_true",
                    help="also emit mutated (near-valid) variants")
    ap.add_argument("--write", action="store_true",
                    help="append new, de-duplicated inputs to inputs.json")
    ap.add_argument("--minimize", metavar="INPUT",
                    help="shrink INPUT to a minimal parse-preserving core and exit")
    args = ap.parse_args()

    parser = load_parser(args.grammar)

    if args.minimize is not None:
        pred = (lambda s: parses(parser, s)) if parses(parser, args.minimize) \
            else (lambda s: not parses(parser, s) and s != "")
        small = minimize(parser, args.minimize, pred)
        print(json.dumps({"grammar": args.grammar, "input": small}, ensure_ascii=False))
        return

    rng = random.Random(args.seed)
    gen = GENERATORS[args.grammar]

    generated = []
    for _ in range(args.count):
        s = gen(rng, args.depth)
        generated.append(s)
        if args.mutate and rng.random() < 0.5:
            generated.append(mutate(rng, s))

    existing = load_inputs()
    seen = {(e["grammar"], e["input"]) for e in existing}
    n_pass = 0
    added = []
    for s in generated:
        if parses(parser, s):
            n_pass += 1
        key = (args.grammar, s)
        if key not in seen:
            seen.add(key)
            added.append({"grammar": args.grammar, "input": s})

    print(f"generated {len(generated)} inputs for {args.grammar!r}: "
          f"{n_pass} parse, {len(generated) - n_pass} reject; "
          f"{len(added)} new after dedup")

    if args.write and added:
        save_inputs(existing + added)
        print(f"appended {len(added)} inputs -> {INPUTS_PATH.relative_to(LARK_RS_DIR)}")
        print("next: python3 tools/generate_oracles.py && "
              "cargo test --test test_fuzz_corpus")
    elif not args.write:
        for e in added[:20]:
            print(f"  {e['input']!r}")
        if len(added) > 20:
            print(f"  … and {len(added) - 20} more (pass --write to keep)")


if __name__ == "__main__":
    main()
