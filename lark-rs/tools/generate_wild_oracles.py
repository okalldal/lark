#!/usr/bin/env python3
"""
Wild-grammar bank oracle generator.

`tests/wild/<project>/` vendors real-world Lark grammars strip-mined from open
source projects (HCL2/Terraform, MapServer mapfiles, GraphQL SDL, PEP 508,
MistQL, Synapse Storm, Vyper, Quil — see each project's `meta.json` for the
upstream repo, commit pin, license, and the exact Lark options the project
itself uses). This script replays every vendored input through Python Lark
(the repo checkout — our oracle) and freezes the result to
`tests/fixtures/oracles/wild/<project>.json`, so `cargo test --test test_wild`
replays the bank without Python.

Big parse trees are frozen as a *digest* rather than full JSON, to keep the
committed fixtures small:

  * `node_count` / `token_count` — subtree and leaf-token totals
  * `canon_len` / `fnv1a64`      — length and FNV-1a 64 hash of the canonical
                                   serialization defined below

Canonical tree serialization (mirrored byte-for-byte by tests/test_wild.rs —
length-prefixed so no escaping is ever needed):

    canon(Token) = "T" + token_type + US + dec(len(utf8(value))) + US + value
    canon(Tree)  = "N" + data       + US + dec(len(children))    + "[" +
                   concat(canon(child) for child) + "]"
    canon(None)  = "_"              (a maybe_placeholders placeholder)

where US is "\x1f" and dec() is the decimal ASCII rendering. The hash is
FNV-1a 64 over the UTF-8 bytes, rendered as 16 lowercase hex digits. Trees
whose canonical form is small (<= EMBED_LIMIT bytes) are additionally embedded
as full JSON for direct diffing in test failures.

Usage:
    python3 tools/generate_wild_oracles.py

Requirements: the `regex` package (pip install regex) for grammars whose
upstream loads them with `regex=True` (synapse_storm).
"""

import json
import sys
from pathlib import Path

SCRIPT_DIR = Path(__file__).parent
LARK_RS_DIR = SCRIPT_DIR.parent
WILD_DIR = LARK_RS_DIR / "tests" / "wild"
ORACLES_DIR = LARK_RS_DIR / "tests" / "fixtures" / "oracles" / "wild"

# Use the repo checkout of Python Lark as the oracle, like generate_oracles.py.
sys.path.insert(0, str(LARK_RS_DIR.parent))

from lark import Lark, Tree, Token  # noqa: E402

EMBED_LIMIT = 16_384  # canonical bytes; smaller trees also embed full JSON

# Don't embed trees deeper than this: each tree level costs ~2 levels of JSON
# nesting and serde_json refuses documents deeper than 128 (Python has no such
# limit, so e.g. CEL's non-collapsed precedence cascade nests 100+ levels).
# Deep trees are still verified — by digest.
EMBED_DEPTH_LIMIT = 55

US = "\x1f"


def canon(node) -> str:
    """The canonical serialization defined in the module docstring."""
    if isinstance(node, Tree):
        body = "".join(canon(c) for c in node.children)
        return f"N{node.data}{US}{len(node.children)}{US}[{body}]"
    if isinstance(node, Token):
        value = str(node)
        return f"T{node.type}{US}{len(value.encode('utf-8'))}{US}{value}"
    if node is None:
        return "_"
    raise TypeError(f"unexpected tree node: {node!r}")


def fnv1a64(data: bytes) -> str:
    h = 0xCBF29CE484222325
    for b in data:
        h ^= b
        h = (h * 0x100000001B3) & 0xFFFFFFFFFFFFFFFF
    return f"{h:016x}"


def tree_depth(node):
    if isinstance(node, Tree):
        return 1 + max((tree_depth(c) for c in node.children), default=0)
    return 1


def counts(node):
    """(tree_node_count, token_count); placeholders count as neither."""
    if isinstance(node, Tree):
        n, t = 1, 0
        for c in node.children:
            cn, ct = counts(c)
            n += cn
            t += ct
        return n, t
    if isinstance(node, Token):
        return 0, 1
    return 0, 0


