# Compliance-Bank Parity Roadmap

**North star:** the compliance-bank percentage. A feature is not "done" until the
bank says it generalizes beyond JSON/arithmetic. Phase 2 (Earley/SPPF) stays
frozen until this roadmap is burned down — see the exit criterion at the bottom.

> **Exit criterion reached (2026-06-03):** the bank is at **99.6%** (≥ 90%), with
> the single remaining XFAIL cluster triaged, root-caused, and deferred below.
> Phase 2 (Earley/SPPF) is eligible to start; this roadmap continues in parallel
> to keep climbing the LALR path. See the exit criterion at the bottom.

> **Sprint update (2026-06-03, "compliance parity"):** the supposed "hard tail"
> was misdiagnosed. Investigating it revealed that **three of the four remaining
> clusters were extractor-fidelity bugs, not engine parity gaps**: the bank
> recorded oracles for grammars built with behaviour-changing options
> (`strict=True`, `g_regex_flags=re.I`) that the extractor *dropped*, so it
> attributed a strict-only construct error to the default mode, and a
> case-insensitive parse tree to a case-sensitive grammar. Recording those two
> options (extractor + Rust harness + `LarkOptions`) and implementing the two
> small features behind them flipped **6 XFAILs** (8 → 2): 14/15 (`g_regex_flags`)
> and 73/74 (strict shift/reduce). Only 57/58 (strict regex-collision) remains,
> deferred with cause (needs an `interegular`-equivalent FSM intersection engine).

> **Phase 2 update (Sprint 0 done):** Earley now has its **own** compliance bank —
> `compliance/earley_bank.json` (147 grammars, 209 cases, 15 explicit-ambiguity),
> strip-mined from Lark's `TestEarleyBasic` + `TestFullEarleyBasic` and replayed by
> `test_earley_compliance.rs`, gated by `earley_xfail.json`. It is a *separate*
> percentage from the LALR bank below (which stays byte-for-byte unchanged). While
> the Earley engine is a stub, every Earley entry is XFAIL; Sprints 1–4 burn it
> down. See [`PHASE_2_PLAN.md`](PHASE_2_PLAN.md).

## Why parity before Earley

The (LALR) bank is **100% LALR grammars** (257/257; zero Earley cases). Implementing
Earley would not move *this* parity number at all — the two are orthogonal work on
two different engines, and Earley now has its own bank (above). Every remaining failure lives on the LALR path, and the
shared `TreeBuilder` / `TokenSource` / `CompiledGrammar` that Earley will be built
on. Hardening that core now means the SPPF forest-walk inherits a *correct*
shaper instead of 125 latent bugs we'd then be debugging across two engines with
no oracle to tell us which one is wrong.

## Current state (2026-06-03, after M1–M8 + the fidelity sprint)

- Bank: **257 grammars, 512 input-cases + construct-error checks**.
- Agreement: **99.6% (510/512)**; **2 XFAIL entries**, **0 skipped**.
  (Was 75.6% / 125 XFAIL at the start of the original sprint, 98.4% / 8 before the
  fidelity sprint — see "Done" below.)
- Remaining XFAIL shape: `construct:57`, `construct:58` — the **same** grammar
  (`A: /e?rez/` vs `B: /erez?/`) under `strict=True`, captured once per lexer
  (contextual + basic). It is a strict-mode regex-collision construct error,
  **deferred** (needs an `interegular`-equivalent overlap engine; see M7b). No
  EBNF/template/placeholder/filtering/typing work remains.

### The bank now records two previously-dropped options

`strict` and `g_regex_flags` change the *outcome* of construction/lexing, so the
extractor and the Rust harness now record and replay them (and `LarkOptions`
carries both). Before this, the bank was silently infidelious for any grammar the
Lark suite built with those options — which is exactly what produced 4 of the 8
"hard tail" XFAILs.

## Done — extractor-fidelity sprint (`strict` + `g_regex_flags`) (latest)

