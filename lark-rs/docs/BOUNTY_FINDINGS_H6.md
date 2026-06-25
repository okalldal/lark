# lark-rs bug-bounty findings — round 6 (h6)

> **Internal maintainer document — not a public bug-bounty program.**
> "Bug-bounty" and "strike team" here are shorthand for an internal differential
> parity audit of `lark-rs/` against Python Lark (our oracle). They imply **no**
> reward eligibility, issue assignment, or acceptance of unsolicited PRs. External
> contributors: see [`/CONTRIBUTING.md`](/CONTRIBUTING.md) and [`/SECURITY.md`](/SECURITY.md) first.

## Target and method

- **Baseline SHA (frozen):** `b4ab6cd578b1bd334f7fddc79781202fc66bba4a`
- **Oracle:** Python Lark **1.3.1** (`pip install lark==1.3.1`), the project ground
  truth. Correctness findings compare against it directly; the one perf finding (H6-7)
  uses Python as a *relative* oracle (it builds the same grammar in linear time) plus a
  deterministic lark-rs scaling table.
- **Harness:** `tools/diffcheck.py` (`compare(grammar, text, **opts)`) + the `diffcheck`
  Rust binary — runs both engines on the same job and reports accept/reject + tree-shape
  divergence. The harness **strips `Tree.meta`** (it compares tree shape, not span), so
  H6-5 (a meta-span divergence) was found with a direct Rust meta probe.
- **Reproduction command:** `cargo test --test test_bounty_findings_h6 -- --ignored`
  (8 ignored XFAIL tests, all failing today). Per-finding `diffcheck` one-liners below.
- **Teams:** 10 strike teams (negative grammar conformance; regex width/ranking; Python
  `re` dialect/taxonomy; standalone & `include_lark!`; bindings; cross-backend
  consistency; tree-shaping algebra; transformer/position parity; wild/hostile grammars;
  deterministic perf bounds).

### Ineligible set (deduped against, not re-counted)

- All prior findings: **RC1–RC10, N1–N10, V1–V4, H1–H12, P1–P2, H4-1…H4-12,
  H5-1…H5-9.**
- Open known-issue root causes: **#275** (`\b`/`\B`, `\Z` anchors), **#281** (bindings
  unbounded recursion), **#286** (`%extend` imported terminal), **#288**
  (`raw_value_len` global-prefix flag), **#302** (Earley adjacent small bounded
  repeats), **#304** (standalone `\Z`/oversized repeat), **#332** (char-class POSIX +
  set-ops), **#337** (`Tree.meta.empty` positionless), **#338** (PyO3
  `g_regex_flags`/`ambiguity='auto'`), **#348** (nullable+recursive Earley),
  **#349** (DFA counted-repeat determinization), **#350** (EOF error token),
  **#360–#367** (the H5 burndown), **#372** (`%import` overlapping closures),
  **#377** (cross-site recurse helper `filter_out`), **#391** (oracle-honesty lexer
  divergences).
