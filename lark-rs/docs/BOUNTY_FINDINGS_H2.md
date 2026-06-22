# lark-rs Bug-Bounty Findings — Round 2 (Phase 2)

Round 1 (`docs/BOUNTY_FINDINGS.md`, PR #263) harvested the "front door too
permissive" layer (missing build-time validation gates + the RC5 lexer width
bug). Round 2 was retargeted at the **subtler classes**: valid grammar → wrong
token/parse, configuration/backend validation drift, distribution & binding
divergence, regex-dialect taxonomy, and deterministic resource growth.

Ten retooled strike teams ran against the same harness. After minimization,
independent re-verification, and dedup against RC1–RC10, this catalog records
**9 fresh harness/API-confirmed root causes** + **1 provisional (source+empirical)
root cause (N7)** + **4 variants** of round-1 causes.

**Accounting note.** N1 is **one root cause** (`%override`/`%extend` directive
modifiers dropped at the parser layer) with **three observable surfaces**
(N1a/N1b/N1c) — not three root causes. The nine confirmed root causes are: N1,
N2, N3, N4 (after its taxonomy fix below), N5, N6, N8, N9, N10. **N7** (binding
deep-tree recursion) is real at the source level and demonstrated empirically, but
the binding toolchains were unavailable, so it is **provisional** and **not
encoded as an executable XFAIL** — see its entry.

## Target & method

- **Target:** the round-1 branch tip (this PR is stacked on PR #263, itself on
  `master` @ `a005423`). RC1–RC10 are KNOWN and were declared ineligible for
  round 2; no fresh find reduces to one.
- **Oracle:** Python Lark `1.3.1`. **Harness:** `tools/diffcheck.py` +
  `diffcheck` binary, plus direct `lark_rs` API checks for positions, config,
  perf, and standalone (the harness doesn't cover `propagate_positions`,
  `ambiguity=`-on-LALR, or grammar size).
- **Reproductions:** `tests/test_bounty_findings_h2.rs` — 11 `#[ignore]` (XFAIL)
  tests (N7 is binding-build-only, documented but not encoded). Run:
  `cargo test --test test_bounty_findings_h2 -- --ignored` → all 11 go red.

## Severity summary

| ID  | Sev      | Bucket                 | One-line |
|-----|----------|------------------------|----------|
| N1a | Critical | grammar-loader         | `%override` **merges** instead of replacing → accepts input Python rejects (valid grammar) |
| N1b | High     | grammar-loader         | `%override` of a non-existent rule/terminal not rejected |
| N1c | High     | grammar-loader         | `%extend` of a non-existent rule/terminal not rejected |
| N2  | High     | lexer                  | Flagged regex terminal (`/aa/i`) mis-ranked: tiebreak uses wrapped `(?i:aa)` length |
| N3  | High     | lexer (regex dialect)  | Global inline flag `(?i)` accepted; Python rejects ("global flags not at start") |
| N5  | High     | config validation      | Illegal parser/lexer pairing (e.g. `lalr`+`dynamic`) silently accepted |
| N6  | High     | config validation      | `ambiguity=` on `parser=lalr` silently ignored, not rejected |
| N7  | High *(provisional)* | bindings (distribution)| C API + PyO3 materialize the tree with unbounded recursion → stack overflow on deep trees (source+empirical; not encoded) |
| N4  | Medium   | lexer (refusal taxonomy)| Named backref `(?P=name)` leaks an uncategorized regex error |
| N8  | Medium   | core (positions)       | `start_pos`/`end_pos` are byte offsets, not char indices (non-ASCII) |
| N9  | Medium   | perf (grammar size)    | `x~n..m` (≥50) lowers to O(n²) grammar size vs Python's O(log n) |
| N10 | Medium   | lexer (taxonomy)       | `\Z` anchor rejected & mis-categorized as lookaround/backtracking |

Variants of round-1 causes (reported, lower tier): **V1** standalone bakes `\Z`
→ compiled panic (extends RC10); **V2** standalone bakes oversized `{n}` repeat →
compiled panic (extends RC10); **V3** RC9 through template instantiation (multiple
un-collapsed wrappers); **V4** template name colliding with a plain rule (extends
RC1).

---

## Findings

