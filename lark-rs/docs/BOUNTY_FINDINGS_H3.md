# lark-rs bug-bounty findings — round 3 (h3)

Round 1 (`BOUNTY_FINDINGS.md`, RC series) harvested the "front door too permissive"
validation gaps plus the RC5 lexer-width bug; round 2 (`BOUNTY_FINDINGS_H2.md`, N
series) took the subtler config/position/ranking layer. Almost all RC/N findings are
fixed on this baseline. Round 3 retargeted the surfaces those rounds declared clean or
never reached: **grammar-loader robustness** (a panic, template-parameter validation,
terminal aliasing), the **Python-`re` dialect of character classes / quantifiers /
escapes** (not just the parked anchor dialect), the **Earley default-lexer
resolution**, **`Tree.meta`**, a **deterministic Earley dynamic-lexer scan
pathology**, and a **standalone tree-shape** break.

Ten retooled strike teams ran against the same harness. After minimization,
independent re-verification (every repro re-run against the live oracle — several
quick agent claims were **overturned** on re-check, see *Invalid/rejected* below), and
dedup against RC1–RC10 / N1–N10 / V1–V4 and the open known-issue set, this catalog
records **12 fresh, confirmed root causes** (with 3 multi-surface findings) plus **2
provisional source-traced binding findings**.

## Target & method

- **Baseline SHA (frozen):** `afa20a07f81d0599a9b6705aae881fc8c8223ccc`
  (branch `claude/bug-hackathon-5uwhmd`).
- **Oracle:** Python Lark `1.3.1` (the in-repo `lark/`).
- **Harness:** `tools/diffcheck.py` (`compare()`) + the `diffcheck` binary over the
  same (grammar, input, options) tuple, plus direct `lark_rs` API checks for the
  panic (H1), `Tree.meta` (H10), the deterministic dynamic-lexer scan counter (H11),
  the standalone bake (H12), and the binding source surfaces (P1/P2). The harness does
  not cover `g_regex_flags`, `propagate_positions`, `Tree.meta`, the standalone
  runtime, or the bindings — those were checked directly.
- **Reproductions:**
  - `tests/test_bounty_findings_h3.rs` — 13 `#[ignore]` (XFAIL) tests (H1–H10), run:
    `cargo test --test test_bounty_findings_h3 -- --ignored`.
  - H11 additionally needs the deterministic work counters:
    `cargo test --features perf-counters --test test_bounty_findings_h3 -- --ignored`
    (14 ignored tests total with the feature on).
  - H12 (standalone) lives in `src/standalone/mod.rs` because the baked runtime is only
    reachable via the module's internal harness:
    `cargo test --lib standalone_expand1_lone_none -- --ignored`.
- **Eligibility:** none reduce to a round-1/2 root cause (RC1–RC10, N1–N10, V1–V4) or
  the open known-issue set: #272 (RC7 LALR R/R), #275 (`\b`/`\B`/`\Z` anchor dialect —
  parked, `needs-decision`), #281 (N7 binding recursion), #286 (`%extend` imported
  terminal), #288 (global `(?i)` raw_value_len), #289 (root `?start:[A]` lone-None),
  #299 (two duplicate-definition surfaces), #302 (Earley adjacent bounded repeats),
  #304 (standalone `\Z`/oversized-repeat mis-category), #208 (fuzzer burndown). Where a
  find is *adjacent* to a known issue the distinction is noted inline.

A recurring theme persists from rounds 1–2 — lark-rs **silently accepts grammars/regex
constructs Python rejects, or diverges on the Python-`re` dialect** — but via new
mechanisms (a panic, template validation, terminal aliasing, char-class/quantifier
dialect, default-lexer resolution) untouched by the prior catalogs.

## Accounting

- **Fresh root causes:** 12 — H1, H2, H3, H4, H5, H6, H7, H8, H9, H10, H11, H12.
- **Multi-surface findings (one root, ≥2 surfaces):** H2 (H2a duplicate param + H2b
  param-shadows-rule), H5 (H5a POSIX class + H5b class set-op), H9 (H9a octal `\101` +
  H9b `[\b]` backspace-in-class).
- **Variants of fresh causes:** none counted separately (surfaces folded above).
- **Known duplicates:** 0 (the dedup set above held).
- **Provisional / source-only (not encoded as executable XFAIL):** 2 — P1 (PyO3
  `g_regex_flags` value-incompatibility), P2 (PyO3/WASM reject `ambiguity='auto'`).