- Documented intentional divergences: `_ambig` dedup (#159, ADR-0017), `\<`/`\>`
  normalization, the lookaround-scope `OutOfScope` refusals (`LOOKAROUND_SCOPE.md`),
  `ESCAPED_STRING`'s adaptation, the per-position token-filtering model (M6).

## Accounting

- **Fresh root causes: 8** (H6-1 … H6-8) — all executable A-level oracle XFAILs.
- **Variants: 3** (H6-8's letterless-name siblings `__`, `_9`, and the terminal-name
  analog) — folded into H6-8, not re-counted.
- **Known duplicates / re-confirmed-not-counted:** the regex-dialect bucket re-confirmed
  `\b`/`\B` (#275) and POSIX-class refusals (#332) as known; not counted.
- **Provisional / source-only: 3** — B1 (C-API `maybe_placeholders` default, evidence A
  but binding-config decision-flavored → `needs-decision`), B2 (bindings error-hierarchy
  collapse, evidence C), L1 (Earley token-filtering divergence, un-minimized, evidence
  C/D). Not encoded as XFAILs in the bounty bank.
- **Invalid / rejected reports:** none. The standalone / `include_lark!` bucket came back
  **clean** (a verified honest negative — the emit→recompile round trip agrees with both
  Python-standalone and in-process lark-rs); the lone strict-mode global-vs-per-state
  collision asymmetry there is by-contract (standalone == Python basic), not a bug.

## Severity summary

| ID    | Severity   | Fresh? | Evidence | Bucket                | One-line |
|-------|------------|--------|----------|-----------------------|----------|
| H6-1  | Medium     | yes    | A        | lexer ranking         | Value-length tiebreak measures the normalized pattern, not the raw source → wrong terminal wins |
| H6-2  | Medium     | yes    | A        | lexer dialect/taxonomy| `{,m}` quantifier rejected & mis-categorized as OutOfScope lookaround (Python accepts as `{0,m}`) |
| H6-3  | Medium-High| yes    | A        | lalr-table            | Aliased nullable alternatives → spurious LALR reduce/reduce (Python LALR + lark-rs Earley accept) |
| H6-4  | Medium     | yes    | A        | ebnf-loader           | Bare nested optional under repetition `[[A]]*` → spurious LALR reduce/reduce (twin empty arms) |
| H6-5  | Medium     | yes    | A        | core / tree-meta      | `Tree.meta` span excludes filtered tokens under `propagate_positions` |
| H6-6  | Medium-High| yes    | A        | loader / lexer        | String literal unified onto a same-source **regex** terminal → kept & mistyped instead of filtered |
| H6-7  | Medium     | yes    | A/B      | perf (grammar build)  | `(X|X)…(X|X)` duplicate-arm groups → `2^k` build cross-product; Python dedups, stays linear |
| H6-8  | Low        | yes    | A        | grammar-loader        | Rule/terminal names with no alphabetic char (`_`, `__`, `_9`) accepted; Python rejects |
| B1    | Medium     | prov.  | A*       | bindings (C API)      | `lark_default_options()` defaults `maybe_placeholders=false`; Python lib (+PyO3/WASM) default true |
| B2    | Medium     | prov.  | C        | bindings              | All three bindings collapse the error hierarchy & drop `.line`/`.column`/`.token` fields |
| L1    | Medium     | lead   | C/D      | earley filtering      | Earley drops a token Python keeps on a winning arm (un-minimized; grammar-global trigger) |

## Findings

### H6-1 — Terminal value-length tiebreak measures the normalized pattern, not Python's raw source

- **Severity:** Medium
- **Evidence:** A (oracle tree divergence; reproduced on basic + contextual lexers)
- **Freshness:** fresh root cause
- **Grammar:** `start: A | B` / `A: /\<\<\</` / `B: /<<<|q/`
- **Input:** `<<<`
- **Options:** `parser=lalr, lexer=basic` (also `lexer=contextual`)
- **Python result:** token **`A`** — `start(Token(A, "<<<"))`. Python's sort key is
  `(-priority, -max_width, -len(pattern.value), name)`; `A`'s `pattern.value` is the
  verbatim source `\<\<\<` (len 6), `B`'s is `<<<|q` (len 5), so `A` sorts first.
- **lark-rs result:** token **`B`** — `start(Token(B, "<<<"))`. `PatternRe::new` runs
  `normalize_python_escapes`, rewriting `\<\<\<` → `<<<` (len 6 → 3) before storage and
  discarding the source; `Pattern::raw_value_len` then measures 3 < 5, so `B` wins.
- **Root cause:** `PatternRe` (`src/grammar/terminal.rs`) stores only the normalized
  pattern; `raw_value_len` counts it. `sort_terminals` (`src/lexer/plan.rs`) consumes
  that as the 3rd sort key, where Python uses `len(pattern.value)` over the original
  source (`lark/lexer.py`).
- **Expected fix contract:** support-and-match. Retain the pre-normalization source on
  `PatternRe` (flag-wrapper excluded as today) and measure it, so
  `raw_value_len() == len(pattern.value)`. Invariant: `max_width` stays on the normalized
  pattern; LALR bank stays 512/512; the N2/#268 flag-wrapper case still holds.
- **Nearest known / why distinct:** N2/#268 stripped the *flag-wrapper* leak; RC5/#268
  is `max_width` (the 2nd key). This is the 3rd key (`raw_value_len`) and a different
  lost-length source — body-escape/comment normalization, which the flag-wrapper strip
  cannot recover. A second trigger (`(?#…)` comment strip) reproduces identically.
- **Test:** `h6_1_value_length_tiebreak_uses_raw_source`
- **Affected surfaces:** combined-scanner path (basic + contextual lexers; baked
  standalone scanner reuses `ScannerPlan`).
- **Unaffected surfaces:** Earley dynamic lexer (per-terminal longest-match, no
  `sort_terminals`).
- **Repro:** `python3 tools/diffcheck.py` via `compare('start: A | B\nA: /\\<\\<\\</\nB: /<<<|q/', '<<<', parser='lalr', lexer='basic')`

### H6-2 — `{,m}` quantifier rejected and mis-categorized as OutOfScope lookaround

- **Severity:** Medium
- **Evidence:** A
- **Freshness:** fresh root cause
- **Grammar:** `start: A` / `A: /a{,3}b/`
- **Input:** `aaab`
- **Options:** any (grammar-build failure; reproduced `lalr`/`contextual`)
- **Python result:** Python `re` reads `{,3}` as `{0,3}` (`re.match(r'a{,3}b','aaab')`
  matches); Python Lark builds and parses `Tree(start, [Token('A','aaab')])`.
- **lark-rs result:** build error `GrammarError::LookaroundScope` / `OutOfScope`
  ("backtracking-only syntax … permanent non-goal"). Two faults: rejects a
  Python-accepted regex, and the refusal category is wrong (a plain dialect gap, not
  lookaround).
- **Root cause:** `src/grammar/terminal.rs` — `normalize_python_escapes` never rewrites
  `{,m}` → `{0,m}`. The `regex` crate requires a decimal lower bound, so `Regex::new`
  fails; `PatternRe::new` falls through to the lookaround analyzer, which can't parse it,
  routing to `DeclineReason::BacktrackingOnlySyntax` → `OutOfScope`. `base_quantifier_len`
  already *recognizes* `{,n}` as a well-formed quantifier — only the normalization is
  missing.
- **Expected fix contract:** support-and-match. Normalize `{,n}` → `{0,n}` in
  `normalize_python_escapes` (class-aware, escape-aware, only when `base_quantifier_len`
  valid). Keep the inverted-bound `a{3,2}` rejection as a negative control (both reject).
- **Nearest known / why distinct:** opposite polarity to H6–H9/#375 (which reject to
  *match* Python's rejection). `{n,}` (`a{2,}`) is handled fine; `\p`/`\P`/`\x{}`/`\z`
  (H4-2/#381) are regex-crate-only constructs Python rejects — `{,m}` is the inverse.
- **Test:** `h6_2_empty_lower_bound_quantifier_accepted`
- **Affected surfaces:** every engine path (grammar build).
- **Unaffected surfaces:** n/a (build-time).
- **Repro:** `compare('start: A\nA: /a{,3}b/', 'aaab')`

### H6-3 — Aliased nullable alternatives produce a spurious LALR reduce/reduce rejection

- **Severity:** Medium-High
- **Evidence:** A (LALR build reject; Earley accept; control with alias removed)
- **Freshness:** fresh root cause
- **Grammar:** `p: "a"? -> al1 | "b"? -> al2` / `start: p`
- **Input:** `""` (build-time divergence; any/no input)
- **Options:** `parser=lalr, lexer={basic,contextual}`
- **Python result:** builds clean; `''→al1`, `'a'→al1`, `'b'→al2` (first-arm-wins). Both
  LALR lexers and Earley accept.
- **lark-rs result:** LALR build error — `Reduce/Reduce collision in state 0 for terminal
  $END: - al1 -> / - al2 ->`. (lark-rs Earley *accepts* but resolves to `al2`, where
  Python resolves to `al1` — a secondary divergence with the same root.)
- **Root cause:** an aliased alternative is kept as a distinct `Rule` with a distinct
  `tree_name` (`src/grammar/loader/ebnf.rs`, `src/grammar/intern.rs`); the R/R detector
  (`src/parsers/lalr.rs`) treats the two `p -> ε` reductions on `$END` as an
  unresolvable collision. Without aliases the arms dedup (`p: "a"? | "b"?` builds).
  Python keeps the alias as tree-naming metadata outside the R/R comparison and resolves
  same-rule ties by order.
- **Expected fix contract:** support-and-match. In R/R resolution, reduce (not error)
  candidates sharing `origin`+`expansion` that differ only by alias/`tree_name`, picking
  the lowest `rule.order` (Python first-arm-wins) — which also fixes the Earley
  `al1`-vs-`al2` resolution.
- **Nearest known / why distinct:** opposite direction to RC7/#272 (recurse-helper
  over-share → *under*-reporting). Different mechanism: alias-induced rule splitting of
  nullable alternatives, not recurse-helper keying. **Adjacent to H6-4** (both are
  spurious LALR R/R on nullable arms) but a distinct trigger and fix site (lalr.rs R/R
  resolution vs ebnf.rs helper-arm dedup).
- **Test:** `h6_3_aliased_nullable_alternatives_build`
- **Affected surfaces:** LALR (both lexers); Earley resolution.
- **Unaffected surfaces:** non-nullable aliased arms; CYK (rejects nullable per ADR-0024).
- **Repro:** `compare('p: "a"? -> al1 | "b"? -> al2\nstart: p', '', parser='lalr', lexer='contextual')`

### H6-4 — Nested bare optional under repetition `[[A]]*` spuriously rejected (twin empty arms)

- **Severity:** Medium
- **Evidence:** A (LALR reject; non-nested controls build; Earley accepts)
- **Freshness:** fresh root cause
- **Grammar:** `start: [[A]]* C` / `A: "a"` / `C: "c"`
- **Input:** `c` (also `ac`, `aac` — build-time reject)
- **Options:** `maybe_placeholders`-independent; `lalr/contextual`
- **Python result:** builds; `c → start[Token(C,'c')]`.
- **lark-rs result:** build error — `Reduce/Reduce collision in state 0 for terminal A:
  - __anon_group_0 -> / - __anon_group_0 ->` (the same empty production listed twice — a
  self-collision).
- **Root cause:** `src/grammar/loader/ebnf.rs::inner_alternatives` only fans out
  `Expr::Group` inner arms; a bare nested `Expr::Maybe` (`[[A]]`) falls to `compile_expr`
  and mints one `__anon_group_0` helper whose rule carries two byte-identical empty
  productions (inner-absent, outer-absent), never collapsed the way a lone `([A])?` is.
  Python's `EBNF_to_BNF`/`SimplifyRule_Visitor` collapses them.
- **Expected fix contract:** support-and-match. Collapse the helper's duplicate empty
  arms (or distribute the nested maybe's arms) so a single ε base arm is emitted; must
  not regress the RC7/#272 audit (which intentionally keeps genuine cross-helper
  collisions).
- **Nearest known / why distinct:** opposite direction to RC7/#272 and #176/#210
  (recurse-arm dedup) — an over-rejection lark-rs *invents*, on a helper's own twin empty
  arms. Controls `[A]* C`, `([A])* C`, `[[A] B]* C` all build; Earley accepts `[[A]]*` —
  isolating the bare-double-bracket-under-repetition path. **Adjacent to H6-3** (see
  there); distinct fix site.
- **Test:** `h6_4_nested_optional_under_repetition_builds`
- **Affected surfaces:** LALR.
- **Unaffected surfaces:** Earley (accepts, correct tree).
- **Repro:** `compare('start: [[A]]* C\nA: "a"\nC: "c"', 'c', parser='lalr', lexer='contextual')`

### H6-5 — `Tree.meta` span excludes filtered tokens under `propagate_positions`

- **Severity:** Medium
- **Evidence:** A (direct Rust meta probe; engine-agnostic — LALR + Earley)
- **Freshness:** fresh root cause
- **Grammar:** `start: "(" A ")"` / `A: /caf./` / `%import common.WS` / `%ignore WS`
- **Input:** `( cafX )` (the ASCII variant; multibyte `( café )` diverges identically)
- **Options:** `propagate_positions=True`, default filtering
- **Python result:** `start` meta `start_pos=0, end_pos=8` — spans the full `( cafX )`
  including the filtered parens (`PropagatePositions` derives meta from the *unfiltered*
  children via `_pp_get_meta`).
- **lark-rs result:** `start` meta `start_pos=2, end_pos=6` — spans only the kept `A`
  token. (`A`'s own token positions are correct in both.)
- **Root cause:** `Meta::from_children` (`src/tree.rs`), called from `Tree::new` on the
  **already-filtered** children that `apply_rule_options` (`src/parsers/tree_builder.rs`)
  produced. The filtered tokens' positions are gone by then.
- **Expected fix contract:** support-and-match. Compute meta from the production's
  pre-filter child span (a filtered token contributes its own start/end; a transparent
  inlined rule contributes its container span), byte-identical to Python's
  `PropagatePositions`. Extend the oracle harness to capture `Tree.meta` (currently
  stripped) as the regression net.
- **Nearest known / why distinct:** distinct from N8 (byte-vs-char `*_pos`, fixed — char
  indexing is correct on both sides here) and H10/#337 (positionless-empty `meta.empty`
  flag — here children are present and positioned). The divergence is *which children*
  feed the meta, not how positions index. The diffcheck harness strips `Tree.meta`, so
  this surface had never been exercised.
- **Test:** `h6_5_meta_span_includes_filtered_tokens`
- **Affected surfaces:** any `propagate_positions=True` consumer on a rule wrapped/
  terminated by filtered literals (`"(" expr ")"`, `stmt ";"`, bracketed lists), all
  engines.
- **Unaffected surfaces:** token positions; `%ignore`d content (correctly excluded from
  the span by both).
- **Repro:** direct Rust probe (the diffcheck harness strips meta) — build with
  `propagate_positions=true`, parse `( cafX )`, read `tree.meta.{start_pos,end_pos}`.

### H6-6 — String literal unified onto a same-source regex terminal (kept & mistyped, not filtered)

- **Severity:** Medium-High
- **Evidence:** A (tree divergence; `keep_all_tokens` proves the distinct `__ANON`; control)
- **Freshness:** fresh root cause
- **Grammar:** `start: AB | "ab"` / `AB: /ab/` (also `assign: NAME EQ NAME | NAME "=" NAME`
  with `EQ: /=/`)
- **Input:** `ab`
- **Options:** `lalr/contextual` (also `lalr/basic`, `earley/basic`; `earley/dynamic`
  agrees)
- **Python result:** the literal `"ab"` is a distinct anonymous `PatternStr`
  (`__ANON_0`), filtered → `start` has **no** children. With `keep_all_tokens=True`,
  token type `__ANON_0`.
- **lark-rs result:** the literal is unified onto `AB` and *not* filtered → `start` has
  one child `Token(AB, "ab")`. With `keep_all_tokens=True`, type `AB` — proving the
  collapse.
- **Root cause:** `patterns_equivalent` (`src/grammar/loader/terminals.rs`) compares
  `a.as_regex_str() == b.as_regex_str() && flags match`, collapsing `PatternStr("ab")`
  and `PatternRe(/ab/)` (both project to `ab`). Python's `Pattern.__eq__` requires
  `type(self)==type(other)`, and `term_reverse` is consulted only for `PatternStr`, so a
  literal never unifies with a regex terminal.
- **Expected fix contract:** support-and-match. Gate `patterns_equivalent` on matching
  `Pattern` kind (never `Str` ≡ `Re`). After the fix `start: AB | "ab"` on `ab` yields a
  childless `start`; regenerate oracles; the wild + compliance banks must stay green.
- **Nearest known / why distinct:** distinct from H4-9/#347 (Str-vs-Str *alternation-arm*
  dedup via `sym_key` in compiler.rs — the exact case `A: "a"` is now clean). This is the
  Re-vs-Str *interning* merge in terminals.rs, upstream of alternation dedup. Control:
  `R: /[ab]/` beside `"a"` does **not** diverge (regex source `[ab]` ≠ literal `a`),
  confirming the trigger is byte-identical regex-source-vs-literal.
- **Test:** `h6_6_string_literal_not_unified_with_regex_terminal`
- **Affected surfaces:** default LALR/contextual (and basic, earley/basic) — the common
  idiom of a named operator terminal (`EQ: /=/`, `COLON: /:/`) plus the bare literal
  inline.
- **Unaffected surfaces:** Earley dynamic lexer; regex terminals whose source isn't a
  literal value.
- **Repro:** `compare('start: AB | "ab"\nAB: /ab/', 'ab')` (and `… keep_all_tokens=True`)

### H6-7 — `O(2^k)` grammar-build blowup on duplicate-arm inline-group cross-products

- **Severity:** Medium
- **Evidence:** A (deterministic lark-rs scaling table) / B (Python relative oracle —
  linear)
- **Freshness:** fresh root cause
- **Grammar (n-parameterized):** `start: (X|X) (X|X) … (X|X)` (k duplicate-arm groups) /
  `X: "x"`. Generalizes to m arms/group (`(X|X|X)^k` = `3^k`).
- **Input:** `"x".repeat(k)` (parses fine; the blowup is at **build**, input-independent).
- **Options:** any parser/lexer (`load_grammar` alone blows up).
- **lark-rs scaling (measured, release):** k=12 → 12 ms, k=14 → 65 ms, k=16 → 325 ms,
  k=18 → 1569 ms (~2× per +1 k; **final surface rules = 1**). The deterministic invariant
  violated: the intermediate `acc` reaches `m^k` while the final grammar is O(1) rules.
- **Python relative behavior:** builds the identical grammar in flat linear time
  (`(X|X)^24` ≈ 7.7 ms, `rules=1`). `SimplifyRule_Visitor` dedups each group's arms
  *before* the cartesian product. Distinct arms `(X|Y)^k` are genuinely `2^k` in **both**
  engines (legitimate — the control).
- **Root cause:** `src/grammar/loader/ebnf.rs::compile_expansion`'s per-position loop
  folds each group into `acc` with the **non-deduping** `concat_alts`; the only dedup is
  a single `acc.retain(seen.insert(…))` *after* the full product is materialized.
- **Expected fix contract:** add a sub-exponential build-scaling gate + fix the cause by
  using `concat_alts_dedup` (already in the file) at the per-position fold. First-occurrence
  dedup produces the byte-identical final alternative set; all banks stay green. This is
  exactly the technique #252 applied to the `~n` repeat path (`repeat_union`), never
  wired into the general loop.
- **Nearest known / why distinct:** distinct from N9 (`~n..m` O(n²) *size*) and #252/
  `test_large_repeat_optional_rejects_without_blowup` (the `~n` repeat path, where Python
  *also* blows up). This is the literal inline-group cross-product path, where Python does
  **not** blow up.
- **Test:** `h6_7_duplicate_group_cross_product_build_blowup` (a 3 s worker-thread
  timeout at k=20; ~6 s today, instant once fixed — fails fast, never hangs).
- **Affected surfaces:** grammar load (all engines).
- **Unaffected surfaces:** runtime parsing.
- **Repro:** Python `lark.Lark("start: "+" ".join(["(X|X)"]*20)+"\nX: \"x\"\n")` is
  instant; lark-rs `load_grammar` of the same is ~6 s.

### H6-8 — Rule/terminal names with no alphabetic char accepted

- **Severity:** Low
- **Evidence:** A
- **Freshness:** fresh root cause (with 3 folded variants)
- **Grammar:** `_: "a"` / `start: _` (variants: `__: "a"`, `_9: "a"`)
- **Input:** `a`
- **Options:** any (load-time divergence)
- **Python result:** REJECT at grammar-lex — `RULE = _?[a-z][_a-z0-9]*` /
  `TERMINAL = _?[A-Z][_A-Z0-9]*` require at least one alphabetic char (and ≤1 leading
  underscore).
- **lark-rs result:** ACCEPT — parses `a` to `Tree(start, [])`.
- **Root cause:** `lex_rule`/`lex_terminal` (`src/grammar/loader/tokenizer.rs`) consume
  any run of name characters with no name-shape validation.
- **Expected fix contract:** reject-like-Python (ADR-0017: being more permissive than the
  oracle is unfalsifiable). Validate the captured name against the Python shape.
- **Nearest known / why distinct:** distinct from H5-2/#361 (`__foo` — a name that *has*
  a letter but a disallowed `__` prefix). This is the no-letter-at-all class — a
  different validation predicate ("must contain `[a-z]`/`[A-Z]`").
