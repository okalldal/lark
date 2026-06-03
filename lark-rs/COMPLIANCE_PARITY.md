# Compliance-Bank Parity Roadmap

**North star:** the compliance-bank percentage. A feature is not "done" until the
bank says it generalizes beyond JSON/arithmetic. Phase 2 (Earley/SPPF) stays
frozen until this roadmap is burned down — see the exit criterion at the bottom.

> **Exit criterion reached (2026-06-03):** the bank is at **90.8%** (≥ 90%), with
> every remaining XFAIL triaged and root-caused below. Phase 2 (Earley/SPPF) is
> now eligible to start; this roadmap continues in parallel to keep climbing the
> LALR path. See the exit criterion at the bottom.

## Why parity before Earley

The bank is **100% LALR grammars** (257/257; zero Earley cases). Implementing
Earley would not move the parity number at all — the two are orthogonal work on
two different engines. Every remaining failure lives on the LALR path, and the
shared `TreeBuilder` / `TokenSource` / `CompiledGrammar` that Earley will be built
on. Hardening that core now means the SPPF forest-walk inherits a *correct*
shaper instead of 125 latent bugs we'd then be debugging across two engines with
no oracle to tell us which one is wrong.

## Current state (2026-06-03, after M1–M3 + M5-global + M5-nested + M8-priority)

- Bank: **257 grammars, 512 input-cases + construct-error checks**.
- Agreement: **90.8% (465/512)**; **47 XFAIL entries**, **0 skipped**.
  (Was 75.6% / 125 XFAIL at the start of this sprint, 89.6% / 53 before the latest
  two fixes — see "Done" below.)
- Remaining XFAIL shape: `build:<ri>` (8), `construct:<ri>` (4),
  `parse:<ri>:<ci>` (35).

## Done — M5-nested + M8-priority (latest, crossed the 90% exit criterion)

Two further root-cause fixes in `loader.rs`, 6 XFAILs flipped (53 → 47), no
regressions, full oracle + JSON-corpus + compliance suite green. Pinned by
`tests/test_placeholders_and_priority.rs`.

1. **M5-nested — recursive `maybe_placeholders` (ids 123/124).** Each anonymous
   maybe/optional/group helper now records its inlined "rule size" (`helper_sizes`),
   and `symbol_size` sums those recursively when counting an absent `[...]`'s `None`
   placeholders — mirroring Python Lark's `FindRuleSize`. A `[...]` nested in another
   `[...]` now contributes its own slot count (so `["a" ["b" "c"]]` empty → 3 Nones).
2. **M8-priority — oversized terminal priority (ids 49/50).** The grammar lexer now
   reads a negative priority sign and saturates a value that overflows `i32` to the
   `i32` extreme, instead of failing to lex. Python Lark's priorities are
   arbitrary-precision ints; saturating preserves the ordering intent.

## Done — Sprint "lexer & terminal-filtering parity" (M1, M2, M3, M5-global) + M7 (partial)

Six root-cause fixes in `loader.rs` / `lexer.rs`, 72 XFAILs flipped (125 → 53), no
regressions, full oracle + JSON-corpus suite green. Pinned by
`tests/test_escapes_and_filtering.rs` and `tests/test_construct_errors.rs`.

In addition to M1–M3 + M5-global below, two **M7** construct-error validations
landed: an empty repetition range (`"A"~3..2`, min > max) and an unresolvable
import (`%import bad_test.NUMBER`, a non-`common` module) now fail to build, as
Python Lark does. The other two M7 cases are deferred (see M7 below): `/e?rez/`
vs `/erez?/` (regex collision) and `a: "."+` (a real LALR conflict, → M8).

1. **M1 — escape decoding.** `unescape_string` now decodes `\xHH`, `\uHHHH`,
   `\UHHHHHHHH` (plus `\f \v \0`), so string terminals and char-range bounds with
   escapes build and match. Malformed escapes fall back to literal text.
2. **M2 — anonymous regex literals are kept.** An inline `/regex/` (or char
   range) produced a `filter_out` token like a string literal, so its tokens
   vanished from the tree. Now only anonymous *string* literals are filtered;
   regex/range literals are kept, matching Lark's `__ANON_n` behavior. *(This,
   not escape handling, was the bulk of the old M2 cluster.)*
3. **M3 — case-insensitive flag honored.** The scanner built its combined regex
   with `as_regex_str()`, which *drops* per-terminal flags, so `"a"i` never
   matched `A`. It now uses `to_inline_regex()`, scoping `(?i:…)` to each group.
4. **M5-global — grammar-wide `keep_all_tokens`.** The `LarkOptions` field was
   defined but never threaded into the loader (only the per-rule `!` modifier
   worked). It now flows into `GrammarCompiler`, so it keeps tokens *and* drives
   `maybe_placeholders` counting.

