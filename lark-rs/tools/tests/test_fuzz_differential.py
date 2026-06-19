#!/usr/bin/env python3
"""Failed-first / regression demonstration for the online divergence-preserving
minimizer (issue #37).

The gap: `--minimize` used to shrink a diverging find while preserving only
*parse-success*. That can over-minimize to a case where lark-rs and Python Lark
actually AGREE — silently losing the divergence signal the fuzzer found.

This test reproduces that gap deterministically without needing a live lark-rs
bug: it points the minimizer at a *fake* `differ` binary that injects a known,
controlled divergence (it rejects any input containing `*`, which Python Lark's
arithmetic grammar happily parses). Then:

  * the legacy parse/reject-preserving predicate over-minimizes `"1*1"` down to
    `"1"`, on which the fake differ and Python AGREE — the divergence is lost
    (this is the bug, asserted to still hold for the legacy predicate); and
  * the new divergence-preserving predicate keeps the `*`, so the shrunk result
    still diverges — the signal is preserved (the fix).

Run directly:  python3 tools/tests/test_fuzz_differential.py
"""

import stat
import sys
import tempfile
from pathlib import Path

TOOLS_DIR = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(TOOLS_DIR))

import fuzz_differential as fz  # noqa: E402


# A fake `differ` binary: parses stdin and prints oracle-shaped JSON, but lies by
# REJECTING any input containing '*'. Python Lark's arithmetic grammar parses
# "1*1" fine, so '*'-containing inputs are exactly the injected divergence class.
# Everything else mirrors Python's accept/reject (here: accept iff non-empty and
# only digits/`+`/`*`, which is enough for the digit-only agreeing cases we shrink to).
_FAKE_DIFFER = """#!/usr/bin/env python3
import sys
data = sys.stdin.read()
# Inject a divergence: pretend lark-rs cannot parse anything containing '*'.
if '*' in data or data == '' or any(c not in '0123456789+*' for c in data):
    print('{"ok": false, "tree": null}')
else:
    # Agree with Python on a plain digit/`+` expression by emitting a tree. The
    # exact shape does not matter for the accept/reject divergence we test; a
    # token root is the simplest valid oracle-shaped node.
    print('{"ok": true, "tree": {"type": "token", "token_type": "NUMBER", "value": "%s"}}' % data)
"""


def _write_fake_differ(tmpdir):
    path = Path(tmpdir) / "fake_differ"
    path.write_text(_FAKE_DIFFER)
    path.chmod(path.stat().st_mode | stat.S_IEXEC | stat.S_IRWXU)
    return str(path)


def main():
    parser = fz.load_parser("arithmetic")
    seed = "1*1"

    # Sanity: Python Lark parses the seed; the fake differ rejects it → divergence.
    assert fz.parses(parser, seed), "seed must parse in Python Lark"

    with tempfile.TemporaryDirectory() as tmpdir:
        fake = _write_fake_differ(tmpdir)

        rs_ok, _ = fz.larkrs_result(fake, "arithmetic", seed)
        assert rs_ok is False, "fake differ should reject the '*' seed"
        assert fz.diverges(parser, fake, "arithmetic", seed), \
            "seed must diverge (Python accepts, fake differ rejects)"

        # ── The BUG: the legacy parse-preserving predicate over-minimizes ──────
        # It only preserves parse-success, so it shrinks '1*1' down to a minimal
        # parsing input ('1') on which the two engines AGREE — divergence lost.
        def parse_pred(s):
            return fz.parses(parser, s)

        legacy_small = fz.minimize(parser, seed, parse_pred)
        legacy_diverges = fz.diverges(parser, fake, "arithmetic", legacy_small)
        assert not legacy_diverges, (
            f"expected the legacy predicate to OVER-minimize to an agreeing case, "
            f"but {legacy_small!r} still diverges")
        print(f"[legacy] minimized {seed!r} -> {legacy_small!r} "
              f"(diverges={legacy_diverges})  <- over-minimized, signal lost")

        # ── The FIX: divergence-preserving predicate keeps the divergence ──────
        def diverge_pred(s):
            if s == "":
                return False
            return fz.diverges(parser, fake, "arithmetic", s)

        fixed_small = fz.minimize(parser, seed, diverge_pred)
        fixed_diverges = fz.diverges(parser, fake, "arithmetic", fixed_small)
        assert fixed_diverges, (
            f"divergence-preserving minimize must keep the divergence, but "
            f"{fixed_small!r} agrees")
        assert "*" in fixed_small, (
            f"the preserved divergence requires a '*', but got {fixed_small!r}")
        print(f"[fixed]  minimized {seed!r} -> {fixed_small!r} "
              f"(diverges={fixed_diverges})  <- divergence preserved")

    print("OK: over-minimization reproduced for the legacy predicate and "
          "prevented by the divergence-preserving predicate.")


if __name__ == "__main__":
    main()
