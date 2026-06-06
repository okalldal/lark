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


def flag_letters(g_regex_flags):
    """Canonical `imsx` letters for a Python ``re`` flag bitset.

    Recorded (rather than the raw int) so the Rust harness is independent of
    CPython's flag values; ``""`` means no global flags. Mirrors lark-rs's
    ``grammar::terminal::flag_letters``.
    """
    if not g_regex_flags:
        return ""
    flags = int(g_regex_flags)
    letters = ""
    if flags & re.IGNORECASE:
        letters += "i"
    if flags & re.MULTILINE:
        letters += "m"
    if flags & re.DOTALL:
        letters += "s"
    if flags & re.VERBOSE:
        letters += "x"
    return letters


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
        # Behaviour-affecting options that change the *outcome* (whether the
        # grammar builds, and how it lexes). Recording them keeps the bank
        # faithful: without `strict`, a strict-only construct error would be
        # attributed to the default mode; without `g_regex_flags`, a
        # case-insensitive oracle would be attributed to a case-sensitive grammar.
        "strict": options.get("strict", False),
        "g_regex_flags": flag_letters(options.get("g_regex_flags", 0)),
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
               rec["maybe_placeholders"], rec["keep_all_tokens"], rec["construct_error"],
               rec["strict"], rec["g_regex_flags"])
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


# ─── Earley bank (Phase 2) ───────────────────────────────────────────────────
#
# A second bank, captured the same way but from Lark's *Earley* test classes,
# replayed by tests/test_earley_compliance.rs. Kept entirely separate from the
# LALR machinery above so bank.json stays byte-identical. Two extra dimensions
# matter for Earley and are recorded here: `ambiguity` (resolve | explicit) and
# the lexer (Earley uses basic/dynamic, never the LALR-only contextual lexer).
# Dynamic-lexer configurations are out of scope until Sprint 5, so they are
# filtered out — the bank captures what Sprints 1–4 can represent.

_EARLEY_RECORDS = []
_EARLEY_ATTR = "_lark_rs_earley_record"


def representable_earley(grammar, options):
    """Can lark-rs's Earley (Sprints 1–4: basic lexer) represent this config?"""
    if not isinstance(grammar, str):
        return False
    if options.get("parser", "earley") != "earley":
        return False
    lexer = options.get("lexer")
    # Dynamic / dynamic_complete and custom (callable) lexers are Sprint 5+.
    if lexer not in (None, "auto", "basic"):
        return False
    if options.get("transformer") is not None:
        return False
    if options.get("postlex") is not None:
        return False
    if RELATIVE_IMPORT.search(grammar):
        return False
    return True


def _earley_meta(grammar, options):
    return {
        "grammar": grammar if isinstance(grammar, str) else None,
        "parser": options.get("parser", "earley"),
        "lexer": options.get("lexer", "auto"),
        "ambiguity": options.get("ambiguity", "resolve"),
        "start": options.get("start", "start"),
        "maybe_placeholders": options.get("maybe_placeholders", True),
        "keep_all_tokens": options.get("keep_all_tokens", False),
        "strict": options.get("strict", False),
        "g_regex_flags": flag_letters(options.get("g_regex_flags", 0)),
    }


def _earley_patched_init(self, grammar, **options):
    rep = representable_earley(grammar, options)
    meta = _earley_meta(grammar, options)
    try:
        _orig_init(self, grammar, **options)
    except Exception as e:
        if rep:
            _EARLEY_RECORDS.append({**meta, "construct_error": True,
                                    "error_kind": type(e).__name__, "cases": []})
        raise
    if rep:
        rec = {**meta, "construct_error": False, "cases": []}
        object.__setattr__(self, _EARLEY_ATTR, rec)
        _EARLEY_RECORDS.append(rec)


def _earley_patched_parse(self, text, *args, **kwargs):
    rec = getattr(self, _EARLEY_ATTR, None)
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


def run_earley_suite():
    Lark.__init__ = _earley_patched_init
    Lark.parse = _earley_patched_parse
    try:
        import tests.test_parser as tp  # noqa: F401

        loader = unittest.TestLoader()
        suite = unittest.TestSuite()
        # Earley test classes that use the basic (not dynamic) lexer.
        for cls_name in ("TestEarleyBasic", "TestFullEarleyBasic"):
            cls = getattr(tp, cls_name, None)
            if cls is not None:
                suite.addTests(loader.loadTestsFromTestCase(cls))
        runner = unittest.TextTestRunner(stream=open("/dev/null", "w"), verbosity=0)
        runner.run(suite)
    finally:
        Lark.__init__ = _orig_init
        Lark.parse = _orig_parse