### N1 — `%override` / `%extend` directives silently dropped (grammar-loader)
The parser consumes the directive modifier and *"treat[s] same as normal for
now"* (`src/grammar/loader/parser.rs`), so the directive never reaches the
compiler and bodies merge like a plain duplicate. Distinct from RC1/RC2 (plain
duplicate merges, no directive).
- **N1a (Critical):** `start: A` + `%override start: B` (`A:"a"`, `B:"b"`). Python
  → grammar is `start: B`, rejects `"a"`. lark-rs → `start: A | B`, **accepts
  `"a"`**. A valid-grammar parse divergence on the default path; reproduces on
  lalr/contextual, lalr/basic, earley/basic.
- **N1b (High):** `%override foo: A` with no prior `foo`. Python:
  `Cannot override a nonexisting rule`. lark-rs builds & parses.
- **N1c (High):** `%extend foo: A` with no prior `foo`. Python:
  `Can't extend rule foo as it wasn't defined before`. lark-rs builds & parses.

### N2 — Flagged regex terminal mis-ranked (lexer)
`start: A | B` / `A: /aa/` / `B: /aa/i` on `"aa"` → Python emits `A`, lark-rs emits
`B`. Python sorts on `len(pattern.value)` (raw source, flags stored separately);
lark-rs bakes the flag into the regex string (`terminals.rs` → `(?i:aa)`) and the
tiebreak (`lexer/plan.rs`) compares the **wrapped** length (7) vs the raw (2),
giving flagged terminals a phantom `4 + len(flag_letters)` rank boost that subverts
the name-asc tiebreak. Distinct from RC5 (max_width): both widths tie here. The
crossover length grows by one char per added flag letter — decisive proof it's the
wrapper, not width.

### N3 — Global inline regex flag accepted (lexer / regex dialect)
`A: /(?i)abc/` on `"ABC"` → Python rejects every terminal with a *global* inline
flag (`global flags not at the start of the expression`, because Lark wraps the
source and demotes the flag off position 0); lark-rs strips the wrapper into a flag
bitset and accepts + applies it. Scoped `(?i:…)` is fine on both — only the global
form diverges. A new more-permissive validation family.

### N4 — Named backreference mis-categorized (lexer / taxonomy)
`A: /(?P<x>a)(?P=x)/`. **Expected behavior is a *categorized refusal*, not
support:** general backreferences are a documented OUT-OF-SCOPE non-goal
(`docs/LOOKAROUND_SCOPE.md`), and a numeric backref `\1` correctly yields the
categorized `GrammarError::LookaroundScope` message *"not supported (by design) …
a backreference …"*. But the classifier's `has_backref` covers `\1`/`\k`/`\g` and
misses the `(?P=name)` spelling, so it slips past classification and lark-rs leaks
a raw `Invalid regex pattern … regex parse error` instead. The XFAIL
(`n4_named_backref_categorized`) asserts the *categorized* refusal (parity with
`\1`), not support — Python's acceptance is irrelevant here because lark-rs
deliberately diverges on general backrefs. Distinct from RC6 (`\b`, different
construct).

### N5 — Parser/lexer pairing legality not enforced (config validation)
`start:"a"` with `lalr`+`dynamic` (also `cyk`+`contextual`, `earley`+`contextual`,
…) → Python raises `ConfigurationError`; lark-rs silently substitutes a working
lexer and parses. The only pairing gate in the tree is the postlex+dynamic refusal
(`src/parsers/mod.rs`). Distinct from RC8 (zero-width *content* on dynamic).

### N6 — `ambiguity=` on LALR not rejected (config validation)
`ambiguity=explicit` with `parser=lalr` → Python:
`'lalr' doesn't support disambiguation`. lark-rs's `build_lalr` never reads
`options.ambiguity`, so it silently builds & parses. Per-backend-scoped rule that
lark-rs omits for LALR.

