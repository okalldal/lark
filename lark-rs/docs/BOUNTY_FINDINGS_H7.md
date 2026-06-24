# lark-rs bug-bounty findings — round 7 (h7)

Round 1 (`BOUNTY_FINDINGS.md`, RC series) harvested the "front door too permissive"
validation gaps plus the RC5 lexer-width bug; round 2 (`_H2.md`, N series) the
config/position/ranking layer; round 3 (`_H3.md`, H series) grammar-loader robustness
and the first wave of Python-`re` regex-dialect divergences; round 4 (`_H4.md`, H4-*)
string-literal escapes, regex-crate-only dialect, error parity, import mangling; round 5
(`_H5.md`, H5-*) cross-backend consistency, tree-shaping algebra, residual dialect
corners; round 6 (`_H6.md`, H6-*) terminal ranking, `{,m}`, aliased/nested nullable
reduce/reduce, meta span, string-vs-regex unification. Round 7 retargeted the corners
those rounds either declared clean or never reached: **two missing loader validation
gates** (`%ignore` of a `%declare`d terminal; literal newlines inside `/regex/` and
`"string"` literals), a **build-accepted Python `re` construct lark-rs rejects**
(group-existence conditionals), the **PyO3 binding's `str`-contract** (a self-broken
eq/hash invariant + surface gaps), and a **standalone-runtime `None`-root** path the
`ParseTree::None` work (#382) never reached.

Ten retooled strike teams ran against the same harness. After minimization, independent
re-verification at intake (every repro re-run against the live oracle — **one finding was
overturned on re-check**, see *Invalid/rejected* below), and dedup against
RC1–RC10 / N1–N10 / V1–V4 / H1–H12 / P1–P2 / H4-1…H4-12 / H5-1…H5-9 / H6-1…H6-8 and the
open known-issue set, this catalog records **3 fresh, confirmed correctness root causes**
(one with two surfaces) encoded as executable XFAILs, **1 fresh PyO3-binding root cause**
(A-level, documented — not encoded as a `cargo` XFAIL, like prior binding findings), and
**1 variant** of a known cause on a new (unfixed) surface.

## Target and method

- **Baseline SHA (frozen):** `9acb50bb203bcf4b5949f3a19bfdc4bfe3f0b2d5`
- **Oracle:** Python Lark **1.3.1** (the in-repo `lark/`), the project ground truth.
- **Harness:** `tools/diffcheck.py` (`compare(grammar, text, **opts)`) + the `diffcheck`
  Rust binary — runs both engines on the same `(grammar, input, options)` tuple and
  reports accept/reject + tree-shape divergence. The harness wires
  `parser`/`lexer`/`start`/`ambiguity`/`maybe_placeholders`/`keep_all_tokens`/`strict`;
  it does **not** wire `propagate_positions`, `g_regex_flags`, file/`base_path` imports,
  `postlex`, or the bindings — those were probed directly. The PyO3 binding finding was
  produced by building PyO3 live (`maturin develop` into a venv with `lark==1.3.1`); it is
  not re-run by `cargo test` and is documented here as A-level-by-hand + source-traced.
- **Reproduction command:**
  ```bash
  cargo test --test test_bounty_findings_h7 -- --ignored      # 4 XFAILs (H7-1, H7-2a, H7-2b, H7-3)
  cargo test --lib standalone_none_root_returns_none_like_core -- --ignored   # V-H7-1
  ```
  All five ignored tests fail today; each asserts the Python-oracle behavior.
- **Teams:** 10 strike teams (negative grammar conformance; regex width/ranking; Python
  `re` dialect/taxonomy; standalone & `include_lark!`; binding surface; cross-backend
  consistency; tree-shaping algebra fuzzer; transformer/position parity; wild/hostile
  grammars; deterministic perf bounds).

### Ineligible set (deduped against, not re-counted)

- All prior findings: **RC1–RC10, RC2b, N1–N10, V1–V4, H1–H12, P1–P2, H4-1…H4-12,
  V-H9/V-H1, H5-1…H5-9, H6-1…H6-8, B1/B2/L1.**
- Open known-issue root causes (all 57 open issues), notably **#275** (`\b`/`\B`, `\Z`
  anchors), **#281** (bindings unbounded recursion), **#286** (`%extend` imported
  terminal), **#288** (`raw_value_len` global `(?i)` prefix), **#302** (Earley adjacent
  bounded repeats), **#304** (standalone `\Z`/oversized repeat), **#332** (POSIX classes
  + set-ops), **#337** (`Tree.meta.empty`), **#338** (PyO3 `g_regex_flags`/`ambiguity`),
  **#347/#377** (Str-vs-Str dedup / cross-site recurse `filter_out`), **#348** (nullable
  +recursive Earley), **#349** (DFA counted-repeat determinization), **#350** (EOF error
  token), **#360–#367** (H5 burndown), **#372** (`%import` overlapping closures), **#382/
  ADR-0033** (`ParseTree::None` public variant), **#391** (oracle-honesty lexer
  divergences), **#399–#407** (H6 burndown + provisional bindings/Earley).
