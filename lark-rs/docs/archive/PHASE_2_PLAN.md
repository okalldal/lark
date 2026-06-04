# Phase 2 — Earley + SPPF: Scope & Implementation Plan

**Status:** Sprints 0–5 ✅ done — **Phase 2 is engine-complete.** Sprint 2 (SPPF +
forest→tree) wired the Earley frontend and, because the curated oracle gate is
all-or-nothing, also brought up `ambiguity='resolve'` (Sprint 3) and `'explicit'`
`_ambig` (Sprint 4) for the curated set; the Earley compliance bank is the
XFAIL-gated burndown net at 211/211 (clean, after the `AmbiguousExpander` port).
**Sprint 5 landed the dynamic lexer +
`dynamic_complete`** (`build_chart_dynamic`/`scan_dynamic`, `DynamicMatcher`), with
its own XFAIL-gated bank (`earley_dynamic_bank.json`, 446/454 ≈ 98.2%).
Phase 2 was unfrozen at 99.6% LALR compliance (see
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

## 3. Sprint 1 — Earley recognizer (over `SymbolId`), standard lexer ✅ DONE

**What landed.** [`EarleyParser`](src/parsers/earley.rs) is an Earley recognizer
over the interned grammar: items keyed by `SymbolId` (rule index + dot + origin),
one chart `Column` per input position, the predict/scan/complete loop with
Aycock–Horspool nullable handling (predictions on a nullable non-terminal eagerly
advance, so ε-derivations complete and the chart terminates — reusing the
precomputed `NULLABLE` from `analysis.rs`). `recognize(tokens, start) -> bool` is
boolean accept/reject only — **no forest**.

It is **not** wired into `build_frontend` yet, and that is deliberate: the
tree-comparing Earley oracle/compliance tests self-activate the moment the Earley
frontend builds (`common::earley_unimplemented`), so wiring an engine that cannot
yet produce trees would flip that gate red. Sprint 1 instead verifies the
recognizer through its own accept/reject oracle, `tests/test_earley_recognizer.rs`
— parity with Python Lark on the Sprint-0 curated grammars (unambiguous + both
ambiguous ones) and on the existing JSON and arithmetic grammars. **Sprint 2 is
what wires the frontend and flips the gate**, because only then are there trees to
compare. A shared `basic_lexer_conf()` helper now backs both the LALR frontend and
the recognizer's lexer, so both scan through one identical `Scanner` setup.

The original Sprint-1 design notes follow.



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

## 4. Sprint 2 — SPPF construction + unambiguous forest → tree ✅ DONE

**What landed.** [`EarleyParser`](src/parsers/earley.rs) now builds Elizabeth
Scott's binarized SPPF during the predict/scan/complete loop (symbol /
intermediate / packed nodes, arena-allocated by `NodeId`; nullable handling via
held completions; the Joop-Leo transitives are omitted because they are dead code
in the reference). A new `Transformer` walks the forest bottom-up and reuses the
shared `TreeBuilder::assemble` for every rule's shaping, so the forest walk and
the LALR reducer cannot diverge. The frontend is wired (`build_frontend` →
`FrontendKind::Earley`, basic lexer only; `Auto`/`Contextual`/`Basic` all resolve
to basic), which flipped `common::earley_unimplemented` and activated the Earley
oracle + bank tests.

Because the curated `test_earley_oracle` is **not** XFAIL-gated (it must pass in
full to flip the gate), Sprint 2 necessarily also implemented `ambiguity='resolve'`
disambiguation (the planned Sprint 3 — pick the highest-priority derivation in
Lark's `ForestSumVisitor` order: non-empty first, then priority, then rule order,
with insertion order breaking ties) and `ambiguity='explicit'` `_ambig` emission
(the planned Sprint 4). The Earley compliance bank went 0 → 210/211 (99.5%); the
single deferred XFAIL is an explicit-ambiguity forest threaded through a
transparent `_rule` and an EBNF `+` helper. Gates added/seen green:
`test_earley_parity` (Earley ≡ LALR on every unambiguous oracle), `test_earley_oracle`,
`test_earley_compliance`, `test_earley_recognizer` (recognizer now derived from the
same chart).

The original Sprint-2 design notes follow.

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
unambiguous oracle in the repo. (The cost-of-generality half of this exit — "…and
within K× of LALR" — was **downgraded to deferred**; see §10: a constant-K ceiling
is not achievable until the Joop-Leo optimization lands, tracked as P2-4.)

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

## 7. Sprint 5 — Dynamic lexer + `dynamic_complete` ✅ DONE

