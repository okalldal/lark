#!/usr/bin/env python3
"""
Differential fuzzer for lark-rs — out-of-band discovery driver.

Fuzzing is a *discovery* activity: it runs explicitly (locally) or on a nightly
schedule, never on the PR critical path, and it does **not** commit the inputs it
generates. The committed regression corpus
(`tests/fixtures/oracles/fuzz/inputs.json`) holds only *minimized finds* — the
small set of inputs that actually exposed a lark-rs ↔ Python-Lark divergence —
exactly like the compliance bank holds real bugs, not random samples.

Three things live here:

  1. Discovery (default) — generate grammar-directed + mutated inputs for the
     trusted grammars and validate them against Python Lark (the oracle). This
     reports stats only; the actual lark-rs-vs-oracle diff happens in Rust. To
     hunt for divergences, dump a batch and replay it (the lark-rs `parse()` is
     what gets diffed, in `cargo test`):

        python3 tools/fuzz_differential.py --grammar all -n 4000 --seed 7 \
            --out /tmp/fuzz_batch.json
        LARK_FUZZ_INPUTS=/tmp/fuzz_batch.json python3 tools/generate_oracles.py
        cargo test --test test_fuzz_corpus      # RED == lark-rs diverged

     The nightly workflow (`.github/workflows/lark-rs-fuzz.yml`) does exactly
     this with fresh entropy and uploads the batch as an artifact on a RED, so a
     find is reproducible and triageable without committing the haystack.

  2. Minimizing a find (`--minimize`) — ddmin-style shrink to the smallest input
     that *still diverges* between lark-rs and Python Lark. Run this on the
     offending input from a red replay to get a tight repro before recording it.

     The minimizer calls into lark-rs via the thin `differ` binary
     (`src/bin/differ.rs`, parses stdin → prints the tree as oracle-shaped JSON)
     and diffs that tree against the Python oracle at every shrink step, accepting
     a candidate only when the divergence is PRESERVED — not merely when lark-rs
     still parses. Without that check the shrink could over-minimize to an input
     the two engines AGREE on, silently losing the divergence signal (issue #37).
     If the differ is unavailable or the seed input does not actually diverge, the
     minimizer falls back to the legacy parse/reject-preserving predicate and says
     so, so it is never silently weaker than before.

  3. Recording a find (`--record`) — append the single minimized input to the
     committed corpus so it is guarded forever:

        python3 tools/fuzz_differential.py --grammar arithmetic --record \
            --input "1" --note "start-rule expand1-to-bare-token gap (see \
            test_fuzz_corpus.rs)"

     A find belongs in the corpus only once lark-rs *agrees* with the oracle on
     it (a fixed bug → a green regression guard), OR it is paired with a
     documented carve-out in `test_fuzz_corpus.rs` for a known-open gap (the
     bare-token-root case is the worked example). Recording an un-carved open
     divergence would — correctly — turn CI red.

Determinism: generation is fully determined by `--seed`, so a nightly RED is
reproducible by re-running with the seed printed in its log.

Usage:
    python3 tools/fuzz_differential.py --grammar arithmetic -n 200
    python3 tools/fuzz_differential.py --grammar all -n 4000 --seed 7 --out batch.json
    python3 tools/fuzz_differential.py --grammar arithmetic --minimize "1 +++ 2"
    python3 tools/fuzz_differential.py --grammar arithmetic --record --input "1" --note "..."
"""

import argparse
import json
import random
import shutil
import subprocess
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

# Cap unbounded repetition so generated inputs stay *legible*. Tree-shape
# coverage comes from nesting depth, not from 2000-char flat operator chains —
# and a divergence on a short input is something a human can actually read.
_MAX_REPEAT = 4


def _sp(rng):
    return rng.choice(_WS)


def _times(rng, p):
    """Yield while a biased coin keeps coming up, capped at _MAX_REPEAT."""
    n = 0
    while n < _MAX_REPEAT and rng.random() < p:
        n += 1
        yield n


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
        prefix = "".join(rng.choice(["+", "-"]) + _sp(rng) for _ in _times(rng, 0.25))
        return prefix + atom(d)

    def term(d):
        out = factor(d)
        for _ in _times(rng, 0.4):
            out += f"{_sp(rng)}{rng.choice(['*', '/'])}{_sp(rng)}{factor(d)}"
        return out

    def expr(d):
        out = term(d)
        for _ in _times(rng, 0.5):
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


def tree_to_dict(node):
    """Serialise a Python-Lark parse tree to the same dict shape the differ binary
    prints (and generate_oracles.py freezes), so the two are directly comparable."""
    # Imported lazily: only the divergence-preserving minimize path needs it.
    from lark import Tree, Token
    if isinstance(node, Tree):
        return {"type": "tree", "data": node.data,
                "children": [tree_to_dict(c) for c in node.children]}
    if isinstance(node, Token):
        return {"type": "token", "token_type": str(node.type), "value": str(node)}
    return {"type": "unknown", "repr": repr(node)}


def oracle_result(parser, text):
    """The Python-Lark oracle verdict for `text`: (ok, tree_dict | None)."""
    try:
        return True, tree_to_dict(parser.parse(text))
    except LarkError:
        return False, None


