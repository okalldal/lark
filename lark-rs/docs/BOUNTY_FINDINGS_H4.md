# lark-rs bug-bounty findings — round 4 (h4)

Round 1 (`BOUNTY_FINDINGS.md`, RC series) harvested the "front door too permissive"
validation gaps plus the RC5 lexer-width bug; round 2 (`_H2.md`, N series) took the
config/position/ranking layer; round 3 (`_H3.md`, H series) took grammar-loader
robustness and the first wave of Python-`re` *regex*-dialect divergences. Round 4
retargeted surfaces those rounds declared clean or never reached: grammar **string-literal**
(not regex) escape decoding, **regex-crate-only** dialect that lark-rs silently accepts,
`%ignore` of a **named** terminal, **error/ParseError parity**, **%import-closure
mangling**, a **nested-optional** collision gate, **named-terminal-vs-literal** rule
unification, a **nullable+recursive Earley** derivation under-count, and a **DFA-build**
determinization blow-up.

Ten retooled strike teams ran against the same harness. After minimization, independent
re-verification (every repro re-run against the live oracle), and dedup against
RC1–RC10 / N1–N10 / V1–V4 / H1–H12 / P1–P2 and the open known-issue set, this catalog
records **12 fresh, confirmed root causes** (three multi-surface) plus **2 variant
clusters** of known root causes.

## Target and method

- **Baseline SHA (frozen):** `a74841ac21d0ab1d115ba5b5d93de814d399ba12`
  (branch `claude/bug-hackathon-dhkyrl`).
- **Oracle:** Python Lark `1.3.1` (the in-repo `lark/`).
- **Harness:** `tools/diffcheck.py` (`compare()`) + the `diffcheck` binary over the same
  (grammar, input, options) tuple, plus direct `lark_rs` API checks for the loader panic,
  the error variants/positions, the import token types, and the deterministic
  `dense_build_bytes` work counter (H4-12). The harness wires
  `parser`/`lexer`/`start`/`ambiguity`/`maybe_placeholders`/`keep_all_tokens`/`strict`;
  `g_regex_flags`, `propagate_positions`, file/`base_path` imports, and `postlex` are not
  wired (no H4 find depends on them).
- **Reproductions:** `tests/test_bounty_findings_h4.rs` — 12 `#[ignore]` (XFAIL) tests
  (H4-1 … H4-12). Run the 11 non-perf XFAILs with
  `cargo test --test test_bounty_findings_h4 -- --ignored`; H4-12 needs the work counters:
  `cargo test --features perf-counters --test test_bounty_findings_h4 -- --ignored`.
- **Eligibility:** none reduce to a round-1/2/3 root cause (RC1–RC10, N1–N10, V1–V4,
  H1–H12, P1–P2) or the open known-issue set: #208 (fuzzer burndown), #275 (`\b`/`\B`/`\Z`
  anchor dialect), #281 (binding recursion), #282 (RC/N burndown epic), #286 (`%extend`
  imported terminal — open fix PR #328), #293 (oracle contradictions), #299 (two
  duplicate-definition surfaces), #302 (Earley adjacent bounded repeats), #304 (standalone
  `\Z`/oversized repeat), and the round-3 burndown epic #329 with #330 (H1), #331 (H2/H3),
  #332 (H5), #333 (H6–H9), #334 (H4), #335 (H11), #336 (H12), #337 (H10), #338 (P1/P2).
  Confirmed against the live tree: **H1/#330 still panics on this baseline** (it is the
  open round-3 XFAIL, not yet fixed), so the explicit-`start=` panic below correctly
  reduces to it (a variant). Where a find is adjacent to a known issue the distinction is
  noted inline.

A recurring theme persists — lark-rs **silently accepts grammars/constructs Python rejects,
or diverges on a Python-`re`/Python-Lark behavior** — but via new mechanisms (string-literal
escape decoding, regex-crate-only dialect, `%ignore`-clone semantics, error classification,
import mangling, forest enumeration, eager determinization) untouched by the prior catalogs.

## Accounting

- **Fresh root causes:** 12 — H4-1 … H4-12.
- **Multi-surface findings (one root, ≥2 surfaces):** H4-2 (`\p{}`/`\x{}`/`\z`), H4-3
  (priority-drop + filter-leak of `%ignore NAME`), H4-9 (LALR reduce/reduce + Earley
  phantom derivation).
