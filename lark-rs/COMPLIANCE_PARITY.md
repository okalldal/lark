# Compliance-Bank Parity Roadmap

**North star:** the compliance-bank percentage. A feature is not "done" until the
bank says it generalizes beyond JSON/arithmetic. Phase 2 (Earley/SPPF) stays
frozen until this roadmap is burned down — see the exit criterion at the bottom.

## Why parity before Earley

The bank is **100% LALR grammars** (257/257; zero Earley cases). Implementing
Earley would not move the parity number at all — the two are orthogonal work on
two different engines. Every remaining failure lives on the LALR path, and the
shared `TreeBuilder` / `TokenSource` / `CompiledGrammar` that Earley will be built
on. Hardening that core now means the SPPF forest-walk inherits a *correct*
shaper instead of 125 latent bugs we'd then be debugging across two engines with
no oracle to tell us which one is wrong.

## Current state (2026-06-03, after M1–M3 + M5-global)

- Bank: **257 grammars, 512 input-cases + construct-error checks**.
- Agreement: **89.6% (459/512)**; **53 XFAIL entries**, **0 skipped**.
  (Was 75.6% / 125 XFAIL before this sprint — see "Done" below.)
- Remaining XFAIL shape: `build:<ri>` (10), `construct:<ri>` (6),
  `parse:<ri>:<ci>` (37).

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

### M5 — `maybe_placeholders` residue (nested `[...]`) — ~4 entries

The grammar-wide `keep_all_tokens` half is **done**; what remains is *nested*
optionals: ids 123/124 (`!start: ["a" ["b" "c"]]`) and 227/228
(`["+"|"-"] float …`). A single `[...]` now emits the right `None` count; a
`[...]` nested inside another `[...]` does not yet. Build failures 108/109
(`!start: ("A"?)?`) are the nullable-EBNF shape of the same gap.

**Fix:** make placeholder counting recurse through nested maybe/optional groups
(the inner group's placeholder slots must surface in the outer empty production).
`Child::None` and `TreeBuilder` support already exist.

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
(`a.2 | b.1` rule-priority disambiguation — build), 49/50 (oversized priority
`A.-99999999999999999999999`).

These are a grab-bag: rule-priority resolution on ambiguous alternations, EBNF
`+`/`*` filtering under `keep_all_tokens`, and an integer-overflow on priority
parsing (49/50 — parse the priority as `i64`/bignum-clamped, not `i32`). Take
them last; reproduce each individually.

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
| **M6** | per-position token filtering (collision) | 5 | High effort | ⬜ open — architectural, load-bearing for Earley |
| **M4** | template tree-shape + higher-order | 12 | Medium | ⬜ open |
| **M8** | EBNF/priority residue (+ 73/74 conflict) | ~10 | Mixed | ⬜ open |
| **M5** | nested `maybe_placeholders` | 4 | Medium | ⬜ open |
| **M7b** | regex collision detection | 2 | Medium | ⬜ open — needs Lark's exact rule reproduced first |

The work so far took the bank from 75.6% to **89.6%** — 72 entries from six
root-cause fixes. The remaining 53 are M4/M6/M8, regex-collision, and
nested-maybe. M6 (and the rest of M5) touch shared lexer/tree-builder code Earley
depends on, so they still pay double. **Recommended next:** M6 — highest value
and the per-position keep-mask it introduces is exactly the chokepoint Earley's
forest-walk will reuse.

## Exit criterion — when Earley unfreezes

Phase 2 (Earley + SPPF) starts when **either**:

- the bank reaches **≥ 90% agreement** with the remaining XFAILs triaged and
  each annotated with a root cause, **or**
- the remaining XFAILs are demonstrably *not* LALR-fixable (they require Earley,
  ambiguity output, or a dynamic lexer) — at which point Earley *is* the way to
  climb them, and the bank should grow Earley-shaped cases alongside it.

Until then, every PR that touches the core should either flip XFAIL entries to
passing or hold the line — never regress the percentage.