# ─── lark-rs bridge (the online differ) ──────────────────────────────────────
#
# The minimizer asks lark-rs what it produces for a candidate via the thin
# `differ` binary, then compares that against the Python oracle. This is the
# "online Rust-side differ" of issue #37: it lets the shrink loop *preserve the
# divergence* rather than merely preserving parse-success.

def find_differ_binary(explicit=None):
    """Locate (building if needed) the `differ` binary. Returns its path, or None
    if it cannot be built (the caller then falls back to the legacy predicate)."""
    if explicit:
        p = Path(explicit)
        return str(p) if p.exists() else None

    # Prefer an already-built binary (release, then debug) to stay fast.
    for profile in ("release", "debug"):
        cand = LARK_RS_DIR / "target" / profile / "differ"
        if cand.exists():
            return str(cand)

    # Not built yet — build it once (debug is enough for a tree print).
    if shutil.which("cargo") is None:
        return None
    print("building the `differ` binary (cargo build --bin differ)...",
          file=sys.stderr)
    proc = subprocess.run(
        ["cargo", "build", "--bin", "differ"],
        cwd=str(LARK_RS_DIR), capture_output=True, text=True, encoding="utf-8")
    if proc.returncode != 0:
        print(f"  cargo build failed:\n{proc.stderr}", file=sys.stderr)
        return None
    cand = LARK_RS_DIR / "target" / "debug" / "differ"
    return str(cand) if cand.exists() else None


def larkrs_result(differ_bin, grammar, text):
    """Run `text` through lark-rs via the differ binary: (ok, tree_dict | None).

    Raises RuntimeError if the differ cannot be invoked or returns malformed
    output — a hard failure the caller surfaces rather than silently treating as
    'no divergence' (which would re-introduce the over-minimization bug)."""
    # Force UTF-8 so a non-ASCII candidate or token value can't raise a locale
    # encode/decode error under a C/POSIX locale (nightly cron/CI).
    proc = subprocess.run(
        [differ_bin, "--grammar", grammar],
        input=text, capture_output=True, text=True, encoding="utf-8")
    if proc.returncode != 0:
        raise RuntimeError(
            f"differ exited {proc.returncode} for grammar {grammar!r}: "
            f"{proc.stderr.strip()}")
    try:
        out = json.loads(proc.stdout)
        return bool(out["ok"]), out.get("tree")
    except (json.JSONDecodeError, KeyError) as e:
        raise RuntimeError(f"differ produced malformed output {proc.stdout!r}: {e}")


def diverges(parser, differ_bin, grammar, text):
    """True iff lark-rs and Python Lark disagree on `text` — either on accept/reject
    or on the produced tree. This is the predicate the minimizer preserves."""
    py_ok, py_tree = oracle_result(parser, text)
    rs_ok, rs_tree = larkrs_result(differ_bin, grammar, text)
    if py_ok != rs_ok:
        return True
    # Both accepted or both rejected; a tree mismatch (only meaningful when both
    # accepted) is also a divergence.
    return py_ok and py_tree != rs_tree