- **Invalid / rejected reports (re-verified to agreement — discarded):** template
  arity mismatch (both reject), `%ignore A B` sequence (both accept), `%ignore
  UNDEFINED` (both reject), negative terminal priority `A.-1` (both accept), exact
  `x~N` grammar size (linear, not quadratic — agrees with Python), `keep_all_tokens`
  drops a sub-rule literal (both agree), CYK rejects an empty rule (both reject), CYK
  `keep_all_tokens` (both agree), `Tree.meta` byte-offset positions (correct, char
  indices), CYK `Tree.meta` propagation (correct), C-API byte positions (inherits #307
  correctly), CYK ambiguity tie-break (non-binding, ineligible per ADR-0017). These
  were quick-pass agent claims that did not survive re-verification.

## Severity summary

| ID  | Severity | Fresh? | Evidence | Bucket          | One-line |
|-----|----------|--------|----------|-----------------|----------|
| H1  | Critical | fresh  | A        | grammar-loader  | Undefined start symbol **panics** in `lower()` instead of `GrammarError` |
| H2  | High     | fresh  | A        | grammar-loader  | Template-parameter validation missing (duplicate param; param shadowing a rule) |
| H3  | High     | fresh  | A        | grammar-loader  | Alias `->` inside a terminal definition accepted (Python forbids) |
| H4  | High     | fresh  | A        | parser-config   | Earley `lexer="auto"` (default) resolves to **basic**, not dynamic |
| H5  | High     | fresh  | A        | lexer (dialect) | Char-class dialect: POSIX `[[:alpha:]]` & set-op `[a-c&&b]` use Rust-regex semantics, not Python `re` |
| H6  | High     | fresh  | A        | lexer (dialect) | Possessive quantifier `a++` silently reinterpreted as greedy `(a+)+` |
| H11 | High     | fresh  | A        | perf (lexer)    | Earley dynamic lexer forward-scans → O(n²); Python's anchored match is O(n) |
| H12 | High     | fresh  | A/B      | distribution    | Standalone bake doesn't collapse expand1 lone-`None` → tree diverges from core+Python |
| H7  | Medium   | fresh  | A        | lexer (dialect) | Stacked quantifier `/a{2}{3}/` accepted; Python rejects "multiple repeat" |
| H8  | Medium   | fresh  | A        | lexer (taxonomy)| `(?#comment)` leaks a raw uncategorized regex error; Python accepts |
| H9  | Medium   | fresh  | A        | lexer (taxonomy)| Octal `\101` / backspace-in-class `[\b]` rejected & mis-categorized as lookaround |
| H10 | Medium   | fresh  | A        | core (meta)     | `Tree.meta.empty` false for a node with only positionless children |
| P1  | High *(provisional)* | fresh | C | bindings | PyO3 `g_regex_flags` forwards raw `u32`; incompatible with Python `re` flag bits |
| P2  | Medium *(provisional)* | fresh | C | bindings | PyO3/WASM reject `ambiguity='auto'` (Python's default & a valid value) |

---

## Findings

### H1 — Undefined start symbol panics instead of GrammarError (Critical, grammar-loader)
- **Grammar:** `foo: "a"` (start symbol `start` is never defined) · **Input:** `""`
- **Options:** default (reproduces on lalr basic+contextual and earley; any undefined
  start symbol, default or custom, triggers it).
- **Python:** build error — `GrammarError: Using an undefined rule: NonTerminal('start')`.
- **lark-rs:** **panics** — `panicked at src/grammar/intern.rs:376: start symbol interned`.
- **Root cause:** `lower()` does
  `grammar.start.iter().map(|s| symbols.id(s).expect("start symbol interned"))`
  (`src/grammar/intern.rs:376`) with no prior check that each start symbol resolves to
  a defined rule. A `.expect()` on user-controlled input is a robustness/DoS hole.
- **Expected fix contract:** reject-like-Python — return a `GrammarError` for an
  undefined start *before* interning.
- **Nearest known:** #256/#251 (default-start *selection* on multi-start grammars) — a
  different surface (those pick among defined starts; this is an *undefined* start).
- **Test:** `h1_undefined_start_rejected_not_panicked`.
- **Affected surfaces:** every parser path through `lower()`. **Unaffected:** grammars
  whose start is defined.

### H2 — Template-parameter validation missing (High, grammar-loader)
lark-rs has no analogue of Python's `GrammarDefinition.validate()` template-parameter
pass. One root cause, two surfaces:
- **H2a** `foo{x,x}: x` / `start: foo{"a","b"}` — Python:
  `GrammarError: Duplicate Template Parameter x (in template foo)`; lark-rs builds it.
- **H2b** `x: "z"` / `foo{x}: x` / `start: foo{"a"}` — Python:
  `GrammarError: Template Parameter conflicts with rule x (in template foo)`; lark-rs
  builds it **and mis-parses** (`"a"` → `start(foo())`, the param shadowing rule `x`).
- **Root cause:** `src/grammar/loader/templates.rs` / `compiler.rs` zip params→args
  without a duplicate-name or param-vs-rule-name check.
- **Expected fix contract:** reject-like-Python (port the two `validate()` checks).
- **Nearest known:** RC1/RC2/#299 (plain duplicate *definitions*) — distinct (template
  parameter-list well-formedness, a separate Python pass).
