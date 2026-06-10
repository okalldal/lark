#!/usr/bin/env python3
"""
Python-Lark side of the wild-grammar benchmark — the cross-engine complement to
`cargo bench --bench wild` (benches/wild.rs).

Replays every project in tests/wild/ through the in-tree Python Lark with the
exact upstream options recorded in each meta.json, and times:

  * build  — single-shot Lark.open wall time (matches wild.rs's single shot)
  * each input individually — calibrated inner loop, median per-iteration

Per-input timing (rather than wild.rs's whole-corpus loop) lets the analysis
aggregate exactly the subset of inputs lark-rs parses (the Rust bench filters
its corpus to inputs that parse), so the two engines are compared over
byte-identical input sets.

Output: JSON to stdout —
  {project: {"build_ns": float, "inputs": {rel: {"bytes": n, "median_ns": f, "min_ns": f}}}}
"""

import json
import sys
import time
from pathlib import Path

SCRIPT_DIR = Path(__file__).parent
LARK_RS_DIR = SCRIPT_DIR.parent
WILD_DIR = LARK_RS_DIR / "tests" / "wild"

sys.path.insert(0, str(LARK_RS_DIR.parent))
sys.setrecursionlimit(100_000)

from lark import Lark  # noqa: E402


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
        import re as _re

        letter_flags = {"i": _re.I, "m": _re.M, "s": _re.S, "x": _re.X}
        flags = 0
        for ch in opts["g_regex_flags"]:
            flags |= letter_flags[ch]
        kwargs["g_regex_flags"] = flags
    if opts.get("regex_module"):
        kwargs["regex"] = True
    if opts.get("postlex") == "PythonIndenter":
        from lark.indenter import PythonIndenter

        kwargs["postlex"] = PythonIndenter()
    elif opts.get("postlex"):
        raise ValueError(f"unknown postlex {opts['postlex']!r} in {pdir}")
    return Lark.open(str(pdir / meta["entry_grammar"]), **kwargs)


def measure(f, min_sample_s=0.01, max_samples=15, budget_s=3.0):
    """Calibrate an inner iteration count, then median per-iter ns over samples."""
    iters = 1
    while True:
        t0 = time.perf_counter()
        for _ in range(iters):
            f()
        if time.perf_counter() - t0 >= min_sample_s or iters >= 1 << 20:
            break
        iters *= 2
    samples = []
    overall = time.perf_counter()
    while len(samples) < max_samples and time.perf_counter() - overall < budget_s:
        t0 = time.perf_counter()
        for _ in range(iters):
            f()
        samples.append((time.perf_counter() - t0) * 1e9 / iters)
    samples.sort()
    return samples[len(samples) // 2], samples[0]


def main():
    only = set(sys.argv[1:])  # optional project-name filter
    out = {}
    projects = sorted(p for p in WILD_DIR.iterdir() if (p / "meta.json").exists())
    for pdir in projects:
        meta = json.loads((pdir / "meta.json").read_text())
        name = meta["name"]
        if only and name not in only:
            continue
        print(f"== {name}", file=sys.stderr, flush=True)
        t0 = time.perf_counter()
        try:
            parser = build_parser(pdir, meta)
        except Exception as e:  # noqa: BLE001
            print(f"   build failed: {e}", file=sys.stderr)
            out[name] = {"build_error": str(e)[:200]}
            continue
        build_ns = (time.perf_counter() - t0) * 1e9
        rec = {"build_ns": build_ns, "inputs": {}}
        for rel in sorted(meta["inputs"]):
            text = (pdir / rel).read_text()
            try:
                parser.parse(text)
            except Exception as e:  # noqa: BLE001
                rec["inputs"][rel] = {"bytes": len(text.encode()), "error": str(e)[:120]}
                continue
            median_ns, min_ns = measure(lambda: parser.parse(text))
            rec["inputs"][rel] = {
                "bytes": len(text.encode()),
                "median_ns": median_ns,
                "min_ns": min_ns,
            }
            print(f"   {rel}: {median_ns/1e6:.3f} ms", file=sys.stderr, flush=True)
        out[name] = rec
    json.dump(out, sys.stdout)
    print()


if __name__ == "__main__":
    main()