- **Variants of known causes (reported, not re-counted):**
  - **V-H9** — regex `(?a:…)` scoped-ASCII flag, `\N{NAME}`, and `a{}` literal braces are
    Python-accepted constructs that lark-rs refuses with a **mis-labelled** "lookaround /
    backtracking-only" diagnostic. Same root cause as **H9/#333** (the
    `route_fancy_only_terminal` catch-all over-claiming `BacktrackingOnlySyntax` for
    constructs the `regex` crate rejects for other reasons), new surfaces.
  - **V-H1** — an explicit `start="NOPE"` option naming an **undefined** rule (on an
    otherwise-valid grammar), and `start=` naming a **terminal**, hit the same
    `intern.rs:376` `.expect("start symbol interned")` as **H1/#330** (the missing
    start-validation gate): the undefined name **panics**, the terminal name is **silently
    accepted**. Same root cause/fix site as H1, new surfaces.
- **Known duplicates:** 0.
- **Provisional / source-only:** 0 (every H4 find is executable; H4-12 is counter-backed B).
- **Invalid / rejected (re-verified to agreement — discarded):** scoped flags
  `(?i:)`/`(?-i:)`/`(?s:)`/`(?u:)`, named groups `(?P<>)`, `\A` anchor, repetition bounds
  `{,3}`/`{3,}`/`{,}`/`{0}`, empty alternations, conditional `(?(1)…)` (both reject),
  global `(?a)` (both reject), `\d`/`\w` Unicode; the full template machinery (arity,
  nesting, recursion, caching, arg kinds); `%ignore` *not* rule-referenced; ~900
  maybe_placeholders/repetition/keep_all_tokens tree-shape cells; CYK across nullable/unit/
  ambiguous/priority; Earley `resolve` picks; non-EOF error positions (multibyte/tab/CR/
  newline). These were quick-pass agent claims or probes that did not survive re-check.

## Severity summary