- Documented intentional divergences: `_ambig` dedup (#159, ADR-0017), `\<`/`\>`
  normalization, the lookaround-scope `OutOfScope` refusals (`LOOKAROUND_SCOPE.md`),
  `ESCAPED_STRING`'s adaptation, the per-position token-filtering model (M6), the CYK
  equal-weight ambiguity tie-break, and the equal-span incidental-source-length lexer
  tie-break (not a valid oracle target).

## Accounting

- **Fresh correctness root causes: 3** (H7-1, H7-2, H7-3) — all executable A-level oracle
  XFAILs in `tests/test_bounty_findings_h7.rs`.
- **Multi-surface findings (one root, ≥2 surfaces):** H7-2 (regex-literal newline
  H7-2a + string-literal newline H7-2b — one missing no-embedded-newline gate in the
  loader's literal tokenizers).
- **Fresh binding root cause: 1** (H7-4, PyO3 `Token` eq/hash invariant violation) —
  A-level (PyO3 built live), with three folded binding-surface gaps (`Tree.meta` absent,
  `__repr__` quote style, `Token` constructor kwargs). Decision-flavored; documented, not
  encoded as a `cargo` XFAIL (consistent with B1/B2/N7).
- **Variants: 1** (V-H7-1) — the #289/#382 lone-`None` root cause on the unfixed
  standalone-runtime surface; encoded as an XFAIL in `src/standalone/mod.rs`.
- **Known duplicates / re-confirmed-not-counted:** the `__`-leading rule-name acceptance
  (H5-2/#361) and the `\b` DFA-leak (RC6/#275) resurfaced in the wild sweep — known, not
  counted. H6-7/#404 (duplicate-arm cross-product build blowup) reconfirmed still-open.
- **Invalid / rejected reports: 1.** A claimed line/column divergence on a terminal whose
  char-range spans `\n` without statically advertising it (the "static `newline_types`
  gate" hypothesis): **overturned at intake** — Python's static lexer *does* count the
  embedded newlines here (B token at `line=3,col=1` on both basic and contextual lexers),
  exactly as lark-rs does. No divergence. (Useful negative evidence: lark-rs's line/column
  accounting matches Python's even when `_regexp_has_newline` returns `False`.)

## Severity summary

| ID     | Severity     | Fresh? | Evidence | Bucket                  | One-line |
|--------|--------------|--------|----------|-------------------------|----------|
| H7-4   | Medium-High  | yes    | A (PyO3) | bindings                | PyO3 `Token == "x"` is True but `hash` differs → set/dict membership silently breaks |
| H7-3   | Low-Medium   | yes    | A        | lexer (regex dialect)   | Conditional regex `(?(1)yes\|no)` build-rejected & mis-categorized; Python builds and matches |
| H7-1   | Low          | yes    | A        | grammar-loader          | `%ignore` of a `%declare`d (pattern-less) terminal accepted; Python rejects at build |
| H7-2   | Low          | yes    | A        | grammar-loader/tokenizer| Literal newline in a `/regex/` (a) or `"string"` (b) literal accepted; Python rejects |
| V-H7-1 | Low-Medium   | variant| A        | distribution (standalone)| Standalone runtime lacks `ParseTree::None` → `?start:[A]` on empty input errors not None |

## Findings

### H7-1 — `%ignore` of a `%declare`d (pattern-less) terminal accepted

- **Severity:** Low
- **Evidence:** A (live oracle build + `diffcheck` binary; both directions confirmed)
- **Freshness:** fresh root cause
- **Grammar:** `%declare Z\nstart: "a"\n%ignore Z\n`
- **Input:** `a`
- **Options:** `parser=lalr, lexer=basic` (also reproduces lalr/contextual, earley/basic)
- **Python result:** **build fails** — `LexError: Ignore terminals are not defined: {'Z'}`
- **lark-rs result:** builds OK and parses → `start []`
- **Root cause:** `grammar/loader/compiler.rs`. A `%declare`d terminal is pushed into
  `self.terminals` as a pattern-less `TerminalDef::declared(...)`; `IgnoreEntry::Named`
  validation only checks the name is present in `self.terminals` (passing for a declared
  terminal), never that the ignore target has an actual pattern. Python builds the lexer
  and raises a `LexError` because a declared terminal carries no pattern and is absent from
  the lexer's terminal list, making the ignore-set difference non-empty (`lark/lexer.py`).
- **Expected fix contract:** reject-like-Python — reject `IgnoreEntry::Named` when the
  resolved terminal is pattern-less (`declared`). Per ADR-0017 (more permissive than the
  oracle is unfalsifiable).
- **Nearest known issue/root cause:** RC1/RC2/#299 (duplicate *definition* gates);
  the `%ignore <rule>` and bare-undefined-name gates lark-rs already has.
- **Why distinct:** the terminal *is* defined (so the existing `UndefinedTerminal` gate
  passes) but has no pattern — a case Python's distinct lexer-build `LexError` catches,
  which lark-rs has no equivalent check for. A `%declare`d terminal *used in a rule* builds
  in both; the rejection is specific to `%ignore`-ing it.
- **Test:** `h7_1_ignore_of_declared_terminal_rejected`
- **Affected surfaces:** all parser/lexer configs (build-time gate).
- **Unaffected surfaces:** a `%declare`d terminal referenced by a rule (builds in both).

### H7-2 — literal newline inside a `/regex/` or `"string"` literal accepted

- **Severity:** Low
- **Evidence:** A (live oracle build; both surfaces confirmed)
- **Freshness:** fresh root cause (one cause, two surfaces)
- **Grammar (H7-2a):** `start: A\nA: /a<LF>b/` (a literal newline inside the regex source)
- **Grammar (H7-2b):** `start: A\nA: "a<LF>b"` (a literal newline inside the string literal)
- **Input:** `a<LF>b`
- **Options:** `parser=lalr, lexer=contextual`
- **Python result:** **build fails** both — H7-2a: `GrammarError: You can only use newlines
  in regular expressions with the `x` (verbose) flag` (`_literal_to_pattern`,
  `lark/load_grammar.py`); H7-2b: `GrammarError: Unexpected input` at the newline (the
  grammar tokenizer's STRING terminal cannot span a newline).
- **lark-rs result:** builds OK and parses, the embedded newline matched as literal text →
  `start [Token A "a<LF>b"]`.
- **Root cause:** `grammar/loader/tokenizer.rs`. `lex_regexp` scans to the closing `/` and
  `lex_string` to the closing `"` with **no no-embedded-newline guard** — the gate Python
  applies in its grammar lexer / `_literal_to_pattern` is absent on both literal kinds.
- **Expected fix contract:** reject-like-Python (a newline inside a `/…/` literal without
  the `x` flag, or inside a `"…"` literal, is a build error).
- **Nearest known issue/root cause:** none specific; adjacent to the "lark-rs silently
  accepts a grammar Python rejects" meta-theme.
- **Why distinct:** a new specific surface — the loader's *literal tokenizers*, not a
  regex-engine dialect screen in `terminal.rs`. Two literal kinds (regex / string) share
  the one missing gate.
- **Test:** `h7_2a_newline_in_regex_literal_rejected`, `h7_2b_newline_in_string_literal_rejected`
- **Affected surfaces:** build-time, all configs.
- **Unaffected surfaces:** a newline expressed as the escape `\n` (both accept).

### H7-3 — conditional regex `(?(id)yes|no)` build-rejected and mis-categorized

- **Severity:** Low-Medium
- **Evidence:** A (lark-rs rejects a Python-accepted regex) + C (refusal category wrong)
- **Freshness:** fresh root cause
- **Grammar:** `start: A\nA: /(a)(?(1)b|c)/`
- **Input:** `ac`
- **Options:** `parser=lalr, lexer=contextual`
- **Python result:** builds OK; parses `"ac"` → `start [Token A "ac"]`. (On `"ab"` it errors
  at *parse* — Lark's combined-regex wrapper renumbers the group so the conditional sees a
  different group 1 — but the terminal is built and usable.)
- **lark-rs result:** **build fails** — `Invalid regex pattern '(a)(?(1)b|c)': regex parse
  error: ... unrecognized flag (and the lookaround analyzer cannot parse it either; note
  that backtracking-only syntax is not supported — see docs/LOOKAROUND_SCOPE.md)`.
- **Root cause:** `grammar/terminal.rs::PatternRe::new`. The Rust `regex` crate has no
  `(?(…)…)` conditional and reports "unrecognized flag"; the lookaround analyzer also
  can't parse it, so it falls into the generic `InvalidRegex` / "backtracking-only syntax"
  lookaround refusal. There is no dialect screen recognizing a group-existence conditional
  as a build-accepted Python construct.
- **Expected fix contract:** support and match Python (lower the group-existence
  conditional, which is a regular-ish alternation a linear engine could host), or at
  minimum re-categorize the refusal as a dialect gap rather than a backtracking-only /
  lookaround refusal. Per ADR-0017's routing, the contract is "support and match Python."
- **Nearest known issue/root cause:** the backref `\1` refusal row (LOOKAROUND_SCOPE);
  #275/#400/#332 dialect items.
- **Why distinct:** `(?(id)yes|no)` is a separate `re` construct (group-existence
  conditional), not a backreference — the message even mis-attributes it to
  "backtracking-only syntax." Python Lark *accepts* it at build, unlike the backref class,
  and unlike #275/#400/#332 (which reject in *both* engines).
- **Test:** `h7_3_conditional_group_reference_build_accepted`
- **Affected surfaces:** build-time, all configs (terminal compile).
- **Unaffected surfaces:** non-conditional alternations (`(a)(b|c)`) build in both.

### H7-4 — PyO3 `Token` violates the eq/hash invariant against plain `str` (binding)

- **Severity:** Medium-High
- **Evidence:** A (PyO3 built live with `maturin develop`, differential vs `lark==1.3.1`);
  C (source-confirmed in this round: `python/src/lib.rs`).
- **Freshness:** fresh root cause (binding). **Documented, not encoded as a `cargo` XFAIL**
  — it lives on the PyO3 surface, which `cargo test` and the `diffcheck` harness don't
  exercise; consistent with how N7/B1/B2 binding findings were handled.
- **Repro (Python, against the built binding):**
  ```python
  import lark_rs
  p = lark_rs.Lark("start: WORD\nWORD: /[a-z]+/\n", parser="lalr")
  tok = p.parse("hello").children[0]
  assert tok == "hello"        # True
  assert tok in {"hello"}      # FAILS — False
  {"hello": 1}[tok]            # FAILS — KeyError
  ```
- **Python contract:** `lark.Token` subclasses `str`, so `hash(Token('WORD','hello')) ==
  hash('hello')`, `tok in {'hello'}` is `True`, and `{'hello':1}[tok] == 1` — the invariant
  *equal objects hash equal* holds.
- **Binding behavior (executed):** `isinstance(rs_tok, str)` → `False`; `tok == "hello"` →
  `True`; `hash(tok) == hash("hello")` → `False`; `tok in {"hello"}` → `False`;
  `{"hello":1}[tok]` → `KeyError`. (Token-vs-Token membership *does* work — both sides hash
  by value — so the breakage is specifically Token-vs-plain-str.)
- **Root cause:** `python/src/lib.rs`. `PyToken::__eq__` (≈line 146) returns `True` against
  a plain Python string (str-like equality), but `PyToken::__hash__` (≈line 135) hashes the
  value with Rust's `DefaultHasher`, which does not equal CPython's `str.__hash__`. The
  class is not a `str` subclass, so the two methods are mutually inconsistent against `str`.
- **Expected fix contract:** make the PyO3 `Token` a genuine `str` subclass (so `__hash__`/
  `__eq__`/`isinstance` all derive from `str`), or at minimum make `__hash__` agree with
  `str.__hash__` of the value. ADR-flavored (binding-surface API decision) → `needs-decision`.
- **Folded binding-surface gaps (same `python/src/lib.rs` fix site):**
  - **H7-4b** — `Tree` has no `.meta` attribute (`hasattr(t,"meta")` is `False`; Python's
    `Tree` always carries a `Meta`). Orthogonal to H10/#337/H6-5 (those concern meta
    *content*, presupposing it exists); this is the binding-surface *absence*. (WASM/C
    source-traced: their tree shapes omit meta too — C-level.)
  - **H7-4c** — `repr()` uses Rust `{:?}` double quotes (`Token("WORD", "ab")`), not
    Python's single-quote `Token('WORD', 'ab')`.
  - **H7-4d** — `Token(type, value, start_pos=…, line=…, …)` is a `TypeError`; the PyO3
    constructor signature omits Python's optional position kwargs.
- **Nearest known issue/root cause:** B2 (#406, error-hierarchy collapse), B1 (#406, C-API
  `maybe_placeholders` default), N7/#281 (recursion), #338 (PyO3 `g_regex_flags`).
- **Why distinct:** none of those touch the `str`-subclass eq/hash invariant, `Tree.meta`
  presence, `repr` quoting, or the `Token` constructor surface. H7-4 is a *self-broken*
  contract the binding's own code introduces, missed by the existing `test_token_is_str_like`
  (which checks only `==`/`str()`/`len()`).
- **Affected surfaces:** PyO3 (executed). `Tree.meta` absence also affects WASM + C
  (source-traced).
- **Unaffected surfaces:** Token-vs-Token equality/hashing; the core lark-rs API (this is a
  binding-only divergence).

## Variants

### V-H7-1 — standalone runtime lacks `ParseTree::None` (a `None`-root errors)

- **Severity:** Low-Medium
- **Evidence:** A (Python basic + Python `standalone` tool + in-process lark-rs all return
  `None`; the compiled baked standalone parser returns `Err`). Verified at intake: Python
  basic → `None`; in-process lark-rs (`diffcheck`, basic & contextual) → `null`.
- **Grammar:** `?start: [A]\nA: "a"`
- **Input:** `""` (empty) · **Options:** `parser=lalr, maybe_placeholders=true`
- **Python / in-process lark-rs result:** bare `None`. (Present branch `parse("a")` →
  `Token('A','a')` in both.)
- **Standalone (baked) result:** `Err("accept with empty value stack")`.
- **Root cause:** `src/standalone/runtime.rs`. The runtime's private `ParseTree` enum has
  only `Tree`/`Token` — it never grew the `None` variant ADR-0033/#382 added to the public
  API + in-process backends + bindings (whose consumer list omits the standalone runtime).
  The H12/#371 fix made `shape()` collapse a lone placeholder to `Inline([None])`, but when
  that is the *root* result it reaches `run()`'s `Action::Accept` arm and falls into its
  `_ => Err("accept with empty value stack")` fallback.
- **Expected fix contract:** support and match — add a `ParseTree::None` variant to the
  runtime and an `Inline([None]) => Ok(None)` Accept arm, mirroring the public API.
- **Nearest known issue/root cause:** RC9/#289 (in-process lone-`None`) and #382/ADR-0033
  (the public `ParseTree::None` variant + its in-process/binding propagation).
- **Why distinct (why a variant, not fresh):** same lone-`None` root cause, but a genuinely
  unfixed code path — #382 never touched `runtime.rs`, and the H12 pin only exercised a
  `None`-as-*child* (spliced into a parent), never a `None` *root*. The H6 "standalone clean"
  verdict missed it. Reported as a variant of #289/#382 on the standalone surface.
- **Test:** `standalone_none_root_returns_none_like_core` (`src/standalone/mod.rs`).

## Clean buckets

Honest negative evidence — these surfaces came back clean at this baseline (useful, not
proof of correctness):

- **Regex width / terminal ranking (Team 2):** `max_width` inference matched Python
  `sre_parse.getwidth()` on 40+ parseable patterns; within-terminal alternative ordering,
  the NAME (4th) sort key, `%ignore`-vs-content ties, and zero-width rejection all parity.
  The only systematic `max_width=None` gap is confined to patterns lark-rs rejects at build
  anyway (#275/#400) — never reaching a buildable grammar.
- **Cross-backend consistency (Team 6):** ~195 curated + ~7600 generative cases across all
  five legal `(parser,lexer)` combos showed **zero** intra-lark-rs accept/reject or
  tree-shape splits Python doesn't also have; error class/position and `lexer='auto'`
  resolution consistent per-backend with Python. (A strict-mode "split" was an environment
  artifact — `interegular` not installed — not a finding.)
- **Tree-shaping algebra (Team 7):** ~8000 fuzzed grammars over `?`/`_`/`!`/aliases/anon
  helpers/`maybe_placeholders`/`%ignore` and their interactions — **zero** fresh
  divergences; the shaping core matches Python. The lone observed cluster reduces to the
  known #32/#90/#302 dynamic-lexer split-point tie-break (gated behind an `_ambig`).
- **Wild / hostile grammars (Team 9):** ~330 realistic grammars (JSON5/TOML/CSS/GraphQL/
  protobuf/operator-precedence cascades/templates/`%import` closures) built and parsed
  tree-identically to Python; the only divergences reduce to H5-2/#361 (`__`-leading names)
  and RC6/#275 (`\b` DFA leak).
- **Deterministic perf bounds (Team 10):** ~20 parameterized grammar families measured by
  structural counts (rules/states) and work counters (`dense_build_bytes`,
  `completer_scan_steps`, `forest_nodes`) — all in-envelope and matching Python, except the
  known H6-7/#404 duplicate-arm cross-product (reconfirmed still-open). LALR state counts are
  byte-identical to Python's `_parse_table.states`.
- **Standalone (Team 4):** ~35 grammars triangulated Python-basic vs in-process-basic vs the
  real compiled baked parser — all agreed byte-for-byte except V-H7-1 (the `None`-root path).

## Harness caveats

- **`diffcheck` strips `Tree.meta`** and does not wire `propagate_positions`,
  `g_regex_flags`, file/`base_path` imports, `postlex`, or the bindings — those were probed
  directly (Teams 5/8) or are out of this round's executable scope.
- **The PyO3 finding (H7-4) is not re-run by `cargo test`.** It requires building the PyO3
  binding (`maturin develop`); the executable evidence is the strike team's live run, plus
  source confirmation in this round. It is documented, not encoded, and is decision-flavored
  (binding-surface API).
- **Python `%import` cycles** raise `RecursionError` (not a clean `GrammarError`) and the
  harness doesn't wire import paths — not a valid oracle target (a Python deficiency, not a
  falsifiable gate); excluded.
- **One first-pass candidate was overturned at intake** (the static `newline_types`
  line/column hypothesis) — see *Invalid/rejected*. Re-running every repro against the live
  oracle before counting it remains load-bearing.
