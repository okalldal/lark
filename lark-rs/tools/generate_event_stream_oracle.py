#!/usr/bin/env python3
"""
Event-stream oracle generator (issue #595, Slice 1 of #594).

Generalizes the 20-case transformer trace oracle
(`tools/generate_transformer_oracles.py`) into a **grammar-agnostic** differential
over the *whole* LALR compliance bank. For every accepted `(grammar, options,
input)` in `tests/fixtures/oracles/compliance/bank.json`, it builds a "log every
callback" tracing transformer, runs it **embedded** (`Lark(…, transformer=T)`,
LALR), and commits the ordered event stream per case as an oracle
(`tests/fixtures/oracles/transformer/event_stream_bank.json`).

A Rust replay (`tests/test_transform_event_stream.rs`) drives each accepted case
through `Lark::parse_into` with an event-logging `OutputBuilder` and asserts its
event stream is byte-identical to this Python oracle, over both basic and
contextual lexers — turning the entire committed compliance corpus into a
regression net for the semantic-output / `parse_into` seam.

The generator READS the committed `compliance/bank.json` (never re-runs Python
Lark's own suite), so it is deterministic and CI can regenerate it and fail on
drift, exactly like the other oracle generators.

Usage:
    python3 tools/generate_event_stream_oracle.py

Output: tests/fixtures/oracles/transformer/event_stream_bank.json
"""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path

SCRIPT_DIR = Path(__file__).parent
LARK_RS_DIR = SCRIPT_DIR.parent
ORACLES_DIR = LARK_RS_DIR / "tests" / "fixtures" / "oracles" / "transformer"
BANK_PATH = LARK_RS_DIR / "tests" / "fixtures" / "oracles" / "compliance" / "bank.json"

# Add the Python Lark source (repo root) to path.
sys.path.insert(0, str(LARK_RS_DIR.parent))

from lark import Lark, Transformer, Tree  # noqa: E402


# The two LALR lexers lark-rs supports and Python's `transformer=` accepts. Both
# are generated for every accepted case; the Rust replay drives both. A grammar
# that only builds under one lexer contributes only that config (the other is a
# build_error on both sides — an outcome-parity concern owned by the compliance
# bank, not this differential).
CONFIGS = ["basic", "contextual"]


def to_re_flags(letters: str) -> int:
    """Map the bank's canonical `imsx` letters back to a Python ``re`` flag int.

    Inverse of `extract_lark_compliance.flag_letters`, so a grammar authored with
    `g_regex_flags` is rebuilt with the same flags.
    """
    mapping = {"i": re.IGNORECASE, "m": re.MULTILINE, "s": re.DOTALL, "x": re.VERBOSE}
    flags = 0
    for ch in letters or "":
        flags |= mapping.get(ch, 0)
    return flags


def record_options(rec: dict, lexer: str) -> dict:
    """The `Lark(**opts)` kwargs for a bank record under one lexer."""
    return {
        "parser": "lalr",
        "lexer": lexer,
        "start": rec["start"],
        "maybe_placeholders": rec.get("maybe_placeholders", True),
        "keep_all_tokens": rec.get("keep_all_tokens", False),
        "strict": rec.get("strict", False),
        "g_regex_flags": to_re_flags(rec.get("g_regex_flags", "")),
    }


def build_tracer(terminal_names):
    """A grammar-agnostic "log every callback" tracing transformer.

    Returns `(transformer_instance, log)` where `log` is the shared ordered event
    list every callback appends to. Each callback returns identity so the parse
    completes untouched.

    Two deliberate alignments with lark-rs's `parse_into` observation points make
    the two event streams directly comparable:

    * **Per-terminal methods, not `__default_token__`.** The embedded path does
      *not* wire `__default_token__` (issue #229): a token callback fires only for a
      terminal with an explicit method. Registering one method per terminal makes
      token events fire symmetrically for *every* shifted terminal — exactly the
      set `parse_into`'s `token()` sees. (`%ignore`d terminals never surface to the
      parser on either side, so their methods simply never fire.)

    * **`__default__` for rules, filtered to non-`_` names.** lark-rs's `reduce()`
      fires only for a rule that builds a kept node — i.e. a *non-transparent* rule.
      A `_rule` / `__anon_*` helper (both start with `_`) is spliced without a
      `reduce()` call, and an `?rule` that collapses to a single child likewise
      builds no node. Python's `__default__` fires for *every* reduction including
      the `_`-prefixed helpers, so we drop those to match; the `?`-collapse is
      handled identically by Python's `ExpandSingleChild` (no callback) and needs no
      filtering here.
    """
    log: list[dict] = []

    class Tracer(Transformer):
        def __default__(self, data, children, meta):
            name = str(data)
            # A transparent rule (`_rule` / `__anon_*`) is spliced by lark-rs
            # without a reduce() event; Python fires __default__ for it, so skip.
            if not name.startswith("_"):
                log.append({"kind": "rule", "name": name})
            return Tree(data, children)

    def make_token_method(term_name: str):
        def method(self, token):
            log.append(
                {"kind": "token", "name": str(token.type), "value": str(token)}
            )
            return token

        method.__name__ = term_name
        return method

    for name in terminal_names:
        setattr(Tracer, name, make_token_method(name))

    return Tracer(), log