| ID    | Severity | Fresh? | Evidence | Bucket           | One-line |
|-------|----------|--------|----------|------------------|----------|
| H4-2  | High     | fresh  | A        | lexer (dialect)  | Regex-crate-only `\p{}` / `\x{}` / `\z` silently accepted; Python rejects at build |
| H4-5  | High     | fresh  | A        | loader (imports) | `%import`-closure mangles a sibling that is independently imported → wrong token type |
| H4-6  | High     | fresh  | A        | error parity     | Contextual lexer reports `UnexpectedToken` for an unlexable char; Python/basic say `UnexpectedCharacters` |
| H4-9  | High     | fresh  | A        | lalr / earley    | Equal named-terminal-vs-literal alternation → spurious LALR reduce/reduce (Python accepts) + Earley phantom derivation |
| H4-10 | High     | fresh  | A        | earley           | Nullable+recursive grammar: Earley under-reports distinct derivations (6 vs Python's 8) — ADR-0017 forbidden direction |
| H4-12 | High     | fresh  | B        | perf (lexer)     | DFA backend eagerly determinizes a counted-repeat terminal to `2^N` states (unbounded); Python `re` is linear |
| H4-1  | Medium   | fresh  | A        | grammar-loader   | String-literal escapes `\v` / `\0` / `\'` over-decoded vs Python `eval_escaping` |
| H4-3  | Medium   | fresh  | A        | lexer / loader   | `%ignore NAME` mints a priority-0 `__IGNORE_n` clone → drops declared priority + fails to filter a rule-referenced terminal |
| H4-7  | Medium   | fresh  | A        | error parity     | EOF error position is the end cursor, not the last token's start (Python `new_borrow_pos`) |
| H4-8  | Medium   | fresh  | A        | ebnf-loader      | Nested optional-of-optional `([A]?) B` silently accepted; Python rejects "Rules defined twice" |
| H4-4  | Low      | fresh  | A        | loader (priority)| Terminal/rule priority clamped to `i32` ties two distinct `>i32::MAX` priorities |
| H4-11 | Low      | fresh  | A        | grammar-loader   | `%declare` of a lowercase (rule-cased) name accepted; Python rejects (terminal-case convention) |

---

## Findings

### H4-1 — Grammar string-literal escapes over-decoded (Medium, grammar-loader)
- **Grammar:** `start: "\v"` (also `"\0"`, `"\'"`) · **Input:** the 2-byte literal `\v`
  (backslash + `v`) · **Options:** default (engine-independent — a loader bug).
- **Python:** `eval_escaping` (`lark/load_grammar.py`) decodes only `\\ \U \u \x \n \f \t \r`;
  every other escape keeps a literal backslash. So `"\v"` is the 2-char string
  backslash+`v` → accepts the 2-byte input `\v`, rejects a bare vertical tab.
- **lark-rs:** `unescape_string` (`src/grammar/loader/tokenizer.rs`) additionally decodes
  `\v`→U+000B, `\0`→NUL, `\'`→`'`, so the `PatternStr` value diverges — it **rejects** `\v`
  and **accepts** a bare vertical tab (bidirectional).
- **Root cause:** the three single-char arms in `unescape_string` should fall through to the
  "unknown escape, keep backslash" arm, matching `eval_escaping`'s `Uuxnftr` set.
- **Expected fix contract:** reject-like-Python (value-level: leave `\v`/`\0`/`\'` literal).
- **Nearest known:** RC5/N2/N3/H5–H9 all concern *regex* `/…/` escapes/widths; this is the
  *string* `"…"` decode path (`tokenizer.rs`), distinct code.
- **Test:** `h4_1_string_literal_escape_overdecoded`.
- **Affected surfaces:** every engine (loader). **Unaffected:** `\x41`/`\u`/`\U`,
  multibyte literals, `\\`/`\"`/`\/`, `\f`/`\t`/`\n`/`\r`, `\a`/`\b`/`\d`/`\w`, multi-digit
  octal `\101` (all agree).

### H4-2 — Regex-crate-only dialect silently accepted (High, lexer dialect)
- **Grammar:** `T: /\p{L}+/` (also `\pL`, `\P{L}`, `\x{41}`, `abc\z`) · **Input:** any match.
- **Python:** rejects at build — `LexError`/`GrammarError: Cannot compile token` (Python `re`
  has no `\p`/`\P` property escapes, no braced `\x{…}`, and uses `\Z` not `\z`).
- **lark-rs:** accepts and parses (the Rust `regex` crate supports all three natively).
- **Root cause:** terminal regexes are delegated to `regex` without screening dialect-only
  syntax Python `re` lacks. Per ADR-0017's corollary, being more permissive than the oracle
  is unfalsifiable → a bug.
- **Expected fix contract:** reject-like-Python (a categorized `InvalidRegex` for `\p`/`\P`/
  `\x{…}`/`\z`).
- **Nearest known:** H5/#332 (char-class POSIX/set-op — *inside* `[]`), H6–H9/#333
  (quantifier/octal/comment), #275 (`\b`/`\B`/`\Z` — which Python *accepts*/parks). This is a
  fresh axis: bare top-level escapes/anchors the `regex` crate accepts and Python rejects.
- **Test:** `h4_2_regex_crate_only_dialect_rejected`.
- **Affected:** every engine. **Unaffected:** scoped flags, named groups, `\A`, `{,n}`
  bounds, empty alternations (all agree).

### H4-3 — `%ignore NAME` mints a duplicate terminal (Medium, lexer / loader)
- **Grammar (a, priority):** `start: A+` / `A: /[a-z]/` / `SKIP.5: /[a-z]/` / `%ignore SKIP`
  · **Input:** `ab`. **(b, filter):** `start: item+` / `item: "a" | WS` / `WS: /\s+/` /
  `%ignore WS` · **Input:** `a a`.
- **Python:** `%ignore NAME` adds the existing terminal's name to `lexer_conf.ignore`
  (`lark/lexer.py`). (a) `SKIP.5` outranks `A`, matches each char, is ignored → nothing for
  `A` → **rejects `ab`**. (b) every `WS` is dropped globally → `start(item(), item())`.
- **lark-rs:** `compiler.rs` mints a **fresh** `__IGNORE_n` clone at hardcoded priority 0
  (`expansion_to_pattern`) and never marks the named terminal ignored. (a) the clone loses to
  `A`, so lark-rs **accepts `ab`**. (b) the rule-referenced `WS` survives un-ignored →
  `start(item(), item(WS=' '), item())` (extra child).
- **Root cause:** one bug, two surfaces. `%ignore NAME` should add `NAME`'s id (with its
  priority) to the ignore set; only inline patterns should synthesize a terminal.
