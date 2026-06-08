# Lexer DFA plan — one combined automaton over all terminals

*Status: active umbrella plan for the lexer's lookaround/throughput work.*
*Supersedes the framing of [`LOOKAROUND_ELIMINATION_PLAN.md`](LOOKAROUND_ELIMINATION_PLAN.md),
which becomes **Phase 1** here. Rationale and the decision reversal are recorded in
[`LOOKAROUND_STRATEGY_ANALYSIS.md`](LOOKAROUND_STRATEGY_ANALYSIS.md) (revision
2026-06-08). Terminal-level classification in
[`TERMINAL_REDUCTION_DIAGNOSIS.md`](TERMINAL_REDUCTION_DIAGNOSIS.md).*
*Date: 2026-06-08.*

## Goal

Lex **every** terminal — lookaround-bearing ones included — in a **single
table-driven pass** over one combined automaton, built once and bakeable as static
data. Concretely: build the combined scanner on `regex-automata` (lazy/dense DFA,
multi-pattern `PatternID`), **lower** each bounded lookaround assertion into
lookaround-free automaton states so it joins the same machine, drive it with a
maximal-munch loop that reproduces Lark's exact selection, and drop `fancy-regex`.

Wins, in priority order:

1. **Throughput.** Today the lookaround terminals are *N separate `fancy-regex`
   side-probes per position* (`Scanner::match_at`); folding them into the one combined
   DFA removes the per-terminal engine entry — one array lookup per byte for the whole
   terminal set.
2. **Bakeability.** A serialized `regex-automata` DFA is static data, so the bundled
   `python`/`lark` (lookaround) grammars finally bake into the standalone / C / WASM
   runtimes — closing the standing limitation that those grammars are not
   standalone-able.
3. **Linearity / no ReDoS** and **removing the `fancy-regex` dependency**, as a
   consequence of (1).

## This is a DFA, not the Pike VM of PR #110

The closed [PR #110](https://github.com/okalldal/lark/pull/110) shipped a runtime
**Pike-VM** that *executes* lookaround at match time, and the strategy memo rejected it
(maintenance/parity surface, slower than a DFA). **This plan does not revive that.**
The engine here is a **DFA** over terminals whose bounded assertions have been *lowered
away* — so:

* there is **no runtime lookaround execution** and **no CPython-`re`-parity surface**:
  the lowered terminals are ordinary regular languages, machine-checkable against the
  `regex` crate (see Verification);
* a DFA is the *fastest* engine for this (one lookup/byte), where the Pike VM is
  linear-but-slower; PR #110's engine was suboptimal on both correctness-surface *and*
  speed.

The salvage from PR #110 is its **lookaround front-end** (`src/lookaround/`, the
assertion parser/classifier) — repurposed as the **lowering** pass that feeds the NFA
builder — **not** its `matcher.rs` Pike-VM.

## Why now (the reversal)

The elimination plan (Phase 1) gets the **Tier-E** terminals — the reducible bulk
(string/comment idioms) — back onto the combined `regex` DFA for free. But the
**G-tier** terminals (`STRING`, `OP`, `DEC_NUMBER`; see the diagnosis) provably *cannot*
be rewritten to a plain `regex` string, so under elimination-alone they stay on the
slow `fancy-regex` side-probe forever. The only way to give *them* single-pass speed
and bakeability is a combined automaton we build ourselves — and because their
assertions are **bounded** (hence regular, hence lowerable into ordinary states), a DFA
suffices. That is the gap this plan closes.

## Phases

Each phase is independently shippable and gated by the oracle from Phase 0.

### L0 — Differential oracle harness *(do this first)*

Stand up a **dual-scanner equality test**: the new `regex-automata` scanner vs today's
`regex`-crate `Scanner`, asserting **byte-identical token streams** over the 512-grammar
compliance bank, the JSON corpus, big real Python files, and exhaustive small inputs.
Scope: **lookaround-free grammars only** — the overlap where the `regex` crate *is* the
ground truth. This validates the new driver (stepping, `PatternID`, maximal munch,
`unless`, tie-breaks) against a rock-solid reference *before* any hard part lands, so
later divergences localize to the new code. The lookaround additions are gated
separately by `test_lookaround.rs` + `matchlen` (Python-Lark / `fancy-regex` oracle).

