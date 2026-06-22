#!/usr/bin/env python3
"""diffcheck — flexible Python-Lark-vs-lark-rs differential for the strike teams.

Give it a grammar, an input, and options; it runs Python Lark (the oracle) and
the `diffcheck` Rust binary on the same job and reports whether they agree on
accept/reject and on tree shape. `_ambig` children are compared as unordered
sets (Lark does not order them; see lark-rs/CLAUDE.md).

As a library:

    from diffcheck import compare
    r = compare(grammar, text, parser="lalr", lexer="contextual",
                maybe_placeholders=False)
    if r["divergent"]:
        print(r)

As a CLI:

    python3 tools/diffcheck.py --grammar-file G.lark --input-file IN \
        --parser lalr --lexer contextual [--maybe-placeholders] [...]

Exit status is 2 on a divergence (a find), 0 on agreement, 1 on harness error.
"""
import argparse
import json
import subprocess
import sys
import tempfile
from pathlib import Path

SCRIPT_DIR = Path(__file__).parent
LARK_RS_DIR = SCRIPT_DIR.parent
REPO_ROOT = LARK_RS_DIR.parent

sys.path.insert(0, str(REPO_ROOT))
from lark import Lark, Tree, Token  # noqa: E402
from lark.exceptions import LarkError  # noqa: E402

_BIN = LARK_RS_DIR / "target" / "debug" / "diffcheck"


def _ensure_bin():
    if not _BIN.exists():
        subprocess.run(
            ["cargo", "build", "--bin", "diffcheck"],
            cwd=str(LARK_RS_DIR), check=True,
        )


def tree_to_dict(node):
    if isinstance(node, Tree):
        return {
            "type": "tree",
            "data": str(node.data),
            "children": [tree_to_dict(c) for c in node.children],
        }
    elif isinstance(node, Token):
        return {"type": "token", "token_type": str(node.type), "value": str(node)}
    elif node is None:
        return None
    return {"type": "unknown", "repr": repr(node)}


def _py_result(grammar, text, **opts):
    """Run Python Lark; return {stage, ok, tree|error} matching the Rust shape."""
    try:
        kwargs = dict(
            parser=opts.get("parser", "lalr"),
            start=opts.get("start", "start"),
            maybe_placeholders=opts.get("maybe_placeholders", False),
            keep_all_tokens=opts.get("keep_all_tokens", False),
        )
        lexer = opts.get("lexer")
        if lexer and lexer != "auto":
            kwargs["lexer"] = lexer
        # Python Lark only accepts `ambiguity=` for the Earley parser; passing it
        # to LALR/CYK raises a build error and would falsely flag a divergence.
        if opts.get("ambiguity") and opts.get("parser", "lalr") == "earley":
            kwargs["ambiguity"] = opts["ambiguity"]
        if opts.get("strict"):
            kwargs["strict"] = True
        lark = Lark(grammar, **kwargs)
    except Exception as e:  # noqa: BLE001 — build/grammar errors are a real outcome
        return {"stage": "build", "ok": False, "error": f"{type(e).__name__}: {e}"}
    try:
        tree = lark.parse(text, start=opts.get("start", "start"))
    except Exception as e:  # noqa: BLE001
        return {"stage": "parse", "ok": False, "error": f"{type(e).__name__}: {e}"}
    return {"stage": "parse", "ok": True, "tree": tree_to_dict(tree)}


def _rs_result(grammar, text, **opts):
    _ensure_bin()
    with tempfile.TemporaryDirectory() as d:
        gp = Path(d) / "g.lark"
        ip = Path(d) / "in.txt"
        gp.write_text(grammar)
        ip.write_text(text)
        argv = [
            str(_BIN),
            "--grammar-file", str(gp),
            "--input-file", str(ip),
            "--parser", opts.get("parser", "lalr"),
            "--lexer", opts.get("lexer", "contextual"),
            "--start", opts.get("start", "start"),
            "--ambiguity", opts.get("ambiguity", "resolve"),
        ]
        if opts.get("maybe_placeholders"):
            argv.append("--maybe-placeholders")
        if opts.get("keep_all_tokens"):
            argv.append("--keep-all-tokens")
        if opts.get("strict"):
            argv.append("--strict")
        proc = subprocess.run(argv, capture_output=True, text=True)
        if proc.returncode != 0:
            return {"stage": "harness", "ok": False, "error": proc.stderr.strip()}
        try:
            return json.loads(proc.stdout.strip().splitlines()[-1])
        except Exception as e:  # noqa: BLE001
            return {"stage": "harness", "ok": False,
                    "error": f"bad output {proc.stdout!r}: {e}"}


def _trees_equal(a, b):
    """Structural equality; `_ambig` children compared as unordered multisets."""
    if a is None or b is None:
        return a is b
    if a.get("type") != b.get("type"):
        return False
    if a["type"] == "token":
        return a["token_type"] == b["token_type"] and a["value"] == b["value"]
    if a["type"] == "tree":
        if a["data"] != b["data"]:
            return False
        ca, cb = a["children"], b["children"]
        if len(ca) != len(cb):
            return False
        if a["data"] == "_ambig":
            keys_a = sorted(json.dumps(c, sort_keys=True) for c in ca)
            keys_b = sorted(json.dumps(c, sort_keys=True) for c in cb)
            return keys_a == keys_b
        return all(_trees_equal(x, y) for x, y in zip(ca, cb))
    return a == b


def compare(grammar, text, **opts):
    py = _py_result(grammar, text, **opts)
    rs = _rs_result(grammar, text, **opts)
    if rs["stage"] == "harness":
        return {"divergent": False, "harness_error": rs["error"], "py": py, "rs": rs,
                "grammar": grammar, "input": text, "opts": opts}

    # Agreement requires: same accept/reject, and matching trees when both accept.
    divergent = False
    kind = None
    if py["ok"] != rs["ok"]:
        divergent = True
        kind = "accept/reject"
    elif py["ok"] and rs["ok"]:
        if not _trees_equal(py.get("tree"), rs.get("tree")):
            divergent = True
            kind = "tree-shape"
    # Note: when both reject we treat it as agreement even if the stage
    # (build vs parse) differs — a team should sanity-check stage mismatches by
    # hand, but most are not user-observable divergences.

    return {
        "divergent": divergent,
        "kind": kind,
        "py": py,
        "rs": rs,
        "grammar": grammar,
        "input": text,
        "opts": opts,
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--grammar-file", required=True)
    ap.add_argument("--input-file", required=True)
    ap.add_argument("--parser", default="lalr")
    ap.add_argument("--lexer", default="contextual")
    ap.add_argument("--start", default="start")
    ap.add_argument("--ambiguity", default="resolve")
    ap.add_argument("--maybe-placeholders", action="store_true")
    ap.add_argument("--keep-all-tokens", action="store_true")
    ap.add_argument("--strict", action="store_true")
    a = ap.parse_args()
    grammar = Path(a.grammar_file).read_text()
    text = Path(a.input_file).read_text()
    r = compare(
        grammar, text, parser=a.parser, lexer=a.lexer, start=a.start,
        ambiguity=a.ambiguity, maybe_placeholders=a.maybe_placeholders,
        keep_all_tokens=a.keep_all_tokens, strict=a.strict,
    )
    print(json.dumps(r, indent=2, sort_keys=True))
    if r.get("harness_error"):
        sys.exit(1)
    sys.exit(2 if r["divergent"] else 0)


if __name__ == "__main__":
    main()