def config_event_streams(rec: dict, lexer: str, accepted: list[dict]) -> dict | None:
    """Embedded-transform every accepted input under one lexer.

    Returns ``{input: [events] | None}`` (``None`` for an input the embedded
    transform rejected under this lexer), or ``None`` if Python Lark cannot build
    the grammar under this lexer at all (that config is simply absent — a build
    divergence is the compliance bank's concern, not this differential's).
    """
    opts = record_options(rec, lexer)

    # Probe build (no transformer) to introspect the terminal name set.
    try:
        probe = Lark(rec["grammar"], **opts)
    except Exception:
        return None
    terminal_names = [t.name for t in probe.terminals]

    tracer, log = build_tracer(terminal_names)
    try:
        parser = Lark(rec["grammar"], transformer=tracer, **opts)
    except Exception:
        # An embedded-transformer build that the plain build accepted is
        # unexpected; treat as "no config" rather than crashing the generator.
        return None

    streams: dict[str, list | None] = {}
    for case in accepted:
        log.clear()
        try:
            parser.parse(case["input"])
        except Exception:
            # Python accepted this input in the bank under *its* recorded lexer,
            # but the same input need not parse under the other lexer (a
            # contextual grammar can reject an input the basic lexer would mis-
            # tokenize). No transform ran, so there is no stream to compare — the
            # replay skips this (lexer, input) pair.
            streams[case["input"]] = None
            continue
        streams[case["input"]] = list(log)
    return streams


def generate() -> dict:
    bank = json.loads(BANK_PATH.read_text())

    entries = []
    divergences = 0
    for ri, rec in enumerate(bank):
        if rec.get("construct_error"):
            continue
        accepted = [c for c in rec.get("cases", []) if c.get("should_parse")]
        if not accepted:
            # Error-only records have no transform to trace (done-when: the
            # differential runs over the accepted subset only).
            continue

        # Per config: {input: [events] | None (rejected under this lexer)}.
        per_config: dict[str, dict] = {}
        for lexer in CONFIGS:
            streams = config_event_streams(rec, lexer, accepted)
            if streams is not None:
                per_config[f"lalr_{lexer}"] = streams
        built_configs = sorted(per_config.keys())
        if not built_configs:
            continue

        cases = []
        for case in accepted:
            inp = case["input"]
            # Configs whose embedded transform produced a stream for this input.
            ok = {c: per_config[c][inp] for c in built_configs if per_config[c][inp] is not None}
            if not ok:
                continue  # rejected under every built lexer — nothing to compare
            ok_configs = sorted(ok.keys())
            distinct = {json.dumps(v, sort_keys=True) for v in ok.values()}
            entry_case = {"input": inp, "ok_configs": ok_configs}
            if len(distinct) == 1:
                # The event stream is the lexer-independent parse post-order; the
                # basic and contextual streams agree (the norm). Store it once; the
                # Rust replay still drives *both* lexers against it, so a future
                # lark-rs lexer-specific divergence is caught.
                entry_case["events"] = next(iter(ok.values()))
            else:
                # A genuine lexer-interleaving artifact: pin each explicitly rather
                # than paper over it (ADR-0030 discipline). Never observed on the
                # current bank, but kept honest by construction.
                divergences += 1
                entry_case["events_by_config"] = ok
            cases.append(entry_case)

        if not cases:
            continue

        entries.append(
            {
                "record_index": ri,
                "grammar": rec["grammar"],
                "start": rec["start"],
                "maybe_placeholders": rec.get("maybe_placeholders", True),
                "keep_all_tokens": rec.get("keep_all_tokens", False),
                "strict": rec.get("strict", False),
                "g_regex_flags": rec.get("g_regex_flags", ""),
                "configs": built_configs,
                "cases": cases,
            }
        )

    return (
        {
            "generator": "tools/generate_event_stream_oracle.py",
            "schema_version": 1,
            "source_bank": "compliance/bank.json",
            "entries": entries,
        },
        divergences,
    )


def main():
    if not BANK_PATH.exists():
        print(
            f"ERROR: {BANK_PATH} not found — run tools/extract_lark_compliance.py first.",
            file=sys.stderr,
        )
        sys.exit(1)

    ORACLES_DIR.mkdir(parents=True, exist_ok=True)
    data, divergences = generate()

    out_path = ORACLES_DIR / "event_stream_bank.json"
    with open(out_path, "w") as f:
        json.dump(data, f, indent=2, sort_keys=False)
        f.write("\n")

    n_entries = len(data["entries"])
    n_cases = sum(len(e["cases"]) for e in data["entries"])
    n_events = sum(
        len(c["events"]) if "events" in c else sum(len(v) for v in c["events_by_config"].values())
        for e in data["entries"]
        for c in e["cases"]
    )
    print(
        f"Wrote {out_path}: {n_entries} grammar entries, {n_cases} accepted cases, "
        f"{n_events} events ({divergences} lexer-divergent cases pinned per-config)."
    )


if __name__ == "__main__":
    main()