### L1 — Rebuild the combined scanner on `regex-automata` (lookaround-free only)

Replace the `regex`-crate alternation-string-plus-capture-groups merge with a
`regex-automata` multi-pattern DFA returning `PatternID`, plus a maximal-munch driver.
Configure `MatchKind::LeftmostFirst` and reproduce Lark's rank ordering + `unless`
retyping so L0 stays green. **Re-add a literal prefilter** (the regex crate's free
optimization we'd otherwise lose) to avoid a throughput regression. Behind a feature
flag; `fancy-regex` side-probes still handle lookaround terminals unchanged.

### L2 — Eliminate Tier E (folds in the elimination plan)

Deploy the proven-equivalent Tier-E rewrites (`LONG_STRING`, `REGEXP`, block comment,
and `STRING`'s body) — they become plain `regex` and join the L1 multi-pattern DFA.
**Gated on the Type-A equivalence proof** (route 1 in the diagnosis) or a cleared
red-team. This is `LOOKAROUND_ELIMINATION_PLAN.md` E2, now landing into the DFA scanner.

### L3 — Lower the G-tier assertions into the combined DFA

Lower the bounded G-tier guards into lookaround-free NFA fragments via
`thompson::Builder`, fold them into the combined DFA, and handle the trailing-context
(`OP`, `DEC_NUMBER`) rewind in the driver. Now **all** terminals are single-pass. Gated
by the `test_lookaround.rs` behavioral matrix + `matchlen` + L0.

### L4 — Remove `fancy-regex`

With every terminal on the DFA, drop the `fancy-regex` dependency and the
`AnyRegex::Fancy` routing. The lexer is `regex-automata`-only.

### L5 — Bake it static (the bakeability payoff)

Serialize the combined DFA (`regex-automata` `to_bytes`) and bake it into the
standalone / C / WASM runtimes, replacing the baked `ScannerPlan` alternation. Confirm
the bundled `python`/`lark` grammars now generate standalone parsers.

## Verification strategy

* **`regex` crate as oracle (the overlap).** L0's dual-scanner test — the new engine
  must reproduce today's `regex`-crate scanner byte-for-byte on every lookaround-free
  grammar. Large, deterministic, CI-gated.
* **Python-Lark / `fancy-regex` (the additions).** `test_lookaround.rs` behavioral
  matrix + `matchlen` gate the lowered lookaround terminals.
* **Equivalence proof debt.** L2 depends on the Type-A match-length proof (diagnosis,
  route 1) — until then, Tier-E deployment rides on the bounded-exhaustive checks + the
  red-team, not a proof.
* **Throughput.** Extend the bench harness (`BENCH.md`) to compare the `regex`-crate
  scanner vs the DFA scanner on shared corpora; add a `perf-counters` scaling gate for
  the new scanner (matching the Earley/CYK gates).

## Risks / open questions

* **Determinization blow-up** from lowering assertions + case-insensitive prefixes —
  mitigate with the **lazy (hybrid) DFA** (states built on demand).
* **Tie-break fidelity** — reproducing Lark's (priority, length, …) selection and
  `unless` retyping on top of raw `PatternID`. L0 is the net.
* **Lost free optimizations** — the regex crate's auto-prefilters; must be re-added
  explicitly in L1 or the common path regresses.
* **Maintenance surface** — the lowering pass + hand-built fragments. Bounded and
  oracle-gated, but real; this is the cost consciously accepted in the strategy
  reversal.

## Salvage map (from closed PR #110)

| Artifact | Disposition |
|---|---|
| `src/lookaround/mod.rs` (assertion front-end) | **Reuse** as the L3 lowering pass |
| `src/lookaround/matcher.rs` (Pike-VM) | **Not used** — a DFA replaces it |
| `tests/test_lookaround.rs` + `fixtures/oracles/lookaround/` | **Reuse** as the lookaround behavioral gate |
| `fancy-regex` removal | **Adopt** at L4 |