- **Decisive control (both agree):** inline `%ignore /\s+/` mints a fresh terminal in *both*
  engines, so only the *named* form diverges — isolating the bug.
- **Expected fix contract:** support-and-match (mark the named terminal ignored, preserve
  priority).
- **Nearest known:** RC5 (`%ignore` width), H11/#335 (dynamic scan cost), N5 (illegal
  pairing) — different code paths; this is the `__IGNORE_n` duplication in the loader.
- **Test:** `h4_3_ignore_named_terminal_priority_and_filter`.

### H4-4 — Priority clamped to `i32` ties distinct large priorities (Low, loader)
- **Grammar:** `start: A | B` / `A.5000000000: "x"` / `B.9000000000: "x"` · **Input:** `x`.
- **Python:** picks `B` (9e9 > 5e9).
- **lark-rs:** both priorities saturate to `i32::MAX` (`tokenizer.rs` clamps the parsed
  `i128`), tie, and `A` wins by name order. Control: both ≤ `i32::MAX` agree.
- **Root cause:** `i32` priority storage vs Python's arbitrary-precision `int`.
- **Expected fix contract:** support-and-match (store priorities wide enough not to collide,
  or reject out-of-range) — narrow (needs > 2.1e9 priorities), reported for completeness.
- **Nearest known:** the documented saturation, but its *behavioral* consequence
  (tie → wrong pick / spurious LALR reduce/reduce) is unrecorded.
- **Test:** `h4_4_priority_i32_saturation_tie`.

### H4-5 — `%import`-closure mangles an independently-imported sibling (High, imports)
- **Grammar:** `start: pattern` / `%import python (pattern, NAME)` / `%ignore " "` ·
  **Input:** `x`.
- **Python:** `start(python__capture_pattern(Token(NAME, 'x')))` — `_get_mangle`'s aliases
  dict (merged across every `%import` of the path) leaves a closure reference **unmangled**
  when its symbol is itself imported.
- **lark-rs:** `start(python__capture_pattern(Token(python__NAME, 'x')))` — `import_rule_closure`
  (`imports.rs`) exempts only the single requested name and prefix-mangles every other
  closure symbol → **wrong token type**, silently (never errors).
- **Root cause:** no per-module alias map merged across import directives; the closure rename
  consults only the requested name.
- **Manifestations (one root):** terminal dep (`NAME`), rule dep (`string`), aliased sibling
  (`-> QUX`), and the same via a template arg or multiple directives.
- **Expected fix contract:** support-and-match (build the per-path alias map like
  `_get_mangle`; consult it for every closure symbol, rules and terminals).
- **Nearest known:** #286/#299 (`%extend` / import-vs-import collision), RC2 (duplicate
  definition) — distinct mechanisms.
- **Test:** `h4_5_import_closure_mangle_exemption`.

### H4-6 — Contextual lexer mis-classes an unlexable char (High, error parity)
- **Grammar:** `start: "a" "b"` · **Input:** `ax` · **Options:** lalr + **contextual** (the
  default LALR lexer).
- **Python:** `UnexpectedCharacters` (line 1, col 2; `x` matches no terminal).
- **lark-rs:** `UnexpectedToken "x"`. The **basic** lexer and the **recovering** contextual
  path are both correct; only the non-recovering contextual driver diverges.
- **Root cause:** `lalr.rs::lex_failure` turns every `SourceError::Lex` into
  `UnexpectedToken`; the recovering path (same `LexFailure`) correctly builds
  `UnexpectedCharacter`.
- **Expected fix contract:** support-and-match (build `UnexpectedCharacter`, mirroring the
  recovering path; this also fixes the downstream `allowed`-set including `$END`).
- **Nearest known:** N8/#307 (token positions, fixed) — distinct (error *classification*).
- **Test:** `h4_6_contextual_unlexable_char_is_unexpected_character`.

### H4-7 — EOF error reports the end cursor, not the last token's start (Medium, error parity)
- **Grammar:** `start: "a" "b"` · **Input:** `a`.
- **Python:** error at column 1 — `Token.new_borrow_pos` copies the last real token's start
  (`a` at col 1); `(1,1,0)` when there were none.
- **lark-rs:** column 2 — the `$END` token is built at the live lexer cursor
  (`token_source.rs`), past the consumed input (and past trailing ignored content).