### N7 — Deep-tree recursive materialization in bindings (distribution) — PROVISIONAL
**Verification: source + empirical (binding toolchains absent; NOT compiled, and
NOT encoded as an executable XFAIL).** Severity High *if* reproduced in an actual
binding test; treat as **provisional** until a binding/recursive-materialization
repro lands. It is listed here as evidence, not as a confirmed, test-backed find.
The C API `node_from_tree` and PyO3 `from_tree`/`pretty_into` walk the output tree
with one unbounded native frame per level, contradicting the engine's own
stack-safety invariant (#33/#151) — which the WASM serializer deliberately honors
with an explicit work-stack and a committed N=100,000 test. Empirically: core parse
+ iterative `Drop` survives N=120,000 nesting on an 8 MB stack, but an
identical-shape recursive walk over that same tree stack-overflows at N=120,000
(8 MB) / N≈10–15k (1 MB WASM-class stack); the real binding frames also allocate
`Box`/`Vec`/`CString`/`PyObject` per node, so they overflow even shallower.
Files: `lark_h/src/lib.rs`, `python/src/lib.rs`, vs `wasm/src/lib.rs`.

### N8 — Positions are byte offsets, not char indices (core)
`A: /h.llo/` on `"héllo"` with `propagate_positions=true` → lark-rs `end_pos=6`
(bytes), Python `5` (chars). `column`/`end_column` are char-based in both and *do*
match — only `*_pos` diverge. Core-rooted (`LexCursor` advances `pos` by byte
length, `src/lexer/mod.rs`), copied verbatim into the PyO3/WASM/C bindings under a
Python-compatible API.

### N9 — `x~n..m` O(n²) grammar size (perf)
`start: "x"~0..n` total RHS symbols: lark-rs grows ≈ n(n+1)/2 (N=100→5051,
N=400→80201) while Python factors to O(log n) (rule counts 19 → 24).
`compile_repeat`'s range branch emits one rule per count `k` with `k` copies;
`inline_repeat` mirrors the `<50` threshold but `compile_repeat` never implements
Python's `small_factors`/`_add_repeat_rule` factoring. Both build correct parsers —
purely a build/size cost. Deterministic (RHS-symbol count, no wall-clock).
- **Ruled out (shared with Python, ineligible):** `(a|b)~1..n` → 2ⁿ rules is
  byte-identical between the engines (Python cartesian-products inline groups the
  same way). Not a lark-rs pathology.

### N10 — `\Z` rejected & mis-categorized as lookaround (lexer / taxonomy)
`A: /x\Z/` on `"x"` → Python accepts and tokenizes; lark-rs build-errors,
categorizing `\Z` as `LookaroundScope` "backtracking-only" — but `\Z` is a plain
end-of-string anchor, neither lookaround nor backtracking. The same
mis-categorization hits oversized bounded repeats (`[a-z]{200000}`, see V2).
Distinct from RC6 (uncategorized leak) — here the category is *wrong*.

---

## Buckets that came back clean (honest negatives)

- **Tree-shaping (generative fuzz, ~1,100 grammars × 2 seeds):** every tree-shape
  divergence reduced to **RC9** (the lone-placeholder-`None` expand1 case). One
  template variant (V3); no new root cause. Multi-child expand1, transparent
  splicing, alias placement, keep_all_tokens, and placeholder positions in
  repeats all match Python.
- **Transformer / semantic output:** lark-rs has **no user-facing transformer or
  semantic-output API yet** (deferred to #227/#231), so there is nothing to
  diverge from. The tree substrate a future transformer will read is byte-identical
  to Python across all probed shapes.
- **Wild / realistic grammars (~140 inputs, 12+ medium grammars, all backends):**
  every realistic input agreed on accept/reject and tree shape. Only RC6 (`\b` in a
  CSS hex terminal) resurfaced — known.
- **Standalone compile-run differential:** the standalone path is **byte-faithful
  to core by construction** (shared `scanner_plan`, `MatchKind::LeftmostFirst`,
  shared runtime driver). No compile-then-wrong-tree exists; the only standalone
  finds are RC10-class generation-boundary bakes (V1/V2).

## Cross-round note
The dominant round-1 theme (missing validation gates) recurs in N1/N5/N6 but via
**new mechanisms** (dropped directives; un-enforced config/pairing legality) rather
than the duplicate-definition family. The genuinely new *behavioural* classes are
N2 (valid grammar, wrong token), N8 (wrong positions), N7 (binding stack safety),
and N9 (resource growth) — exactly the "subtler" layer round 2 was aimed at.