- **Tests:** `h2a_duplicate_template_param_rejected`, `h2b_template_param_shadows_rule_rejected`.

### H3 — Alias `->` inside a terminal definition accepted (High, grammar-loader)
- **Grammar:** `A: "a" -> foo` / `start: A` · **Input:** `"a"`
- **Python:** build error —
  `GrammarError: Aliasing not allowed in terminals (You used -> in the wrong place)`.
- **lark-rs:** accepts; silently drops the alias (`"a"` → `start(Token(A,"a"))`).
- **Root cause:** the loader accepts an `-> name` alias on a terminal body; Python
  forbids aliases anywhere in terminal definitions.
- **Expected fix contract:** reject-like-Python.
- **Nearest known:** RC4a/b/c (aliases on *rules* / inside groups) — distinct (terminal
  definition, separate Python check + message).
- **Test:** `h3_alias_in_terminal_rejected`.

### H4 — Earley `lexer="auto"` resolves to basic instead of dynamic (High, parser-config)
- **Grammar:** `start: "print" NAME` / `NAME: /[a-z]+/` / `%ignore " "` ·
  **Input:** `"printx"` · **Options:** `parser=earley`, `lexer=auto` (the default).
- **Python:** accepts — `auto` resolves Earley to the **dynamic** (parse-directed)
  lexer (no postlex), which matches `"print"` then `NAME="x"`.
- **lark-rs:** rejects — `auto` falls into the catch-all **basic**-lexer arm of
  `build_earley` (`src/parsers/mod.rs`), which maximal-munches `"printx"` as one
  `NAME`. Also produces wrong trees on ambiguous-segmentation grammars.
- **Root cause:** `build_earley`'s lexer match has no `LexerType::Auto` arm; Python
  (`lark/lark.py`) resolves earley+auto→dynamic (basic only with a postlex). The
  validation comment at `mod.rs:585` already documents the correct rule; the code
  disagrees.
- **Expected fix contract:** support-and-match — resolve `Auto`→`Dynamic` for earley
  when `postlex.is_none()`, else `Basic`.
- **Nearest known:** N5 (parser/lexer pairing *legality*, fixed) — distinct (this is
  the *default resolution* of the legal `auto`, not an illegal pairing). The
  postlex+earley→basic path is documented; the no-postlex case silently assumed basic
  is the bug.
- **Test:** `h4_earley_auto_lexer_is_dynamic`.

### H5 — Character-class Python-`re` dialect divergence (High, lexer)
The Rust `regex` crate's char-class extensions are not normalized toward Python `re`
(`normalize_python_escapes`, `src/grammar/terminal.rs`, rewrites only `\<`/`\>`). One
root cause, two surfaces, both **silent wrong-language**:
- **H5a** `A: /[[:alpha:]]/` on `"a]"` — Python has no POSIX classes: it reads
  `[[:alph a]` + literal `]` and matches the 2-char `"a]"`; lark-rs uses the Rust POSIX
  class (single alpha) and rejects.
- **H5b** `A: /[a-c&&b]/` on `"a"` — Python reads `&&` as literal (class `{a,b,c,&}`,
  matches `"a"`); lark-rs reads set-intersection (`{b}`) and rejects `"a"`.
- **Expected fix contract:** **fork (consider `needs-decision`)** — either normalize
  toward Python's literal reading (the ADR-0017 oracle default, which the tests assert)
  or refuse with a categorized `InvalidRegex`. Silent re-interpretation is the bug
  either way.