## Methodology (unchanged — this is the discipline, not a detour)

Each milestone below follows the project loop:

1. Pick the cluster. Find one representative XFAIL id and read its grammar+case.
2. Reproduce: confirm the failing tree/error against the oracle (it is already
   captured in `bank.json`; for a focused oracle add it to `generate_oracles.py`).
3. Fix at the root, not the symptom.
4. `cargo test` green, then regenerate the allow-list:
   `LARK_COMPLIANCE_WRITE_XFAIL=1 cargo test --test test_compliance`.
5. Commit the **shrunk** `xfail.json` with the fix; the prose count above and in
   `CLAUDE.md` gets bumped to the new percentage.

`LARK_COMPLIANCE_TRACE=1` prints each grammar before it runs. Never push without
`scripts/check.sh` green.

## Milestones

**M1–M3 and the global-`keep_all_tokens` half of M5 are done** (see the "Done"
section above). The remaining work, ordered by leverage × confidence:

### M4 — Template instantiation tree-shape — ~10 entries (+2 build, +2 construct-adjacent)

**Symptom:** ids 2/3 (`sep{NUMBER,","}`), 4/5 (`!_expr{t}` transparent +
keep_all), 6/7 (`expr{"B"}` string arg), 8/9 (`expr{t}: … | … -> b` alias arm).
Build failures 245/246 (`a{b}` / `a{t}: t{"a"}` — **higher-order templates**,
a template passed as a template argument).

**Fix:** the BUG-7 work fixed *recursive* and *nested* template substitution; the
remaining divergences are tree-shape (transparent/alias/keep_all interaction
inside instantiated bodies) and the higher-order case where a parameter is itself
applied as a template. Confirm each against the oracle; the higher-order case may
need `instantiate_template` to resolve a parameter that resolves to another
template.

### M5 — `maybe_placeholders` residue (nested `[...]`) — ✅ nested done; 227/228 + 108/109 reclassified

- ✅ **Nested `[...]` placeholder counting — ids 123/124** (`!start: ["a" ["b" "c"]]`).
  **Done.** `compile_maybe`/`compile_group`/`opt` now record each helper's inlined
  "rule size" (`helper_sizes`) and `symbol_size` sums it recursively, mirroring
  Python Lark's `FindRuleSize`: a `[...]` nested inside another `[...]` contributes
  its own slot count, so an absent `["a" ["b" "c"]]` emits 3 `None`s, not 1. Pinned
  by `tests/test_placeholders_and_priority.rs`.
- **227/228** (`["+"|"-"] float …`) — **reclassified.** The failing case is `1.2`
  raising `UnexpectedToken`, *not* a placeholder mismatch: `digit* "." …` vs
  `digit+ exp` is an LALR alternation the engine commits to wrongly. Belongs with
  M8 (EBNF repetition / branch-choice), not here.
- **108/109** (`!start: ("A"?)?`) — **reclassified.** This is a *build* failure: two
  nested nullable optionals reduce-empty in the same state, which lark-rs reports as
  an R/R conflict. It is a nullable-EBNF LALR-construction gap (M8-adjacent), not a
  placeholder-counting gap.

### M6 — Inline-pattern ↔ named-terminal collision — ~5 entries

**Symptom:** ids 14/15 (`C: "C" | D` terminal algebra typing), 155
(`start: "a" A` / `A: "a"` — input `aa`), 194/195 (`start: /a/` / `A: /a/`).
When an inline pattern is identical to a named terminal's pattern, Lark reuses the
named terminal's type (so the token is `A`, not `__ANON_n`).

**Dead end (tried, reverted):** the obvious fix — dedup an anonymous literal
against an existing same-pattern terminal (gated on equal `filter_out`) — flips
194/195 but **regresses 6 other cases**, because Python does not key tree
filtering on the terminal's `filter_out` at all. It filters per *rule-symbol
occurrence*: in `start: "a" A`, position 0 (the literal) is dropped and position
1 (the `A` ref) is kept even though both lex to the *same* unified terminal.
lark-rs instead carries one `filter_out` per terminal, so once two symbols share
a terminal they share a keep/drop fate — and merging changes the type of tokens
in unrelated grammars.