- **Root cause:** EOF-token construction position.
- **Expected fix contract:** support-and-match (borrow the last token's start position).
- **Sibling (needs-decision, same EOF cluster):** lark-rs raises `UnexpectedEof` at EOF where
  Python's **LALR** raises `UnexpectedToken($END)` (Python's Earley uses `UnexpectedEOF` with
  `-1` sentinel positions). Whether to match Python's per-backend split or keep the unified
  `UnexpectedEof` is an API-shape decision — escalate/ADR. The XFAIL pins only the falsifiable
  *position*.
- **Nearest known:** H10/#337 (`Tree.meta`) — unrelated.
- **Test:** `h4_7_eof_error_borrows_last_token_position`.

### H4-8 — Nested optional-of-optional collision not detected (Medium, ebnf-loader)
- **Grammar:** `start: ([A]?) B` (also `[[A]?] B`, `[[[A]?]?] B`) / `A: "a"` / `B: "b"` ·
  **Input:** any (build-time).
- **Python:** build error — `Rules defined twice: <start : B> / <start : B>
  (… colliding expansion of optionals: [] or ?)`, every backend.
- **lark-rs:** builds and parses. The single term's two arms both reduce to the same
  `(syms=[B], gaps=[0,0])` `CompiledAlt`, so `dedup_and_check_alts` (`compiler.rs`) merges
  them at its stage-1 `seen.insert` **before** the stage-2 `seen_syms` collision check sees
  the duplicate.
- **Root cause:** `CompiledAlt` discards the `_EMPTY`-marker provenance Python keeps through
  dedup; the #252/#259 fix only covers *sibling* collisions (`[A] [A]`), not this single-term
  self-collision.
- **Expected fix contract:** reject-like-Python (preserve enough provenance to collide at
  stage 2).
- **Nearest known:** RC3/#252/#259 (sibling optionals), #289/RC9 (lone-None expand1 parse
  divergence) — distinct.
- **Test:** `h4_8_nested_optional_of_optional_collision_rejected`.
- *Out-of-scope note (filed separately, not an H4 find):* at this baseline the RC3 guard
  `test_literal_optional_pair_collides`-style XFAIL `rc3_*` appears stale — lark-rs now
  correctly rejects `[A] [A] "c"` (the #259 fix is present). Flagged for the RC burndown.

### H4-9 — Equal named-terminal-vs-literal alternation duplicates an arm (High, lalr/earley)
- **Grammar:** `start: A | "a"` / `A: "a"` · **Input:** `a`.
- **Python:** unifies the literal onto `A` and collapses to a single `<start : A>` → LALR
  accepts and parses `start(A='a')`; Earley `explicit` yields the single tree.
- **lark-rs:** keeps **two** `CompiledRule`s differing only in `filter_pos`
  (`terminals.rs`/`intern.rs`) → a spurious **LALR reduce/reduce build rejection** (a
  Python-valid grammar refused) and, under Earley `explicit`, a **phantom** extra empty
  `start()` derivation (`_ambig`). With `keep_all_tokens` the divergence vanishes (both alts
  keep the token → byte-identical → correctly deduped), confirming the `filter_pos` root.
- **Root cause:** anon-terminal unification leaves a duplicate alternative Python collapses.
- **Expected fix contract:** support-and-match (dedup alternatives lowering to byte-identical
  expansions, preferring the kept-token occurrence).
- **Nearest known:** RC7/#272 (recurse-helper over-share reduce/reduce), #159 (byte-identical
  `_ambig` dedup) — distinct.
- **Test:** `h4_9_terminal_vs_literal_alternation`.

### H4-10 — Nullable+recursive Earley under-reports derivations (High, earley)
- **Grammar:** `start: z` / `z: | "b" z | z z` · **Input:** `bbb` · **Options:**
  `parser=earley`, `ambiguity=explicit`.
- **Python:** 8 distinct disambiguated derivations (and 2/48/352 on `bb`/`bbbb`/`bbbbb`).
- **lark-rs:** 6 — a strict **subset** (deficit 2→26→262 across `bbb`/`bbbb`/`bbbbb`).
- **Root cause:** the SPPF forest→tree enumeration (`earley.rs`, `node_value_key` dedup +
  lazy Leo spine reconstruction) over-merges or fails to enumerate distinct sub-derivations
  for a nullable + direct-binary-recursive rule — the **forbidden** direction of ADR-0017
  (structurally-distinct trees lost, not byte-identical duplicates collapsed). Reproduces on
  basic + dynamic and with `keep_all_tokens` (isolating it to the forest, not the lexer).