- **Nearest known:** the `\<`/`\>` normalization (documented dialect rewrite) — H5 is a
  new dialect axis (char-class extensions). Distinct from #275 (anchors).
- **Tests:** `h5a_posix_class_python_re_dialect`, `h5b_class_setop_python_re_dialect`.

### H6 — Possessive quantifier silently reinterpreted as greedy (High, lexer)
- **Grammar:** `A: /a++a/` · **Input:** `"aaa"`
- **Python:** rejects — `a++` is possessive (no give-back), `re.match("a++a","aaa")` is
  `None`.
- **lark-rs:** accepts — the Rust `regex` crate parses `a++` as nested repetition
  `(a+)+` (greedy) and matches `"aaa"`.
- **Root cause:** possessive markers (`*+`/`++`/`?+`) never reach
  `route_fancy_only_terminal` because the regex crate *accepts* the syntax with a
  different meaning; `docs/LOOKAROUND_SCOPE.md` lists possessive as a by-design
  *categorized refusal*, but reality is a silent mis-match.
- **Expected fix contract:** reject-with-categorized refusal (the documented non-goal)
  — or match Python; never silently accept the greedy reading.
- **Nearest known:** the documented backtracking-only refusal class — distinct (here it
  *fails to refuse* and mis-matches).
- **Test:** `h6_possessive_not_silently_greedy`.

### H7 — Stacked quantifier accepted (Medium, lexer)
- **Grammar:** `A: /a{2}{3}/` · **Input:** `"aaaaaa"`
- **Python:** build error — `sre_parse` raises "multiple repeat" → `Cannot compile
  token A`.
