# Phase 2 — Earley + SPPF: Scope & Implementation Plan

**Status:** Sprint 0 (test harness) ✅ done; Sprint 1 (Earley recognizer) is next.
Phase 2 was unfrozen at 99.6% compliance (exit criterion met — see
[`COMPLIANCE_PARITY.md`](COMPLIANCE_PARITY.md)).

This document answers four questions before any engine code is written:

1. What is the scope of Phase 2?
2. What preparations are in order?
3. One massive plan-and-implement PR, or sprints across sessions?
4. Do we set up / refine the test harness first?

**TL;DR.** Build it as **six sprints, one PR per session**, not one mega-PR.
**Sprint 0 is the test harness** — it must land first, because the oracle suite
currently has *zero* ambiguity support and *zero* Earley cases, and the whole
project discipline ("Python Lark is our oracle; never merge what the oracle
can't check") collapses without it. The load-bearing *engine* abstractions
(`CompiledGrammar`, `TokenSource`, `TreeBuilder`) are already done and were built
for exactly this, so no engine prep remains — only verification scaffolding.

---

## 1. Why sprints, not one massive PR

The repository's entire proven methodology is incremental and oracle-gated: the
LALR path went 68% → 99.6% compliance through ~13 small root-cause PRs, each
landing green against the oracle suite, each one forbidden from regressing the
bank. Phase 2 should not abandon the method that worked.

Concrete reasons a single ~2000-line Earley+SPPF PR is the wrong shape here:

- **SPPF is a shared DAG that Rust ownership can't express naively** (the stub
  already notes this — arena / index-based nodes). It is the single trickiest
  component in the whole rewrite. A big-bang PR means reviewing arena lifetimes,
  recognizer correctness, *and* disambiguation semantics at once, against an
  oracle that doesn't yet exist — precisely the "silent mis-resolution" trap
  `CLAUDE.md` warns about.
- **Earley has natural, independently-verifiable milestones**: recognize →
  single-tree forest → resolve ambiguity → explicit ambiguity → dynamic lexer.
  Each is green-against-oracle on its own.
- **Claude-code-driven sessions favour bounded scope.** A sprint that starts
  red and ends green within one session, with the compliance bank as the
  regression net, is the unit this repo is built around. A multi-session mega-PR
  has no green checkpoint to land on.

So: **multiple sessions, multiple PRs.** Each sprint below is sized to one
session and ends with `scripts/check.sh` green and the bank not regressed.

---

## 2. Sprint 0 — Test harness for ambiguity (✅ DONE)

The harness could not express what Earley produces. This sprint made it able to,
so every later sprint has an oracle to land against. **No parser code** — only
oracle generation, serialization, matching, and an Earley bank.

What landed:

1. ✅ **Earley oracle generation** — `tools/generate_oracles.py::generate_earley`
   parses a curated grammar set with `parser='earley'` at both
   `ambiguity='resolve'` and `ambiguity='explicit'`, writing
   `tests/fixtures/oracles/earley/cases.json`. The set covers an *unambiguous*
   grammar (Earley must match LALR's single tree), an *ambiguity at the root*
   (`!start: start start | "a"`), and an *ambiguity nested* below the start rule
   (`(aaa)` → `_ambig` as a child, not the root).
2. ✅ **`_ambig` matching.** `tree_to_dict` already serializes an `_ambig` node
   (it is just a `Tree`); the Rust side learned to compare it. `match_node_tree`
   special-cases `data == "_ambig"` and matches its children as an *unordered set*
   (Lark does not guarantee `_ambig` child order) via a small backtracking
   bijection (`match_ambig` / `match_child`). `tests/common/mod.rs` also gained
   `make_earley(grammar, ambiguity)` and the `earley_unimplemented()` self-gate.
3. ✅ **Earley compliance bank.** `tools/extract_lark_compliance.py` now also
   instruments Lark's `TestEarleyBasic` + `TestFullEarleyBasic` classes (basic
   lexer; dynamic-lexer configs filtered out for Sprint 5) into
   `compliance/earley_bank.json` — **147 grammars, 209 parse cases, 15
   explicit-ambiguity**. Replayed by `tests/test_earley_compliance.rs`, gated by
   `compliance/earley_xfail.json`. The LALR `bank.json` is byte-for-byte unchanged.
4. ✅ **Self-activating gate.** Both Earley tests probe `earley_unimplemented()`
   and skip while the backend is a stub, so Sprint 0 lands green; the moment
   Sprint 1 wires a real Earley frontend the probe flips and the oracles start
   being enforced — no edit to the tests required (the fuzz-corpus pattern).
   Every Earley bank entry is currently a uniform XFAIL (350 ids), honestly
   including construct-error records (a build that fails *because Earley is
   unimplemented* is not a grammar rejection, so it is not allowed to count as a
   spurious agreement).

The harness is now the spec for Sprints 1–5.

> **This was the answer to "do we need to set up / refine the test harness
> first?" — yes, and it was its own sprint.** Everything after it is gated by it.

---

## 3. Sprint 1 — Earley recognizer (over `SymbolId`), standard lexer

Map of the oracle source: `lark/parsers/earley.py` (~312 lines) is the
recognizer; `earley_common.py` (~42) the item type.

- Earley `Item` over `SymbolId` (rule index + dot + origin), Earley chart
  (one item-set per input position).
- predict / scan / complete loop, with **nullable handling** (Aycock–Horspool:
  complete ε-derivations eagerly so the chart terminates) — `analysis.rs`
  already computes `NULLABLE`.
- Consume tokens through the existing **`TokenSource`** trait. Wire
  `ParserAlgorithm::Earley` + `LexerType::Basic` (and `Contextual` only insofar
  as it is meaningful — see §7) through `build_frontend`, replacing the fail-loud
  guard in `parsers/mod.rs`.