Root cause for 6 of the last 8 XFAILs: the extractor dropped two
behaviour-changing options, so the bank's oracle did not match the *recorded*
configuration. Fixed by recording them end-to-end and implementing the two
features they gate:

- **`g_regex_flags` (ids 14/15).** From Lark's `test_g_regex_flags`, built with
  `g_regex_flags=re.I` (the test asserts only that it parses; the bank froze the
  case-insensitive trees). `LarkOptions.g_regex_flags` now threads a flag bitset
  into `LexerConf`; the `Scanner` prepends a global `(?i)`-style group to the
  combined regex (and the `unless` membership tests), so **every** terminal —
  string literals included — matches under the flag without mutating any
  `TerminalDef`. Zero-cost and zero-risk for the other 508 cases (the flag is 0
  for them). Pinned by `tests/test_g_regex_flags.rs`.
- **`strict` shift/reduce (ids 73/74).** `start: a "."` / `a: "."+` is a genuine
  S/R conflict Lark resolves as a shift by default but rejects under
  `strict=True`. `LarkOptions.strict` flows into `build_lalr_table`, which now
  raises `GrammarError::Conflict` on an S/R conflict in strict mode (R/R was
  already always-fatal, matching Lark). Default mode is unchanged. Pinned by
  `tests/test_strict_mode.rs`.

This corrected the earlier mis-triage: there was no "terminal-algebra token
typing" gap (14/15) and no "conflict-*detection* parity" engine gap (73/74) — both
were the missing options. See the M6b/M8b sections below for the retraction.

## Done — M8 EBNF repetition / branch-choice / nullable

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

**14/15 — ✅ done, and the original triage was wrong.** This was *not* a
terminal-algebra token-typing gap. The grammar comes from Lark's
`test_g_regex_flags`, built with `g_regex_flags=re.I`; the bank had frozen the
case-insensitive trees while recording the grammar as case-sensitive. lark-rs was
typing the tokens correctly all along — it just lexed case-sensitively because the
option was never recorded. Fixed by the fidelity sprint (`g_regex_flags` support);
see the "Done" section above.

### M7 — Construct-error parity — 2 entries remaining (57/58, deferred → M7b)

lark-rs must *reject at build time* grammars Python Lark rejects:
- ✅ ids 90/91 — `"A"~3..2` invalid repetition range (`min > max`). **Done.**
- ✅ ids 65/66 — `%import bad_test.NUMBER` from a non-existent module. **Done.**
- ids 57/58 — `/e?rez/` vs `/erez?/`, **only under `strict=True`** (confirmed
  against Lark 1.3.1: builds fine in default mode, raises `LexError` in strict).
  Lark delegates the check to the **`interegular`** library: it groups regex
  terminals by priority, compiles each to an FSM, and reports a collision when two
  same-priority regexes have a non-empty intersection (with a concrete example
  string). **Deferred — too large for this sprint** (M7b). Reproducing it needs an
  FSM-intersection-emptiness engine, and the doc's own warning stands: a hand-rolled
  approximation risks over-rejecting valid overlapping terminals. Now that `strict`
  is recorded, these two entries are honestly labelled as a strict-mode collision
  gap rather than a phantom default-mode error.
- ids 73/74 — `start: a "."` / `a: "."+`, **only under `strict=True`**. ✅ **Done**
  via the fidelity sprint: it is a genuine S/R conflict that lark-rs (like Lark)
  resolves as a shift by default, and now raises in strict mode. There was no
  default-mode conflict-detection gap to close.

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

- ✅ **73/74 done** (`start: a "."` / `a: "."+`). The conflict was real but only
  *fatal under `strict=True`* — lark-rs and Lark both resolve it as a shift by
  default. The fidelity sprint added `strict` and now raises the S/R conflict in
  strict mode. Not a default-mode conflict-detection gap. See the "Done" section.

## Follow-up tickets / index

