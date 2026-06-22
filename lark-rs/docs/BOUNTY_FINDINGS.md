# lark-rs Bug-Bounty Findings

A differential strike-team sweep of lark-rs against **Python Lark 1.3.1** (the
oracle). Ten teams probed disjoint root-cause buckets; after minimization,
independent re-verification, and dedup-by-root-cause this catalog records:

- **11 fresh, confirmed root causes** — RC1, RC2 (two surfaces, RC2/RC2b), RC4a,
  RC4b, RC4c, RC5, RC6, RC7, RC8, RC9, RC10. Each has an executable XFAIL test in
  `tests/test_bounty_findings.rs`.
- **1 known-issue guard** — RC3 is the `maybe_placeholders=false` colliding-optional
  parity gap of #252, already fixed by the merged PR #259; it reproduces here only
  because that fix has not reached `master`. Kept as a guard, **not** counted fresh.

Total: 13 tests (11 fresh causes + RC2b variant surface + RC3 known guard).

## Target & method

- **Target SHA (frozen baseline):** `a005423`
  (branch `claude/hackathon-baseline-bounty-08oolp`).
- **Oracle:** Python Lark `1.3.1` (the in-repo `lark/`).
- **Harness:** `tools/diffcheck.py` (`compare(grammar, input, **opts)`) drives the
  `diffcheck` binary (`src/bin/diffcheck.rs`) and Python Lark over the same
  (grammar, input, options) tuple, diffing accept/reject + tree shape (`_ambig`
  children compared unordered). It exposes the **most commonly-tested** public
  options — `parser`, `lexer`, `start`, `ambiguity`, `maybe_placeholders`,
  `keep_all_tokens`, `strict` — but **not** the whole `LarkOptions` surface:
  `g_regex_flags`, `base_path`, `import_sources`, `postlex`, and `lexer_backend`
  are not yet wired (a worthwhile extension; some finds below were source-traced
  precisely because they sit outside this matrix). Every find was re-run through
  `compare()` after minimization (RC10 via `generate_standalone()` directly).
- **Reproductions:** `tests/test_bounty_findings.rs` — one `#[ignore]` (XFAIL)
  test per find, asserting the Python-oracle behavior. They are green-by-ignore in
  CI; run `cargo test --test test_bounty_findings -- --ignored` to watch all 12 go
  red. Drop a test's `#[ignore]` when its bug is fixed to turn it into a
  regression guard.
- **Eligibility:** none overlap the ineligible baseline set (#176 seed-13, #210
  seed-99, #258, #250, #228/#229, #253, or the documented equal-span lexer
  tie-break). Where a find is *adjacent* to a known issue, the distinction is
  noted inline.

A recurring theme: lark-rs **silently accepts grammars Python rejects at build**
(missing validation/conflict-detection gates). Per ADR-0017's corollary, being
more permissive than the oracle is unfalsifiable → a bug.

## Severity summary

| ID  | Sev      | Fresh? | Bucket          | One-line |
|-----|----------|--------|-----------------|----------|
| RC5 | Critical | fresh  | lexer           | Regex `max_width` inference returns `None`, so finite regexes sort as unbounded → wrong terminal chosen |
| RC1 | High     | fresh  | grammar-loader  | Duplicate rule definition silently merged, not rejected |
| RC2 | High     | fresh  | grammar-loader  | Duplicate terminal definition (import + `%declare`/local) not rejected |
| RC4a| High     | fresh  | grammar-loader  | Alias on an inlined `_rule` not rejected |
| RC4b| High     | fresh  | grammar-loader  | `?` modifier on an inlined `_rule` not rejected |
| RC4c| High     | fresh  | grammar-loader  | Alias inside a parenthesized group not rejected |
| RC7 | High     | fresh  | lalr-table      | Undetected LALR reduce/reduce collision |
| RC8 | High     | fresh  | earley          | Zero-width regexp under dynamic lexer not rejected |
| RC9 | High     | fresh  | tree-shaping    | `expand1` keeps wrapper around a lone placeholder-`None` |
| RC6 | Medium   | fresh  | lexer           | `\b`/`\B` leaks an uncategorized `regex-automata` build error |
| RC10| Medium   | fresh  | distribution    | Standalone/`include_lark!` bakes lookaround instead of rejecting it → runtime panic |
| RC3 | —        | KNOWN  | grammar-loader  | Colliding optional expansion `[A] [A]` (mp=false) — #252, fixed by merged PR #259 |