- **Test:** `h6_8_letterless_names_rejected`
- **Affected surfaces:** grammar load.
- **Unaffected surfaces:** valid names (`_x`, `_X` accepted by both).
- **Repro:** `compare('_: "a"\nstart: _', 'a')`

## Variants (reported, not re-counted)

- **V-H6-8a:** `__: "a"` (all underscores, rule name). Same root cause as H6-8.
- **V-H6-8b:** `_9: "a"` (underscore + digit, no letter). Same root cause.
- **V-H6-8c:** terminal-name analog (`__FOO`-style letterless terminal names) — same
  missing tokenizer name-shape validation; the rule-name forms are the cleanest repros.

## Provisional / source-only findings (not encoded as executable XFAIL)

### B1 — C-API `lark_default_options()` defaults `maybe_placeholders` to false (needs-decision)

- **Severity:** Medium · **Evidence:** A* (built `lark_h`, ran a C differential) but
  **decision-flavored**, so provisional.
- **Detail:** `lark_h::lark_default_options()` (`lark_h/src/lib.rs`) forwards the core
  `LarkOptions::default()` (`maybe_placeholders: false`, deliberately false in core). The
  **PyO3** binding (`python/src/lib.rs`) and the **WASM** binding (`wasm/src/lib.rs`)
  both *override* this to `true` to match Python Lark's library default
  (`LarkOptions._defaults['maybe_placeholders'] == True`). So on `start: [A] B` / input
  `b`, Python (and PyO3/WASM) yield `[None, Token(B,'b')]` (2 children) while the C API's
  documented default yields `[B]` (1 child) — silently dropping the `None` placeholder
  and breaking tree-index parity.
