# ADR-0040: The reconstructor is grounded metamorphically, not by the Python oracle

- **Status:** Proposed (pending architect ratification)
- **Date:** 2026-07-01
- **Depends on:** ADR-0026 (behaviour scoped to the oracle), ADR-0030 (oracle
  honesty / no silent skips)

## Context

The architect commissioned a serialization engine: turn a shaped parse tree back
into source text (`lark_rs::reconstruct::Reconstructor`). This is new public API
whose *output* has no byte-level Python oracle:

- Python Lark's counterpart (`lark.reconstruct.Reconstructor`) is explicitly
  experimental, its output text depends on which Earley derivation its matcher
  happens to resolve, and it is not canonical — two correct reconstructors can
  emit different bytes for the same tree. Freezing its bytes as fixtures would
  pin an implementation artifact, exactly what ADR-0017 tells us not to chase.
- ADR-0026 requires behaviour beyond the oracle to carry an architect-approved
  validation story in the escalation order *partial oracle → relative oracle →
  property tests → curated residue*. The architect approved metamorphic
  grounding for this feature explicitly when commissioning it.

What *is* falsifiable is the reconstructor's purpose: the emitted text must be
a sentence of the same grammar that re-parses to the same tree.

## Decision

Reconstruction is grounded by the **metamorphic round-trip property**, enforced
in-repo with no Python in the loop:

> for any grammar G and input x that lark-rs parses,
> `parse(reconstruct(parse(x)))` is structurally equal to `parse(x)`
> (node labels, child shapes, token types and values; positions excluded).

Two enforcement layers, both in CI:

1. **Curated property tests** (`tests/test_reconstruct.rs`): one grammar per
   tree-shaping feature the reconstructor must invert (filtered punctuation,
   transparent rules, expand1 collapse, aliases, `!`/`keep_all_tokens`, EBNF
   helpers, templates, `%ignore`, `term_subs`), the typed refusals, and
   small-stack robustness.
2. **A whole-bank sweep** (`tests/test_reconstruct_bank.rs`): every accepted
   case of the LALR compliance bank is round-tripped, gated by an XFAIL ledger
   (`reconstruct_xfail.json`, regenerated via `LARK_RECONSTRUCT_WRITE_XFAIL=1`)
   under the usual burndown discipline — the ledger only shrinks, and the build
   fails on regressions. Refusals and residue are typed and listed, never
   silently skipped (ADR-0030 spirit).

The *matching algorithm* still mirrors Python's `TreeMatcher` design (recons
rules over a tree-children alphabet, Earley matching, discarded-terminal
write-back, `term_subs`, `insert_spaces`), so the architecture stays
recognizable next to the reference implementation — but its behaviour is
allowed to exceed Python's where the metamorphic gate proves the improvement.
Three deliberate divergences, each caught-or-motivated by the gate:

- **Bridge rules for every expand1 origin** (Python only bridges some): an
  uncollapsed `?list: item+` node was unmatchable in Python's scheme; the bank
  sweep found it.
- **Duplicate-shape alternatives keep the writable, most-explicit variant**:
  no non-literal discarded terminals if avoidable (`_WS? → ε` instead of an
  error), then the most discarded literals (dropping a distinguishing `"B"`
  re-parsed as a higher-priority sibling rule; the bank sweep found it).
- **Separator insertion is grammar-aware**: the inserted separator is one an
  `%ignore` terminal can actually absorb (`" "`, `"\n"`, or `"\t"`); a grammar
  that ignores none of them gets exact concatenation. Python inserts `" "`
  unconditionally, which can never re-parse in whitespace-significant
  grammars — this one change took the bank sweep from 185/364 to 360/364.

## Consequences

- **Buys:** a serialization engine whose correctness is checked by CI over
  hundreds of real grammars without any new Python tooling; a reusable pattern
  (metamorphic banks) for future oracle-less features; bug discovery pressure —
  the sweep found two real matcher bugs before the first commit.
- **Costs / rules out:** no byte-stability guarantee — the output is *a*
  canonical text, not the input text, and its exact bytes may change as the
  dedup/derivation policy evolves (only the round-trip property is pinned).
  Grammars whose trees genuinely underdetermine the text (a *required*
  discarded regex terminal, e.g. an explicit `_WS` separator rule) need
  `term_subs`, exactly as in Python; they are the typed `recons:` residue in
  the ledger. `maybe_placeholders` grammars are refused up front (typed
  error), matching Python's assert — supporting them is future work that would
  burn down the ledger's `placeholders:` entries.
- **Tripwires:** if a real dependent ever needs byte-stable output, that is a
  new escalate-tier decision (a canonical-formatting contract, not just a
  round-trip). If the reconstructor is wired into a binding (PyO3/WASM/C),
  the public-output-mode rule of ADR-0026 applies to that surface again.