---

## Findings

### RC1 — Duplicate rule definition silently merged (High, grammar-loader)
- **Grammar:** `start: a` / `a: "x"` / `a: "y"` · **Input:** `"y"`
- **Options:** default (reproduces on all of lalr/contextual, lalr/basic,
  earley/dynamic, earley/basic, cyk/basic).
- **Python:** build error — `Rule 'a' defined more than once`.
- **lark-rs:** builds; merges the two bodies into alternatives and accepts both.
- **Root cause:** `GrammarCompiler` merges rule bodies only via `|` and never
  enforces single-definition-per-origin.
- *Distinct from #258 (a `maybe_placeholders=true` optional-desugaring artifact).*

### RC2 — Duplicate terminal definition not rejected (High, grammar-loader)
- **Grammar (canonical):** `%import common.INT` / `%declare INT` / `start: INT`
  · **Input:** `"5"`. **Variant (RC2b):** `%import common.INT` / `INT: "x"` /
  `start: INT` on `"5"`.
- **Python:** build error — `Terminal 'INT' defined more than once`.
- **lark-rs:** keeps one definition silently and builds. Order-independent; same
  gap across the `%import`/`%declare`/local-redefinition surfaces.

### RC3 — Colliding optional expansion not rejected (KNOWN — #252 / PR #259)
- **Grammar:** `start: [A] [A] "c"` / `A: "a"` · **Input:** `"c"`
- **Options:** `maybe_placeholders=false` (default).
- **Python:** build error — `Rules defined twice ... (colliding expansion of
  optionals)`. **lark-rs:** builds and accepts.
- **Status — NOT a fresh find.** This is the `maybe_placeholders=false`
  colliding-optional parity gap tracked by **#252** and **already fixed by the
  merged PR #259**, which oracle-checks `[A] [A]` by name (test
  `test_literal_optional_pair_collides`). It still reproduces on the frozen target
  SHA only because #259 landed on the sprint integration branch, not `master`; it
  will pass once #255 lands. Retained as a guard, excluded from the fresh count.
  (Earlier drafts mis-cited only #258 as the adjacent issue — the real prior art is
  #252/#259.)

### RC4 — Alias / `?`-modifier placement not validated (High, grammar-loader)
Three sibling gaps, all build-time validation Python performs and lark-rs skips:
- **RC4a** `start: _w` / `_w: A -> aliased` — Python: *"Rule `_w` is marked for
  expansion … isn't allowed to have aliases"*; lark-rs emits an `aliased` node.
- **RC4b** `?_w: A` / `start: _w` — Python: *"Inlined rules (`_rule`) cannot use
  the `?rule` modifier."*; lark-rs accepts.
- **RC4c** `start: (A -> foo) B` — aliases are legal only at the top level of an
  alternative; inside a group Python reads `foo` as a rule reference → *"Rule
  `'foo'` used but not defined"*; lark-rs builds a `foo` node. (Reproduces for
  `(A -> foo)?`, `(A -> foo)+`, `(A -> foo | B -> bar)`.)

### RC5 — Regex `max_width` inference returns `None` (Critical, lexer)
- **Grammar:** `start: A | B` / `A: /a+/` / `B: /aa?/` · **Input:** `"aaa"`
- **Options:** reproduces under both `basic` and `contextual` lexers.
- **Python:** `A = "aaa"` (the maximal match).
- **lark-rs:** tries `B` first, commits to its greedy `"aa"`, leftover `"a"`
  rejects the parse.
