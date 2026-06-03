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

## Current state (2026-06-03)

- Bank: **257 grammars, 504 input-cases + construct-error checks**.
- Agreement: **≈68%**; **125 XFAIL entries**, **0 skipped**.
- XFAIL shape: `build:<ri>` (16), `construct:<ri>` (8), `parse:<ri>:<ci>` (101,
  of which 22 are downstream of a build failure and 79 are standalone tree/error
  divergences across 71 grammars).

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

## Milestones, ordered by leverage × confidence

The clusters are sized by how many XFAIL entries each should flip. Order is
chosen so the highest-leverage, highest-confidence root causes land first, and so
that build failures (which also drag 22 downstream `parse:` entries) are cleared
early.

### M1 — Escape-sequence decoding (`\x`, `\u`, `\U`) — ~26 entries, highest leverage

**Root cause (confirmed in code):** `unescape_string` in
`src/grammar/loader.rs` handles only `\n \t \r \\ \" \'`. Hex/unicode escapes
fall through to the `Some(c) => push('\\'); push(c)` arm, so `"\x01"` becomes the
literal 4-char string `\x01`, and a char-range bound like `"\x01".."\x03"` is
built from un-decoded bounds.

This single gap explains:
- **Build failures** `B: char-range with escape bounds` — ids 202–207
  (`A: "\U0000FFFF".."\U00010002"`, `"a".."c"`, `"\x01".."\x03"`).
- **Parse divergences** in string terminals with escapes — ids 200/201
  (`"\x01"`, `"\xABCD"`), 221–226 (`\U0010FFFF`, `ā`, mixed).

**Fix:** extend `unescape_string` to decode `\xHH`, `\uHHHH`, `\U00HHHHHH` (and
the `\0` / octal forms Lark accepts) to the corresponding `char`. Mirror Python
Lark's `lark/utils.py` escape handling exactly. Add an oracle test for an astral
codepoint (so we don't regress UTF-8 column counting from BUG-5).

### M2 — Regex terminal escape / char-class semantics — ~14 entries

**Symptom:** `/regex/` terminals with backslash escapes and character classes
diverge — ids 164–193 (`/[^x]+/`, `/[ab]/`, `/\//`, `/\[/`, `/\[ab]/`, `/\\/`,
`/\\[ab]/`, `/\\\t/`, `/\\t/`, `/\\w/`, `/\n/`, `/\t/`, `/\w/`).

**Likely root cause:** regex literal text is passed to the `regex` crate
verbatim (`loader.rs` ~1117) without translating Python `re` escape conventions,
and/or the anonymous-terminal value/typing differs. Python `re` and the Rust
`regex` crate mostly agree, so each id needs its failing case reproduced to split
"pattern compiles but matches wrong span" from "pattern fails to compile" from
"token_type/`__ANON_n` naming differs". Triage the 14 against the oracle first,
then fix the shared cause(s). This is the cluster with the most uncertainty —
budget reproduction time before committing to a fix.

### M3 — Case-insensitive terminals (`"a"i`, `/a/i`) — ~12 entries

**Symptom:** ids 42/43, 105, 106/107, 110/111, 114, 115/116. With
`keep_all_tokens`, the token *value* must preserve the source casing (`'A'` for
input `Aa`) while the terminal still matches case-insensitively; `"INT"i` keyword
retyping must interact correctly with `%ignore`.

**Fix:** ensure the `i` flag sets `IGNORECASE` on the compiled pattern (string
terminals `"a"i` currently take the `Pattern::Str` path — they must become a
case-insensitive regex, not a literal) and that the retained token carries the
matched source text, not the pattern text.

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

### M5 — `maybe_placeholders` (None for absent optionals) — ~4 entries

**Symptom:** ids 123/124 (`!start: ["a" ["b" "c"]]`), 232/234
(`start: ["a"] ["b"] ["c"]`) — for empty input the oracle tree has explicit
`None` children (one per absent `[...]` group); lark-rs omits them. Build
failures 108/109 (`!start: ("A"?)?`) are the nested-nullable shape of the same
feature.

**Fix:** when `maybe_placeholders` is on, each `[...]` (optional) that matched
nothing must emit a `None` placeholder child in position. This needs a
placeholder representation in the tree (`Child::None`) and `TreeBuilder` support —
note this is the one milestone that touches the shared tree representation Earley
will also use, so get it right here.

### M6 — Inline-pattern ↔ named-terminal collision — ~5 entries

**Symptom:** ids 14/15 (`C: "C" | D` terminal algebra typing), 155
(`start: "a" A` / `A: "a"` — input `aa`), 194/195 (`start: /a/` / `A: /a/`).
When an inline pattern is identical to a named terminal's pattern, Lark reuses the
named terminal's type (so the token is `A`, not `__ANON_n`).

**Fix:** in terminal collection, dedup an anonymous pattern against an existing
named terminal with the same pattern and reuse the named id.

### M7 — Construct-error parity — 8 entries

lark-rs must *reject at build time* grammars Python Lark rejects:
- ids 90/91 — `"A"~3..2` invalid repetition range (`min > max`).
- ids 65/66 — `%import bad_test.NUMBER` from a non-existent module.
- ids 57/58 — `/e?rez/` vs `/erez?/` regex-terminal collision detection.
- ids 73/74 — `start: a "."` / `a: "."+` (anonymous-terminal collision).

**Fix:** add the corresponding validation passes; each is small and independent.
Lowest leverage per fix but removes the "lark-rs is too permissive" class.

### M8 — Residual EBNF repetition / branch-choice tree-shape — ~6 entries

**Symptom:** ids 156/157 (`start: "a"* "b" | "a"+`), 158/159 (`start: "a"+`
with `keep_all_tokens`), 160/161 (`start: "a"+ "b" | "a"+` — build), 77/78
(`a.2 | b.1` rule-priority disambiguation — build), 49/50 (oversized priority
`A.-99999999999999999999999`).

These are a grab-bag: rule-priority resolution on ambiguous alternations, EBNF
`+`/`*` filtering under `keep_all_tokens`, and an integer-overflow on priority
parsing (49/50 — parse the priority as `i64`/bignum-clamped, not `i32`). Take
them last; reproduce each individually.

## Leverage summary

| Milestone | Theme | ~entries | Confidence |
|-----------|-------|---------:|------------|
| M1 | escape decoding `\x \u \U` | 26 | High (root cause confirmed) |
| M2 | regex escape / char-class | 14 | Medium (needs per-id repro) |
| M3 | case-insensitive terminals | 12 | High |
| M4 | template tree-shape + higher-order | 12 | Medium |
| M5 | maybe_placeholders | 6 | High |
| M6 | inline↔named terminal collision | 5 | High |
| M7 | construct-error parity | 8 | High |
| M8 | EBNF/priority residue | 8 | Mixed |

Clearing M1–M3 alone (~52 entries, all high/medium confidence) takes the bank
from ≈68% past ≈78%. M1+M5+M6 are the ones that touch shared lexer/tree-builder
code Earley depends on, so they pay double.

## Exit criterion — when Earley unfreezes

Phase 2 (Earley + SPPF) starts when **either**:

- the bank reaches **≥ 90% agreement** with the remaining XFAILs triaged and
  each annotated with a root cause, **or**
- the remaining XFAILs are demonstrably *not* LALR-fixable (they require Earley,
  ambiguity output, or a dynamic lexer) — at which point Earley *is* the way to
  climb them, and the bank should grow Earley-shaped cases alongside it.

Until then, every PR that touches the core should either flip XFAIL entries to
passing or hold the line — never regress the percentage.
