# Lexer Strategy — Remove `fancy-regex`, Unify on a Linear DFA Lexer (B1)

**Status:** ⬜ planned. Strategy decided 2026-06-06; no code yet.

**Decision:** retire `fancy-regex` from lark-rs entirely. Replace it with a
**general lowering of bounded lookaround to finite automata** ("B1" in the issue
#40 solution table), so every terminal — bundled *and* user-supplied — runs on the
linear `regex` / `regex-automata` engine. No backtracking engine remains on any
path.

This document records *why* `fancy-regex` is the wrong foundation given the two
project goals (full Python-Lark parity **and** 10–100× performance + multi-target
distribution), what the target architecture is, the B1 lowering design, and the
oracle-gated, milestone-by-milestone path there. It follows the discipline of the
[recovery plan](PHASE_3_RECOVERY_PLAN.md) and `CLAUDE.md`'s testing rules.

---

## 1. Why this, and why now

`fancy-regex` was introduced (issue #40) to let the bundled `python.lark` /
`lark.lark` use lookaround terminals verbatim. It works, and the recent `\G`
anchoring fix (PR #104) made it linear *per position*. But it is the wrong
long-term foundation, and every workaround we build on top of it is investment in
that wrong foundation.

### 1.1 We pay all of its costs for none of its capability

The #40 spike established two facts about **every** real Lark grammar (the four
bundled ones, plus every grammar under `examples/`):

- **Zero backreferences.** A repo-wide scan for `\1`, `(?P=…)` finds none.
- **Every lookaround used is *bounded*.** Bounded lookaround adds no expressive
  power over regular languages (regular languages are closed under intersection and
  complement), so every one of these terminals denotes a regular language.

A backtracking engine's *only* unique capability is non-regular matching
(backreferences / unbounded backtracking). We use **none** of it. Meanwhile we pay
its full price:

1. **A real ReDoS in a *shipped* grammar.** `lark.lark`'s `REGEXP` terminal
   backtracks catastrophically — ~11 ms at a 25-byte input, doubling every +2 chars
   (#40 §5). This is in a grammar we bundle.
2. **A possible *wrong answer* in a shipped grammar.** On a pathological-but-valid
   input, `fancy-regex` can hit its internal backtrack limit and bail, returning the
   wrong match rather than just being slow — a **parity violation**, flagged in #40
   §5 as "to confirm" and never confirmed.
3. **A constant-factor tax (~20×) on the hottest path** — string terminals. This is
   why the cross-engine bench's Python row sits at ~1.8× while JSON/SQL hit ~4.5×
   (`BENCH.md`).
4. **It cannot be baked into a DFA.** The standalone / WASM / C-API runtime is
   pure-`regex`, so `python.lark` and `lark.lark` **cannot ship** to those targets
   at all (`CLAUDE.md`, standalone limitations). That is a permanent hole in the
   distribution goal.

So `fancy-regex` is not a perf nit — it is a correctness-and-distribution liability
already sitting in `main`. Removing it serves *both* project goals: it closes the
Python perf outlier *and* removes the ReDoS / wrong-answer / undeliverable-grammar
liabilities.

### 1.2 What this is *not*

This is **not** the road to the 10–100× headline. There are two separate
performance ceilings, and they must not be conflated:

- **The headline ceiling is cross-cutting and lives elsewhere:** tree
  representation, ~3 allocations per input byte, allocation-bound not
  algorithm-bound (`BENCH.md`). JSON/SQL hit it too and never touch `fancy-regex`.
  That work (`Box<str>` / arena labels, zero-copy spans) is the headline lever and
  is **out of scope here**.
- **The Python-specific outlier (1.8× vs ~4.5×) is the `fancy-regex` tax.** This
  plan closes *that* — it brings Python in line with the other grammars and, via the
  unified DFA lexer (§3, milestone D), likely lifts JSON/SQL a little too.

Stated plainly so we don't oversell: this plan makes the lexer correct, safe,
deliverable, and on-par across grammars. The 100× headline is a different project.

---

## 2. The insight that makes B1 possible

Because every assertion in every real grammar is *bounded* and *regular*, we never
need backtracking — we need to **compile the assertions into automata**. Concretely,
a zero-width assertion is a constraint on the text adjacent to a position:

| Assertion | Constraint at position `p` | Regular set |
|-----------|----------------------------|-------------|
| `(?=B)`   | suffix from `p` begins with a `B` match | `L(B)·Σ*` |
| `(?!B)`   | suffix from `p` does **not** | `Σ* \ (L(B)·Σ*)` |
| `(?<=B)`  | prefix ending at `p` ends with a `B` match | `Σ*·L(B)` |
| `(?<!B)`  | prefix ending at `p` does **not** | `Σ* \ (Σ*·L(B))` |

All four are regular whenever `B` is regular (which, with no backreferences, it
always is). lark-rs **already owns the back half of this machinery**: `src/lexer.rs`
builds anchored dense DFAs (`regex-automata`, `StartKind::Anchored`) and runs
product-construction over them for the #35 strict-collision check. B1 reuses exactly
that to realize the intersections/complements above.

---

## 3. Target architecture: one linear DFA lexer

Today the lexer is a hybrid: a single combined `regex` alternation for plain
terminals, plus `fancy-regex` matched per-terminal for lookaround ones, merged by
rank in `Scanner::match_at`. The target collapses this to **one engine**:

```
Scanner = a regex-automata multi-pattern, anchored DFA
            (MatchKind::LeftmostFirst, PatternID -> SymbolId)
          + a small per-terminal "boundary assertion" check (§4) for the
            handful of terminals that carry one
```

Why this shape:

- **Anchored search solves `\G` natively** for *every* terminal — no unanchored
  forward scan, no overlay. The PR #104 pathology cannot recur by construction.
- **`PatternID` → terminal dispatch** removes the named-capture-group index lookup
  the current plain path uses. Lark terminals return the whole match as the token
  value (no sub-captures), so the DFA needs no capture machinery at all.
- **`MatchKind::LeftmostFirst` is the same semantics the `regex` crate already
  uses** — we are moving to the lower layer of the *same* engine, so the
  terminal-ordering tie-break behavior already validated at 100% compliance carries
  over unchanged (the `(-priority, -pattern_len, name)` sort still decides
  `PatternID` order).
- **It is bakeable.** A DFA serializes, so the standalone / WASM / C-API runtime can
  finally ship `python.lark` / `lark.lark`.

The `unless` keyword-retyping, per-terminal inline flags (`(?i:…)`), and global
`g_regex_flags` prefix all survive unchanged — they are properties of the pattern
source and the post-match value, independent of which engine runs the match.

---

## 4. B1 lowering design

### 4.1 A front-end is the real cost — name it

The honest crux: neither `regex-syntax` (the `regex` crate's parser) nor
`regex-automata` will parse lookaround — they reject it exactly as the `regex` crate
does. So B1's first real cost is a **lookaround-aware regex front-end**: parse a
terminal pattern into an AST that exposes assertion nodes, so we can strip and
compile them. Options, cheapest-first:

1. **Hand-rolled mini-parser for the bounded-assertion subset.** The pattern
   language actually used by lexer terminals is small; a focused parser that
   recognizes `(?=)`, `(?!)`, `(?<=)`, `(?<!)` (plus the ordinary constructs it
   passes through untouched) is tractable and dependency-free. Preferred.
2. **Borrow a parser crate's AST** (e.g. `regress`) for the parse only, discarding
   its engine. Adds a dependency but no backtracking on the hot path.

We explicitly do **not** keep `fancy-regex` "just for parsing" — that retains the
dependency and the temptation to fall back to its engine.

### 4.2 Boundary assertions — the complete-for-all-known-grammars path

Every assertion in every real grammar sits at a **token boundary**: a *leading*
lookbehind (`(?<!\\)` guarding `STRING`/`LONG_STRING`) or a *trailing* lookahead
(`DEC_NUMBER`'s `(?![1-9])`, `lark.OP`'s `(?![a-z])`, the forbid-`//` in `REGEXP`,
the `(?!"")` opening guard in `STRING`). For these:

1. **Strip** the assertion(s) from the pattern; the assertion-free **core** joins
   the combined multi-pattern anchored DFA (§3).
2. **Compile each assertion body to a boundary DFA** via `regex-automata`:
   - trailing `(?=B)` / `(?!B)` → a forward anchored DFA for `B`, evaluated over
     `text[match_end..]`;
   - leading `(?<=B)` / `(?<!B)` → an anchored DFA for the **reversed** `B`,
     evaluated right-to-left over `text[..match_start]`.
   Because `B` is bounded, each check reads a bounded number of bytes — **O(1) per
   token**, linear overall.
3. **Validate at match time.** A candidate `(terminal, start, end)` from the core
   DFA is accepted only if its leading/trailing boundary DFAs are satisfied
   (positive → must match; negative → must not).

**Length-changing backtracking.** In Python's `re`, a trailing lookahead can force a
preceding greedy quantifier to backtrack to a *shorter* match. A naive "match
longest, then peek" does not reproduce that. We recover it without a backtracking
engine: for an assertion-bearing terminal, ask the core DFA for its candidate match
ends (overlapping / all-matches search, which `regex-automata` supports) and take
the **longest end whose boundary assertion holds** — the leftmost-first/maximal-munch
winner under the constraint. Bounded `B` keeps this linear. (For the bundled
terminals the longest match already satisfies or the terminal simply doesn't apply,
so this reduces to a single peek; the general path covers the rest.)

### 4.3 Internal assertions — the theoretical extension

An assertion in the *middle* of a pattern (not at a token boundary) is not expressed
by a boundary peek; it requires compiling the assertion into the core automaton via
the intersection/complement constructions of §2 (build NFAs for the surrounding
fragments and the assertion body, intersect at the assertion position, determinize).
**No bundled or `examples/` grammar uses one.** So:

- Ship B1 with the boundary path (§4.2), which is complete for every grammar we
  know of.
- A genuinely internal bounded assertion is **rejected at build time with a clear,
  specific error** ("internal lookaround is not yet supported; rewrite as a boundary
  assertion or file an issue"). This residual is strictly *smaller* than today's
  unstated "no backreferences" residual, and it is reachable only by an exotic
  user grammar.
- Implement the full internal-assertion compiler only if a real grammar ever needs
  it — at which point §2 is the recipe and #35's product-construction is the engine.

### 4.4 Selection interaction

When an assertion-bearing terminal is among a state's candidates, the single
leftmost-first DFA pass (which returns one winner) is not enough: if the winner's
assertion fails we need the next-ranked candidate. Two-tier strategy:

- **Common case (no assertion-bearing terminal in this state):** the fast
  single-winner anchored pass, unchanged.
- **Assertion present:** fall to an overlapping search to enumerate candidates at
  `pos`, then apply the rank + boundary-assertion filter. Only ~4 terminals across
  all bundled grammars carry assertions and they are confined to specific lexer
  states, so this narrower path is rare and still linear.

---

## 5. Milestones (each independently shippable and oracle-gated)

Per `CLAUDE.md`: a suspected perf pathology must be pinned by a committed,
deterministic scaling gate **before** the fix — and every behavior change is gated
against the Python oracle. The ordering front-loads the safety nets.

- **M0 — Deterministic lexer-scan-step gate.** Add a `lexer_scan_steps` counter to
  `src/perf.rs` (behind `perf-counters`, zero-cost otherwise) incremented per
  scan-position attempt, and a `tests/test_lexer_scaling.rs` asserting flat-per-byte
  scaling on a sparse-terminal workload (the python.lark STRING shape). This is both
  the migration safety net *and* the regression net PR #104's `\G` fix is currently
  missing. **Do this first.**
- **M1 — Lookaround-aware front-end (§4.1).** The mini-parser that strips boundary
  assertions from a pattern, leaving a `regex`-crate-clean core + a list of
  `(side, polarity, body)` assertions. Unit-tested against the four bundled
  lookaround terminals.
- **M2 — Boundary-assertion lowering + validation (§4.2),** still alongside the
  existing scanner. Route the four bundled terminals through it; oracle suite
  (`test_stdlib`, `test_common`, `test_json_corpus`) stays green. This is the step
  that **removes the `REGEXP` ReDoS and the bail-wrong-answer risk** (those
  terminals are now on the linear engine).
- **M3 — Delete `fancy-regex`.** Remove the `AnyRegex::Fancy` arm, the dependency,
  and the dual-engine merge in `match_at`. Internal assertions now hit the M-level
  rejection error (§4.3). Compliance banks regenerated; `CLAUDE.md` parity-gap note
  rewritten.
- **M4 — Unify the scanner on the `regex-automata` multi-pattern anchored DFA
  (§3).** The perf step: single DFA pass, `PatternID` dispatch, no capture-index
  lookup. Closes the Python outlier; re-measure `BENCH.md`.
- **M5 — Bake the DFA into the standalone / WASM / C-API runtime.** Lets
  `python.lark` / `lark.lark` ship to those targets — a parity win the current
  architecture cannot deliver. (Removes the standalone "lookaround terminals not
  supported" limitation.)

M0–M3 deliver the correctness/safety win and can land without M4–M5. M4 is the
perf payoff; M5 is the distribution payoff.

---

## 6. Risks & open questions

- **Front-end scope creep (M1).** Keep the mini-parser to the assertion subset; do
  not reimplement a full PCRE parser. If it grows, switch to borrowing a parser
  crate's AST (§4.1 option 2) rather than expanding the hand-roll.
- **Leftmost-first parity under `regex-automata` (M4).** Must confirm
  `MatchKind::LeftmostFirst` over a multi-pattern DFA reproduces the exact winner the
  current combined `regex` alternation picks. Mitigation: the full compliance + JSON
  corpus banks are the gate; M4 lands only at 100% parity, and the
  `(-priority, -pattern_len, name)` → `PatternID` order is preserved.
- **Length-changing trailing lookahead (§4.2).** The "longest end whose assertion
  holds" rule must be oracle-checked against Python on a constructed case (a greedy
  tail forced to backtrack by a trailing `(?!…)`), not just the bundled terminals
  where the longest match trivially wins. Add such a case to `generate_oracles.py`.
- **Reverse-DFA for lookbehind (§4.2).** `regex-automata` supports reverse
  searches; confirm the reversed-body anchored DFA evaluated right-to-left over the
  prefix matches Python's lookbehind semantics on the even-backslash `STRING` guard.
- **`g_regex_flags` / inline flags through the new front-end.** The global prefix
  and per-terminal `(?i:…)` must survive stripping; covered by the existing
  `g_regex_flags` oracle and a new flagged-assertion case.

---

## 7. Relationship to prior work

- **Issue #40** bundled the grammar stdlib and chose `fancy-regex` (its option A1)
  as the *low-effort* unblock, explicitly deferring the B1/B2 linear-engine path to
  a follow-up and asking for a committed ReDoS bench. This plan **is** that
  follow-up, taken to its B1 conclusion: lower the lookaround away so the terminals
  return to the linear engine, which is the unifying fix #40 §5 itself identified
  ("rewrite the lookaround away → terminal goes back on the linear engine").
- **PR #104** found and fixed (with `\G`) an O(n²) *driver* pathology that #40's
  per-terminal analysis missed. That fix is correct and stays as cheap insurance
  until M3 deletes the engine it guards; M0 adds the deterministic gate it shipped
  without.
- **Issue #35** built the `regex-automata` anchored-DFA + product-construction
  machinery this plan's §2/§4 reuse for the assertion lowering.
