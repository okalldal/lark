# Compliance-Bank Parity Roadmap

**North star:** the compliance-bank percentage. A feature is not "done" until the
bank says it generalizes beyond JSON/arithmetic. Phase 2 (Earley/SPPF) stays
frozen until this roadmap is burned down — see the exit criterion at the bottom.

> **Exit criterion reached (2026-06-03):** the bank is at **98.4%** (≥ 90%), with
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

## Current state (2026-06-03, after M1–M6 + M4 + M8)

- Bank: **257 grammars, 512 input-cases + construct-error checks**.
- Agreement: **98.4% (504/512)**; **8 XFAIL entries**, **0 skipped**.
  (Was 75.6% / 125 XFAIL at the start of this sprint, 94.1% / 30 before M8 — see
  "Done" below.)
- Remaining XFAIL shape: `construct:<ri>` (4), `parse:<ri>:<ci>` (4). These are the
  genuinely-hard tail: regex-collision construct errors (57/58), an LALR
  conflict-*detection* parity case (73/74), and terminal-algebra token typing
  (14/15). No more EBNF/template/placeholder/filtering work remains.

## Done — M8 EBNF repetition / branch-choice / nullable (latest)

Two root causes, both matching Python Lark: (1) identical `x+`/`x*` occurrences now
*share* one recurse rule (`plus_helper` + `recurse_cache`, Lark's `rules_cache`),
and `x*` wraps the same shared rule — collapsing the duplicate reductions that were
an unresolvable reduce/reduce; (2) redundant nullable wrappers collapse (single-symbol
groups inline; a `?` over an already-nullable helper is dropped). Flipped 22 XFAILs
(30 → 8): 156/157, 160/161, 77/78, 227/228, and 108/109. Full suite + compliance
green, no regressions. Pinned by `tests/test_ebnf_sharing.rs`. Details in the M8
milestone below.

## Done — M4 template instantiation tree-shape

All template tree-shape divergences fixed in `instantiate_template` /
`subst_value` / `lower()`: instances now form a node labeled with the *base* name
(or inline when the base is `_`-prefixed), inherit the template's `!`/`?`/priority
options, and resolve higher-order templates (a parameter applied as a template).
14 XFAILs flipped (44 → 30); full suite + compliance green, no regressions. Pinned
by `tests/test_templates.rs`. Details in the M4 milestone below.

## Done — M6 per-position token filtering (architectural)

The load-bearing refactor the roadmap flagged for Earley. Filtering moved off the
per-terminal `filter_out` flag onto a **per-rule-position keep mask**
(`CompiledRule::filter_pos`), and anonymous literals/ranges now **unify** with an
existing same-pattern terminal by adopting its name (`intern_anon_pattern`).
3 XFAILs flipped (47 → 44); full oracle + JSON-corpus + compliance suites green,
no regressions. Pinned by `tests/test_terminal_unification.rs`. Details in the M6
milestone below.

## Done — M5-nested + M8-priority (crossed the 90% exit criterion)

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

### M4 — Template instantiation tree-shape — ✅ done

**Symptom:** ids 2/3 (`sep{NUMBER,","}`), 4/5 (`!_expr{t}` transparent +
keep_all), 6/7 (`expr{"B"}` string arg), 8/9 (`expr{t}: … | … -> b` alias arm).
Build failures 245/246 (`a{b}` / `a{t}: t{"a"}` — **higher-order templates**,
a template passed as a template argument).

**✅ Done.** Three root causes in `instantiate_template`, all matching Python Lark's
`ApplyTemplates`:

1. **Instance naming / tree label.** Instances were named `__{name}_{parent}_{N}`,
   so they always started with `_` (wrongly *transparent*) and the tree label was
   the mangled name. Now an instance is named `base{N}` — the `{` marks it as a
   template instance whose tree label is the *base* name (Lark's `template_source`,
   stripped in `lower()` via `template_base`), and the intact base prefix makes
   `_expr` transparent while `expr`/`sep` form a node.
2. **Inherited options.** Instances used the anon-helper defaults, dropping the
   template's `!` keep-all / `?` expand1 / priority. They now build their own
   `RuleOptions` from the template's modifiers (stored alongside the template), and
   `current_keep_all` is set while the body compiles — so `!expr{t}` keeps tokens.
3. **Higher-order templates (245/246).** `subst_value` now substitutes a template
   *usage's name*, not just its args: `a{t}: t{"a"}` instantiated as `a{b}` resolves
   `t{"a"}` to `b{"a"}` instead of erroring on undefined `t`.

Pinned by `tests/test_templates.rs`.

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

### M6 — Inline-pattern ↔ named-terminal collision — ✅ core done (155, 194/195); 14/15 remain

**Symptom:** ids 14/15 (`C: "C" | D` terminal algebra typing), 155
(`start: "a" A` / `A: "a"` — input `aa`), 194/195 (`start: /a/` / `A: /a/`).
When an inline pattern is identical to a named terminal's pattern, Lark reuses the
named terminal's type (so the token is `A`, not `__ANON_n`).

**✅ Done — the architectural fix landed.** Token filtering moved from a
per-terminal `filter_out` flag to a **per-rule-position keep mask** (Lark's model):

- `TerminalDef` no longer carries `filter_out`. Each `Symbol::Terminal`
  *occurrence* carries its own `filter_out` (string literal → dropped, regex /
  range / non-`_` named ref → kept), and `lower()` collapses those into
  `CompiledRule::filter_pos` — a `Vec<bool>` parallel to the expansion. The
  `TreeBuilder` keeps/drops the token at position `i` by `filter_pos[i]`, so two
  symbols that share a terminal can still have *different* keep/drop fates.
- `intern_anon_pattern` now **unifies** an anonymous literal/range with an existing
  same-pattern terminal (named or anon) by adopting its name — exactly Lark's
  `PrepareAnonTerminals`. So `"a"` lexes as `A` when `A: "a"` exists (fixes the
  basic-lexer case 155, which could not parse at all before), and an inline `/a/`
  reuses `A` (fixes 194/195's `__ANON_0` → `A`).

This is the same chokepoint Earley's forest-walk will reuse: the SPPF→tree
conversion collects one value per expansion symbol and applies `filter_pos[i]`
identically. Pinned by `tests/test_terminal_unification.rs`.

**Still open — 14/15** (`start: "a" /b+/ C` with `C: "C" | D`, `D: "D" E`, `E: "e"`).
These pair the unification above with terminal *algebra* (a terminal referencing
another). The remaining divergence is in how `C: "C" | D` types its token under
the contextual lexer, not in filtering — track under terminal-algebra follow-up.

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

### M8 — Residual EBNF repetition / branch-choice tree-shape — ✅ done

**Symptom:** ids 156/157 (`start: "a"* "b" | "a"+`), 160/161 (`start: "a"+ "b" | "a"+`
— build), 77/78 (`a.2 | b.1` rule-priority disambiguation — build), 108/109
(`!start: ("A"?)?` — nested nullable optionals, build R/R), 227/228
(`digit* "." … | digit+ exp` — branch-choice parse error on `1.2`).

**✅ Done.** Two root causes, both matching Python Lark:

- **Shared EBNF recurse helpers (`plus_helper` + `recurse_cache`).** lark-rs created
  a *separate* `P: x | P x` recurse rule for every `x+`/`x*` occurrence, so two
  branches over the same repetition (`"a"+ "b" | "a"+`) had duplicate, conflicting
  `… -> "a"` reductions → unresolvable reduce/reduce. They are now cached by
  `(inner, keep_all)` and shared (Lark's `rules_cache`), and `x*` is an optional
  wrapper over the *same* shared recurse rule. This single change made 156/157,
  160/161, 77/78 (the rule-priority R/R now resolves cleanly: `a.2` beats `b`'s
  empty `"B"?` reduction) and 227/228 (`1.2`) all LALR-parseable.
- **Collapse redundant nullable wrappers.** A single-symbol group is inlined to its
  symbol, and a `?` over an already-nullable `?`/`*` helper is dropped, so `("A"?)?`
  compiles to one nullable rule instead of two ambiguous empty rules (108/109).
  This is what Python achieves via distribute + `dedup_list`.

Pinned by `tests/test_ebnf_sharing.rs`.

- ✅ **Oversized priority — ids 49/50** (`A.-99999999999999999999999`). **Done.**
  The grammar lexer now accepts the negative sign and saturates a priority that
  overflows `i32` to `i32::MIN`/`MAX` (Python Lark uses arbitrary-precision int
  priorities), so the grammar builds and `ab` lexes as the higher-priority `AB`.
  Pinned by `tests/test_placeholders_and_priority.rs`.

- **Still open — 73/74** (`start: a "."` / `a: "."+`). A genuine LALR conflict Lark
  reports as unresolvable but lark-rs resolves (S/R → shift). This is
  conflict-*detection* parity (report the conflict instead of silently shifting),
  tracked with the M7 construct-error work, not the repetition tree-shape above.

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
| M6-core | per-position token filtering + unify (155, 194/195) | — | High | ✅ done |
| M4 | template tree-shape + higher-order (2–9, 245/246) | — | Medium | ✅ done |
| M8 | EBNF repetition / branch-choice / nullable (156/157, 160/161, 77/78, 227/228, 108/109) | — | Mixed | ✅ done |
| **M6b** | terminal-algebra typing under collision (14/15) | 4 | Medium | ⬜ open |
| **M7b** | regex collision construct errors (57/58) | 2 | Medium | ⬜ open — needs Lark's exact rule reproduced first |
| **M8b** | conflict-*detection* parity (73/74) | 2 | Hard | ⬜ open — report S/R conflict instead of silently shifting |

The work so far took the bank from 75.6% to **98.4%** — 117 entries from eleven
root-cause fixes. The remaining **8** are the genuinely-hard tail: regex-collision
construct errors (57/58, needs Lark's exact collision rule reproduced first), an
LALR conflict-*detection* parity case (73/74, requires reporting a shift/reduce
conflict lark-rs currently auto-resolves), and terminal-algebra token typing
(14/15). All EBNF / template / placeholder / filtering / priority work is done.
**Recommended next:** Phase 2 (Earley/SPPF) — the exit criterion is far exceeded and
the shared `CompiledGrammar` / `TreeBuilder` (`filter_pos`) contract is settled; the
remaining 8 are niche LALR-side parity items that can proceed in parallel.

## Exit criterion — when Earley unfreezes

Phase 2 (Earley + SPPF) starts when **either**:

- the bank reaches **≥ 90% agreement** with the remaining XFAILs triaged and
  each annotated with a root cause, **or**
- the remaining XFAILs are demonstrably *not* LALR-fixable (they require Earley,
  ambiguity output, or a dynamic lexer) — at which point Earley *is* the way to
  climb them, and the bank should grow Earley-shaped cases alongside it.

Until then, every PR that touches the core should either flip XFAIL entries to
passing or hold the line — never regress the percentage.