def minimize(parser, text, predicate):
    """ddmin-style shrink: smallest substring still satisfying `predicate`.

    The divergence-preserving predicate (`diverges`, which also runs lark-rs via
    the differ binary) is what `--minimize` uses by default; the legacy
    parse/reject-preserving predicate is the documented fallback. The shrink loop
    itself is predicate-agnostic.
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


# ─── Discovery batch ─────────────────────────────────────────────────────────

def generate_batch(grammars, count, seed, depth, do_mutate):
    """Generate `count` (grammar, input) pairs, deterministically given `seed`."""
    rng = random.Random(seed)
    batch = []
    for _ in range(count):
        grammar = rng.choice(grammars)
        s = GENERATORS[grammar](rng, depth)
        batch.append((grammar, s))
        if do_mutate and rng.random() < 0.5:
            batch.append((grammar, mutate(rng, s)))
    return batch


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    grammar_choices = sorted(GENERATORS) + ["all"]
    ap.add_argument("--grammar", choices=grammar_choices, default="arithmetic",
                    help="grammar to fuzz ('all' fans out over every generator)")
    ap.add_argument("-n", "--count", type=int, default=100,
                    help="number of inputs to generate")
    ap.add_argument("--seed", type=int, default=0, help="RNG seed (deterministic)")
    ap.add_argument("--depth", type=int, default=4, help="max generation depth")
    ap.add_argument("--mutate", action="store_true",
                    help="also emit mutated (near-valid) variants")
    ap.add_argument("--out", metavar="FILE",
                    help="write the generated batch to FILE (a scratch corpus for "
                         "replay/nightly discovery); never the committed corpus")
    ap.add_argument("--minimize", metavar="INPUT",
                    help="shrink INPUT to a minimal core that still DIVERGES between "
                         "lark-rs and Python Lark (via the differ binary), and exit; "
                         "falls back to parse/reject-preserving if the input does not "
                         "diverge or the differ is unavailable")
    ap.add_argument("--differ-bin", metavar="PATH",
                    help="path to the lark-rs `differ` binary (default: build/locate "
                         "target/{release,debug}/differ)")
    ap.add_argument("--no-differ", action="store_true",
                    help="force the legacy parse/reject-preserving minimize (skip the "
                         "online lark-rs differ entirely)")
    ap.add_argument("--record", action="store_true",
                    help="append a single minimized find (--input/--note) to the "
                         "committed regression corpus and exit")
    ap.add_argument("--input", metavar="STR", help="the input to --record")
    ap.add_argument("--note", metavar="STR",
                    help="why this find is guarded (required by --record)")
    args = ap.parse_args()

    grammars = sorted(GENERATORS) if args.grammar == "all" else [args.grammar]

    # ── Record a minimized find into the committed corpus ────────────────────
    if args.record:
        if args.grammar == "all" or not args.input or not args.note:
            ap.error("--record requires a concrete --grammar, --input and --note")
        entries = load_inputs()
        if any(e["grammar"] == args.grammar and e["input"] == args.input
               for e in entries):
            print("already recorded — nothing to do")
            return
        verdict = "parses" if parses(load_parser(args.grammar), args.input) \
            else "rejects"
        entries.append({"grammar": args.grammar, "input": args.input,
                        "note": args.note})
        save_inputs(entries)
        rel = INPUTS_PATH.relative_to(LARK_RS_DIR)
        print(f"recorded [{args.grammar}] {args.input!r} (Python Lark {verdict}) -> {rel}")
        print("next: python3 tools/generate_oracles.py && "
              "cargo test --test test_fuzz_corpus")
        return

    # ── Minimize a single input ──────────────────────────────────────────────
    if args.minimize is not None:
        if args.grammar == "all":
            ap.error("--minimize requires a concrete --grammar")
        parser = load_parser(args.grammar)
        seed = args.minimize

        # Legacy parse/reject-preserving predicate (the documented fallback).
        def parse_pred(s):
            if parses(parser, seed):
                return parses(parser, s)
            return not parses(parser, s) and s != ""

        mode = "parse/reject-preserving (legacy)"
        pred = parse_pred

        # Default: divergence-preserving. Only engage it when (a) the differ is
        # available and (b) the SEED input actually diverges — otherwise there is
        # no divergence to preserve and the parse-preserving fallback is correct.
        if not args.no_differ:
            differ_bin = find_differ_binary(args.differ_bin)
            if differ_bin is None:
                print("warning: differ binary unavailable — falling back to the "
                      "legacy parse/reject-preserving minimize.", file=sys.stderr)
            else:
                try:
                    seed_diverges = diverges(parser, differ_bin, args.grammar, seed)
                except RuntimeError as e:
                    print(f"warning: differ call failed ({e}) — falling back to the "
                          "legacy parse/reject-preserving minimize.", file=sys.stderr)
                    seed_diverges = None
                if seed_diverges is True:
                    mode = "divergence-preserving (lark-rs vs Python Lark)"
                    # Only accept a candidate that still diverges. A differ error
                    # mid-shrink is treated as 'does not satisfy' (reject the
                    # reduction) so a flaky call can never silently widen the input
                    # down to an agreeing case.
                    def diverge_pred(s):
                        if s == "":
                            return False
                        try:
                            return diverges(parser, differ_bin, args.grammar, s)
                        except RuntimeError:
                            return False
                    pred = diverge_pred
                elif seed_diverges is False:
                    print("warning: the seed input does NOT diverge (lark-rs agrees "
                          "with Python Lark) — nothing to preserve; using the legacy "
                          "parse/reject-preserving minimize.", file=sys.stderr)

        print(f"minimizing under: {mode}", file=sys.stderr)
        small = minimize(parser, seed, pred)
        print(json.dumps({"grammar": args.grammar, "input": small}, ensure_ascii=False))
        return

    # ── Discovery: generate, validate against the oracle, report ─────────────
    batch = generate_batch(grammars, args.count, args.seed, args.depth, args.mutate)
    parsers = {}  # grammar -> Lark, built once
    n_pass = 0
    for grammar, s in batch:
        if grammar not in parsers:
            parsers[grammar] = load_parser(grammar)
        if parses(parsers[grammar], s):
            n_pass += 1

    print(f"generated {len(batch)} inputs over {grammars} (seed={args.seed}): "
          f"{n_pass} parse, {len(batch) - n_pass} reject")

    if args.out:
        # De-dupe; a scratch batch for the Rust replay to diff lark-rs against.
        seen = set()
        scratch = []
        for grammar, s in batch:
            key = (grammar, s)
            if key not in seen:
                seen.add(key)
                scratch.append({"grammar": grammar, "input": s})
        Path(args.out).write_text(
            json.dumps(scratch, indent=2, ensure_ascii=False) + "\n")
        print(f"wrote {len(scratch)} de-duped inputs -> {args.out}")
        print(f"next: LARK_FUZZ_INPUTS={args.out} python3 tools/generate_oracles.py && "
              "cargo test --test test_fuzz_corpus")
    else:
        print("(discovery only — pass --out FILE to replay, "
              "or --record to keep a minimized find)")


if __name__ == "__main__":
    main()
