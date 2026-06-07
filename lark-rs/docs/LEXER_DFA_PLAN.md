# Lexer Strategy — Remove `fancy-regex`, Unify on a Linear DFA Lexer (B1)

**Status:** 🟡 in progress. Strategy decided 2026-06-06; PR 1 (the lookaround
behavioral oracles, `tests/test_lookaround.rs`) landed. **Amended 2026-06-06** after
a direct re-scan of the grammar corpus invalidated the boundary-assertion premise —
see the **Amendment** callout in §4.

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

> **⚠️ Amendment (2026-06-06) — corrected pattern census.** A direct re-scan of
> *every* `.lark` file (bundled `src/grammars/` **and** `examples/`) contradicts the
> boundary-assertion premise the rest of this section was built on. Only **2** of the
> assertions in the corpus are true token-boundary assertions; the rest — including
> ones in the **bundled** `python.lark` / `lark.lark` that already ship via
> `fancy-regex` (#40) — are **internal** (mid-pattern, position data-dependent).
>
> | Terminal | Grammar | Assertion(s) | Position | Boundary? |
> |---|---|---|---|---|
> | `DEC_NUMBER` | `python.lark` (bundled) | `(?![1-9])` | token end | ✅ trailing |
> | `OP` | `lark.lark` (bundled) | `(?![a-z])` | end of `?` alt (= token end) | ✅ trailing |
> | `STRING` | `python.lark` (bundled) | `(?!"")`/`(?!'')`, `(?<!\\)(\\\\)*?` | after opening quote; before closing quote | ❌ internal |
> | `LONG_STRING` | `python.lark` (bundled) | `(?<!\\)(\\\\)*?` | before closing triple-quote | ❌ internal |
> | `REGEXP` | `lark.lark` (bundled) | `(?!\/)` | after opening `/` | ❌ internal |
> | `MULTILINE_COMMENT` | `verilog.lark` (`examples/`) | `\*(?!\/)` | **inside a `(…)*` loop** | ❌ internal |
>
> (`common.lark`'s `ESCAPED_STRING` is already hand-adapted to a lookaround-free
> regex, so it does not appear here. Zero backreferences corpus-wide, as before.)
>
> **Consequences — what is now wrong below:**
> 1. **§4.2** ("Every assertion … sits at a token boundary"; "the
>    complete-for-all-known-grammars path") is true *only* for `DEC_NUMBER` and `OP`.
>    It even mislabels `STRING`'s `(?!"")` / `(?<!\\)` as boundary assertions; both
>    are mid-pattern. The boundary peek (strip + peek left/right at the *token* edge)
>    structurally cannot validate an assertion whose position depends on where a
>    `.*?` stopped.
> 2. **§4.3** ("No bundled or `examples/` grammar uses [an internal assertion]") is
>    false. The internal path is **required for parity with grammars already in
>    `main`**, not a "theoretical extension."
> 3. **M3 cannot fall back to build-time rejection for internal assertions.**
>    `%import python.STRING`, `python.LONG_STRING`, and `lark.REGEXP` ship today;
>    rejecting them after deleting `fancy-regex` would regress the stdlib (#40). The
>    internal-assertion lowering must land *with* M2/M3.
>
> **Recommended mechanism.** Abandon the boundary-peek-vs-internal-compiler split and
> implement the single **general regular lowering** of §2 for *every* assertion:
> compile the terminal pattern to an NFA, realize each bounded assertion as the §2
> intersection/complement constraint *at its position* (build a sub-automaton for the
> assertion body, splice it as a guarded ε-transition), then determinize into the
> combined anchored DFA — reusing the #35 product-construction machinery the repo
> already owns. Token-boundary assertions fall out as the position-0 / final-position
> special case, so the §4.2 boundary peek becomes an *optional* fast-path, never the
> contract. This keeps every terminal on one `regex-automata` DFA with no separate
> per-token boundary-DFA peeking, and is the literal realization of §2's "compile the
> assertions into automata."
>
> The milestone re-scope this implies is folded into §5 below (M2/M3 marked).

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

### 4.2 Boundary assertions — a fast-path special case (⚠️ see §4 Amendment)

> **Superseded premise.** The opening sentence below is false except for
> `DEC_NUMBER` and `OP` — see the §4 Amendment census. Read this section as
> *"the fast-path for the two assertions that happen to sit at the token edge,"* not
> as a complete strategy. The escaped-quote (`(?<!\\)`), quote-open (`(?!"")`), and
> `REGEXP` (`(?!\/)`) guards it names are **internal**, not boundary, assertions.

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

> **⚠️ Corrected (§4 Amendment).** The original text here claimed "No bundled or
> `examples/` grammar uses one" and proposed deferring this compiler / rejecting
> internal assertions at build time. That is wrong: the **bundled** `python.lark`
> (`STRING`, `LONG_STRING`) and `lark.lark` (`REGEXP`), plus `verilog.lark` in
> `examples/`, all use internal assertions. So this compiler is **required**, and the
> recommendation is to make it the *primary* path (general regular lowering, §4
> Amendment), with §4.2's boundary peek demoted to an optional fast-path. The build-
> time-rejection fallback below is retained **only** for any assertion that is
> genuinely unbounded/non-regular (none exist in the corpus) — never for the bounded
> internal assertions in the shipped grammars.

The plan is therefore:

- Implement the general regular lowering (§2 / §4 Amendment) so *every* bounded
  assertion — boundary or internal — compiles into the combined DFA. This is
  complete for every grammar in the corpus, bundled and `examples/`.
- The §4.2 boundary peek is kept only as an optional micro-optimization for the two
  literal token-edge assertions (`DEC_NUMBER`, `OP`); it is not load-bearing.
- A genuinely *unbounded* or non-regular assertion (which would require
  backtracking, and which no corpus grammar contains) is **rejected at build time
  with a clear, specific error**. This residual is strictly *smaller* than today's
  unstated "no backreferences" residual.
- §2 is the recipe and #35's product-construction is the engine.

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
- **M1 — Lookaround-aware front-end (§4.1).** ✅ **Landed** (`src/lookaround.rs`).
  A faithful recursive-descent regex parser produces a [`Node`] tree whose only
  structural variants are concat / alt / group / **assertion**; every other
  construct (literals, escapes, character classes, anchors, quantifiers) is kept
  verbatim in `Node::Atom` runs, so `to_source()` round-trips byte-identically — the
  property that lets M2 re-emit assertion-free fragments straight to
  `regex-automata`. `Node::assertions()` enumerates every assertion left-to-right
  tagged with its boundary context (`at_concat_start` / `at_concat_end`), boundary
  *and* internal alike per the §4 Amendment. Unit-tested against all the corpus
  assertions (`STRING`/`LONG_STRING`/`REGEXP`/`DEC_NUMBER`/`OP` + verilog
  `MULTILINE_COMMENT`) — round-trip + correct internal-vs-boundary classification —
  not just the boundary pair.
- **M2 — General regular lowering + validation (§2 / §4 Amendment),** still alongside
  the existing scanner. Compile each assertion into the terminal's automaton via the
  intersection/complement construction (boundary assertions take the §4.2 peek
  fast-path; internal ones the product-construction). Route the bundled lookaround
  terminals through it; oracle suite (`test_stdlib`, `test_common`, `test_json_corpus`,
  `test_lookaround`) stays green. This is the step that **removes the `REGEXP` ReDoS
  and the bail-wrong-answer risk** (those terminals are now on the linear engine).
  *(Re-scoped 2026-06-06: was "boundary-assertion lowering" only, which the §4
  Amendment shows covers just `DEC_NUMBER`/`OP` — the bundled `STRING`/`LONG_STRING`/
  `REGEXP` need the internal path here, not in a deferred follow-up.)*
- **M3 — Delete `fancy-regex`.** Remove the `AnyRegex::Fancy` arm, the dependency,
  and the dual-engine merge in `match_at`. With M2's general lowering, the bundled
  internal-assertion terminals stay green on the pure-`regex` engine; only a
  genuinely *unbounded/non-regular* assertion (none in the corpus) hits the rejection
  error. Compliance banks regenerated; `CLAUDE.md` parity-gap note rewritten.
  *(Re-scoped 2026-06-06: the original "internal assertions now hit the rejection
  error" would have regressed `%import python.STRING` / `lark.REGEXP`, which ship
  today — see §4 Amendment consequence 3.)*
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