def tree_to_dict(node):
    """Same JSON shape as generate_oracles.py (incl. the None placeholder)."""
    if isinstance(node, Tree):
        return {
            "type": "tree",
            "data": node.data,
            "children": [tree_to_dict(c) for c in node.children],
        }
    if isinstance(node, Token):
        return {"type": "token", "token_type": str(node.type), "value": str(node)}
    return {"type": "unknown", "repr": repr(node)}


def build_parser(pdir: Path, meta: dict) -> Lark:
    opts = meta["lark_options"]
    kwargs = dict(
        parser=opts["parser"],
        lexer=opts["lexer"],
        start=opts["start"],
        maybe_placeholders=opts["maybe_placeholders"],
        propagate_positions=opts["propagate_positions"],
        keep_all_tokens=opts["keep_all_tokens"],
    )
    if opts.get("g_regex_flags"):
        # Canonical `imsx` letters, like the compliance bank records them.
        import re as _re

        letter_flags = {"i": _re.I, "m": _re.M, "s": _re.S, "x": _re.X}
        flags = 0
        for ch in opts["g_regex_flags"]:
            flags |= letter_flags[ch]
        kwargs["g_regex_flags"] = flags
    if opts.get("regex_module"):
        kwargs["regex"] = True  # upstream loads this grammar with the regex module
    if opts.get("postlex") == "PythonIndenter":
        from lark.indenter import PythonIndenter

        kwargs["postlex"] = PythonIndenter()
    elif opts.get("postlex"):
        raise ValueError(f"unknown postlex {opts['postlex']!r} in {pdir}")
    # Lark.open so relative %import (poetry_pep508 -> .markers) resolves
    # against the vendored grammar directory, exactly as upstream loads it.
    return Lark.open(str(pdir / meta["entry_grammar"]), **kwargs)


def generate_project(pdir: Path) -> dict:
    meta = json.loads((pdir / "meta.json").read_text())
    name = meta["name"]
    parser = build_parser(pdir, meta)

    cases = []
    for input_rel in sorted(meta["inputs"]):
        text = (pdir / input_rel).read_text()
        try:
            tree = parser.parse(text)
            ok, err = True, None
        except Exception as e:  # noqa: BLE001 — freeze any parse failure verbatim
            ok, err = False, str(e).splitlines()[0][:200] if str(e) else type(e).__name__
            tree = None
        if not ok:
            print(f"  WARNING: {name}: {input_rel} does not parse: {err}")
            cases.append({"input_file": input_rel, "ok": False, "error": err})
            continue
        c = canon(tree)
        n_nodes, n_tokens = counts(tree)
        case = {
            "input_file": input_rel,
            "ok": True,
            "node_count": n_nodes,
            "token_count": n_tokens,
            "canon_len": len(c.encode("utf-8")),
            "fnv1a64": fnv1a64(c.encode("utf-8")),
        }
        if case["canon_len"] <= EMBED_LIMIT and tree_depth(tree) <= EMBED_DEPTH_LIMIT:
            case["tree"] = tree_to_dict(tree)
        cases.append(case)
    return {"name": name, "cases": cases}


def main():
    ORACLES_DIR.mkdir(parents=True, exist_ok=True)
    projects = sorted(p for p in WILD_DIR.iterdir() if (p / "meta.json").exists())
    if not projects:
        print("no wild projects found — nothing to do")
        return
    for pdir in projects:
        result = generate_project(pdir)
        out = ORACLES_DIR / f"{result['name']}.json"
        # Compact separators, not indent=2: pretty-printing inflates these
        # fixtures ~7x (every token costs 5 lines) for files nobody reads
        # linearly. Pipe through `python3 -m json.tool` to inspect one.
        out.write_text(
            json.dumps(result, ensure_ascii=False, separators=(",", ":")) + "\n"
        )
        n_ok = sum(1 for c in result["cases"] if c["ok"])
        n_embedded = sum(1 for c in result["cases"] if "tree" in c)
        print(
            f"  wrote {out.relative_to(LARK_RS_DIR)} — {n_ok}/{len(result['cases'])} "
            f"parse, {n_embedded} embedded trees"
        )


if __name__ == "__main__":
    main()