- **lark-rs:** accepts (the Rust regex crate allows stacked quantifiers) and lexes it.
- **Expected fix contract:** reject-like-Python (ADR-0017: don't out-permit the oracle).
- **Nearest known:** none; new dialect axis (quantifier shape).
- **Test:** `h7_stacked_quantifier_rejected`.

### H8 — Inline comment group `(?#...)` leaks a raw regex error (Medium, lexer taxonomy)
- **Grammar:** `A: /a(?#c)b/` · **Input:** `"ab"`
- **Python:** accepts — `(?#c)` is a comment, `"ab"` matches.
- **lark-rs:** build error — raw uncategorized leak
  `Invalid regex pattern '…': regex parse error: … unrecognized flag` (the RC6/N4-class
  symptom on a fresh construct).
- **Expected fix contract:** support-and-match (strip the comment) — at minimum, stop
  leaking a raw engine error.
- **Nearest known:** RC6 (`\b` raw leak) / N4 (`(?P=name)`) — same symptom class,
  different construct.
- **Test:** `h8_inline_comment_group_supported`.

### H9 — Python-accepted escapes rejected & mis-categorized as lookaround (Medium, lexer taxonomy)
The `route_fancy_only_terminal` catch-all over-claims `BacktrackingOnlySyntax` for
constructs the Rust regex crate rejects for *other* reasons. One root cause, two
verified surfaces (Team 3 also reports `\0` and `\N{name}`):
- **H9a** `A: /\101/` on `"A"` — Python reads octal `0o101 == 'A'` and matches; lark-rs
  build-errors, mis-categorized as "backtracking-only … backreference" (the crate
  reads `\1` as a backref).
- **H9b** `A: /[\b]/` on `"\x08"` — Python reads `\b` *inside a class* as backspace and
  matches; lark-rs mis-categorizes it as lookaround.
- **Expected fix contract:** support-and-match — translate the octal/backspace escapes
  (regular, Python-accepted) to their regex-crate equivalents; failing that, give a
  correct category (not "backtracking-only").
- **Nearest known:** N10/#275 (`\Z` *anchor* mis-categorization, parked) — distinct:
  H9 covers Python-*accepted* regular escapes, not the parked anchor-policy fork.
- **Tests:** `h9a_octal_escape_supported`, `h9b_backspace_in_class_supported`.

### H10 — `Tree.meta.empty` wrong for positionless children (Medium, core)
- **Grammar:** `start: empty` / `empty:` · **Input:** `""` · **Options:**
  `propagate_positions=true`.
- **Python:** `start.meta.empty == True` and the inner `empty` subtree's
  `meta.empty == True` (Python's `Meta.empty` defaults `True` and is cleared only when
  a position-bearing first/last child is found, skipping empty subtrees).
- **lark-rs:** `start.meta.empty == false` (the inner `empty` node, with no children,
  is correctly `true`).
- **Root cause:** `Meta::from_children` (`src/tree.rs`) sets `empty=true` **only when
  `children.is_empty()`**, leaving the derived `false` for the "children present, none
  positional" case. The position *spans* are correct — the defect is isolated to the
  flag. #307 fixed `Token` char-vs-byte positions but never touched this field.
- **Expected fix contract:** support-and-match — set `empty` true whenever no child
  contributed a position.
- **Nearest known:** N8/#307 (`start_pos`/`end_pos` char-vs-byte) — distinct field,
  distinct code path; survived the #307 fix.
- **Test:** `h10_meta_empty_for_positionless_children`.

### H11 — Earley dynamic lexer forward-scans → O(n²) (High, perf)
- **Grammar:** `start: (WORD | STR)+` / `WORD: /[a-z]+/` / `STR: /\#[^\#]*\#/` /
  `%ignore " "` · **Input:** `n` words then one trailing `" #z#"` · **Options:**
  `parser=earley`, `lexer=dynamic`.
- **Python:** O(n) total lexing — its dynamic matcher uses
  `re.Pattern.match(text, index)`, anchored at `index`.
- **lark-rs:** O(n²) — `DynamicMatcher::match_end_at` (`src/lexer/dynamic.rs`) uses
  `Regex::find_at`, an **unanchored forward search**; a sparse terminal (`STR`) in the
  per-position scan set forward-scans O(remaining input) at every word position.
  Deterministic `lexer_scan_steps`/byte: **67 → 259 → 1027 → 4099** across n=64→4096
  (linear in n ⇒ O(n²) total).
- **Root cause:** the `\G` start-of-search anchor that fixed the combined `Scanner`
  (#104) was never ported to `DynamicMatcher`; `test_lexer_scaling.rs` gates only
  basic/contextual, leaving the dynamic path uncovered.
- **Expected fix contract:** support-and-match — anchor the dynamic per-terminal match
  at the query position (as `scanner.rs` does), restoring O(n).
- **Nearest known:** #104 (basic/contextual `\G`) and the documented `dynamic_complete`
  inner-prefix O(n²) (Python does the same there) — distinct: plain `dynamic`, the
  `find_at` forward scan Python's anchored `match` avoids.
- **Test:** `h11_dynamic_lexer_scan_is_flat_per_byte` (needs `--features perf-counters`).

### H12 — Standalone bake doesn't collapse expand1 lone-`None` (High, distribution)
- **Grammar:** `start: w "x"` / `?w: [A]` / `A: "a"` · **Input:** `"x"` · **Options:**
  `parser=lalr`, `lexer=basic`, `maybe_placeholders=true`.
- **Python / core lark-rs:** `Tree(start, [None])` — the lone-`None` `?w` collapses.
- **lark-rs standalone (baked):** `Tree(start, [Tree(w, [None])])` — the wrapper
  survives, diverging from **both** core and Python.
- **Root cause:** the baked runtime's `shape` (`src/standalone/runtime.rs:377`) carries
  an extra guard `&& !matches!(children[0], Child::None)` on the expand1 arm, so a lone
  `None` is wrapped instead of spliced. The core builder
  (`src/parsers/tree_builder.rs`) handles it via `Slot::Inline(vec![Child::None])` (the
  RC9 carve-out). The RC9 fix landed in `tree_builder.rs` and was never mirrored into
  the hand-reimplemented standalone driver.
- **Expected fix contract:** support-and-match — mirror the RC9 lone-`None` carve-out
  in `runtime::shape` (the lexer/`scanner_plan` path is genuinely shared; only the
  tree-shaping re-expression drifted).
- **Nearest known:** RC10/V1/V2 (standalone *bake-time refusals* for lookaround/`\Z`/
  oversized repeats) — distinct: this grammar bakes & runs, the divergence is the
  *runtime tree shape*. It **falsifies** round-2's "byte-faithful by construction"
  clean bucket (`BOUNTY_FINDINGS_H2.md`).
- **Evidence:** A/B — generated source compiled & run in an isolated crate; core +
  the `runtime.rs:377` guard confirmed directly.
- **Test:** `standalone_expand1_lone_none_collapses_like_core` (in `src/standalone/mod.rs`).

---

## Provisional / source-only findings (not encoded as executable XFAIL)

These are real at the source level and (P1) demonstrated empirically by the team, but
the binding toolchains (maturin/wasm-pack) are not reliably available in CI, so they
are documented as provisional — the round-2 N7 precedent. Done-when: a binding test
that builds the extension and reproduces the divergence.

### P1 — PyO3 `g_regex_flags` value-incompatibility (High, provisional, bindings)
`python/src/lib.rs` takes `g_regex_flags: u32` and forwards it **raw** to the core,
whose bit values are `IGNORECASE=1, MULTILINE=2, DOTALL=4, VERBOSE=8`
(`src/grammar/terminal.rs`). Python users pass `re`-module constants
(`re.I=2, re.M=8, re.S=16, re.X=64`); the bitsets do not align, so `g_regex_flags=re.I`
silently applies MULTILINE and `re.S`/`re.X` are silently ignored. The **WASM** binding
maps flag letters to the same core constants correctly — confirming this is a
PyO3-specific defect. Expected fix contract: translate Python `re` bits → core bits in
`PyLark::new` (or accept the letter-string form WASM uses).

### P2 — PyO3/WASM reject `ambiguity='auto'` (Medium, provisional, bindings)
Python Lark's `ambiguity` default is `'auto'` and `'auto'` is a legal value (verified).
PyO3 `parse_ambiguity` (`python/src/lib.rs`) and WASM `parse_options`
(`wasm/src/lib.rs`) accept only `resolve|explicit|forest` and raise on `'auto'`, so a
user copying `ambiguity='auto'` from the Python docs hits a hard error. Expected fix
contract: accept `"auto"` as the resolve default in both option parsers.

---

## Clean buckets (honest negatives)

- **Tree-shaping algebra (Team 7, ~5,400 generated grammars × flag combos + deep
  hand-curated compositions):** every accept-on-both grammar produced byte-identical
  trees vs Python — expand1/transparent/alias/keep_all_tokens/maybe_placeholders all
  compose correctly. The earlier "keep_all_tokens drops a sub-rule literal" claim did
  not survive re-verification (both agree). Only the known #289 root remains.
- **Cross-backend consistency (Team 6, ~120 cases + a fuzzer across 6 parser/lexer
  combos):** lark-rs's LALR/Earley/CYK are self-consistent and match Python on every
  *unambiguous* grammar. The only divergence is a CYK *ambiguity tie-break*
  (non-binding, ineligible per ADR-0017 / the testing-philosophy tie-break ban). The
  "CYK empty rule" and "CYK keep_all_tokens" claims did not survive re-check.
- **Positions (Team 8):** `start_pos`/`end_pos` are correct char indices across
  multibyte, `%ignore`, and all backends (#307 inherited correctly); `Tree.meta`
  *spans*, line/column (incl. CRLF, tabs, newline-spanning tokens) all match Python.
  Only the `Tree.meta.empty` *flag* (H10) diverges.
- **Grammar-size resource growth (Team 10):** exact `~N`, range `~mn..mx`, nested and
  grouped repeats, templates, big alternations all track Python's logarithmic/linear
  factoring (the #279 port is faithful). The earlier "exact `~N` is Θ(N²)" claim was
  retracted on re-measure (16→18→20 RHS symbols for N=100→200→400). The only resource
  find is the dynamic-lexer scan (H11).
- **C-API positions (Team 5):** the C binding copies positions straight from the core
  token (char indices) — the earlier byte-offset claim did not survive an actual
  ctypes run.

## Harness caveats

- When **both** engines reject, `compare()` reports agreement even if the *stage*
  differs (build vs parse) — H2a surfaces as a build-level over-acceptance whose chosen
  input then fails to parse, so it is asserted with `assert_build_rejected`, not via
  the harness's accept/reject verdict.
- Python emits `FutureWarning: Possible nested set / set intersection` for H5a/H5b —
  confirming the constructs are the deprecated-but-accepted dialect, not errors.
- H11 reads `perf::lexer_scan_steps` (the deterministic work counter), which is a no-op
  without `--features perf-counters`; the test is `#[cfg(feature = "perf-counters")]`
  so it does not false-green when the feature is off.
- The binding findings (P1/P2) are source-traced; the toolchains were not reliably
  reproducible, so they are provisional, not payable at A/B.
