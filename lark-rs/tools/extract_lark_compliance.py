#!/usr/bin/env python3
"""
Strip-mine Python Lark's own test suite into a language-agnostic compliance
bank for lark-rs.

Rather than statically parsing the tests (whose grammars are embedded in call
sites and parametrised by a metaclass), we *instrument* Lark at runtime:
monkeypatch ``Lark.__init__`` and ``Lark.parse`` to record every
``(grammar, options, input, tree | error)`` the suite exercises, then run the
LALR test classes. Each captured record is filtered down to configurations
lark-rs can represent today (parser='lalr', no transformer/postlex, string
grammar, no relative imports) and serialised to JSON.

The Rust harness (tests/test_compliance.rs) replays each record and compares
against this oracle, gated by an XFAIL allow-list so deferred features stay
visible without blocking the build.

Usage:
    python3 tools/extract_lark_compliance.py
"""

import json
import re
import sys
import unittest
from pathlib import Path

SCRIPT_DIR = Path(__file__).parent
LARK_RS_DIR = SCRIPT_DIR.parent
REPO_ROOT = LARK_RS_DIR.parent
OUT_DIR = LARK_RS_DIR / "tests" / "fixtures" / "oracles" / "compliance"

# Import the local Python Lark (our oracle) and its test suite.
sys.path.insert(0, str(REPO_ROOT))

from lark import Lark, Tree, Token  # noqa: E402

RELATIVE_IMPORT = re.compile(r"%import\s*\.")


def tree_to_dict(node):
    """Convert a Lark parse tree to the same dict shape generate_oracles.py uses."""
    if isinstance(node, Tree):
        return {
            "type": "tree",
            "data": str(node.data),
            "children": [tree_to_dict(c) for c in node.children],
        }
    elif isinstance(node, Token):
        return {"type": "token", "token_type": str(node.type), "value": str(node)}
    else:
        # e.g. a None from maybe_placeholders, or a transformer result.
        return {"type": "unknown", "repr": repr(node)}


def representable(grammar, options):
    """Can lark-rs (LALR + basic/contextual lexer) represent this configuration?"""
    if not isinstance(grammar, str):
        return False
    if options.get("parser", "earley") != "lalr":
        return False
    lexer = options.get("lexer")
    if lexer not in (None, "auto", "basic", "contextual"):
        return False
    if options.get("transformer") is not None:
        return False
    if options.get("postlex") is not None:
        return False
    if RELATIVE_IMPORT.search(grammar):
        return False
    return True


# Flat list of all captured records. Each successfully-built instance also
# carries a back-reference to its record as an attribute (NOT keyed by id(),
# which Python reuses after GC and would misattribute parse calls).
_RECORDS = []
_RECORD_ATTR = "_lark_rs_compliance_record"

_orig_init = Lark.__init__
_orig_parse = Lark.parse


def _patched_init(self, grammar, **options):
    rep = representable(grammar, options)
    meta = {
        "grammar": grammar if isinstance(grammar, str) else None,
        "parser": options.get("parser", "earley"),
        "lexer": options.get("lexer", "auto"),
        "start": options.get("start", "start"),
        "maybe_placeholders": options.get("maybe_placeholders", True),
        "keep_all_tokens": options.get("keep_all_tokens", False),
    }
    try:
        _orig_init(self, grammar, **options)
    except Exception as e:
        if rep:
            _RECORDS.append({**meta, "construct_error": True,
                             "error_kind": type(e).__name__, "cases": []})
        raise
    if rep:
        rec = {**meta, "construct_error": False, "cases": []}
        object.__setattr__(self, _RECORD_ATTR, rec)
        _RECORDS.append(rec)


def _patched_parse(self, text, *args, **kwargs):
    rec = getattr(self, _RECORD_ATTR, None)
    # Only capture the plain `parse(text)` form — start=/on_error= variants
    # would not round-trip through the simple Rust replay.
    capture = rec is not None and not args and not kwargs and isinstance(text, str)
    try:
        result = _orig_parse(self, text, *args, **kwargs)
        if capture and isinstance(result, Tree):
            rec["cases"].append({"input": text, "should_parse": True,
                                 "tree": tree_to_dict(result), "error_kind": None})
        return result
    except Exception as e:
        if capture:
            rec["cases"].append({"input": text, "should_parse": False,
                                 "tree": None, "error_kind": type(e).__name__})
        raise


def run_suite():
    Lark.__init__ = _patched_init
    Lark.parse = _patched_parse
    try:
        import tests.test_parser as tp  # noqa: F401
        import tests.test_grammar as tg  # noqa: F401

        loader = unittest.TestLoader()
        suite = unittest.TestSuite()
        # LALR test classes from the parametrised parser suite.
        for cls_name in ("TestLalrContextual", "TestLalrBasic"):
            cls = getattr(tp, cls_name, None)
            if cls is not None:
                suite.addTests(loader.loadTestsFromTestCase(cls))
        # The grammar-loading / error suite (lots of conflict & feature cases).
        suite.addTests(loader.loadTestsFromModule(tg))

        # Run quietly; we only care about the captured Lark calls, not pass/fail.
        runner = unittest.TextTestRunner(stream=open("/dev/null", "w"), verbosity=0)
        runner.run(suite)
    finally:
        Lark.__init__ = _orig_init
        Lark.parse = _orig_parse


def dedup_and_save():
    # Dedup whole records by (grammar, options); within a record, dedup cases by input.
    seen = {}
    for rec in _RECORDS:
        if rec["grammar"] is None:
            continue
        key = (rec["grammar"], rec["parser"], rec["lexer"], str(rec["start"]),
               rec["maybe_placeholders"], rec["keep_all_tokens"], rec["construct_error"])
        tgt = seen.get(key)
        if tgt is None:
            tgt = {**rec, "cases": []}
            seen[key] = tgt
        case_inputs = {c["input"] for c in tgt["cases"]}
        for c in rec["cases"]:
            if c["input"] not in case_inputs:
                tgt["cases"].append(c)
                case_inputs.add(c["input"])

    records = list(seen.values())
    # Stable order so the committed JSON is deterministic.
    records.sort(key=lambda r: (r["grammar"], str(r["start"])))

    OUT_DIR.mkdir(parents=True, exist_ok=True)
    out = OUT_DIR / "bank.json"
    out.write_text(json.dumps(records, indent=2, ensure_ascii=False) + "\n")

    n_parse = sum(len(r["cases"]) for r in records)
    n_conflict = sum(1 for r in records if r["construct_error"])
    print(f"  wrote {out.relative_to(LARK_RS_DIR)}")
    print(f"  {len(records)} grammars, {n_parse} parse cases, "
          f"{n_conflict} construct-error (conflict) grammars")


if __name__ == "__main__":
    print("Instrumenting Python Lark and running its LALR test suite...")
    run_suite()
    dedup_and_save()
    print("Done. Commit tests/fixtures/oracles/compliance/ to track the bank.")