- **Expected fix contract:** support-and-match (enumerate every distinct derivation; dedup may
  only ever collapse byte-identical trees).
- **Nearest known:** #159/ADR-0017 keeps byte-identical dedup; the guard tests
  `explicit_keeps_structurally_distinct_ambig_alternatives` /
  `node_value_key_separates_distinct_collapses_identical` assert this cannot happen — this
  grammar shape evades them. Adjacent to the #208 fuzzer epic but minimized to a specific root.
- **Test:** `h4_10_nullable_recursive_earley_enumerates_all_derivations`.

### H4-11 — `%declare` of a lowercase name accepted (Low, grammar-loader)
- **Grammar:** `%declare foo` / `start: "a"` · **Input:** `a`.
- **Python:** rejects at build (a declared symbol must be an UPPERCASE terminal;
  `%declare FOO` builds fine).
- **lark-rs:** accepts and parses.
- **Root cause:** no terminal-case-convention gate on `%declare` targets.
- **Expected fix contract:** reject-like-Python. **Oracle caveat:** Python's rejection
  surfaces as an internal `AttributeError`, not a clean `GrammarError`, so only the
  accept/reject verdict is asserted (per ADR-0017's "more permissive → match the rejection").
- **Nearest known:** N1 (`%override`/`%extend`), `%declare` of an already-defined terminal —
  distinct (case-convention gate).
- **Test:** `h4_11_declare_lowercase_name_rejected`.

### H4-12 — DFA backend eagerly determinizes a counted-repeat terminal (High, perf)
- **Grammar:** `start: T` / `T: /[01]*1[01]{N}/` (the classic `.*a.{N}` family) · **Input:**
  `"1"`×(N+6) · **Options:** default DFA lexer backend (`LexerType::Basic`/`Contextual`).
- **Python:** linear build — Python `re` compiles to a lazy/backtracking NFA, no
  determinization (flat ~0.011 s even at N=200).
- **lark-rs:** `build_combined_dfa` (`src/lexer/dfa.rs`) builds a Thompson NFA then **eagerly,
  fully determinizes** it with `dense::Builder::new()` under **no** `dfa_size_limit`, blowing
  the determinizer to `2^(N+1)` states and hanging unbounded — no graceful error.
- **Deterministic metric (`dense_build_bytes`, the determinized heap size):** N=4 → 5184 B,
  N=10 → 311616 B (≈60× — doubling per +1 in N); the work counter, not wall-clock, is the
  binding evidence. Both engines **accept** the inputs that do build (a resource pathology,
  not a behavioral divergence).
- **Scope:** only the default DFA backend. The `regex` scanner backend (`scanner.rs`,
  lazy/hybrid DFA with size limits) and the Earley dynamic lexer (per-terminal `regex`
  matching) are both linear — verified.
- **Root cause:** no determinization size bound on the `dense::Builder`.
- **Expected fix contract:** support-and-match while bounding cost — fall back to the
  lazy/hybrid DFA for over-budget terminals so `dense_build_bytes` stays ~flat per source.
  **Alternative (needs-decision):** a `dfa_size_limit` → categorized build-time
  `GrammarError` refusal (the existing `test_lexer_dfa_build_scaling` gate's philosophy). The
  XFAIL pins sub-exponential `dense_build_bytes` growth; either fix flips it (the
  hybrid-fallback directly; the refusal via the test's `Ok(...)?` early-out on a bounded N).
- **Nearest known:** #335/H11 (dynamic-lexer per-position *scan* O(n²)) and
  `test_lexer_dfa_build_scaling` (sweeps only lowered lookaround, never a user counted-repeat
  terminal) — distinct. N9/#279 is *grammar-size* blowup, not terminal-DFA build.
- **Evidence:** B (deterministic counter; wall-clock corroboration is provisional/non-binding).
- **Test:** `h4_12_dense_dfa_build_is_subexponential` (needs `--features perf-counters`).

---

## Variants (reported, not re-counted)

### V-H9 — regex `(?a:…)` / `\N{NAME}` / `a{}` mis-labelled as lookaround
`T: /(?a:\d)/` (scoped ASCII flag), `/\N{LATIN SMALL LETTER A}/`, and `/a{}/` (literal
braces) are **accepted by Python** but refused by lark-rs with a misleading "lookaround …
backtracking-only syntax" message — the `regex` crate rejects each for an unrelated reason
(unknown flag / unknown escape / malformed quantifier) and the `route_fancy_only_terminal`
catch-all over-claims `BacktrackingOnlySyntax`. Same root cause as **H9/#333**; new surfaces.
Fix contract: support-and-match or, at minimum, a correct category — never the bogus
"lookaround" label.

### V-H1 — explicit `start=` naming an undefined rule (panic) or a terminal (silent accept)
With an otherwise-valid grammar (`start: "a"` / `foo: "b"`), an explicit `start="NOPE"` option
**panics** at `intern.rs:376` `.expect("start symbol interned")`, and `start="A"` naming a
*terminal* is **silently accepted** (the terminal interns in the id range, so the `.expect`
succeeds and the parser runs). Python rejects both: `GrammarError: Using an undefined rule:
NonTerminal('NOPE'|'A')`. Same missing start-validation gate / panic site as **H1/#330**; the
fix must validate that every requested start resolves to a *defined non-terminal rule*.

---

## Clean buckets (honest negatives)

- **Templates (Team 4):** arity 0/recursion/nesting/caching/arg-kinds all match Python; the
  template machinery is innocent (H4-5 only *surfaces* through it).
- **Tree-shaping / maybe_placeholders / repetition (Team 6, ~900 cells):** every
  accept-on-both grammar is byte-identical — `[A][B]`, `[A B]`, `[[A]]`, `[A]?/+/*`, `A~0`,
  `(A B)~2`, `[A]~n` distribution, group flattening, `keep_all_tokens`, expand1 — only the
  nested-optional *build gate* (H4-8) and the listed-known #289 lone-None diverge.
- **CYK (Team 8):** no divergences across nullable/unit/ambiguous/priority/keep_all_tokens or
  vs LALR/Earley on the same unambiguous grammars.
- **Earley `resolve` (Team 8):** priority picks match Python; the large class of `_ambig`
  diffs was the intentional byte-identical dedup (#159), correctly excluded. Only H4-9
  (phantom) and H4-10 (under-count) survived a recursive byte-identical filter on both sides.
- **Loader robustness (Team 9):** cyclic terminals, 50-deep groups, 200-way alternation,
  template recursion, comments, CRLF, `%override`/`%extend` undefined — all agree (or share
  the H1 panic site). Only V-H1 and H4-11 are fresh gaps.
- **Error positions (Team 5):** non-EOF error line/column are correct (char-based, multibyte/
  tab/CR/newline); the EOF cluster (H4-6 type, H4-7 position) is the only error divergence; no
  parse-time panic found on adversarial input.
- **Priority (Team 3):** terminal-priority-beats-length, negative priority, string-literal
  auto-priority, rule priority (Earley + LALR resolution), associativity, ForestSum across
  depths, contextual per-state — all faithful. Only the `%ignore`-clone priority drop (H4-3)
  and the `i32` saturation tie (H4-4) diverge.
- **Repeat factoring / DFA-build (Team 10):** EBNF `~n`/`~n..m`/grouped/nested factor
  logarithmically (the #279 port is faithful); literal `"a"~N` is a linear DFA chain; template
  fan-out memoizes. The only resource find is the ambiguous-prefix counted-repeat
  determinization (H4-12).

## Harness caveats

- When **both** engines reject, `compare()` reports agreement even if the *stage* differs
  (build vs parse) — the build-time finds (H4-2, H4-8, H4-11) are asserted via `Lark::new(..)
  .is_err()`/`.is_ok()` directly, and an input the lenient side *accepts* was chosen where
  the find is an over-acceptance.
- Python emits `FutureWarning` for some deprecated-but-accepted char-class constructs — not
  relevant to the H4 finds (those are bare escapes/anchors, hard rejects on the Python side).
- H4-12 reads `perf::dense_build_bytes`, a no-op without `--features perf-counters`; the test
  is `#[cfg(feature = "perf-counters")]` so it never false-greens with the feature off.
- H4-11's oracle is a Python `AttributeError` (an internal crash, not a designed
  `GrammarError`), so only the accept/reject verdict is falsifiable — the message is not pinned.