- **Fork (why needs-decision):** Python's *standalone tool* default is also `False`, and
  the core default is deliberately false — so "the C API should match Python's *library*
  default" vs "the C API faithfully forwards the core/standalone default and documents
  it" is a genuine contract decision. Recommend aligning the C default to `true` for
  cross-binding consistency (PyO3/WASM already do), or documenting the deviation in
  `lark.h` + an ADR. Done-when: an executable `lark_h` test pinning the chosen default.
- **Nearest known:** distinct from #244 (bindings OutputMode taxonomy — the *shape* of a
  None node) and P1/P2/#338 (`g_regex_flags`/`ambiguity='auto'`).

### B2 — Bindings collapse the error hierarchy and drop structured fields

- **Severity:** Medium · **Evidence:** C (source-traced; Python contract verified live)
- **Detail:** all three bindings map every parse/lex failure to a single message-only
  error (`python/src/lib.rs::map_parse_error`, `wasm/src/lib.rs`, `lark_h`), losing
  Python's `UnexpectedToken`/`UnexpectedCharacters`/`UnexpectedEOF` subclasses and the
  `.line`/`.column`/`.pos_in_stream`/`.token`/`.expected` fields (which exist on the core
  `ParseError`). PyO3 additionally raises `ParseError` for what Python calls
  `UnexpectedCharacters` (a `LexError`, not a `ParseError`).