- **Root cause (corrected):** both engines sort terminals
  `(-priority, -max_width, -len(pattern), name)` — Python at `lark/lexer.py:583`,
  **lark-rs at `src/lexer/plan.rs:312`** (the sort key already includes
  `max_width`; the `CLAUDE.md` note saying otherwise is stale). The real bug is in
  *width inference*: `Pattern::max_width()` returns `None` for **every** regex
  (`src/grammar/terminal.rs:23` — `Pattern::Re(_) => None`) and `plan.rs` maps
  `None → usize::MAX`. So the *finite* `/aa?/` (true width 2) is treated as
  unbounded, ties with the genuinely-unbounded `/a+/`, and `-len(pattern)` breaks
  the tie the wrong way (longer source `aa?` first). Python computes the finite
  width and keeps `/a+/` (∞) ahead of `/aa?/` (2). **Fix point: compute finite
  max-width for bounded regexes — not the sort key.** Adding an explicit priority
  (`A.2`) makes lark-rs agree, isolating the diagnosis.
- **Same root cause, other public surfaces:**
  - `%ignore` steals a content char: `start: A+` / `A: /a+/` / `WS: /a? /` /
    `%ignore WS` on `"a a"` — Python emits `A A`; lark-rs emits one `A` (tree-shape
    divergence). The `start: A B` form on `"a b"` is an accept/reject divergence.
  - Longest-vs-higher-rank, no `%ignore`: `start: (A | C)+` / `A: /a+/` /
    `C: /a? /` on `"a a"` — Python `A C A`; lark-rs `C A`.
- *Not the documented equal-span tie-break: the competing spans differ in length.*

### RC6 — `\b`/`\B` leaks an uncategorized backend error (Medium, lexer)
- **Grammar:** `start: A` / `A: /x\b/` · **Input:** `"x"`
- **Python:** tokenizes `A = "x"`.
- **lark-rs:** *build* error — raw `regex-automata` leak: *"cannot build DFAs for
  regexes with Unicode word boundaries"* — instead of either supporting `\b` or
  emitting the documented `GrammarError::LookaroundScope` refusal. Reproduces for
  `\b` prefix/suffix/bare and `\B`, both lexers. Distinct from the documented
  `\<`/`\>` normalization.

### RC7 — Undetected LALR reduce/reduce collision (High, lalr-table)
- **Grammar:** `start: r0* | (r0)*` / `r0: "a"` · **Input:** `"a"`
- **Python:** build error — `Reduce/Reduce collision … between <__start_star_0 :
  r0>` and `<__start_star_1 : r0>`.
- **lark-rs:** builds the table and parses, masking the ambiguity. LALR-only
  (Earley agrees → the conflict detector, not the loader). `r0+ | (r0)+` and
  arm-order variants diverge identically.

### RC8 — Zero-width regexp under dynamic lexer not rejected (High, earley) — FIXED (#276)
- **Grammar:** `start: A` / `A: /a*/` · **Input:** `"a"` · **Options:**
  `parser=earley`, `lexer=dynamic` (and `dynamic_complete`).