**Real fix (deferred, architectural):** move token filtering from a per-terminal
`filter_out` flag to a per-rule-position keep mask (Lark's model). Then
same-pattern terminals can be unified for *lexing* while each rule position keeps
its own drop/keep decision. This is the same chokepoint Earley's forest-walk will
use, so it is worth doing once, carefully — not as a quick dedup.

### M7 — Construct-error parity — 4 entries remaining

lark-rs must *reject at build time* grammars Python Lark rejects:
- ✅ ids 90/91 — `"A"~3..2` invalid repetition range (`min > max`). **Done.**
- ✅ ids 65/66 — `%import bad_test.NUMBER` from a non-existent module. **Done.**
- ids 57/58 — `/e?rez/` vs `/erez?/`. Lark raises a *terminal collision* error
  when two regex terminals can match the same input ambiguously; needs Lark's
  exact collision rule reproduced before implementing (don't guess — over-eager
  rejection would regress valid overlapping terminals).
- ids 73/74 — `start: a "."` / `a: "."+`. This is **not** a simple validation: it
  is a genuine LALR conflict Lark reports as unresolvable but lark-rs resolves
  (S/R → shift). Belongs with M8 (conflict-detection parity), not here.

### M8 — Residual EBNF repetition / branch-choice tree-shape — ~6 entries

**Symptom:** ids 156/157 (`start: "a"* "b" | "a"+`), 158/159 (`start: "a"+`
with `keep_all_tokens`), 160/161 (`start: "a"+ "b" | "a"+` — build), 77/78
(`a.2 | b.1` rule-priority disambiguation — build), 108/109 (`!start: ("A"?)?` —
nested nullable optionals, build R/R), 227/228 (`digit* "." … | digit+ exp` —
branch-choice parse error on `1.2`).

These are a grab-bag: rule-priority resolution on ambiguous alternations, EBNF
`+`/`*` filtering under `keep_all_tokens`, nullable-EBNF LALR construction, and
branch-choice on overlapping repetition alternations. Take them last; reproduce
each individually.

- ✅ **Oversized priority — ids 49/50** (`A.-99999999999999999999999`). **Done.**
  The grammar lexer now accepts the negative sign and saturates a priority that
  overflows `i32` to `i32::MIN`/`MAX` (Python Lark uses arbitrary-precision int
  priorities), so the grammar builds and `ab` lexes as the higher-priority `AB`.
  Pinned by `tests/test_placeholders_and_priority.rs`.

## Follow-up tickets / index

> **GitHub issues are disabled on this repository, so this section is the
> tracker.** Each open ticket below has a stable ID and a self-contained
> milestone section above (root cause, compliance-bank ids, proposed fix, files,
> done-when). If issues get enabled later, lift each ticket into one verbatim.

| Ticket | Theme | ~entries | Confidence | Status |
|--------|-------|---------:|------------|--------|
| M1 | escape decoding `\x \u \U` | — | High | ✅ done (PR #15) |
| M2 | anonymous regex literals kept | — | High | ✅ done (PR #15) |
| M3 | case-insensitive terminals | — | High | ✅ done (PR #15) |
| M5-global | grammar-wide `keep_all_tokens` | — | High | ✅ done (PR #15) |
| M7a | invalid range + bad import | — | High | ✅ done (PR #15) |
| M5-nested | nested `maybe_placeholders` (123/124) | — | High | ✅ done |
| M8-priority | oversized terminal priority (49/50) | — | High | ✅ done |
| **M6** | per-position token filtering (collision) | 5 | High effort | ⬜ open — architectural, load-bearing for Earley |
| **M4** | template tree-shape + higher-order | 12 | Medium | ⬜ open |
| **M8** | EBNF/priority residue (156–161, 77/78, 108/109, 227/228, + 73/74 conflict) | ~12 | Mixed | ⬜ open |
| **M7b** | regex collision detection | 2 | Medium | ⬜ open — needs Lark's exact rule reproduced first |

The work so far took the bank from 75.6% to **90.8%** — 78 entries from eight
root-cause fixes. The remaining 47 are M4, M6, and M8 (the EBNF/priority residue
absorbed the old "nested-maybe" cluster once 123/124 were fixed and 227/228 + 108/109
were reclassified as LALR-shape issues), plus regex-collision. M6 touches shared
lexer/tree-builder code Earley depends on, so it pays double. **Recommended next:**
M6 — highest value, and the per-position keep-mask it introduces is exactly the
chokepoint Earley's forest-walk will reuse. With the 90% exit criterion met, Phase 2
may also begin in parallel.

## Exit criterion — when Earley unfreezes

Phase 2 (Earley + SPPF) starts when **either**:

- the bank reaches **≥ 90% agreement** with the remaining XFAILs triaged and
  each annotated with a root cause, **or**
- the remaining XFAILs are demonstrably *not* LALR-fixable (they require Earley,
  ambiguity output, or a dynamic lexer) — at which point Earley *is* the way to
  climb them, and the bank should grow Earley-shaped cases alongside it.

Until then, every PR that touches the core should either flip XFAIL entries to
passing or hold the line — never regress the percentage.