- **Fix contract:** add the subclasses + structured fields per binding (overlaps the
  #244 taxonomy decision → escalate). Done-when: an executable repro per binding.

### L1 — Earley token-filtering divergence (un-minimized lead)

- **Severity:** Medium · **Evidence:** C/D (reproduced, not minimized)
- **Detail:** on a multi-rule grammar (`_p: B "c" | B` / `!q: …` / `!r: …` / `start:
  "a"~1..2 q | A "a"* | "c"* "a"+`), input `cca` under `earley/basic` default filtering:
  Python → `start[A('a')]`, lark-rs → `start[]` (drops the `A` token Python keeps on the
  winning `"c"* "a"+` arm). With `keep_all_tokens=True` both agree, isolating it to
  token-**filtering** on the winning arm. Removing rule `r` makes it vanish — a
  grammar-global terminal-naming/filter interaction. **Done-when:** produce a minimal
  executable repro first (then promote to a counted finding).

## Clean buckets (honest negatives)

- **Standalone / `include_lark!`** (Team 4): no fresh divergence. The emit→recompile round
  trip (the path the in-crate bank doesn't cover) agrees with both Python-standalone and
  in-process lark-rs across tree-shaping (aliases, expand1, transparent inlining,
  `keep_all_tokens`, ε rules), `maybe_placeholders` distribution, the lexer recipe
  (`unless` retype incl. `"kw"i`, per-terminal flags, `%ignore`, ordering, EBNF/templates,
  multi-start, `%import`), escape round-tripping, and `regex`-vs-`regex-automata` span
  agreement. The strict-mode global-vs-per-state collision asymmetry is by-contract
  (standalone == Python basic, can only over-reject, never under-reject).
- **Regex dialect** (Team 3): `\d`/`\w`/`\s` (+ negations) Unicode membership, `(?i)`
  special folds (Kelvin, long-s), `{n,}`, inverted bound `{3,2}` — all parity. `\b`/`\B`
  (#275) and POSIX classes (#332) re-confirmed as known refusals, not counted.
- **Tree-shaping algebra** (Team 7): ~1000 random grammars × {LALR,Earley,CYK} × option
  combos plus hand-crafted `?`/`_`/`!`/alias/`[...]`/template/nested-EBNF compositions —
  zero divergences beyond H6-4 and the documented #159 `_ambig` dedup. Placeholder
  FindRuleSize, `[x]` vs `x?`, anon flattening, CYK CNF round-trip — clean.
- **Positions** (Team 8): token `start_pos`/`end_pos`/`line`/`column`/`end_*` with
  multibyte (é, emoji, CJK) across all engines/lexers — char-based, all match (N8 is
  genuinely fixed). `\r\n`/`\r` columns, `%ignore`d-content exclusion, `ESCAPED_STRING`
  value decoding — clean. Only the H6-5 meta span diverges.
- **Cross-backend** (Team 6): `%ignore`, `keep_all_tokens`, `maybe_placeholders`,
  empty/EOF, contextual-vs-basic acceptance, S/R + R/R conflict reporting, terminal
  priority/longest-match — byte-identical PY↔RS across backends (beyond H6-3). Config
  legality matches Python except the documented N6 `lalr + ambiguity=resolve`.
- **Negative grammar conformance** (Team 1): negative/zero/huge/float priorities,
  `%declare`/`%import`/`%ignore`/`%override`/`%extend` bad-arg handling, template
  arity/recursion, alias placement, rule modifiers, unicode/self-referential/empty rules,
  keyword-named terminals — all parity (beyond H6-8).
- **Wild/hostile grammars** (Team 9): TOML/INI/CSS/JSON5 fragments, operator-precedence
  cascades, multi-param/nested templates, `_separated`/trailing-separator lists,
  `%override`/`%extend`, large/case-insensitive keyword sets, contextual-lexer overlaps,
  multi-start, deep recursion — all parity (beyond H6-6). The wild-bank `OutOfScope`
  lookaround refusals are intentional.
- **Perf** (Team 10): the Earley/CYK/lexer/DFA-build scaling gates are well-netted; no
  fresh violation beyond the H6-7 grammar-build path.

## Harness caveats

- **`Tree.meta` is stripped by `diffcheck`** (it compares tree shape, not span), so H6-5
  required a direct Rust meta probe and is not reproducible via the dc.py one-liner.
- **CYK/Earley resolve-mode tie-breaks are `PYTHONHASHSEED`-dependent** in Python; fuzzing
  those backends needs a pinned hash seed, and a divergence that only appears under a
  random seed is a harness artifact, not a finding (CLAUDE.md tie-break discipline).
- **The H6-7 XFAIL runs the build on a worker thread with a 3 s timeout** so it fails fast
  (the worker keeps running ~6 s after the timeout and exits silently); do not remove the
  timeout, or `--ignored` would hang on the unfixed path.
- **B1/B2 require building the bindings** (`lark_h`/PyO3/WASM); B1 was run as a live C
  differential, B2 is source-traced.