**What landed.** A close port of `lark/parsers/xearley.py`:
[`EarleyParser::build_chart_dynamic`](src/parsers/earley.rs) reuses the existing
predict/complete phase but replaces the scanner with `scan_dynamic`, which matches
each scan-set item's *predicted* terminal at the current position via a new
[`DynamicMatcher`](src/lexer.rs) (one anchored regex per terminal — no `unless`
retyping, since the parser context already chooses the terminal). Matches are held
in a `delayed_matches` buffer keyed by the step where they end (so variable-length
and overlapping terminals work), and `%ignore` spans carry scan-set items — and any
completed start item — past the ignored text. `dynamic_complete` additionally queues
every shorter prefix tokenization. Terminal priorities now feed the forest
`ForestSumVisitor` sum (the basic lexer consumes them in its terminal ordering; the
dynamic lexer has no such ordering, so they must sum in the forest). Wired through
`build_frontend` as `FrontendKind::EarleyDynamic` for `LexerType::Dynamic` /
`DynamicComplete`. Gated by curated oracles (`test_earley_dynamic.rs`,
`earley/dynamic_cases.json`) and a new XFAIL-gated compliance bank
(`test_earley_dynamic_compliance.rs`, `earley_dynamic_bank.json` — 441/454 ≈ 97.1%
strip-mined from Lark's `TestEarleyDynamic[_complete]` + `TestFullEarleyDynamic[_complete]`
classes). The basic-lexer `earley_bank.json` and the LALR `bank.json` stay
byte-identical. Remaining XFAILs: `%ignore`-of-content edge cases,
`dynamic_complete` resolve tie-break ordering, and nested-`_ambig`-through-EBNF-helper
cases; `priority="invert"` is filtered as an orthogonal unimplemented option.

The original Sprint-5 design notes follow.

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
| **1 ✅** | Earley recognizer, basic lexer | accept/reject parity (JSON, arithmetic) | yes |
| **2 ✅** | SPPF + unambiguous forest→tree | **every existing oracle, identical to LALR** | yes |
| **3 ✅** | `ambiguity='resolve'` | ambiguous grammars → Lark's chosen tree (landed with Sprint 2) | yes |
| **4 ✅** | `ambiguity='explicit'` + `_ambig` | set-of-derivations match (landed with Sprint 2; bank 210/211) | yes |
| **5 ✅** | dynamic lexer / `dynamic_complete` | dynamic-lexer Earley bank (441/454 ≈ 97.1%) | yes |

Each row = one session, one PR, `scripts/check.sh` green, bank not regressed.
North star unchanged: **the (now two-engine) compliance percentage**, not the
feature checklist.

---

## 10. Performance baseline & its implications for these sprints

A perf baseline harness (`cargo bench --bench parse`) + a profiling spike landed
alongside this plan — see [`BENCH.md`](BENCH.md). It exists so Earley has a *number*
to be measured against, not a release gate. Three findings change how the sprints
above are gated:

1. **LALR parse is allocation-bound, decisively** (measured, not assumed): one
   parse of a 92 KB input does ~301K allocations / 105 MB of churn; ~40% of
   instructions are `malloc`/`free`/`memcpy`, ~10% SipHash. Of total parse time,
   **~55% is lexing** (dominated by the `regex` engine + capture handling) and
   **~32% is reduce/tree-building** (`String` clones, `Tree`/`Vec` allocation).

2. ✅ **A cheap, engine-shared lexer win sat *before* Sprint 1 — now landed
   (perf sprint, 2026-06-04).** Two localized inefficiencies in `Scanner::match_at`
   — capture groups resolved *by name* per token (the SipHash cost) and a fresh
   `Captures` allocated per match — were pure `lexer.rs` changes that touch no
   public type. Resolving each terminal's capture-group index once and reusing a
   `CaptureLocations` scratch buffer cut allocations 300,957 → 271,892 per
   `json_large` parse and gave a ~17–20% wall-clock speedup across all parse
   workloads (see [`BENCH.md`](BENCH.md)). Crucially the **Earley basic and dynamic
   lexers scan through the same `Scanner`**, so this win is shared, not LALR-only —
   Sprint 1 inherits it for free.

3. **The tree-representation change is now profiler-justified — but defer it past
   Sprint 2.** The ~32% tree-building cost is exactly the `Box<str>`/arena-label +
   zero-copy-span change `CLAUDE.md` parks behind the `TreeBuilder` chokepoint. The
   profiler now asks for it, but it is best made *once the SPPF→tree walk (Sprint
   2) is a second consumer* of that representation, so both engines co-design it in
   one pass rather than hardening it against LALR alone.

**Cost-of-generality budget — DEFERRED (was a Sprint 2 exit add-on).** Earley is
O(n³) worst case and solves a strictly harder problem, so "slower than LALR" is
expected, not a regression — but unbounded slowness on *unambiguous* input is. The
add-on originally proposed *asserting* that Earley parses within an agreed **K×** of
LALR on the shared unambiguous workloads (a regression ceiling, K read off the
harness).

> **Status (2026-06-04, P2-1 resolved by downgrade).** The Earley side of the
> shared bench harness is now wired (`benches/parse.rs`): it re-runs the
> unambiguous workloads under `parser='earley'`, prints the per-row Earley/LALR
> ratio, and adds a reported-only pathological-ambiguous workload. Wiring it up
> **disproved the constant-K premise**: the ratio *grows* with input size
> (≈15×→32×→196× as JSON scales 0.4K→8.7K→92K on the reference box). That is
> structural — the completer rescans the whole origin column
> (`earley.rs::predict_and_complete`) because the Joop-Leo transitive optimization
> is deliberately omitted, so Earley is super-linear on list-shaped unambiguous
> input. A single-K ceiling is therefore not achievable until Leo lands, so per the
> P2-1 ticket's stated alternative this criterion is **downgraded from an exit
> criterion to deferred**. The ratios are reported as a trend (so a future Leo win
> shows up as the numbers dropping). The super-linearity itself is tracked as
> **P2-4** (Earley Leo / completer-index optimization) in
> [`COMPLIANCE_PARITY.md`](COMPLIANCE_PARITY.md).