def dedup_and_save_earley():
    seen = {}
    for rec in _EARLEY_RECORDS:
        if rec["grammar"] is None:
            continue
        key = (rec["grammar"], rec["parser"], rec["lexer"], rec["ambiguity"],
               str(rec["start"]), rec["maybe_placeholders"], rec["keep_all_tokens"],
               rec["construct_error"], rec["strict"], rec["g_regex_flags"])
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
    records.sort(key=lambda r: (r["grammar"], r["ambiguity"], str(r["start"])))

    OUT_DIR.mkdir(parents=True, exist_ok=True)
    out = OUT_DIR / "earley_bank.json"
    out.write_text(json.dumps(records, indent=2, ensure_ascii=False) + "\n")

    n_parse = sum(len(r["cases"]) for r in records)
    n_conflict = sum(1 for r in records if r["construct_error"])
    n_explicit = sum(1 for r in records if r["ambiguity"] == "explicit")
    print(f"  wrote {out.relative_to(LARK_RS_DIR)}")
    print(f"  {len(records)} grammars, {n_parse} parse cases, "
          f"{n_conflict} construct-error, {n_explicit} explicit-ambiguity grammars")


# ─── Earley dynamic-lexer bank (Phase 2, Sprint 5) ───────────────────────────
#
# A third bank, captured from Lark's *dynamic-lexer* Earley test classes
# (TestEarleyDynamic[_complete] + TestFullEarleyDynamic[_complete]), replayed by
# tests/test_earley_dynamic_compliance.rs. Kept separate from the basic-lexer
# earley_bank.json (which stays byte-identical) so the two lexers burn down
# independently. The `lexer` dimension is "dynamic" | "dynamic_complete".

_EARLEY_DYN_RECORDS = []
_EARLEY_DYN_ATTR = "_lark_rs_earley_dyn_record"


def representable_earley_dynamic(grammar, options):
    """Can lark-rs's Earley dynamic lexer (Sprint 5) represent this config?"""
    if not isinstance(grammar, str):
        return False
    if options.get("parser", "earley") != "earley":
        return False
    # Only the dynamic / dynamic_complete lexers belong in this bank.
    if options.get("lexer") not in ("dynamic", "dynamic_complete"):
        return False
    if options.get("transformer") is not None:
        return False
    if options.get("postlex") is not None:
        return False
    # `priority="invert"` flips the ForestSumVisitor ordering (lower value wins) —
    # an orthogonal disambiguation option lark-rs does not implement yet, so it is
    # filtered here rather than recorded as a dynamic-lexer gap.
    if options.get("priority") is not None:
        return False
    if RELATIVE_IMPORT.search(grammar):
        return False
    return True


def _earley_dyn_patched_init(self, grammar, **options):
    rep = representable_earley_dynamic(grammar, options)
    meta = _earley_meta(grammar, options)
    try:
        _orig_init(self, grammar, **options)
    except Exception as e:
        if rep:
            _EARLEY_DYN_RECORDS.append({**meta, "construct_error": True,
                                        "error_kind": type(e).__name__, "cases": []})
        raise
    if rep:
        rec = {**meta, "construct_error": False, "cases": []}
        object.__setattr__(self, _EARLEY_DYN_ATTR, rec)
        _EARLEY_DYN_RECORDS.append(rec)


def _earley_dyn_patched_parse(self, text, *args, **kwargs):
    rec = getattr(self, _EARLEY_DYN_ATTR, None)
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


def run_earley_dynamic_suite():
    Lark.__init__ = _earley_dyn_patched_init
    Lark.parse = _earley_dyn_patched_parse
    try:
        import tests.test_parser as tp  # noqa: F401

        loader = unittest.TestLoader()
        suite = unittest.TestSuite()
        for cls_name in ("TestEarleyDynamic", "TestEarleyDynamic_complete",
                         "TestFullEarleyDynamic", "TestFullEarleyDynamic_complete"):
            cls = getattr(tp, cls_name, None)
            if cls is not None:
                suite.addTests(loader.loadTestsFromTestCase(cls))
        runner = unittest.TextTestRunner(stream=open("/dev/null", "w"), verbosity=0)
        runner.run(suite)
    finally:
        Lark.__init__ = _orig_init
        Lark.parse = _orig_parse


def dedup_and_save_earley_dynamic():
    seen = {}
    for rec in _EARLEY_DYN_RECORDS:
        if rec["grammar"] is None:
            continue
        key = (rec["grammar"], rec["parser"], rec["lexer"], rec["ambiguity"],
               str(rec["start"]), rec["maybe_placeholders"], rec["keep_all_tokens"],
               rec["construct_error"], rec["strict"], rec["g_regex_flags"])
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
    records.sort(key=lambda r: (r["grammar"], r["lexer"], r["ambiguity"], str(r["start"])))

    OUT_DIR.mkdir(parents=True, exist_ok=True)
    out = OUT_DIR / "earley_dynamic_bank.json"
    out.write_text(json.dumps(records, indent=2, ensure_ascii=False) + "\n")

    n_parse = sum(len(r["cases"]) for r in records)
    n_conflict = sum(1 for r in records if r["construct_error"])
    n_complete = sum(1 for r in records if r["lexer"] == "dynamic_complete")
    print(f"  wrote {out.relative_to(LARK_RS_DIR)}")
    print(f"  {len(records)} grammars, {n_parse} parse cases, "
          f"{n_conflict} construct-error, {n_complete} dynamic_complete grammars")


