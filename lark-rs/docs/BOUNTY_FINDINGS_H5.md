# lark-rs bug-bounty findings — round 5 (h5)

> **Internal maintainer document — not a public bug-bounty program.**
> "Bug-bounty" and "strike team" here are shorthand for an internal differential
> parity audit of `lark-rs/` against Python Lark (our oracle). They imply **no**
> reward eligibility, issue assignment, or acceptance of unsolicited PRs. External
> contributors: see [`/CONTRIBUTING.md`](/CONTRIBUTING.md) and [`/SECURITY.md`](/SECURITY.md) first.

## Target and method

- **Baseline SHA:** `325444f5c0a16a284b362289194b6f97402b3053`
- **Oracle:** Python Lark 1.3.1 (`pip install lark`), compared per-backend.
- **Harness:** `tools/diffcheck.py` `compare()` + the `target/debug/diffcheck` binary
  (runs lark-rs and Python on the same `(grammar, input, options)` tuple, reports
  accept/reject and tree-shape divergence). Findings re-run by hand at intake.
- **Teams:** 10 strike teams (retargeted off the heavily-mined buckets of rounds 1–4
  toward: cross-backend consistency, tree-shaping algebra, priority/ambiguity
  resolution, CYK semantics, residual regex-dialect corners, token-naming, new wild
  grammars, and deterministic resource bounds).