> **GitHub issues are disabled on this repository, so this section is the
> tracker.** Each open ticket below has a stable ID and a self-contained
> milestone section above (root cause, compliance-bank ids, proposed fix, files,
> done-when). If issues get enabled later, lift each ticket into one verbatim.

### Active backlog (next up)

These are the open follow-ups from the Phase-2 review (2026-06-04) plus the Phase-3
Sprint-1 (`common.lark`) review, ordered by priority. None blocks the roadmap;
they are the loose ends to land before they get more expensive to fix. Each has a
self-contained detail block under **"Active backlog — detail"** below.

| Ticket | Theme | Confidence | Status |
|--------|-------|------------|--------|
| **P2-1** | Earley cost-of-generality perf gate — Sprint-2's documented exit criterion (within K× of LALR on unambiguous input) | High | ✅ resolved — Earley bench wired; constant-K premise disproved (super-linear), criterion downgraded to deferred, super-linearity tracked as **P2-4** |
| **P2-2** | Earley deferred-XFAIL burndown — nested `_ambig` via `_rule`+EBNF helper (on **both** banks), `%ignore`-of-content, `dynamic_complete` resolve tie-break | Mixed | ⬜ open |
| **P3-1** | `ESCAPED_STRING` lookbehind-adaptation hardening — parity rests on 4 oracle cases; add adversarial cases to lock the edges (from PR #28 review) | High | ✅ done — 8 adversarial cases added; lark-rs ≡ Python Lark on all, adaptation confirmed correct |
| **P2-3** | De-recurse the forest→tree walk — drop the 256 MB scoped-thread stack band-aid for an explicit-stack iterative walk | Medium | ⬜ open (profiler/robustness-gated) |
| **P2-4** | Earley super-linearity on unambiguous input — implement the Joop-Leo transitive optimization (or a completer reverse-index) so the completer stops rescanning the whole origin column | High | ⬜ open (surfaced by P2-1) |

### Active backlog — detail

#### P2-1 — Earley cost-of-generality perf gate ✅ RESOLVED (by downgrade)

**Resolution (2026-06-04 sprint):** the Earley side of the bench harness is wired
(`benches/parse.rs`): `cargo bench --bench parse` re-runs the unambiguous workloads
under `parser='earley'`, prints the per-row Earley/LALR ratio + a `ratio` summary
line, and adds a reported-only pathological-ambiguous workload (`S → S S | "b"`,
n = 4/8/12/16). **The K× assertion was *not* shipped, on purpose:** wiring the
measurement up disproved the ticket's constant-K premise. The Earley/LALR ratio is
not a constant — it grows with input size (≈15×→32×→196× as JSON scales
0.4K→8.7K→92K on the reference box; json_large = ~3.3 s for 92 KB). That is
structural: the completer (`earley.rs::predict_and_complete`) rescans the whole
origin column because the Joop-Leo transitive optimization is deliberately omitted,
so Earley is super-linear on list-shaped unambiguous input. A single-K ceiling is
therefore unmeetable pre-Leo, so per this ticket's stated alternative the criterion
is **downgraded from a Sprint-2 exit criterion to deferred** (`PHASE_2_PLAN.md` §4,
§10). The ratios are reported as a trend so a future Leo win shows up as the numbers
dropping. The newly-surfaced super-linearity is tracked as **P2-4**.

**Files (shipped):** `benches/parse.rs`, `BENCH.md`, `PHASE_2_PLAN.md` §4 + §10.

#### P2-2 — Earley deferred-XFAIL burndown

**Done-when:** the Earley XFAIL allow-lists shrink as each cluster is root-caused
and fixed (regenerate with `LARK_COMPLIANCE_WRITE_XFAIL=1 cargo test --test
test_earley_compliance` / `--test test_earley_dynamic_compliance`, commit the
shrunk lists). Current state: basic bank **211/211** (0 XFAILs — clean), dynamic
bank **446/454** (8 XFAILs).

**Clusters, by value:**
1. **Nested `_ambig` through a transparent `_rule` + an EBNF (`+`) helper** —
   ✅ **FIXED.** Was the single most recurring deferral, appearing on **both** banks
   (basic `parse:44`; dynamic `parse:96/97/130/131`, and `parse:87` as a knock-on).
   Representative: `start: _field+ / _field: f1 | f2 | f3 / …` on input `1M2` at
   `ambiguity='explicit'`. Root cause: lark-rs only ever built `_ambig` at the
   symbol-node level, so an `_ambig` arising *inside* a transparent (`_rule` /
   `__anon_*`) child was spliced in as `parent(_ambig(…))` instead of being lifted
   to `_ambig(parent(…), parent(…))`. Fixed by porting Lark's `AmbiguousExpander`:
   `Transformer::expand_packed` now detects an ambiguous transparent child and
   distributes each of its derivations as a separate alternative child-list of the
   parent (so the ambiguity is shifted up the tree). See
   `src/parsers/earley.rs::{symbol_derivations, is_transparent_node, expand_packed}`.
   (Lark's `AmbiguousIntermediateExpander` needs no port: lark-rs already enumerates
   intermediate-node alternatives directly via `expand_intermediate`.)
2. **`%ignore`-of-content edge cases** — e.g. `%ignore "1"` with overlapping
   string alternatives (`foo12`, `a12b`): dynamic `parse:16–19, 46–47` (`parse:87`
   was incidentally fixed by cluster 1).
3. **`dynamic_complete` resolve tie-break ordering** — segmentation order differs
   from Lark's: dynamic `parse:49, 72`.

`priority="invert"` is *filtered*, not XFAIL'd — an orthogonal, unimplemented
disambiguation option, out of scope here.

**Files:** `src/parsers/earley.rs` (forest→tree walk for cluster 1; `scan_dynamic`
`%ignore` carry-over for cluster 2; `sorted_families` ordering for cluster 3).

#### P3-1 — `ESCAPED_STRING` lookbehind-adaptation hardening ✅ DONE

**Resolution (2026-06-04 sprint):** the four `ESCAPED_STRING` oracle cases grew to
twelve, adding eight adversarial inputs that exercise the backslash-counting and
newline edges the lookbehind-free rewrite has to get right:
`"a\\"` (ends in an escaped backslash then the real quote → accept),
`"a\"` (trailing `\"` escapes the quote → unterminated, reject),
`"\\"` (body is one escaped backslash → accept),
`"\"` (`\"` escapes the only quote → reject),
`"a\\\"b"` (escaped backslash then escaped quote → accept),
`"a\nb"` (two-char `\n` escape → accept),
a *raw* newline in the body (reject), and a backslash directly before a raw newline
(reject). **lark-rs matches Python Lark on all twelve** (`test_common`), so the
documented lookbehind-free adaptation in `src/grammars/common.lark` is confirmed
correct — no grammar change was needed.

**Files (shipped):** `tools/generate_oracles.py` (`COMMON_TERMINAL_CASES`),
`tests/fixtures/oracles/common/cases.json` (regenerated).

#### P2-3 — De-recurse the forest→tree walk

**Done-when:** `EarleyParser::forest_to_tree` no longer needs a hand-rolled
oversized thread stack; the SPPF→tree walk uses an explicit work stack (or bounded
recursion) that cannot overflow on deep left-recursive forests.

**Why open:** the walk recurses to O(input length) on left-recursive list
grammars, so today it runs on a dedicated `std::thread` with a 256 MB stack
(`src/parsers/earley.rs::forest_to_tree`). It works and is correct, but the fixed
stack size is a band-aid — a long enough input on a deep grammar can still exceed
it. Profiler/robustness-gated: defer until a real input or a profiler asks for it.

**Files:** `src/parsers/earley.rs`.

#### P2-4 — Earley super-linearity on unambiguous input

**Done-when:** Earley parses list-shaped *unambiguous* input in time that scales
roughly linearly with LALR (the Earley/LALR ratio printed by `cargo bench --bench
parse` stops growing with input size — today ≈15×→32×→196× across 0.4K→8.7K→92K
JSON).

**Why open:** surfaced by P2-1. The completer
(`earley.rs::predict_and_complete`, the `originators` filter ~line 522) rescans the
entire origin column for every completed item, which is O(n²) on right-/list-shaped
grammars because the Joop-Leo transitive optimization was deliberately omitted
(documented as "dead code in the reference" — true for *that* reference's shape, but
it is what keeps Earley linear on LR-regular input). Two candidate fixes, smaller
first:
1. **Completer reverse-index** — maintain, per column, a `HashMap<SymbolId,
   Vec<Item>>` of items keyed by the non-terminal they `expect()`, so the completer
   does a hash lookup of *just* the waiting items instead of filtering the whole
   column. Contained, no algorithmic theory; cuts the constant and helps many
   grammars, though not the asymptotic worst case.
2. **Joop-Leo transitive items** — the full optimization that makes Earley linear on
   LR-regular grammars. Larger, the principled fix.

Profiler-justified now (the bench shows it), but a real subproject — not a leaf
fix — so tracked separately rather than folded into P2-1.

**Files:** `src/parsers/earley.rs`.

### Compliance milestones (LALR bank)

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
| ~~M6b~~ | ~~terminal-algebra typing (14/15)~~ → **`g_regex_flags`** | 4 | — | ✅ done (fidelity sprint; mis-triaged) |
| ~~M8b~~ | ~~conflict-detection parity (73/74)~~ → **strict S/R** | 2 | — | ✅ done (fidelity sprint; strict-only) |
| **M7b** | strict regex-collision construct errors (57/58) | 2 | Hard | ⬜ deferred — needs an `interegular`-equivalent FSM-intersection engine |

The work took the bank from 75.6% to **99.6%** — 123 entries from thirteen
root-cause fixes. The remaining **2** (57/58) are a single strict-mode
regex-collision grammar, deferred with cause (an FSM-intersection-emptiness engine
is out of scope for one sprint and risks over-rejection). All EBNF / template /
placeholder / filtering / typing / priority / `g_regex_flags` / strict-conflict
work is done. **Recommended next:** Phase 2 (Earley/SPPF) — the exit criterion is
far exceeded and the shared `CompiledGrammar` / `TreeBuilder` (`filter_pos`)
contract is settled; M7b can proceed in parallel whenever the FSM engine is built.

### M7b — strict regex-collision detection (deferred, with a plan)

**Done-when:** under `strict=True`, lark-rs raises a `LexError`-equivalent when two
same-priority regex terminals can match a common string, matching Python Lark.

**Why deferred:** Python Lark delegates to `interegular`
(`lexer.py::_check_regex_collisions`): group terminals by priority, build an FSM per
regex, and report any pair whose intersection is non-empty (plus an example). lark-rs
has no FSM layer — the lexer compiles straight to the `regex` crate, which offers no
intersection/emptiness test. Building a faithful, non-over-rejecting collision
checker (regex → NFA/DFA → product-construction emptiness, over the exact terminal
feature set Lark allows) is a self-contained subproject, not a leaf fix. The
`strict`-mode plumbing it would hang off of is already in place (this sprint), so the
remaining work is purely the overlap engine. Candidate building block:
`regex-automata`'s DFA support for the product construction.

## Exit criterion — when Earley unfreezes

Phase 2 (Earley + SPPF) starts when **either**:

- the bank reaches **≥ 90% agreement** with the remaining XFAILs triaged and
  each annotated with a root cause, **or**
- the remaining XFAILs are demonstrably *not* LALR-fixable (they require Earley,
  ambiguity output, or a dynamic lexer) — at which point Earley *is* the way to
  climb them, and the bank should grow Earley-shaped cases alongside it.

Until then, every PR that touches the core should either flip XFAIL entries to
passing or hold the line — never regress the percentage.