# ─── CYK bank (Phase 3) ──────────────────────────────────────────────────────
#
# A fourth bank, captured the same way but from Lark's *CYK* test class
# (TestCykBasic), replayed by tests/test_cyk_compliance.rs. Kept separate from the
# other banks (which stay byte-identical) so CYK burns down independently. CYK
# always uses the basic lexer (the contextual lexer is LALR-only) and always
# resolves ambiguity (there is no `ambiguity='explicit'` for CYK), so neither the
# lexer nor an ambiguity dimension varies — the recorded shape matches the LALR
# bank's, with parser='cyk'.

_CYK_RECORDS = []
_CYK_ATTR = "_lark_rs_cyk_record"


def representable_cyk(grammar, options):
    """Can lark-rs's CYK (basic lexer) represent this configuration?"""
    if not isinstance(grammar, str):
        return False
    if options.get("parser", "earley") != "cyk":
        return False
    lexer = options.get("lexer")
    # CYK only supports the basic lexer; custom (callable) lexers are out of scope.
    if lexer not in (None, "auto", "basic"):
        return False
    if options.get("transformer") is not None:
        return False
    if options.get("postlex") is not None:
        return False
    if RELATIVE_IMPORT.search(grammar):
        return False
    return True


def _cyk_meta(grammar, options):
    return {
        "grammar": grammar if isinstance(grammar, str) else None,
        "parser": options.get("parser", "earley"),
        "lexer": options.get("lexer", "auto"),
        "start": options.get("start", "start"),
        "maybe_placeholders": options.get("maybe_placeholders", True),
        "keep_all_tokens": options.get("keep_all_tokens", False),
        "strict": options.get("strict", False),
        "g_regex_flags": flag_letters(options.get("g_regex_flags", 0)),
    }


def _cyk_patched_init(self, grammar, **options):
    rep = representable_cyk(grammar, options)
    meta = _cyk_meta(grammar, options)
    try:
        _orig_init(self, grammar, **options)
    except Exception as e:
        if rep:
            _CYK_RECORDS.append({**meta, "construct_error": True,
                                 "error_kind": type(e).__name__, "cases": []})
        raise
    if rep:
        rec = {**meta, "construct_error": False, "cases": []}
        object.__setattr__(self, _CYK_ATTR, rec)
        _CYK_RECORDS.append(rec)


def _cyk_patched_parse(self, text, *args, **kwargs):
    rec = getattr(self, _CYK_ATTR, None)
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


def run_cyk_suite():
    Lark.__init__ = _cyk_patched_init
    Lark.parse = _cyk_patched_parse
    try:
        import tests.test_parser as tp  # noqa: F401

        loader = unittest.TestLoader()
        suite = unittest.TestSuite()
        for cls_name in ("TestCykBasic",):
            cls = getattr(tp, cls_name, None)
            if cls is not None:
                suite.addTests(loader.loadTestsFromTestCase(cls))
        runner = unittest.TextTestRunner(stream=open("/dev/null", "w"), verbosity=0)
        runner.run(suite)
    finally:
        Lark.__init__ = _orig_init
        Lark.parse = _orig_parse


def dedup_and_save_cyk():
    seen = {}
    for rec in _CYK_RECORDS:
        if rec["grammar"] is None:
            continue
        key = (rec["grammar"], rec["parser"], rec["lexer"], str(rec["start"]),
               rec["maybe_placeholders"], rec["keep_all_tokens"], rec["construct_error"],
               rec["strict"], rec["g_regex_flags"])
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
    records.sort(key=lambda r: (r["grammar"], str(r["start"])))

    OUT_DIR.mkdir(parents=True, exist_ok=True)
    out = OUT_DIR / "cyk_bank.json"
    out.write_text(json.dumps(records, indent=2, ensure_ascii=False) + "\n")

    n_parse = sum(len(r["cases"]) for r in records)
    n_conflict = sum(1 for r in records if r["construct_error"])
    print(f"  wrote {out.relative_to(LARK_RS_DIR)}")
    print(f"  {len(records)} grammars, {n_parse} parse cases, "
          f"{n_conflict} construct-error grammars")


if __name__ == "__main__":
    print("Instrumenting Python Lark and running its LALR test suite...")
    run_suite()
    dedup_and_save()
    print("Instrumenting Python Lark and running its Earley test suite...")
    run_earley_suite()
    dedup_and_save_earley()
    print("Instrumenting Python Lark and running its Earley dynamic-lexer suite...")
    run_earley_dynamic_suite()
    dedup_and_save_earley_dynamic()
    print("Instrumenting Python Lark and running its CYK test suite...")
    run_cyk_suite()
    dedup_and_save_cyk()
    print("Done. Commit tests/fixtures/oracles/compliance/ to track the banks.")