- Scope cap: **boolean accept/reject only**, no forest yet. Validate accept/reject
  parity against the oracle on the *existing unambiguous* grammars (JSON,
  arithmetic): Earley must accept exactly what LALR accepts.

Exit: Earley recognizes JSON + arithmetic + the Sprint-0 unambiguous grammars
with accept/reject parity; ambiguous-grammar *acceptance* (not yet trees) passes.

---

## 4. Sprint 2 — SPPF construction + unambiguous forest → tree

The bulk: `lark/parsers/earley_forest.py` (~802 lines).

- Build the **Shared Packed Parse Forest**: Symbol / Intermediate / Packed nodes,
  **arena- or index-allocated** (a `Vec<Node>` + `NodeId` indices is the
  ownership-friendly form of the DAG; avoid `Rc<RefCell>` churn).
- Forest → tree walk that, for an **unambiguous** parse, collects one
  `NodeValue` per expansion symbol and calls **the existing
  `TreeBuilder::assemble`** — no second shaper. This is the whole reason
  `TreeBuilder` / `filter_pos` / `NodeValue::Inline` exist.
- **Huge leverage:** the regression net is the *entire existing oracle suite*.
  Gate the sprint on: every committed oracle (JSON corpus 293/293, arithmetic,
  python_numbers, …) produces an **identical tree under Earley as under LALR**.
  Reuse `tree_matches_oracle` verbatim.

Exit: `parser='earley'` produces byte-identical trees to LALR on every
unambiguous oracle in the repo.

---

## 5. Sprint 3 — Disambiguation (`ambiguity='resolve'`, the default)

- Port Lark's forest disambiguation (`ForestSumVisitor` / priority + rule-order
  resolution) so an ambiguous grammar collapses to the *same single tree* Lark
  picks.
- Validate against the Sprint-0 ambiguous grammars at `ambiguity='resolve'`, and
  start flipping `earley_xfail.json` entries to passing (the same XFAIL-shrink
  loop the LALR path used).

Exit: ambiguous grammars resolve to Lark's chosen tree; Earley bank parity climbs.

---

## 6. Sprint 4 — `ambiguity='explicit'` + `_ambig` nodes

- Emit **all** derivations as `_ambig` nodes through the Sprint-0 ambiguity-aware
  matcher. `Ambiguity::Explicit` is already in the `LarkOptions` enum.
- This is where Sprint 0's set-of-derivations comparator pays off.

Exit: `ambiguity='explicit'` matches Lark's `_ambig` forests on the bank.

---

## 7. Sprint 5 — Dynamic lexer + `dynamic_complete` (separable; can defer)

`lark/parsers/xearley.py` (~174 lines). This is a **distinct sub-phase** and the
one piece of *engine* prep that isn't already done:

- The dynamic lexer **integrates scanning into the Earley loop** — it matches
  terminals at each chart position driven by what the parser predicts, rather
  than lexing up front. The current `TokenSource` is pull-based / single-token
  and suits pre-lexed + contextual; the dynamic lexer needs a
  position-driven-scan extension to that contract.
- `dynamic_complete` tries *all* tokenizations.

**Lexer note (decision point for Sprints 1–5):** the **contextual lexer is
LALR-only** — it narrows terminals by *LALR parser state*, which Earley does not
have. Earley's lexer options are therefore **basic** (Sprints 1–4) and **dynamic
/ dynamic_complete** (Sprint 5). `LexerType::Auto` under `parser='earley'` should
resolve to `basic`, **not** contextual. Wire this in Sprint 1.

This sprint can ship after Phase 2 is otherwise "done", or be folded into Phase 3
— nothing in Sprints 1–4 depends on it.

---

## 8. What is already prepared (no work needed)

Done deliberately as Phase-2 groundwork (see `CLAUDE.md` "Load-bearing" list):

- **`CompiledGrammar` / `SymbolId`** — forest nodes key on `Copy` ids, never
  names. Done (core IR consolidation, 2026-06-03).
- **`TokenSource` trait** — the Earley driver consumes the same input interface
  as `LalrParser::run`. Done (Sprint 2, #10).
- **`TreeBuilder::assemble` + `NodeValue`** — the single shaper the forest-walk
  reuses; `filter_pos` per-position filtering is the exact chokepoint the SPPF→
  tree conversion needs. Done (Sprint 3).
- **Differential fuzzer** — exists; grow it with ambiguous grammars during
  Sprints 3–4 so divergences are found automatically, not just on the static bank.

The one engine abstraction *not* yet built is the **dynamic-lexer extension to
`TokenSource`** (Sprint 5), and it is deliberately deferred to last.

---

## 9. Sequencing summary

| Sprint | Deliverable | Oracle / gate | Engine code? |
|-------:|-------------|---------------|:------------:|
| **0 ✅** | Ambiguity harness + Earley bank | new Earley oracles, all XFAIL-gated | no |
| **1** | Earley recognizer, basic lexer | accept/reject parity (JSON, arithmetic) | yes |
| **2** | SPPF + unambiguous forest→tree | **every existing oracle, identical to LALR** | yes |
| **3** | `ambiguity='resolve'` | ambiguous grammars → Lark's chosen tree | yes |
| **4** | `ambiguity='explicit'` + `_ambig` | set-of-derivations match | yes |
| **5** | dynamic lexer / `dynamic_complete` | dynamic-lexer Earley cases | yes (TokenSource ext.) |

Each row = one session, one PR, `scripts/check.sh` green, bank not regressed.
North star unchanged: **the (now two-engine) compliance percentage**, not the
feature checklist.