- **Python:** build error — *"Dynamic Earley doesn't allow zero-width regexps"*.
- **lark-rs:** ~~builds and parses under both dynamic lexers.~~ **Fixed:**
  `DynamicMatcher::new` now rejects any terminal whose regexp can derive the empty
  string, using the assertion-aware min-width oracle
  (`lookaround::pattern_min_width_is_zero`, = Python's `get_regexp_width(...)[0] ==
  0`) so it matches Python on lookaround (`/a*(?=b)/`) and bare-boundary (`/\b/`)
  zero-width terminals too, not just `/a*/`. The XFAIL
  `rc8_zero_width_regexp_dynamic_rejected` is green and joined by a differential
  audit (`rc8_zero_width_dynamic_differential_audit`).

### RC9 — `expand1` keeps wrapper around a lone placeholder-`None` (High, tree-shaping)
- **Grammar:** `start: w` / `?w: [A]` / `A: "a"` · **Input:** `""` · **Options:**
  `maybe_placeholders=true`.
- **Python:** `start[None]` — the single-child `?w` collapses.
- **lark-rs:** `start[w[None]]` — the `w` wrapper survives. With a real single
  child both collapse correctly, isolating the bug to the lone-`None` case.
  Backend-independent (LALR + Earley); also under `keep_all_tokens` and
  `?start: [A] B?`.

### RC10 — Standalone bakes lookaround → runtime panic (Medium, distribution)
- **Verification:** confirmed at the generation boundary. `generate_standalone()`
  returns `Ok` (baking raw lookaround) for both `A: /foo(?!bar)/` and
  `%import python.STRING`, where the contract is to reject; the executable XFAIL
  `rc10_standalone_rejects_lookaround` asserts the rejection and fails today. (The
  downstream runtime panic itself is source-traced, not executed — the generated
  parser is not compiled.)
- **Grammar:** `start: A` / `A: /foo(?!bar)/` · **Options:** `parser=lalr`,
  `lexer=basic` (standalone-eligible subset).
- `standalone::bake()` → `lexer::scanner_plan()` maps each terminal via
  `to_inline_regex()` **without** going through the lookaround refusal seam
  (`route_fancy_only_terminal`). A lowered-lookaround terminal (e.g. `/foo(?!bar)/`,
  or the bundled `python.STRING` / `lark.REGEXP` / `python.LONG_STRING`) is baked
  verbatim into `scan_groups`; the pure-`regex` standalone runtime then hits
  `Regex::new(...).expect("baked scanner regex is valid")` → **panic** at
  `Parser::new()`. This contradicts the documented contract (STATUS.md /
  `lark_proc/src/lib.rs`: lookaround grammars are "rejected at compile time with a
  clear error" / "not standalone-able") and defeats #49's compile-time guarantee
  for `include_lark!`. The in-process core handles these grammars correctly.

---

## Buckets that came back clean

- **CYK (Team 6):** no finds across ~60 probes — provenance-based ε-rejection
  (ADR-0024/#101/#144), nullable-helper carve-outs, bounded repeats, and CNF-revert
  tree reconstruction all match Python. *Footnote:* Python's CYK actually **hangs**
  on some pure-nullable plus/star helpers (`(B?)+`, `(B*)+`) where lark-rs cleanly
  rejects with "CYK doesn't support empty rules" — lark-rs is the better-behaved
  side, so this is not a scoreable divergence.
- **Interactive / recovery (Team 7):** no finds. The batch path shared by the
  interactive cursor and `on_error` driver (`ParserStack::feed_token`/`accepts`)
  matches Python across state-invalid tokens, premature EOF, conflict grammars,
  and ~28k fuzzed grammars.
- **Earley ambiguity/SPPF (Team 5):** the derivation engine is robust —
  visible-token recursion, dangling-else, Catalan grammars, and priority
  resolution all agree; byte-identical `_ambig` dedup is intentional (ADR-0017).
  Both Team-5 finds were missing *build-time validation gates* (RC8 + a
  duplicate-rule instance of RC1), not derivation errors.

## Harness caveats (for reproducers)
- Python Lark rejects `ambiguity=` on LALR/CYK; `compare()` only forwards it for
  Earley.
- `strict=True` requires the `interegular` package (absent here) — Python
  build-errors without it, so `strict` divergences from the harness are environment
  artifacts, not finds.
- When **both** engines reject an input the harness reports agreement even if the
  *stage* differs (build vs parse). A grammar lark-rs builds but Python rejects can
  hide behind a parse error on a poorly-chosen input — choose an input the lenient
  side *accepts* (e.g. RC2 on `"5"`, not `"x"`).