- **Ineligible set:** all prior root causes — RC1–RC10 (round 1), N1–N10 + V1–V4
  (round 2), H1–H12 + P1–P2 (round 3), H4-1…H4-12 + V-H9/V-H1 (round 4) — plus the open
  known-issue set (#69, #164, #165, #208, #209, #225, #230, #232–234, #242–244, #275,
  #281, #282, #286, #288, #289, #293, #298, #299, #302, #304, #313, #324–327, #329–353)
  and the documented intentional divergences (`_ambig` dedup #159/ADR-0017, `\<`/`\>`
  normalization, the lookaround `OutOfScope` taxonomy, `ESCAPED_STRING`'s adaptation).
- **Reproduction command:**
  ```bash
  cargo test --test test_bounty_findings_h5 -- --ignored   # 8 XFAILs, all fail today
  ```
  (H5-9 is a measured perf/representation finding with no committed gate — see below.)

## Accounting

- **Fresh root causes: 9** — 8 correctness divergences with committed XFAIL tests
  (H5-1…H5-8) + 1 deterministic perf/representation finding documented with a
  measurement table (H5-9, no committed gate yet).
- **Variants (folded into a parent, not separately counted):** the `"\r\n"`→`CRLF`
  surface of H5-8 (same `TERMINAL_NAMES` gap as `"\\"`→`BACKSLASH`); the `\W` surface of
  H5-4 (symmetric to `\w`); the lookbehind form of H5-1.
- **Known duplicates re-confirmed (NOT re-counted):** `\b`/`\B` (RC6/#275), `\Z` (N10),
  POSIX `[[:alpha:]]` / set-ops (H5/#332), `(?#comment)` (H8), octal `\101`/`[\b]` (H9),
  `\p{}`/`\x{}`/`\z` (H4-2), global inline `(?i)` (N3 — both reject), `(?P=name)` (N4),
  the H10/#337 `meta.empty` positionless-children bug.
- **Provisional / source-only:** H5-9 (perf, B-evidence by measurement but no committed
  counter); a CYK equal-weight ambiguity tie-break (policy-excluded per CLAUDE.md's
  tie-break discipline, and did not reliably reproduce); a `propagate_positions`
  end-position-over-ignored-gap hint (diffcheck strips `meta` — repro-first required).
- **Invalid / rejected reports (first-pass candidates the intake re-run falsified):**
  template-arity mismatch (both engines reject — clean); `%ignore <rule>` (both reject);
  standalone bake of `maybe_placeholders`/`?start:` (byte-identical on recompile —
  only the known H12 reproduced); CYK `keep_all_tokens` / `maybe_placeholders` /
  empty-input (all clean under the valid `lexer='basic'` pairing — the first pass misread
  a `cyk`+`contextual` config error as engine behavior); Earley `resolve` rule-priority
  selection incl. negative priority (matches Python over ~240 randomized cases);
  in-process `maybe_placeholders` multi-symbol group count and `keep_all_tokens` +
  `%ignore` retention (both clean). These are **useful negative evidence**: the
  tree-shaping core, the standalone bake, CYK tree-shaping, and Earley priority
  resolution are all faithful — the round-5 finds live in narrow loader/lexer corners.

## Severity summary

| ID | Severity | Fresh? | Evidence | Bucket | One-line |
|----|----------|--------|----------|--------|----------|
| H5-1 | Medium | fresh | A | lexer (ranking) | Lowerable-lookaround terminal gets `max_width=None`→unbounded, mis-ranked ahead of a wider finite terminal |
| H5-2 | Medium | fresh | A | grammar-loader | `__`-leading names (rule/terminal/alias/template-param) accepted; Python rejects at grammar-parse |
| H5-3 | Medium | fresh | A | ebnf-loader | `[A]` optional alternative beside an explicit empty `\|` arm → spurious LALR reduce/reduce; Python accepts |
| H5-4 | Medium | fresh | A | lexer (dialect) | `\w`/`\W` Unicode membership diverges (Rust UTS#18 vs Python `isalnum`): U+0301 over-accepted, U+00B2 under-accepted |
| H5-5 | Low–Med | fresh | A | lexer (dialect/taxonomy) | `\N{NAME}` rejected + mis-categorized as backtracking; Python accepts |
| H5-6 | Low | fresh | A | lexer (dialect) | Regex-crate angle named-group `(?<name>...)` accepted; Python `re` rejects at build |
| H5-7 | Low | fresh (needs-decision) | A | lexer (dialect) | Turkish dotless-i U+0131 not folded to ASCII `i`/`I` under `/i`; Python matches it |
| H5-8 | Low | fresh | A | grammar-loader | Anonymous `"\\"` / `"\r\n"` literals named `__ANON_n`, not `BACKSLASH`/`CRLF` |
| H5-9 | Medium | fresh | B (measured) | perf (lalr table) | In-process `ParseTable` is dense `O(states×terminals)` where Python is sparse `O(entries)` |

## Findings

### H5-1 — Lowerable-lookaround terminal sizes as unbounded, mis-ranking it

- **Severity:** Medium
- **Evidence:** A (oracle, both lexers; lookahead + lookbehind forms)
- **Freshness:** fresh (residual of RC5/#268, distinct code branch)
- **Grammar:** `start: t B` / `t: LA | REG` / `LA: /a(?=b)/` / `REG: /a|zz/` / `B: "b"`
- **Input:** `ab` (`keep_all_tokens=True` to surface the chosen terminal)
- **Options:** `parser=lalr`, `lexer ∈ {basic, contextual}`
- **Python result:** `t` child is `Token('REG','a')` — `LA`=`/a(?=b)/` sizes to max_width 1, `REG`=`/a|zz/` to 2, so the `-max_width` key picks `REG`.
- **lark-rs result:** `t` child is `Token('LA','a')`.
- **Root cause:** `Pattern::max_width()` (`src/grammar/terminal.rs`) is
  `regex_syntax::parse(...).ok().and_then(hir_max_width_chars)`; `regex_syntax` rejects
  any lookaround source, so `.ok()` is `None`, which `plan.rs` maps to `usize::MAX`. The
  terminal then sorts first. Python's `get_regexp_width` sizes lookaround finitely via
  `sre_parse` (assertions zero-width).
- **Expected fix contract:** support & match — size lowerable-lookaround terminals to
  their finite `sre_parse` width (assertions zero-width). The sort key itself is correct;
  the docstring at `terminal.rs` claiming Python also hits a `MAXWIDTH` fallback here is
  wrong (`sre_parse` sizes every standard lookaround).
- **Nearest known / why distinct:** RC5/#268 — that fix added the `hir_max_width_chars`
  walk for parseable patterns (`/a+/`/`/aa?/`). H5-1 is the **parse-failure fallback
  branch** #268 left untouched, reachable only by lookaround terminals the RC5 pin never
  builds.
- **Test:** `h5_1_lookaround_terminal_width_misrank`
- **Affected surfaces:** basic + contextual lexers; lookahead and lookbehind shapes.
- **Unaffected surfaces:** plain bounded regex (RC5 fix covers it); no bundled/wild/corpus
  grammar triggers it today (banks stay green).

### H5-2 — `__`-leading names accepted; Python rejects

- **Severity:** Medium
- **Evidence:** A
- **Freshness:** fresh
- **Grammar:** `start: __x` / `__x: "a"` (also `__X:` terminals, `-> __x` aliases,
  template params `t{__x}`, bare references)
- **Input:** `a`
- **Options:** `parser=lalr`, `lexer=contextual`
- **Python result:** build error — `GrammarError: Unexpected input ... start: __x` (the
  `RULE`/`TOKEN` name token is `/_?[a-z]…/`: at most one leading underscore + a letter).
- **lark-rs result:** accepts, parses to `Tree(start, [])`.
- **Root cause:** `lex_rule`/`lex_terminal` (`src/grammar/loader/tokenizer.rs`) take a
  permissive `[A-Za-z0-9_]*`, swallowing any number of leading underscores.
- **Expected fix contract:** reject-like-Python — mirror Lark's name-token shape (≤1
  leading `_`, then a letter; rules also allow one leading `?`/`!`).
- **Nearest known / why distinct:** the validation layer (H1–H3, H4-8/11, RC1–4) is
  mined, but the *lexical legality of the name token itself* is not. Boundary confirmed:
  `_x`/`_X` and trailing/mid `x__`/`a__b` are accepted by both — only *leading* `__` (or
  `_`-then-non-letter) diverges.
- **Test:** `h5_2_double_underscore_name_rejected`
- **Affected surfaces:** rule defs, terminal defs, references, alias targets, template
  parameters.
- **Unaffected surfaces:** single leading `_`, non-leading underscores.

### H5-3 — `[A]` optional beside an explicit empty `|` arm → spurious reduce/reduce

- **Severity:** Medium
- **Evidence:** A
- **Freshness:** fresh
- **Grammar:** `start: x` / `x: [A]` / `|` / `A: "a"`
- **Input:** `` (empty)
- **Options:** `parser=lalr`, `lexer=contextual` (independent of `maybe_placeholders`
  and `keep_all_tokens`)
- **Python result:** accepts → `start[ x[] ]` (with MP: `start[ x[None] ]`).
- **lark-rs result:** build error — `Reduce/Reduce collision in state 0 for terminal
  $END: x -> / x ->`.
- **Root cause:** the distributed `[A]` absent arm carries a positional gap marker
  (`gaps=[..]`) while the explicit `|` arm is bare (`gaps=[]`); `dedup_and_check_alts`
  (`src/grammar/loader/compiler.rs`) keys dedup on the full `(syms,gaps)`, so the two
  empty `x ->` arms both survive and collide. The within-expansion canonicalizer
  (`ebnf.rs`) that fixes `A?`/`(A)?` never sees the two top-level `|` alternatives
  together.
- **Expected fix contract:** support & match — in `dedup_and_check_alts`, collapse empty
  (`syms.is_empty()`) arms that differ only in gap markers to one surviving arm, reusing
  `ebnf.rs`'s MP-vs-non-MP None-count rule. LALR bank must stay 512/512.
- **Nearest known / why distinct:** adjacent to RC7/#272 (recurse-helper over-share) and
  #258/#289 (nested-optional collapse), but those act *within one expansion*; H5-3 is the
  **cross-`|`-alternative** empty-arm collision in `dedup_and_check_alts`. The controls
  `A? | ε` and `(A)? | ε` both build.
- **Test:** `h5_3_optional_plus_empty_alt_accepted`
- **Affected surfaces:** `[A] | ε`, `[A B] | ε`, root-level and nested forms.
- **Unaffected surfaces:** `A? | ε`, `(A)? | ε`.

### H5-4 — `\w`/`\W` Unicode word-class membership diverges

- **Severity:** Medium
- **Evidence:** A (bidirectional; `\d`/`\s` are parity controls)
- **Freshness:** fresh
- **Grammar:** `start: A` / `A: /\w/`
- **Input:** U+0301 (combining acute, `Mn`) and U+00B2 (superscript two, `No`)
- **Options:** `parser=lalr lexer=basic` and `parser=earley lexer=dynamic` (identical)
- **Python result:** rejects U+0301 (Python `\w` excludes combining marks); accepts U+00B2
  (`\w` follows `str.isalnum()`, which includes `No`).
- **lark-rs result:** accepts U+0301 (Rust `\w` includes `\p{M}`); rejects U+00B2 (Rust
  `\w` excludes `\p{No}`).
- **Root cause:** terminal regex bodies go to the Rust `regex` crate verbatim, so `\w` is
  the UTS#18 perl-word class; Python `re`'s `\w` is `isalnum()|"_"`. Same screening gap
  as ADR-0004/H4-2, on a different axis (matched set, not syntax). Fix site: the dialect
  normalization in `PatternRe::new` / `normalize_python_escapes`.
- **Expected fix contract:** support & match (rewrite `\w`/`\W` to Python's word set) **or**
  record an ADR-0004 deviation pinned by this test.
- **Nearest known / why distinct:** H4-2 covers syntax Python *rejects at build*; H5 covers
  POSIX classes *inside* `[...]`. H5-4 is a silently-wrong *membership* divergence on a
  construct **both** engines accept. `\d`/`\s` confirmed in parity.
- **Test:** `h5_4_w_class_unicode_membership`
- **Affected surfaces:** `\w`/`\W` against accented / combining-mark / CJK / `No` input,
  on every lexer.
- **Unaffected surfaces:** `\d`, `\s`, ASCII `\w`.

### H5-5 — `\N{NAME}` rejected and mis-categorized

- **Severity:** Low–Medium
- **Evidence:** A
- **Freshness:** fresh
- **Grammar:** `start: A` / `A: /\N{BULLET}/`
- **Input:** `•` (U+2022) (also `\N{LATIN SMALL LETTER A}` on `a`)
- **Options:** `parser=lalr lexer=basic` (and earley/dynamic — build-stage, lexer-independent)
- **Python result:** accepts — `\N{NAME}` is a named-character escape → `•`.
- **lark-rs result:** build error labelled "backtracking-only syntax (backreference /
  atomic group / possessive) ... see docs/LOOKAROUND_SCOPE.md" — none of which `\N{}` is;
  the underlying regex-crate error is `unrecognized escape sequence`.
- **Root cause:** the Rust `regex` crate has no `\N{}` escape, so compilation fails and the
  failure is routed through the lookaround analyzer's catch-all (wrong taxonomy).
- **Expected fix contract:** support & match — translate `\N{NAME}` to its codepoint before
  compiling (Python *accepts* it). At minimum, re-bucket the error as `InvalidRegex`, not
  `LookaroundScope`.
- **Nearest known / why distinct:** H4-2 enumerates `\p`/`\P`/`\x{}`/`\z` with contract
  *reject-like-Python*; `\N{}` is **not** in that set and carries the **opposite** contract
  (Python accepts). The mis-categorization echoes H8/H9/N10 but on a new escape.
- **Test:** `h5_5_named_unicode_escape_supported`
- **Affected surfaces:** any terminal using `\N{...}`.
- **Unaffected surfaces:** other named/numeric escapes (`\x41`, `A`, `\101` per H9).

### H5-6 — Regex-crate angle named-group `(?<name>...)` accepted

- **Severity:** Low
- **Evidence:** A
- **Freshness:** fresh
- **Grammar:** `start: A` / `A: /(?<x>a)/`
- **Input:** `a`
- **Options:** `parser=lalr lexer=basic`
- **Python result:** build error — `LexError: Cannot compile token A: '(?<x>a)'` (raw `re`:
  `unknown extension ?<x`). Python `re` only spells named captures `(?P<name>...)`.
- **lark-rs result:** accepts, parses `start[A 'a']` (the Rust `regex` crate supports the
  angle spelling natively).
- **Root cause:** `PatternRe::new`'s `Regex::new` succeeds on `(?<name>...)`; no dialect
  screen rejects the regex-crate-only spelling.
- **Expected fix contract:** reject-like-Python — categorized build error alongside
  `reject_global_inline_flags`. The lookbehind spellings `(?<=`/`(?<!` must stay exempt;
  only `(?<` + name + `>` is the divergent capture form. (`(?'name'...)` is rejected by
  both — the regex crate also rejects quote syntax — so it is *not* a finding.)
- **Nearest known / why distinct:** N4 is `(?P=name)` (a *backref*, routed via the
  lookaround seam). H5-6 is a *capture-group* spelling lark-rs silently compiles.
- **Test:** `h5_6_angle_named_group_rejected`
- **Affected surfaces:** any terminal using the angle named-group spelling.
- **Unaffected surfaces:** `(?P<name>...)` (both accept), `(?'name'...)` (both reject).

### H5-7 — Turkish dotted/dotless-i case-fold under `/i` (NEEDS-DECISION)

- **Severity:** Low
- **Evidence:** A
- **Freshness:** fresh (genuine fix-contract fork)
- **Grammar:** `start: A` / `A: /I/i` (and `A: /i/i`)
- **Input:** `ı` (U+0131 dotless i) / `İ` (U+0130 dotted capital I)
- **Options:** `parser=lalr lexer=basic`
- **Python result:** accepts — `re.match('I','ı',re.I)` is truthy (Python folds the Turkish
  i-pair against ASCII i/I).
- **lark-rs result:** rejects — the Rust `regex` crate's Unicode *simple* case fold excludes
  U+0130/U+0131 from the `I`/`i` class. (A *less*-permissive divergence.)
- **Root cause:** the `/i` flag lowers to a `(?i)` prefix with no per-character fold
  remapping; the regex crate's simple-fold table differs from Python's. Controls that
  *agree*: Kelvin K, micro µ, angstrom Å, ß, Σ — only the Turkish i-pair diverges.
- **Expected fix contract:** **needs-decision** — match Python's fold table (expensive,
  "circumstantial + expensive" per ADR-0017) vs preserve the divergence via an ADR (the
  `\<`/`\>` precedent). The test pins the falsifiable Python behavior; if the verdict is
  diverge-and-document, delete the test rather than un-ignore it.
- **Nearest known / why distinct:** distinct from the Unicode-class items (H4-2, H5-4) —
  this is the case-fold *equivalence table*, not class membership.
- **Test:** `h5_7_turkish_i_casefold`
- **Affected surfaces:** `/i`-flagged terminals against U+0130/U+0131.
- **Unaffected surfaces:** all other case-fold pairs tested.

### H5-8 — Anonymous `"\\"` / `"\r\n"` literals mis-named

- **Severity:** Low
- **Evidence:** A
- **Freshness:** fresh (one root cause, two surfaces)
- **Grammar:** `start: "\\" NAME` / `NAME: /[a-z]+/` (and `start: "\r\n" NAME`)
- **Input:** `\foo` (and CR-LF + `foo`); `keep_all_tokens=True` to surface the token type
- **Options:** `parser=lalr lexer=contextual`
- **Python result:** `Token('BACKSLASH','\\')` / `Token('CRLF','\r\n')`.
- **lark-rs result:** `Token('__ANON_0','\\')` / `Token('__ANON_0','\r\n')` (value correct,
  type diverges).
- **Root cause:** `TERMINAL_NAMES` (`src/grammar/loader/terminals.rs`) reproduces all 35
  single-char rows of Python's `_TERMINAL_NAMES` but is missing exactly `"\\"`→`BACKSLASH`
  and `"\r\n"`→`CRLF`, so `terminal_name_hint()` falls through to `fresh_terminal()`.
- **Expected fix contract:** support & match — add the two missing rows.
- **Nearest known / why distinct:** not H4-1 (escape *decoding*), N8 (positions), H4-3
  (`%ignore` clone), or N2 (`unless` retype) — a pure naming-table gap.
- **Test:** `h5_8_anon_terminal_naming_table` (both surfaces)
- **Affected surfaces:** anonymous `"\\"`/`"\r\n"` literals in the tree (`keep_all_tokens`)
  and in error messages.
- **Unaffected surfaces:** the 35 single-char rows; multi-char punctuation → `__ANON`
  (matches Python).

### H5-9 — In-process LALR `ParseTable` is dense `O(states × terminals)` (perf / provisional)

- **Severity:** Medium (memory/build cost — not a wrong-answer bug)
- **Evidence:** B (deterministic structural-count measurement; **no committed gate**)
- **Freshness:** fresh
- **Grammar shape:** `start: r0 | … | rn` / `ri: Ai Bi Ci` with distinct terminals per
  alternative (states ~2n, terminals ~3n — both linear in n)
- **Size sequence / measurement** (dimensions read from the built `ParseTable.action`):

  | n | states | terms | dense_cells (S×T) | filled (`Some`) | Python action entries |
  |---|--------|-------|-------------------|-----------------|-----------------------|
  | 4   | 18  | 13  | 234     | 21  | 25  |
  | 16  | 66  | 49  | 3,234   | 81  | 97  |
  | 64  | 258 | 193 | 49,794  | 321 | 385 |
  | 128 | 514 | 385 | 197,890 | 641 | 769 |

  States match Python exactly; `dense_cells` grows quadratically (~12n²) while `filled`
  and Python's sparse entry count grow linearly (~6n). `Option<Action>` is 16 bytes, so
  n=128 ≈ 3.2 MB for a table Python holds in 769 dict entries.
- **Root cause:** `src/parsers/lalr.rs` allocates `action = vec![vec![None; n_terminals];
  n_states]` (and `goto` likewise) — eager dense `[state][terminal]` matrices. Python Lark
  stores a sparse dict-of-dicts; lark-rs's own **standalone** emitter already bakes a sparse
  `&[(u32, Action)]` row, so the sparse form exists in-tree — only the in-process runtime
  table is dense.
- **Expected fix contract:** support (perf) — switch `ParseTable.action`/`goto` to a sparse
  per-state representation (the standalone `&[(u32, Action)]` shape is the template) and
  gate it with a new deterministic counter `parse_table_action_cells` over this size sweep.
  No parse-result change.
- **Nearest known / why distinct:** N9 (`x~n..m` grammar-text size), H11/#335 (Earley
  dynamic-lexer scan), H4-12/#349 (DFA 2^N determinization) are different axes; H5-9 is the
  LALR parse-*table* memory/build representation.
- **Test:** **none committed** — a clean gate needs a `parse_table_action_cells` perf
  counter and internal table access from a separate test crate, neither of which exists
  today. Documented here with the measurement; the burndown issue's done-when is "add the
  counter + sparse table + the scaling gate."
- **Affected surfaces:** any grammar with many independent keyword/alternative families
  (large SQL dialects, enum-like grammars).
- **Unaffected surfaces:** parse results (identical); small grammars (constant overhead).

## Variants

- **H5-8 / `"\r\n"`→`CRLF`** — same missing-`TERMINAL_NAMES`-rows root cause as the
  `"\\"`→`BACKSLASH` surface; both asserted by `h5_8_anon_terminal_naming_table`.
- **H5-4 / `\W`** — symmetric complement of `\w`; same UTS#18-vs-`isalnum` root cause.
- **H5-1 / lookbehind** — `(?<=x)a` mis-ranks identically to the `(?=b)` lookahead form;
  same `max_width=None` root cause.

## Clean buckets

Probed and matched the oracle (negative evidence — not proof of correctness):

- **Tree-shaping algebra (116 combination cases):** `maybe_placeholders` × `expand1` ×
  `keep_all_tokens` × alias × transparent, incl. multi-symbol `[A B]`/`[A][B][C]` placeholder
  counts, nested `?a:?b:?c`, alias-vs-expand1 precedence, `!rule` + `%ignore`. Only H5-3 fell out.
- **Standalone / `include_lark!` (≈50 compiled-and-run probes):** `keep_all_tokens`,
  `maybe_placeholders` None insertion, aliases, priorities, `unless` retyping, modifiers,
  templates, `%ignore`, positions, unicode, multi-start. Byte-identical to in-process +
  Python; only the known H12 reproduced.
- **CYK semantics:** `keep_all_tokens`, `maybe_placeholders`, empty input, aliases,
  transparent rules, priorities, templates, unit chains — all match Python under `lexer='basic'`.
- **Earley priority / ambiguity resolution (~80 curated + 240 randomized):** positive &
  negative rule priority, terminal priority forest-sum (dynamic-only zeroing), tie-break
  fallback, explicit `_ambig` set/structure. Faithful to `ForestSumVisitor`; the only
  standing divergence is the architect-ratified #159 dedup.
- **Cross-backend consistency:** LALR/Earley/CYK accept-reject and unambiguous trees agree
  with Python per backend.
- **Token-value fidelity:** capture-group values, zero-width matches, newlines/tabs/astral,
  `%ignore`d-content exclusion, keyword/`unless` retype values, 35 single-char anon names.
- **Wild / hostile grammars + a ~12,500-case grammar fuzzer (7 seeds):** JSON5, INI, CSS
  selectors, semver, SQL keyword/identifier overlap, calculators, verilog/template_lark —
  0 fresh divergences (verilog/template_lark hit only the known internal-lookahead refusal).
- **Resource bounds:** contextual per-state scanner dedup holds; Earley right/left recursion
  linear; EBNF leading-nullable distribution is a faithful port of Python's `EBNF_to_BNF`
  (off-by-one `$root`), not a pathology.

## Harness caveats

- `tools/diffcheck.py compare()` strips `Tree.meta`, so all position/`propagate_positions`
  findings are provisional under it (need a custom probe binary) — none were counted this round.
- `compare()` accepts only inline grammar text, not file paths, so file-`%import` corners
  (circular / nested) were not exercised (already heavily mined per the ineligible set).
- CYK requires `lexer='basic'`; passing the default `contextual` yields a config error that
  must not be misread as engine behavior (it falsified three first-pass CYK candidates).
- H5-9's quantities are deterministic structural counts, not wall-clock — but with no
  committed counter they remain documentation, not a gate.
